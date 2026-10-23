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
    /// Answers to an `Event::Questions`, one per question in order. `stop_asking`: the
    /// user wants no more questions this turn.
    AnswerQuestions {
        call_id: CallId,
        answers: Vec<Answer>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        stop_asking: bool,
    },
    SetModel {
        provider: ProviderId,
        model: ModelId,
    },
    /// Ask for the model catalogue; answered with `Event::ModelCatalog`.
    ListModels,
    /// Ask which instruction files (AGENTS.md and the like) the model has; answered with
    /// `Event::Instructions`.
    ListInstructions,
    /// Ask for the installed profiles, each previewed for the current model; answered
    /// with `Event::Profiles`.
    ListProfiles,
    /// Use this profile for every model for the rest of the session; `auto` goes back to
    /// each model's own choice. Waits for a running turn.
    SetProfile {
        id: String,
    },
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
    /// Ask for this session's usage so far; answered with `Event::Stats`.
    GetStats,
    /// `read-only`, `ask`, `auto-edit`, `full-auto` (capped by policy), or `plan`.
    SetPermissionMode {
        mode: String,
    },
    /// Run shell commands without the OS sandbox for the rest of the session (`off`), or
    /// go back to `[sandbox] mode`. Permission modes and approvals are unchanged. Refused
    /// when the system policy locks `sandbox.mode`.
    SetSandbox {
        off: bool,
    },
    /// Leave plan mode for `mode` and carry out the plan. When a `[model] planner` wrote
    /// it, the session's model gets the requests, the user's answers and the plan in one
    /// message instead of the planner's transcript.
    AcceptPlan {
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
    /// Ask for the language servers' state; answered with `Event::LspServers`.
    ListLsp,
    /// `/init`: a turn that writes AGENTS.md, merging other agents' instruction files
    /// into it when there are any.
    InitInstructions,
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
    /// Ask for the hosted services in `providers.d` that take a key; answered with
    /// `Event::Services`.
    ListServices,
    /// Store a provider's API key in the OS credential store. When its key is already read
    /// from there, the entry is replaced and the provider file left alone
    /// (`Event::ProviderAdded` without a `path`); otherwise its provider file (copied to the
    /// user layer) is pointed at the new entry. A session using the provider sends the new
    /// key from its next request. Answered with `Event::ProviderAdded` and then
    /// `Event::ModelCatalog` for that provider alone.
    SetProviderKey {
        id: String,
        key: SecretText,
    },
    /// Add any OpenAI-compatible server by its URL, with its key (if any) in the OS
    /// credential store. Answered like `SetProviderKey`.
    AddServer {
        base_url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<SecretText>,
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
    /// Record a decision about this workspace's `.quack/` (`trusted`,
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
    /// The instruction files in the system prompt, then those attached to tool results
    /// so far.
    Instructions {
        files: Vec<InstructionFile>,
    },
    /// Custom slash commands found at startup.
    Commands {
        commands: Vec<CommandInfo>,
    },
    /// Prompts of the ready MCP servers, run as `/<server>:<prompt> args`. Sent again
    /// whenever the list changes (a server connects, stops or re-lists).
    McpPrompts {
        prompts: Vec<CommandInfo>,
    },
    /// The installed profiles, previewed for the current model.
    Profiles {
        /// The profile in use.
        active: String,
        /// Set with `/profiles <id>` or `[model] profile`, rather than chosen per model.
        forced: bool,
        profiles: Vec<ProfileInfo>,
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
    /// The workspace has a `.quack/` that needs a trust decision (`Op::TrustProject`).
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
    /// The model asks the user (`ask_user`); answered with `Op::AnswerQuestions`.
    Questions {
        call_id: CallId,
        questions: Vec<Question>,
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        window: Option<u32>,
        cache_hit_tokens: Option<u32>,
        /// Above this the next request is pruned, then compacted.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        threshold: Option<u32>,
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
    /// This session's usage: tokens, cost, timings, tools.
    Stats {
        report: StatsReport,
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
    LspServers {
        servers: Vec<LspServerInfo>,
    },
    ServersDetected {
        servers: Vec<DetectedServer>,
    },
    /// The hosted services setup offers, grouped as they should be listed.
    Services {
        services: Vec<ServiceInfo>,
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
    /// Whether the user turned the sandbox off for this session (`Op::SetSandbox`).
    Sandbox {
        off: bool,
    },
    Compacted {
        summary_tokens: u32,
        /// History messages the summary replaced.
        #[serde(default)]
        replaced: u32,
    },
    /// A hook ran (docs/plan/11-security.md#hooks) on event `on`. `outcome` is `ok`, `context` (it
    /// added context for the model), `allowed`, `denied`, `asked`, `updated` (it changed a
    /// tool call's arguments) or `failed`.
    HookRan {
        id: String,
        /// The hook's event, e.g. `user_prompt`.
        on: String,
        outcome: String,
        ms: u64,
        /// What the hook said, or why it failed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
        /// Estimated tokens of context it added.
        #[serde(default)]
        context_tokens: u32,
    },
    // sub-agents and the plan
    /// A sub-agent (`agents.d`) started for the `task` call `call_id`. `id` names this run,
    /// e.g. `explore-2`; `agent` is the agent's id.
    AgentStarted {
        id: String,
        agent: String,
        description: String,
        call_id: CallId,
        /// `<provider>/<model>` it runs on.
        model: String,
    },
    /// An event of a sub-agent's own turn, for clients to show apart from the conversation.
    /// Its approval requests are sent unwrapped instead, so every client answers them.
    Agent {
        id: String,
        inner: Box<Event>,
    },
    AgentFinished {
        id: String,
        ok: bool,
        /// e.g. `12 steps · 34.5k tokens`.
        summary: String,
    },
    /// The plan (`todo`) changed: one `[x]`, `[~]` or `[ ]` line per item; empty when cleared.
    Plan {
        checklist: String,
    },
    // diagnostics
    /// Something worth knowing that is not a problem, e.g. an offer.
    Notice {
        message: String,
    },
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

/// A question from the model to the user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Question {
    /// A word or two naming the topic, e.g. `Storage`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub header: String,
    pub question: String,
    /// Choices; empty means a free-text answer.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<QuestionOption>,
    /// The option the model recommends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recommended: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionOption {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// The user's answer to one [`Question`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "answer", rename_all = "snake_case")]
pub enum Answer {
    /// An option, by index.
    Picked {
        index: usize,
    },
    Text {
        text: String,
    },
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ApprovalRequest {
    Shell {
        command: String,
        cwd: String,
        /// The sandbox is on, but this command would run outside it.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        outside_sandbox: bool,
    },
    Edit {
        path: String,
        diff: String,
    },
    Tool {
        name: String,
        arguments: String,
    },
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

/// A custom slash command (`commands/<name>.md`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandInfo {
    /// Without the slash, e.g. `review` or `test:flaky`.
    pub name: String,
    pub description: String,
    /// What to type after the name, e.g. `[focus]`.
    pub args: String,
    /// The layer it came from.
    pub source: String,
    pub path: String,
}

/// One instruction file the model has seen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstructionFile {
    /// The path as shown to the model, relative to the workspace when inside it.
    pub source: String,
    /// In the system prompt (`false`: attached to a tool result when the model first
    /// worked in its folder).
    pub in_prompt: bool,
    /// Estimated tokens.
    pub tokens: u32,
    pub text: String,
}

/// A profile as it would shape the current model's session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileInfo {
    pub id: String,
    pub description: String,
    /// Where its prompt and tool shapes come from.
    pub source: String,
    /// `auto` would pick it for the current model.
    pub matches: bool,
    pub edit_format: String,
    /// The tool names the model would see.
    pub tools: Vec<String>,
    /// The rendered system prompt, or why it does not render.
    pub system: String,
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
    /// Where its key is read from, when it takes one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<KeySource>,
}

/// A language server from `lsp.d/` and its state (one per project folder it serves).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LspServerInfo {
    pub id: String,
    /// The command line.
    pub command: String,
    pub extensions: Vec<String>,
    /// `not installed`, `idle`, `starting`, `ready`, `indexing`, `failed` or `exited`.
    pub state: String,
    /// The program's path, what it is working on, or why it failed.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
    /// The project folder it serves, relative to the workspace.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub root: String,
    pub errors: u32,
    pub warnings: u32,
    /// Last stderr lines.
    pub log: Vec<String>,
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
    /// Prompts the user can run as `/<server>:<prompt>`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prompts: Vec<String>,
    /// Resource URIs and URI templates the model can read.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resources: Vec<String>,
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

/// A hosted service from `providers.d` that takes a key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceInfo {
    pub id: String,
    pub name: String,
    /// Heading it is listed under.
    pub group: String,
    pub base_url: String,
    /// The variable its key is read from, when it is read from one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<String>,
    /// Its key can be read now.
    pub ready: bool,
    /// Where its key is read from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<KeySource>,
}

/// Where a provider's `api_key` is read from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "from", rename_all = "kebab-case")]
pub enum KeySource {
    /// The OS credential store, under this name.
    Keyring {
        name: String,
    },
    Env {
        var: String,
    },
    File {
        path: String,
    },
    /// The output of a command.
    Cmd,
    /// The value written in the provider file.
    Value,
}

/// Text the user typed that must not be shown: serialized as is, but `Debug` hides it, so
/// it never reaches a log.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SecretText(pub String);

impl std::fmt::Debug for SecretText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretText(…)")
    }
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
    /// Prompt tokens written to the server's cache, for servers that bill writes apart
    /// (Anthropic). Part of `prompt_tokens`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<u32>,
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

/// Usage of one session. Token totals include earlier runs of a resumed session; timings
/// and tool counts cover only the time since it was opened (`resumed`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StatsReport {
    /// The current `<provider>/<model>`.
    pub model: String,
    pub turns: u32,
    /// Model calls; a turn makes one per step.
    pub requests: u32,
    /// Requests whose server reported no usage, so their tokens are not counted.
    #[serde(default)]
    pub unreported: u32,
    /// Prompt tokens summed over requests: what was billed, not the context size.
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    /// Prompt tokens the server served from its cache.
    pub cached_tokens: u64,
    /// Requests that said how much was cached; zero when the server never does.
    #[serde(default)]
    pub cache_reported: u32,
    /// US dollars, from the `cost` of the models used. `None` when none had one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
    /// Requests to models without a `cost`, left out of `cost`.
    #[serde(default)]
    pub unpriced: u32,
    /// Time to first token, median and slowest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttft_ms: Option<(u32, u32)>,
    /// Median generation speed, tokens per second.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_per_sec: Option<f32>,
    /// Since the session was opened: in the session, waiting for the model, running tools,
    /// and waiting for the user to approve.
    pub open_ms: u64,
    pub model_ms: u64,
    pub tools_ms: u64,
    pub approval_ms: u64,
    /// Busiest first.
    pub tools: Vec<ToolStat>,
    pub compactions: u32,
    /// Tool results replaced by a placeholder to save room.
    pub pruned: u32,
    /// A saved session that was resumed: timings start at the resume.
    #[serde(default)]
    pub resumed: bool,
    /// The sampling settings the current model's requests carry (`temperature`, `top_p`,
    /// ...), from its model entry and profile. Empty: the server's defaults apply.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sampling: Vec<(String, f64)>,
    /// Each request, oldest first; at most the last [`StatsReport::HISTORY`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<RequestStat>,
}

impl StatsReport {
    /// How many requests `history` keeps.
    pub const HISTORY: usize = 200;
}

/// One model call, for the charts in `/stats`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RequestStat {
    pub prompt: u32,
    pub cached: u32,
    pub completion: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
    /// Absent for requests from before a resume, and for replies too short to time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttft_ms: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_per_sec: Option<f32>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolStat {
    pub name: String,
    pub calls: u32,
    pub failed: u32,
    pub ms: u64,
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
