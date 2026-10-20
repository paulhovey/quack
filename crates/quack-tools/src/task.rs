//! `task`: start a sub-agent (`agents.d`) on a piece of work. The engine runs it
//! ([`ToolCtx::spawn`]) with a fresh context, and only its final report comes back as the
//! result. A call can name an item of the plan (`todo`), which is marked in progress while
//! the agent works and done when it succeeds.

use futures::future::BoxFuture;
use quack_protocol::ApprovalRequest;
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

use crate::registry::{Subject, Tool, ToolCtx, ToolKind, ToolOutput, arg_str, opt_str};
use crate::todo::Status;

/// An agent the model may start, from `agents.d`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentInfo {
    pub id: String,
    pub description: String,
    pub read_only: bool,
}

/// A sub-agent to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRequest {
    pub agent: String,
    /// A few words for the transcript.
    pub description: String,
    pub prompt: String,
    /// The plan item it carries out, now marked in progress.
    pub todo: Option<String>,
}

/// How a sub-agent's run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskReport {
    /// It finished with a final reply.
    pub ok: bool,
    /// Its final reply, or why it stopped and what it had said so far.
    pub text: String,
    /// One line for the transcript, e.g. `12 steps · 34.5k tokens`.
    pub summary: String,
}

/// Sub-agents on their way to the engine, with the channel for the report.
pub type SpawnRequest = (TaskRequest, oneshot::Sender<TaskReport>);
pub type Spawner = mpsc::UnboundedSender<SpawnRequest>;

#[derive(Debug)]
pub struct Task;

/// The agent a call names: `agent` (or the names other harnesses use), else `general`
/// when it exists, else the first one.
fn agent<'a>(args: &Value, ctx: &'a ToolCtx) -> Result<&'a AgentInfo, String> {
    let list = || {
        ctx.agents
            .iter()
            .map(|a| a.id.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    match opt_str(args, &["agent", "subagent_type", "agent_type", "type"]) {
        Some(name) => ctx
            .agents
            .iter()
            .find(|a| a.id.eq_ignore_ascii_case(name.trim()))
            .ok_or_else(|| format!("unknown agent `{name}`; agents: {}", list())),
        None => ctx
            .agents
            .iter()
            .find(|a| a.id == "general")
            .or_else(|| ctx.agents.first())
            .ok_or_else(|| "no agents are installed".to_owned()),
    }
}

fn prompt(args: &Value) -> Result<&str, String> {
    arg_str(args, &["prompt", "task", "instructions", "message"])
}

/// The call's short description, or the start of its prompt.
fn description(args: &Value) -> String {
    match opt_str(args, &["description", "title", "summary"]) {
        Some(d) => d.trim().to_owned(),
        None => {
            let first = prompt(args)
                .ok()
                .and_then(|p| p.lines().find(|l| !l.trim().is_empty()))
                .unwrap_or_default()
                .trim();
            let short: String = first.chars().take(60).collect();
            if short.len() < first.len() {
                format!("{short}…")
            } else {
                short
            }
        }
    }
}

/// The plan item a call hands over: its number, or its text.
fn todo_item(args: &Value) -> Option<String> {
    match args.get("todo").or_else(|| args.get("todo_item"))? {
        Value::Number(n) => Some(n.to_string()),
        Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_owned()),
        _ => None,
    }
}

impl Tool for Task {
    fn name(&self) -> &str {
        "task"
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Mutating
    }

    fn available(&self, ctx: &ToolCtx) -> bool {
        !ctx.agents.is_empty()
    }

    fn description(&self, ctx: &ToolCtx) -> String {
        let agents: String = ctx
            .agents
            .iter()
            .map(|a| {
                let ro = if a.read_only { " (read-only)" } else { "" };
                format!("\n- {}{ro}: {}", a.id, a.description)
            })
            .collect();
        format!(
            "Start a sub-agent on a piece of work. It has a fresh context and returns only its \
             final report, so delegating keeps your own context small. It cannot see this \
             conversation: put everything it needs in `prompt` (the goal, files and names \
             you already know, what to report). Agents:{agents}\n\nSeveral read-only agents \
             started in one reply run at the same time. In plan mode only read-only agents \
             run. To hand over an item of your todo plan, pass its number as `todo`: it is \
             marked in progress while the agent works and done when it finishes."
        )
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "agent": { "type": "string", "description": "Which agent to start" },
                "description": { "type": "string", "description": "A few words on what it does" },
                "prompt": { "type": "string", "description": "The full task for the agent" },
                "todo": { "type": "integer", "description": "The plan item (1-based) this hands over" }
            },
            "required": ["agent", "prompt"]
        })
    }

    fn subject(&self, args: &Value, ctx: &ToolCtx) -> Result<Subject, String> {
        prompt(args)?;
        let a = agent(args, ctx)?;
        Ok(Subject::Agent {
            read_only: a.read_only,
        })
    }

    fn summary(&self, args: &Value, ctx: &ToolCtx) -> String {
        let name = agent(args, ctx).map_or("agent", |a| a.id.as_str());
        format!("{name}: {}", description(args))
    }

    fn preview(&self, args: &Value, ctx: &ToolCtx) -> Result<ApprovalRequest, String> {
        Ok(ApprovalRequest::Tool {
            name: "task".into(),
            arguments: self.summary(args, ctx),
        })
    }

    fn run<'a>(&'a self, args: Value, ctx: &'a ToolCtx) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            let Some(spawn) = &ctx.spawn else {
                return ToolOutput::error("sub-agents cannot start here");
            };
            let (a, text) = match (agent(&args, ctx), prompt(&args)) {
                (Ok(a), Ok(p)) => (a, p.to_owned()),
                (Err(e), _) | (_, Err(e)) => return ToolOutput::error(e),
            };
            let item = match todo_item(&args) {
                Some(sel) => match ctx.plan.mark(&sel, Status::InProgress) {
                    Ok(t) => Some(t),
                    Err(e) => return ToolOutput::error(e),
                },
                None => None,
            };
            let request = TaskRequest {
                agent: a.id.clone(),
                description: description(&args),
                prompt: text,
                todo: item.clone(),
            };
            let (tx, rx) = oneshot::channel();
            let report = match spawn.send((request, tx)) {
                Ok(()) => tokio::select! {
                    r = rx => r.ok(),
                    () = ctx.cancel.cancelled() => None,
                },
                Err(_) => None,
            };
            let ok = report.as_ref().is_some_and(|r| r.ok);
            let mut note = String::new();
            if let Some(t) = &item {
                let status = if ok { Status::Done } else { Status::Pending };
                if ctx.plan.mark(t, status).is_ok() {
                    note = if ok {
                        format!("\n\nPlan item marked done: {t}")
                    } else {
                        format!("\n\nPlan item back to pending: {t}")
                    };
                }
            }
            let Some(report) = report else {
                return ToolOutput::error(format!("the {} agent was interrupted{note}", a.id));
            };
            if report.ok {
                ToolOutput::ok(
                    format!("Report from the {} agent:\n\n{}{note}", a.id, report.text),
                    report.summary,
                )
                .with_detail(report.text)
            } else {
                ToolOutput {
                    ok: false,
                    model: format!(
                        "Error: the {} agent did not finish. {}{note}",
                        a.id, report.text
                    ),
                    summary: report.summary,
                    detail: Some(report.text),
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ToolCtx {
        let mut c = crate::registry::test_ctx();
        c.agents = std::sync::Arc::new(vec![
            AgentInfo {
                id: "explore".into(),
                description: "search".into(),
                read_only: true,
            },
            AgentInfo {
                id: "general".into(),
                description: "work".into(),
                read_only: false,
            },
        ]);
        c
    }

    #[test]
    fn picks_the_agent_leniently() {
        let c = ctx();
        let pick = |a: Value| agent(&a, &c).map(|a| a.id.clone());
        assert_eq!(
            pick(json!({"subagent_type": "Explore"})),
            Ok("explore".into())
        );
        assert_eq!(pick(json!({})), Ok("general".into()));
        assert!(pick(json!({"agent": "nope"})).is_err());
        assert_eq!(
            Task.subject(&json!({"agent": "explore", "prompt": "p"}), &c),
            Ok(Subject::Agent { read_only: true })
        );
        assert!(Task.subject(&json!({"agent": "explore"}), &c).is_err());
    }

    #[test]
    fn description_falls_back_to_the_prompt() {
        assert_eq!(
            description(&json!({"prompt": "\nfind the parser\nand more"})),
            "find the parser"
        );
        assert_eq!(description(&json!({"description": " d "})), "d");
    }

    #[tokio::test]
    async fn marks_the_plan_item_and_returns_the_report() {
        let (tx, mut rx) = mpsc::unbounded_channel::<SpawnRequest>();
        let mut c = ctx();
        c.spawn = Some(tx);
        c.plan.set(vec![
            ("read".into(), Status::Done),
            ("fix the parser".into(), Status::Pending),
        ]);
        let plan = c.plan.clone();
        let engine = tokio::spawn(async move {
            let (req, reply) = rx.recv().await.expect("request");
            assert_eq!(req.agent, "general");
            assert!(plan.checklist().contains("[~] fix the parser"));
            let _ = reply.send(TaskReport {
                ok: true,
                text: "fixed it".into(),
                summary: "2 steps".into(),
            });
        });
        let out = Task
            .run(json!({"agent": "general", "prompt": "fix", "todo": 2}), &c)
            .await;
        engine.await.expect("engine");
        assert!(out.ok, "{out:?}");
        assert!(out.model.contains("fixed it") && out.model.contains("marked done"));
        assert_eq!(c.plan.checklist(), "[x] read\n[x] fix the parser");
    }

    #[tokio::test]
    async fn a_failed_agent_puts_the_item_back() {
        let (tx, mut rx) = mpsc::unbounded_channel::<SpawnRequest>();
        let mut c = ctx();
        c.spawn = Some(tx);
        c.plan.set(vec![("fix".into(), Status::Pending)]);
        tokio::spawn(async move {
            let (_, reply) = rx.recv().await.expect("request");
            let _ = reply.send(TaskReport {
                ok: false,
                text: "stopped after 50 steps".into(),
                summary: "stopped".into(),
            });
        });
        let out = Task
            .run(
                json!({"agent": "general", "prompt": "fix", "todo": "fix"}),
                &c,
            )
            .await;
        assert!(!out.ok);
        assert_eq!(c.plan.checklist(), "[ ] fix");
        let bad = Task
            .run(json!({"agent": "general", "prompt": "x", "todo": 9}), &c)
            .await;
        assert!(!bad.ok && bad.model.contains("no plan item 9"), "{bad:?}");
    }
}
