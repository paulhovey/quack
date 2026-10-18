//! `memory`: markdown notes kept across sessions (docs/plan/07-sessions-memory-checkpoints.md#memory).

use quack_protocol::ApprovalRequest;
use quack_store::memory::{KINDS, MemoryStore, Scope};
use futures::future::BoxFuture;
use serde_json::{Value, json};

use crate::registry::{Subject, Tool, ToolCtx, ToolKind, ToolOutput, opt_str};

#[derive(Debug)]
pub struct Memory;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    View,
    Create,
    Update,
    Delete,
}

fn action(args: &Value) -> Result<Action, String> {
    match opt_str(args, &["action", "command"]).unwrap_or("view") {
        "view" | "list" | "read" => Ok(Action::View),
        "create" | "add" | "save" => Ok(Action::Create),
        "update" | "edit" => Ok(Action::Update),
        "delete" | "remove" => Ok(Action::Delete),
        other => Err(format!(
            "unknown action `{other}`; use view, create, update or delete"
        )),
    }
}

/// `None` when the model did not say; reads then look in both scopes.
fn scope(args: &Value) -> Result<Option<Scope>, String> {
    match opt_str(args, &["scope"]) {
        None => Ok(None),
        Some(s) => Scope::parse(s)
            .map(Some)
            .ok_or_else(|| format!("unknown scope `{s}`; use project or global")),
    }
}

fn store(ctx: &ToolCtx) -> Result<&MemoryStore, String> {
    ctx.memory
        .as_deref()
        .ok_or_else(|| "memory is not available in this session".to_owned())
}

impl Tool for Memory {
    fn name(&self) -> &str {
        "memory"
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Mutating
    }

    fn description(&self, _ctx: &ToolCtx) -> String {
        "Long-term memory: markdown notes kept across sessions. The index of saved notes is \
         in the system prompt. `view` lists notes, or reads one with `name`. `create` and \
         `update` write a note (`name`, a one-line `description`, `type`, `content`); `delete` \
         removes one. Scope `project` (default) is for this repository, `global` for every \
         project. Save what will matter in later sessions: the user's preferences and \
         corrections, project facts that are not written down in the repository. Never save \
         secrets, and nothing that is already in the code or docs."
            .into()
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["view", "create", "update", "delete"] },
                "scope": { "type": "string", "enum": ["project", "global"] },
                "name": { "type": "string", "description": "Note name: lowercase letters, digits and dashes" },
                "description": { "type": "string", "description": "One line, shown in the index" },
                "type": { "type": "string", "enum": KINDS },
                "content": { "type": "string", "description": "The note, in markdown" }
            },
            "required": ["action"]
        })
    }

    fn subject(&self, args: &Value, _ctx: &ToolCtx) -> Result<Subject, String> {
        Ok(Subject::Memory {
            write: action(args)? != Action::View,
        })
    }

    fn summary(&self, args: &Value, _ctx: &ToolCtx) -> String {
        let verb = match action(args) {
            Ok(Action::View) => "view",
            Ok(Action::Create) => "create",
            Ok(Action::Update) => "update",
            Ok(Action::Delete) => "delete",
            Err(_) => "?",
        };
        match opt_str(args, &["name"]) {
            Some(n) => format!("memory {verb} {n}"),
            None => format!("memory {verb}"),
        }
    }

    fn preview(&self, args: &Value, _ctx: &ToolCtx) -> Result<ApprovalRequest, String> {
        action(args)?;
        scope(args)?;
        Ok(ApprovalRequest::Tool {
            name: "memory".into(),
            arguments: serde_json::to_string_pretty(args).unwrap_or_default(),
        })
    }

    fn available(&self, ctx: &ToolCtx) -> bool {
        ctx.memory.is_some()
    }

    fn run<'a>(&'a self, args: Value, ctx: &'a ToolCtx) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            match run(&args, ctx) {
                Ok(out) => out,
                Err(e) => ToolOutput::error(e),
            }
        })
    }
}

/// Models often send titles ("Code style"); turn them into note names.
fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let mut out: String = out.trim_end_matches('-').chars().take(64).collect();
    while out.ends_with('-') {
        out.pop();
    }
    out
}

fn run(args: &Value, ctx: &ToolCtx) -> Result<ToolOutput, String> {
    let m = store(ctx)?;
    let act = action(args)?;
    let scope = scope(args)?;
    let name = opt_str(args, &["name", "title"]).map(slug);
    let name = name.as_deref().filter(|n| !n.is_empty());
    if act == Action::View {
        return Ok(match name {
            None => list(m, scope),
            Some(n) => {
                let found = match scope {
                    Some(s) => m.read(s, n).map(|t| (s, t)),
                    None => m
                        .read(Scope::Project, n)
                        .map(|t| (Scope::Project, t))
                        .or_else(|_| m.read(Scope::Global, n).map(|t| (Scope::Global, t))),
                };
                let (s, text) = found?;
                ToolOutput::ok(text, format!("read {} note {n}", s.label()))
            }
        });
    }
    let name = name.ok_or("missing required string argument `name`")?;
    let scope = scope.unwrap_or(Scope::Project);
    let exists = m.exists(scope, name);
    match act {
        Action::Create if exists => Err(format!(
            "a {} note named `{name}` exists; use update",
            scope.label()
        )),
        Action::Update | Action::Delete if !exists => {
            Err(format!("no {} note named `{name}`", scope.label()))
        }
        Action::Delete => {
            m.delete(scope, name)?;
            Ok(ToolOutput::ok(
                format!("Deleted {} note `{name}`.", scope.label()),
                format!("deleted {} note {name}", scope.label()),
            ))
        }
        _ => {
            // Small models often send only one of the two; each can stand in for the other.
            let content = opt_str(args, &["content", "body", "text"]);
            let description = opt_str(args, &["description", "summary"]);
            let content = content
                .or(description)
                .ok_or("missing required string argument `content`")?;
            let description =
                description.unwrap_or_else(|| content.lines().next().unwrap_or_default());
            let kind = opt_str(args, &["type", "kind"]).unwrap_or(match scope {
                Scope::Project => "project",
                Scope::Global => "user",
            });
            let path = m.write(scope, name, description, kind, content)?;
            let verb = if exists { "updated" } else { "saved" };
            Ok(ToolOutput::ok(
                format!("{verb} {} note `{name}`.", scope.label()),
                format!("{verb} {} note {name}", scope.label()),
            )
            .with_detail(format!("{}\n\n{content}", path.display())))
        }
    }
}

fn list(m: &MemoryStore, only: Option<Scope>) -> ToolOutput {
    let mut out = String::new();
    let mut count = 0;
    for s in [Scope::Project, Scope::Global] {
        if only.is_some_and(|o| o != s) {
            continue;
        }
        let notes = m.list(s);
        count += notes.len();
        if notes.is_empty() {
            continue;
        }
        out.push_str(&format!("{} notes:\n", s.label()));
        for (name, desc) in notes {
            out.push_str(&format!("- {name}: {desc}\n"));
        }
    }
    if out.is_empty() {
        out = "No notes yet.".into();
    }
    ToolOutput::ok(out.trim_end().to_owned(), format!("{count} notes"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn ctx(dir: &std::path::Path) -> ToolCtx {
        ToolCtx {
            root: dir.to_path_buf(),
            shell: quack_shell::ShellKind::detect("auto", None),
            default_timeout: std::time::Duration::from_secs(5),
            max_timeout: std::time::Duration::from_secs(5),
            cancel: Default::default(),
            on_output: None,
            reads: Default::default(),
            memory: Some(Arc::new(MemoryStore::new(dir.join("g"), dir.join("p")))),
            skills: Default::default(),
        }
    }

    #[test]
    fn create_view_update_delete() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let c = ctx(tmp.path());
        let call = |a: Value| run(&a, &c);
        let made = call(json!({"action":"create","name":"uses-pnpm","description":"Use pnpm, not npm","content":"Always pnpm."}))
            .expect("create");
        assert!(made.ok && made.model.starts_with("saved project note"));
        assert!(
            call(json!({"action":"create","name":"uses-pnpm","description":"d","content":"c"}))
                .is_err()
        );
        let listed = call(json!({"action":"view"})).expect("list");
        assert_eq!(
            listed.model,
            "project notes:\n- uses-pnpm: Use pnpm, not npm"
        );
        let read = call(json!({"action":"view","name":"uses-pnpm"})).expect("read");
        assert!(read.model.contains("Always pnpm."));
        call(json!({"action":"update","name":"uses-pnpm","description":"pnpm 9","content":"v9"}))
            .expect("update");
        assert!(call(json!({"action":"update","scope":"global","name":"uses-pnpm","description":"d","content":"c"})).is_err());
        call(json!({"action":"delete","name":"uses-pnpm"})).expect("delete");
        assert_eq!(call(json!({})).expect("list").model, "No notes yet.");
    }

    #[test]
    fn lenient_names_and_fields() {
        assert_eq!(slug("Code style preference!"), "code-style-preference");
        assert_eq!(slug("  --x__y--  "), "x-y");
        let tmp = tempfile::tempdir().expect("tempdir");
        let c = ctx(tmp.path());
        let out = run(
            &json!({"action":"create","name":"Code style","description":"Use 4 spaces"}),
            &c,
        )
        .expect("create");
        assert!(out.ok, "{}", out.model);
        let note = run(&json!({"action":"view","name":"code-style"}), &c).expect("view");
        assert!(note.model.ends_with("Use 4 spaces\n"));
        assert!(run(&json!({"action":"create","name":"!!","content":"x"}), &c).is_err());
    }

    #[test]
    fn writes_are_a_separate_subject() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let c = ctx(tmp.path());
        let s = |a: Value| Memory.subject(&a, &c).expect("subject");
        assert_eq!(
            s(json!({"action":"view"})),
            Subject::Memory { write: false }
        );
        assert_eq!(
            s(json!({"action":"delete"})),
            Subject::Memory { write: true }
        );
    }
}
