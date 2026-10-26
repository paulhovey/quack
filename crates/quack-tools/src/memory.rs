//! `memory`: markdown notes kept across sessions (docs/plan/07-sessions-memory-checkpoints.md#memory).

use futures::future::BoxFuture;
use quack_protocol::ApprovalRequest;
use quack_store::memory::{KINDS, MemoryStore, Scope, fill_warning};
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
         shown at the start of the session. `view` lists notes, or reads one with `name`. \
         `create` and `update` write a note (`name`, a one-line `description`, `type`, \
         `content`); `delete` removes one. Scope `project` (default) is for this repository, \
         `global` for every project.\n\
         Save a note when the user corrects how you work, states a lasting preference \
         (\"always\", \"never\", \"from now on\", \"I prefer\"), accepts an approach after \
         pushing back, or when you learn a project fact that is not written down anywhere. \
         Say why in the note, so a later session can judge cases it doesn't cover. Update \
         the note that covers a topic instead of adding a near-duplicate. Never save secrets, \
         what the code or docs already say, or details that only matter to the current task. \
         A note is as old as its `updated` date: check that a file, function or flag it \
         names still exists before relying on it."
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
                let (s, mut text) = found?;
                // Global notes are about every project, so their paths need not be here.
                let missing = if s == Scope::Project {
                    missing_paths(&text, &ctx.root)
                } else {
                    Vec::new()
                };
                if !missing.is_empty() {
                    text.push_str(&format!(
                        "\n[Not in the workspace now: {}. Check the note is still true before \
                         relying on it, then update or delete it.]",
                        missing.join(", ")
                    ));
                }
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
            let mut said = format!("{verb} {} note `{name}`.", scope.label());
            if let Some(w) = fill_warning(m.index_fill()) {
                said.push_str(&format!(
                    " The memory {w}; merge notes that overlap and delete stale ones."
                ));
            }
            Ok(
                ToolOutput::ok(said, format!("{verb} {} note {name}", scope.label()))
                    .with_detail(format!("{}\n\n{content}", path.display())),
            )
        }
    }
}

/// Paths a note names in backticks that are not in the workspace now. Only words with a
/// path separator count, so `foo.bar()` and `--flag` are left alone.
fn missing_paths(note: &str, root: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    for (i, word) in note.split('`').enumerate() {
        // Odd pieces are inside backticks.
        let looks_like_path = i % 2 == 1
            && (word.contains('/') || word.contains('\\'))
            && !word.contains("://")
            && !word.starts_with('-')
            && !word
                .chars()
                .any(|c| c.is_whitespace() || "*?<>{}$~|\"'()".contains(c));
        if !looks_like_path {
            continue;
        }
        // `src/main.rs:12` names a line.
        let path = word.trim_end_matches(|c: char| c.is_ascii_digit() || c == ':');
        let path = path.strip_prefix("./").unwrap_or(path);
        let shown = format!("`{word}`");
        if !path.is_empty() && !root.join(path).exists() && !out.contains(&shown) {
            out.push(shown);
        }
    }
    out
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
            ask: None,
            reads: Default::default(),
            memory: Some(Arc::new(MemoryStore::new(dir.join("g"), dir.join("p")))),
            skills: Default::default(),
            lsp: None,
            sandbox: Default::default(),
            plan: Default::default(),
            agents: Default::default(),
            spawn: None,
            edit_format: quack_config::settings::EditFormat::StrReplace,
            vision: false,
            outline_above: 0,
            grammars: crate::grammar::Grammars::builtin(),
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

    #[test]
    fn reading_a_note_flags_paths_that_are_gone() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let c = ctx(tmp.path());
        std::fs::create_dir_all(tmp.path().join("src")).expect("mkdir");
        std::fs::write(tmp.path().join("src/main.rs"), "").expect("write");
        let body = "Entry is `src/main.rs:3`; config in `src/old.rs` and `./conf/x.toml`. \
                    Not paths: `foo.bar()`, `a b/c`, `https://x/y`, `--out/x`.";
        run(
            &json!({"action":"create","name":"layout","description":"d","content":body}),
            &c,
        )
        .expect("create");
        let read = run(&json!({"action":"view","name":"layout"}), &c).expect("view");
        assert!(
            read.model.ends_with(
                "[Not in the workspace now: `src/old.rs`, `./conf/x.toml`. Check the note is \
                 still true before relying on it, then update or delete it.]"
            ),
            "{}",
            read.model
        );
    }

    #[test]
    fn a_full_index_asks_for_merging() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut c = ctx(tmp.path());
        c.memory = Some(Arc::new(
            MemoryStore::new(tmp.path().join("g"), tmp.path().join("p")).with_index_cap(100),
        ));
        let first = run(&json!({"action":"create","name":"a","content":"short"}), &c).expect("a");
        assert_eq!(first.model, "saved project note `a`.");
        let second = run(
            &json!({"action":"create","name":"b","content":"a longer description of b"}),
            &c,
        )
        .expect("b");
        assert!(
            second.model.contains("The memory index ")
                && second.model.ends_with("delete stale ones."),
            "{}",
            second.model
        );
    }
}
