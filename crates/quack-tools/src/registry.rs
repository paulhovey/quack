//! The `Tool` trait, the context tools run in, and the registry keyed by internal names.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use futures::future::BoxFuture;
use quack_config::settings::EditFormat;
use quack_protocol::ApprovalRequest;
use quack_shell::ShellKind;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolKind {
    /// Safe to run in parallel with other read-only calls.
    ReadOnly,
    Mutating,
}

/// What a call touches, for the permission engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Subject {
    Read(PathBuf),
    Edit(PathBuf),
    /// One call that changes several files (`apply_patch`, `edit_blocks`).
    Edits(Vec<PathBuf>),
    Shell(String),
    /// A command run outside the OS sandbox while the sandbox is on (or when it is on but
    /// unavailable on this machine). Always asks, whatever the mode.
    ShellOutsideSandbox(String),
    /// Memory notes, outside the workspace.
    Memory {
        write: bool,
    },
    /// Loading an installed skill's instructions.
    Skill,
    /// Turning on deferred MCP tools, or listing MCP resources; changes nothing.
    ToolSearch,
    /// Updating the model's own plan (`todo`); changes nothing.
    Plan,
    /// Starting a sub-agent (`task`). Its own calls are checked one by one; this decides
    /// only whether it may start (plan mode runs only read-only agents).
    Agent {
        read_only: bool,
    },
    /// A tool on an MCP server.
    Mcp {
        server: String,
        tool: String,
        /// The server marks it read-only (a hint; it still asks unless approved).
        read_only: bool,
        /// The server file auto-approves it.
        auto_approve: bool,
    },
}

/// Result of a tool call: what the model sees and what the UI shows.
impl Subject {
    /// The files a call changes.
    pub fn edited(&self) -> &[PathBuf] {
        match self {
            Self::Edit(p) => std::slice::from_ref(p),
            Self::Edits(ps) => ps,
            _ => &[],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutput {
    pub ok: bool,
    /// Model-facing text, already trimmed to budget.
    pub model: String,
    /// One line for the transcript.
    pub summary: String,
    /// Full diff or output for the UI, when it differs from `model`.
    pub detail: Option<String>,
}

impl ToolOutput {
    pub fn ok(model: impl Into<String>, summary: impl Into<String>) -> Self {
        Self {
            ok: true,
            model: model.into(),
            summary: summary.into(),
            detail: None,
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        let message = message.into();
        Self {
            ok: false,
            summary: message.lines().next().unwrap_or_default().to_owned(),
            model: format!("Error: {message}"),
            detail: None,
        }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

/// File stamps recorded when the model reads or writes a file (stale-read guard).
#[derive(Debug, Default)]
pub struct ReadTracker {
    seen: Mutex<HashMap<PathBuf, (Option<SystemTime>, u64)>>,
    /// With hashline edits: each version of a file the model was shown, newest last.
    views: Mutex<HashMap<PathBuf, Vec<View>>>,
}

/// One version of a file the model was shown under a hashline tag: its text, and which
/// lines (1-based, inclusive) a `read`, `grep` or edit result showed of it.
#[derive(Debug, Clone)]
pub struct View {
    pub tag: String,
    pub text: Arc<str>,
    pub shown: Vec<(usize, usize)>,
    /// Only an edit result showed it: the first to go when too many are kept.
    pub edited: bool,
}

impl View {
    pub fn covers(&self, line: usize) -> bool {
        self.shown.iter().any(|(a, b)| (*a..=*b).contains(&line))
    }

    /// Whether any lines of it were shown.
    pub fn covers_any(&self) -> bool {
        self.shown.iter().any(|(a, b)| *a >= 1 && b >= a)
    }
}

/// Versions kept per file: enough to read numbers from a few edits back.
const VIEWS_KEPT: usize = 8;

impl ReadTracker {
    fn stamp(path: &Path) -> Option<(Option<SystemTime>, u64)> {
        let m = std::fs::metadata(path).ok()?;
        Some((m.modified().ok(), m.len()))
    }

    pub fn record(&self, path: &Path) {
        if let (Some(s), Ok(mut seen)) = (Self::stamp(path), self.seen.lock()) {
            seen.insert(path.to_path_buf(), s);
        }
    }

    /// True when the file changed on disk since the model last read or wrote it.
    pub fn is_stale(&self, path: &Path) -> bool {
        let Ok(seen) = self.seen.lock() else {
            return false;
        };
        match seen.get(path) {
            Some(recorded) => Self::stamp(path).is_some_and(|now| now != *recorded),
            None => false,
        }
    }

    /// Note that the model was shown `lines` of `text` (the file's whole, LF-normalized
    /// content) under `tag`. Showing a version again makes it the newest.
    pub fn saw(&self, path: &Path, tag: &str, text: &str, lines: (usize, usize)) {
        self.note(path, tag, text, &[lines], false);
    }

    /// Note the version an edit made, of which its result gave `lines` (the new ones).
    pub fn edited(&self, path: &Path, tag: &str, text: &str, lines: &[(usize, usize)]) {
        self.note(path, tag, text, lines, true);
    }

    fn note(&self, path: &Path, tag: &str, text: &str, lines: &[(usize, usize)], edit: bool) {
        let Ok(mut views) = self.views.lock() else {
            return;
        };
        let list = views.entry(path.to_path_buf()).or_default();
        let mut view = match list.iter().position(|v| v.tag == tag) {
            Some(i) => list.remove(i),
            None => View {
                tag: tag.to_owned(),
                text: text.into(),
                shown: Vec::new(),
                edited: edit,
            },
        };
        view.edited &= edit;
        view.shown.extend_from_slice(lines);
        list.push(view);
        if list.len() > VIEWS_KEPT {
            // Versions only edit results showed go first, keeping those read from.
            let edited = list[..list.len() - 1].iter().position(|v| v.edited);
            list.remove(edited.unwrap_or(0));
        }
    }

    /// The versions of `path` the model was shown, oldest first.
    pub fn views(&self, path: &Path) -> Vec<View> {
        self.views
            .lock()
            .ok()
            .and_then(|v| v.get(path).cloned())
            .unwrap_or_default()
    }
}

/// Everything a tool call may use.
#[derive(Debug, Clone)]
pub struct ToolCtx {
    /// Workspace root; relative paths resolve against it.
    pub root: PathBuf,
    pub shell: ShellKind,
    pub default_timeout: Duration,
    pub max_timeout: Duration,
    pub cancel: CancellationToken,
    /// Live output for the UI (shell).
    pub on_output: Option<mpsc::UnboundedSender<String>>,
    /// Questions for the user (`ask_user`); `None` when no one can answer.
    pub ask: Option<crate::ask::Asker>,
    pub reads: Arc<ReadTracker>,
    /// Memory notes; `None` hides the `memory` tool.
    pub memory: Option<Arc<quack_store::memory::MemoryStore>>,
    /// Skills the `skill` tool can load; empty hides it.
    pub skills: Arc<Vec<quack_instructions::skills::Skill>>,
    /// Language servers; `None` (or none installed) hides the `lsp` tool.
    pub lsp: Option<Arc<quack_lsp::LspManager>>,
    pub sandbox: SandboxState,
    /// The model's plan (`todo`), shown to it with every step.
    pub plan: Arc<crate::todo::Plan>,
    /// Sub-agents the `task` tool can start; empty hides it.
    pub agents: Arc<Vec<crate::task::AgentInfo>>,
    /// Where `task` sends sub-agents to run; `None` when none can start.
    pub spawn: Option<crate::task::Spawner>,
    /// Which edit tool the model gets (never `auto`: resolved at session start).
    pub edit_format: EditFormat,
    /// `read` without a range returns the outline of files longer than this many bytes,
    /// when their language has one. 0: always the content.
    pub outline_above: usize,
    /// Grammars and their `grammars/` query overrides, for outlines.
    pub grammars: Arc<crate::grammar::Grammars>,
}

impl ToolCtx {
    /// Hash-anchored edits: `read` tags lines and `edit_hashline` replaces `edit`.
    pub fn hashline(&self) -> bool {
        self.edit_format == EditFormat::Hashline
    }
}

/// Whether shell commands run in the OS sandbox.
#[derive(Debug, Clone, Default)]
pub enum SandboxState {
    #[default]
    Off,
    On(Arc<quack_sandbox::Policy>),
    /// Configured, but this machine can't provide it (the reason). Commands ask first.
    Unavailable(String),
}

pub trait Tool: Send + Sync {
    /// Internal name (docs/plan/04-agent-loop-and-tools.md#internal-tool-registry).
    fn name(&self) -> &str;
    fn kind(&self) -> ToolKind;
    fn description(&self, ctx: &ToolCtx) -> String;
    /// JSON Schema of the arguments.
    fn schema(&self) -> Value;
    fn subject(&self, args: &Value, ctx: &ToolCtx) -> Result<Subject, String>;
    /// One line for the transcript, e.g. `read src/main.rs`.
    fn summary(&self, args: &Value, ctx: &ToolCtx) -> String;
    /// What the user approves: the diff or the command. `Err` means the call will fail anyway.
    fn preview(&self, args: &Value, ctx: &ToolCtx) -> Result<ApprovalRequest, String>;
    fn run<'a>(&'a self, args: Value, ctx: &'a ToolCtx) -> BoxFuture<'a, ToolOutput>;
    /// Whether to offer the tool in this session.
    fn available(&self, _ctx: &ToolCtx) -> bool {
        true
    }
}

impl std::fmt::Debug for dyn Tool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

#[derive(Debug, Default)]
pub struct Registry {
    tools: Vec<Arc<dyn Tool>>,
}

impl Registry {
    /// The M1 built-in tools.
    pub fn builtin() -> Self {
        let tools: Vec<Arc<dyn Tool>> = vec![
            Arc::new(crate::read::Read),
            Arc::new(crate::search::ListDir),
            Arc::new(crate::search::Glob),
            Arc::new(crate::search::Grep),
            Arc::new(crate::index::Index),
            Arc::new(crate::write::Write),
            Arc::new(crate::edit::StrReplace),
            Arc::new(crate::hashline::Hashline),
            Arc::new(crate::patch::ApplyPatch),
            Arc::new(crate::blocks::EditBlocks),
            Arc::new(crate::shell::Shell),
            Arc::new(crate::memory::Memory),
            Arc::new(crate::skill::LoadSkill),
            Arc::new(crate::lsp::Lsp),
            Arc::new(crate::todo::Todo),
            Arc::new(crate::ask::AskUser),
            Arc::new(crate::task::Task),
        ];
        Self { tools }
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.iter().find(|t| t.name() == name).cloned()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Arc<dyn Tool>> {
        self.tools.iter()
    }
}

/// A context for unit tests: the current directory, no extras.
#[cfg(test)]
pub(crate) fn test_ctx() -> ToolCtx {
    ToolCtx {
        root: PathBuf::from("."),
        shell: ShellKind::detect("sh", None),
        default_timeout: Duration::from_secs(5),
        max_timeout: Duration::from_secs(5),
        cancel: CancellationToken::new(),
        on_output: None,
        ask: None,
        reads: Arc::default(),
        memory: None,
        skills: Arc::default(),
        lsp: None,
        sandbox: SandboxState::Off,
        plan: Arc::default(),
        agents: Arc::default(),
        spawn: None,
        edit_format: EditFormat::StrReplace,
        outline_above: 0,
        grammars: crate::grammar::Grammars::builtin(),
    }
}

// ---- argument helpers shared by the tools ----

/// A string argument, accepting the aliases models commonly use.
pub(crate) fn arg_str<'a>(args: &'a Value, keys: &[&str]) -> Result<&'a str, String> {
    keys.iter()
        .find_map(|k| args.get(*k).and_then(Value::as_str))
        .ok_or_else(|| format!("missing required string argument `{}`", keys[0]))
}

pub(crate) fn opt_str<'a>(args: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|k| args.get(*k).and_then(Value::as_str))
        .filter(|s| !s.is_empty())
}

/// A number argument; models often send numbers as strings.
pub(crate) fn opt_u64(args: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter().find_map(|k| match args.get(*k)? {
        Value::Number(n) => n.as_u64().or_else(|| n.as_f64().map(|f| f.max(0.0) as u64)),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    })
}

pub(crate) fn opt_bool(args: &Value, keys: &[&str]) -> bool {
    keys.iter().any(|k| match args.get(*k) {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => s.eq_ignore_ascii_case("true"),
        _ => false,
    })
}
