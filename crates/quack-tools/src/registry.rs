//! The `Tool` trait, the context tools run in, and the registry keyed by internal names.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use futures::future::BoxFuture;
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
    /// Turning on deferred MCP tools; changes nothing.
    ToolSearch,
    /// Updating the model's own plan (`todo`); changes nothing.
    Plan,
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
}

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
    /// Hash-anchored edits: `read` tags lines and `edit_hashline` replaces `edit`.
    pub hashline: bool,
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
            Arc::new(crate::write::Write),
            Arc::new(crate::edit::StrReplace),
            Arc::new(crate::hashline::Hashline),
            Arc::new(crate::shell::Shell),
            Arc::new(crate::memory::Memory),
            Arc::new(crate::skill::LoadSkill),
            Arc::new(crate::lsp::Lsp),
            Arc::new(crate::todo::Todo),
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
