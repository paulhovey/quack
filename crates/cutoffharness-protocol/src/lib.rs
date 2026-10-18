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
    Compact,
    Undo {
        checkpoint: Option<CheckpointId>,
    },
    ResumeSession {
        id: SessionId,
    },
    ForkSession {
        at: EventIndex,
    },
    ReloadConfig,
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
    ShutdownComplete,
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
    Compacted {
        summary_tokens: u32,
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u32>,
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
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_prompt_tokens: Option<u32>,
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
