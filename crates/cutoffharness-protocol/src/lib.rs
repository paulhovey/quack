//! Engine protocol: clients send [`Op`]s, the engine emits [`Event`]s.
//!
//! Modelled on codex's submission/event split (docs/plan/00-architecture.md#engine-protocol).
//! This crate has no async-runtime dependency so it can be used by ACP translation,
//! headless output and tests without pulling in tokio.

mod ids;
mod startup;

pub use ids::{CallId, CheckpointId, EventIndex, ModelId, ProviderId, SessionId, TurnId};
pub use startup::{LoadState, Subsystem};

use serde::{Deserialize, Serialize};

/// A request from a client to the engine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    UserTurn {
        items: Vec<InputItem>,
        #[serde(default)]
        overrides: TurnOverrides,
    },
    Interrupt,
    ApprovalResponse {
        call_id: CallId,
        decision: ApprovalDecision,
    },
    SetModel {
        provider: ProviderId,
        model: ModelId,
    },
    /// Ask for the model catalogue; answered with `Event::ModelCatalog`.
    ListModels,
    /// Summarize older history now. `focus` says what the summary should keep.
    Compact {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        focus: Option<String>,
    },
    /// Revert the files changed by the last edit call. `force` also overwrites files that
    /// changed again since.
    Undo {
        checkpoint: Option<CheckpointId>,
        #[serde(default)]
        force: bool,
    },
    /// Switch to a saved session (the current one stays saved).
    ResumeSession {
        id: SessionId,
    },
    /// Start a fresh session (the current one stays saved).
    NewSession,
    /// Ask for this project's saved sessions; answered with `Event::SessionList`.
    ListSessions,
    /// Ask what is using the context; answered with `Event::ContextReport`.
    GetContext,
    /// `read-only`, `ask`, `auto-edit`, `full-auto` (capped by policy), or `plan`.
    SetPermissionMode {
        mode: String,
    },
    /// Run the environment checks; answered with `Event::DoctorReport`.
    RunDoctor,
    /// Ask for the MCP servers' state; answered with `Event::McpServers`.
    ListMcp,
    /// Stop and start an MCP server again.
    RestartMcp {
        id: String,
    },
    /// Look for model servers on this machine (loopback ports only); answered with
    /// `Event::ServersDetected`.
    DetectServers,
    /// Write `providers.d/<id>.toml` to the user layer (unless a provider with this
    /// `base_url` exists), then switch to `model`. It becomes `model.default` when none is set.
    AddProvider {
        id: String,
        base_url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        api: Option<String>,
        model: String,
    },
    /// Ask for the memory notes; answered with `Event::MemoryNotes`.
    ListMemory,
    /// Delete a memory note. `scope` is `global` or `project`.
    DeleteMemory {
        scope: String,
        name: String,
    },
    SetTitle {
        title: String,
    },
    /// Write a setting to a layer's `config.toml`. `value` is TOML, falling back to a string.
    SaveSetting {
        key: String,
        value: String,
        scope: WriteScope,
    },
    ForkSession {
        at: EventIndex,
    },
    /// Ask for the messages the conversation can go back to; answered with
    /// `Event::RewindPoints`.
    ListRewindPoints,
    /// Go back to before the user message `back` messages from the end (1 = the last one).
    /// `conversation` drops it and everything after; `files` puts back the files changed
    /// since by write and edit calls (`force`: also files changed again since).
    Rewind {
        back: u32,
        conversation: bool,
        files: bool,
        #[serde(default)]
        force: bool,
    },
    /// Read the config files again and rebuild the session around them, keeping the
    /// conversation and the model in use. Waits for a running turn.
    ReloadConfig,
    /// Record a decision about this workspace's `.cutoffharness/` (`trusted`,
    /// `settings-only` or `ignored`), then reload.
    TrustProject {
        decision: String,
    },
    Shutdown,
}

/// A notification from the engine to its clients.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    // lifecycle
    StartupProgress {
        subsystem: Subsystem,
        state: LoadState,
    },
    StartupComplete,
    SessionConfigured {
        session: SessionInfo,
    },
    /// Every configured provider with its models (declared and discovered).
    ModelCatalog {
        providers: Vec<ProviderModels>,
        /// `<provider>/<model>` in use, if any.
        current: Option<String>,
    },
    /// A saved session was loaded; `items` is its transcript for display.
    SessionResumed {
        session: SessionInfo,
        items: Vec<ReplayItem>,
    },
    /// This project's saved sessions, most recent first.
    SessionList {
        sessions: Vec<SessionSummary>,
    },
    /// A setting was written to `path`; `note` says if something still overrides it.
    SettingSaved {
        key: String,
        value: String,
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    ShutdownComplete,
    /// The config files were read again; startup progress follows, then `StartupComplete`.
    ConfigReloaded,
    /// The workspace has a `.cutoffharness/` that needs a trust decision (`Op::TrustProject`).
    /// `summary` says what it holds; `changed` when it was trusted before and changed since.
    ProjectTrust {
        path: String,
        summary: String,
        changed: bool,
    },
    // turn
    TurnStarted {
        turn: TurnId,
    },
    TurnQueued {
        waiting_for: String,
    },
    AssistantDelta {
        text: String,
    },
    ReasoningDelta {
        text: String,
    },
    /// Replaces the text streamed so far in this step, e.g. after tool calls printed as
    /// text were turned into real calls.
    AssistantRewrite {
        text: String,
    },
    ToolCallBegin {
        call: ToolCallInfo,
    },
    ToolCallOutputDelta {
        call_id: CallId,
        chunk: String,
    },
    ToolCallEnd {
        call_id: CallId,
        result: ToolResultSummary,
    },
    ApprovalRequest {
        call_id: CallId,
        request: ApprovalRequest,
    },
    TurnComplete {
        turn: TurnId,
        usage: Usage,
    },
    TurnAborted {
        turn: TurnId,
        reason: String,
    },
    // context
    ContextUpdate {
        used: u32,
        budget: u32,
        cache_hit_tokens: Option<u32>,
    },
    ContextPruned {
        report: PruneReport,
    },
    /// Timing of one model call: time to the first token, and generation speed after it.
    Speed {
        ttft_ms: u32,
        /// Output tokens per second; `None` when the reply was too short to measure.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tokens_per_sec: Option<f32>,
    },
    /// A summary is being written; `reason` says why (e.g. `/compact`, the threshold).
    Compacting {
        reason: String,
    },
    /// What is using the context, segment by segment.
    ContextReport {
        report: ContextReport,
    },
    MemoryNotes {
        notes: Vec<MemoryNote>,
    },
    DoctorReport {
        checks: Vec<DoctorCheck>,
    },
    McpServers {
        servers: Vec<McpServerInfo>,
    },
    ServersDetected {
        servers: Vec<DetectedServer>,
    },
    /// A provider file was written (`path`), or an existing one reused (`path` is `None`).
    ProviderAdded {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },
    /// Skills the model can load, and the user can run as `/<name> [args]`.
    Skills {
        skills: Vec<SkillInfo>,
    },
    /// User messages `/rewind` can go back to, most recent first.
    RewindPoints {
        points: Vec<RewindPoint>,
    },
    /// The conversation went back to before a user message (`text`, so a client can offer
    /// it for editing). Files are reported by `FilesRestored`.
    Rewound {
        back: u32,
        text: String,
    },
    /// Files put back by `/undo`; `conflicts` changed since and were left alone.
    FilesRestored {
        restored: Vec<String>,
        conflicts: Vec<String>,
    },
    /// The session's permission mode, e.g. `ask` or `plan`.
    PermissionMode {
        mode: String,
    },
    Compacted {
        summary_tokens: u32,
        /// History messages the summary replaced.
        #[serde(default)]
        replaced: u32,
    },
    // diagnostics
    Warning {
        message: String,
    },
    Error {
        error: EngineError,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InputItem {
    Text { text: String },
    Image { path: String },
    File { path: String },
}

impl InputItem {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }
}

/// Per-turn overrides of session settings.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnOverrides {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WriteScope {
    Session,
    Project,
    User,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum ApprovalDecision {
    AllowOnce,
    AllowSession,
    AlwaysAllow { scope: WriteScope },
    Deny,
    DenyWithFeedback { feedback: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ApprovalRequest {
    Shell { command: String, cwd: String },
    Edit { path: String, diff: String },
    Tool { name: String, arguments: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallInfo {
    pub call_id: CallId,
    pub name: String,
    /// One-line description for the transcript, e.g. `edit src/main.rs`.
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolResultSummary {
    pub ok: bool,
    pub summary: String,
    /// Full diff or output for display.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderModels {
    pub id: String,
    /// False when the provider cannot be used right now (missing key, unreachable).
    pub available: bool,
    /// Why it is unavailable, or a short status.
    pub note: String,
    pub models: Vec<ModelInfo>,
    /// From the defaults bundle and not changed by the user: when unavailable it is an
    /// offer (a key not set, a server not running), not a problem.
    #[serde(default)]
    pub bundled: bool,
}

/// An MCP server from `mcp.d/` and its state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerInfo {
    pub id: String,
    /// `stdio` or `http`.
    pub transport: String,
    /// The command line or the URL.
    pub target: String,
    pub description: String,
    /// `starting`, `ready`, `failed` or `stopped`.
    pub state: String,
    /// Why it failed.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub error: String,
    pub tools: Vec<String>,
    /// Whether its tools are behind `tool_search` to save context.
    pub deferred: bool,
    /// Estimated tokens of its tool definitions.
    pub tool_tokens: u32,
    /// Last stderr lines.
    pub log: Vec<String>,
}

/// A model server found on this machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetectedServer {
    /// Suggested provider id, e.g. `ollama`.
    pub id: String,
    /// Human name, e.g. `Ollama`.
    pub name: String,
    pub base_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api: Option<String>,
    /// What this engine needs for tool calls, e.g. a server flag. May be empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
    pub models: Vec<ModelInfo>,
    /// The configured provider that already points here, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub configured: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: SessionId,
    pub title: String,
    pub model: String,
    pub turns: u32,
    /// Human age of the last update, e.g. `3h ago`.
    pub age: String,
    /// Start date, `YYYY-MM-DD`.
    pub started: String,
    /// The session this client is in.
    pub current: bool,
}

/// A user message the conversation can go back to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RewindPoint {
    /// 1 for the last user message, 2 for the one before, and so on.
    pub back: u32,
    pub text: String,
    /// Files changed by write and edit calls since this message, workspace-relative.
    pub files: Vec<String>,
}

/// A transcript entry of a resumed session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReplayItem {
    User {
        text: String,
    },
    Assistant {
        text: String,
    },
    Tool {
        summary: String,
        ok: bool,
        result: String,
    },
    /// Earlier history was summarized here.
    Compacted,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_prompt_tokens: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextReport {
    /// `<provider>/<model>`.
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<u32>,
    /// Requests estimated above this get pruned, then compacted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threshold: Option<u32>,
    /// Estimated prompt tokens of the next request.
    pub used: u32,
    /// `[context] cache_strategy`.
    pub strategy: String,
    pub segments: Vec<ContextSegment>,
    /// Caveats, e.g. that the history is in use by a running turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
    /// Where it was found, e.g. `user` or `project (.agents/skills)`.
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub dir: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoctorCheck {
    pub name: String,
    pub status: CheckStatus,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Ok,
    Info,
    Warn,
    Fail,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryNote {
    /// `global` or `project`.
    pub scope: String,
    pub name: String,
    pub description: String,
    pub path: String,
    pub text: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextSegment {
    pub name: String,
    pub tokens: u32,
    /// One line, e.g. `12 messages, 3 pruned`.
    pub detail: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PruneReport {
    pub removed_tokens: u32,
    pub items: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: SessionId,
    pub cwd: String,
    /// `<provider-id>/<model-id>`, if a model is selected.
    pub model: Option<String>,
    pub profile: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineError {
    pub kind: EngineErrorKind,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineErrorKind {
    Config,
    Provider,
    Tool,
    Internal,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ops_and_events_round_trip_through_json() {
        let op = Op::UserTurn {
            items: vec![InputItem::text("hi")],
            overrides: TurnOverrides::default(),
        };
        let json = serde_json::to_string(&op).unwrap_or_default();
        assert_eq!(
            json,
            r#"{"op":"user_turn","items":[{"type":"text","text":"hi"}],"overrides":{}}"#
        );
        assert_eq!(serde_json::from_str::<Op>(&json).ok(), Some(op));

        let ev = Event::StartupProgress {
            subsystem: Subsystem::Config,
            state: LoadState::Ready {
                detail: "3 layers".into(),
            },
        };
        let json = serde_json::to_string(&ev).unwrap_or_default();
        assert_eq!(serde_json::from_str::<Event>(&json).ok(), Some(ev));
    }
}
