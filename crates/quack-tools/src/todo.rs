//! `todo`: the model's plan for the task, a short checklist it keeps up to date. The
//! current plan is shown to the model again with every step ([`Plan::reminder`]), so it
//! survives long tool output and compaction.

use std::sync::Mutex;

use futures::future::BoxFuture;
use quack_protocol::ApprovalRequest;
use serde_json::{Value, json};

use crate::registry::{Subject, Tool, ToolCtx, ToolKind, ToolOutput};

const MAX_ITEMS: usize = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Pending,
    InProgress,
    Done,
}

impl Status {
    fn parse(s: &str) -> Self {
        match s
            .trim()
            .to_ascii_lowercase()
            .replace(['-', ' '], "_")
            .as_str()
        {
            "in_progress" | "active" | "doing" | "current" | "started" => Self::InProgress,
            "done" | "completed" | "complete" | "finished" | "x" => Self::Done,
            _ => Self::Pending,
        }
    }

    fn mark(self) -> &'static str {
        match self {
            Self::Pending => "[ ]",
            Self::InProgress => "[~]",
            Self::Done => "[x]",
        }
    }
}

/// The session's plan.
#[derive(Debug, Default)]
pub struct Plan {
    items: Mutex<Vec<(String, Status)>>,
}

impl Plan {
    fn set(&self, items: Vec<(String, Status)>) {
        if let Ok(mut i) = self.items.lock() {
            *i = items;
        }
    }

    fn render(items: &[(String, Status)]) -> String {
        items
            .iter()
            .map(|(t, s)| format!("{} {t}", s.mark()))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The plan to show the model with the next step, while anything is left to do.
    pub fn reminder(&self) -> Option<String> {
        let items = self.items.lock().ok()?;
        if items.is_empty() || items.iter().all(|(_, s)| *s == Status::Done) {
            return None;
        }
        Some(format!(
            "[Your plan; update it with the todo tool as you go]\n{}",
            Self::render(&items)
        ))
    }
}

#[derive(Debug)]
pub struct Todo;

/// Items from `{"items": [{"text", "status"}]}`, accepting the shapes models send:
/// `todos`/`plan`/`steps`, `content`/`task`/`step`, or plain strings.
fn items(args: &Value) -> Result<Vec<(String, Status)>, String> {
    let list = ["items", "todos", "plan", "steps", "tasks"]
        .iter()
        .find_map(|k| args.get(*k))
        .ok_or("missing required argument `items`")?;
    let list = match list {
        // Some models send the array as a JSON string.
        Value::String(s) => serde_json::from_str::<Value>(s)
            .map_err(|_| "`items` must be an array of {text, status}".to_owned())?,
        v => v.clone(),
    };
    let arr = list
        .as_array()
        .ok_or("`items` must be an array of {text, status}")?;
    let mut out = Vec::new();
    for v in arr.iter().take(MAX_ITEMS) {
        let (text, status) = match v {
            Value::String(s) => (s.clone(), Status::Pending),
            Value::Object(o) => {
                let text = ["text", "content", "task", "step", "title", "description"]
                    .iter()
                    .find_map(|k| o.get(*k).and_then(Value::as_str))
                    .unwrap_or_default()
                    .to_owned();
                let status = o
                    .get("status")
                    .and_then(Value::as_str)
                    .map_or(Status::Pending, Status::parse);
                let status = match o.get("done").or_else(|| o.get("completed")) {
                    Some(Value::Bool(true)) => Status::Done,
                    _ => status,
                };
                (text, status)
            }
            _ => continue,
        };
        let text = text.trim().replace('\n', " ");
        if !text.is_empty() {
            out.push((text, status));
        }
    }
    Ok(out)
}

impl Tool for Todo {
    fn name(&self) -> &str {
        "todo"
    }

    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }

    fn description(&self, _ctx: &ToolCtx) -> String {
        "Keep a short plan for the task as a checklist. For work with several steps, write \
         the plan before starting, mark one item in_progress while you work on it, and mark \
         items done as you finish them. Each call replaces the whole list. The current plan \
         is shown to you with every step."
            .into()
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "items": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "text": { "type": "string" },
                            "status": { "type": "string", "enum": ["pending", "in_progress", "done"] }
                        },
                        "required": ["text", "status"]
                    }
                }
            },
            "required": ["items"]
        })
    }

    fn subject(&self, _args: &Value, _ctx: &ToolCtx) -> Result<Subject, String> {
        Ok(Subject::Plan)
    }

    fn summary(&self, args: &Value, _ctx: &ToolCtx) -> String {
        match items(args) {
            Ok(i) => {
                let done = i.iter().filter(|(_, s)| *s == Status::Done).count();
                let current = i
                    .iter()
                    .find(|(_, s)| *s == Status::InProgress)
                    .map(|(t, _)| format!(" · {t}"))
                    .unwrap_or_default();
                format!("plan {done}/{} done{current}", i.len())
            }
            Err(_) => "plan".into(),
        }
    }

    fn preview(&self, args: &Value, ctx: &ToolCtx) -> Result<ApprovalRequest, String> {
        Ok(ApprovalRequest::Tool {
            name: "todo".into(),
            arguments: self.summary(args, ctx),
        })
    }

    fn run<'a>(&'a self, args: Value, ctx: &'a ToolCtx) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            let list = match items(&args) {
                Ok(l) => l,
                Err(e) => return ToolOutput::error(e),
            };
            let text = Plan::render(&list);
            let summary = self.summary(&args, ctx);
            ctx.plan.set(list);
            if text.is_empty() {
                return ToolOutput::ok("Plan cleared.", "plan cleared");
            }
            ToolOutput::ok(format!("Plan updated:\n{text}"), summary).with_detail(text)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_the_shapes_models_send() {
        let a = json!({"todos": [
            {"content": "read the code", "status": "completed"},
            {"task": "fix the bug", "status": "in-progress"},
            "run the tests"
        ]});
        let i = items(&a).expect("items");
        assert_eq!(
            i,
            vec![
                ("read the code".to_owned(), Status::Done),
                ("fix the bug".to_owned(), Status::InProgress),
                ("run the tests".to_owned(), Status::Pending),
            ]
        );
        let s = json!({"items": "[{\"text\": \"a\", \"done\": true}]"});
        assert_eq!(
            items(&s).expect("string"),
            vec![("a".to_owned(), Status::Done)]
        );
        assert!(items(&json!({})).is_err());
    }

    #[test]
    fn reminder_while_work_is_left() {
        let p = Plan::default();
        assert_eq!(p.reminder(), None);
        p.set(vec![
            ("read".into(), Status::Done),
            ("fix".into(), Status::InProgress),
        ]);
        assert_eq!(
            p.reminder().as_deref(),
            Some("[Your plan; update it with the todo tool as you go]\n[x] read\n[~] fix")
        );
        p.set(vec![("fix".into(), Status::Done)]);
        assert_eq!(p.reminder(), None);
    }
}
