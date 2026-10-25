//! One ACP session: an engine, and the task that turns its events into `session/update`
//! notifications and answers the session's `session/prompt` requests.

use std::collections::HashSet;
use std::time::Duration;

use quack_core::EngineHandle;
use quack_protocol::{
    ApprovalDecision, ApprovalRequest, CallId, Event, InputItem, LoadState, ModelId, Op,
    ProviderId, ProviderModels, Subsystem, TurnOverrides, WriteScope,
};
use serde_json::{Value, json};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::convert::{self, chunk, text_content, tool_content};
use crate::rpc::{Peer, RpcError};
use crate::wire::{PermissionOutcome, PermissionResponse};

/// How long opening a session may take: config, the model, and replaying a saved log.
const OPEN_TIMEOUT: Duration = Duration::from_secs(60);

/// The modes an editor can switch between, as the engine names them.
const MODES: &[(&str, &str, &str)] = &[
    ("ask", "Ask", "Ask before edits and commands"),
    (
        "auto-edit",
        "Auto-edit",
        "Edit files without asking; ask before commands",
    ),
    (
        "full-auto",
        "Full auto",
        "Run without asking, inside the sandbox",
    ),
    ("read-only", "Read-only", "Look around; change nothing"),
    (
        "plan",
        "Plan",
        "Look around and propose a plan; change nothing",
    ),
];

pub(crate) const MODEL_OPTION: &str = "model";

#[derive(Debug)]
pub(crate) enum Cmd {
    Prompt {
        id: Value,
        items: Vec<InputItem>,
    },
    Cancel,
    Mode(String),
    Model {
        value: String,
        /// The config options after the change.
        reply: oneshot::Sender<Result<Value, RpcError>>,
    },
}

/// The server's side of a running session.
#[derive(Debug)]
pub(crate) struct Handle {
    pub cmds: mpsc::UnboundedSender<Cmd>,
    task: JoinHandle<()>,
}

impl Handle {
    /// Stop the session's engine and wait for it.
    pub async fn close(self) {
        drop(self.cmds);
        let _ = self.task.await;
    }
}

/// What was seen while waiting for a session to open.
#[derive(Debug, Default)]
pub(crate) struct Startup {
    /// MCP servers finished starting, so their tools are there for the first turn.
    pub mcp_ready: bool,
}

/// Read events until `want` returns something.
pub(crate) async fn wait_for<T>(
    engine: &mut EngineHandle,
    startup: &mut Startup,
    mut want: impl FnMut(&Event) -> Option<T>,
) -> Result<T, String> {
    let wait = async {
        loop {
            match engine.events.recv().await {
                Ok(e) => {
                    startup.mcp_ready |= mcp_settled(&e);
                    if let Some(t) = want(&e) {
                        return Ok(t);
                    }
                }
                Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => return Err("the engine stopped".to_owned()),
            }
        }
    };
    tokio::time::timeout(OPEN_TIMEOUT, wait)
        .await
        .unwrap_or_else(|_| Err("the session did not open within a minute".to_owned()))
}

fn mcp_settled(e: &Event) -> bool {
    matches!(e, Event::StartupProgress { subsystem: Subsystem::Mcp, state }
        if !matches!(state, LoadState::Pending | LoadState::Running { .. }))
}

/// The modes, with `current` selected (`SessionModeState`).
pub(crate) fn modes(current: &str) -> Value {
    let available: Vec<Value> = MODES
        .iter()
        .map(|(id, name, about)| json!({"id": id, "name": name, "description": about}))
        .collect();
    json!({"currentModeId": current, "availableModes": available})
}

/// Start the task that runs session `id` on `engine`.
pub(crate) fn start(id: String, engine: EngineHandle, peer: Peer, startup: Startup) -> Handle {
    let (cmds, rx) = mpsc::unbounded_channel();
    let run = Run {
        id,
        ops: engine.ops.clone(),
        peer,
        ready: startup.mcp_ready,
        queued: None,
        prompt: None,
        started: false,
        cancelled: false,
        error: None,
        announced: HashSet::new(),
        output: Default::default(),
        catalog: Vec::new(),
        current_model: None,
    };
    let task = tokio::spawn(run.run(engine, rx));
    Handle { cmds, task }
}

struct Run {
    id: String,
    ops: mpsc::Sender<Op>,
    peer: Peer,
    /// MCP startup settled; until then the first prompt waits, as in `quack exec`.
    ready: bool,
    queued: Option<(Value, Vec<InputItem>)>,
    /// The `session/prompt` request being answered.
    prompt: Option<Value>,
    /// The engine started a turn for it.
    started: bool,
    cancelled: bool,
    /// The last error in this turn.
    error: Option<String>,
    /// Tool calls the editor has been told about.
    announced: HashSet<String>,
    /// Streamed output of the running tool calls.
    output: std::collections::HashMap<String, String>,
    /// `<provider>/<model>` choices.
    catalog: Vec<(String, String)>,
    current_model: Option<String>,
}

impl Run {
    async fn run(mut self, mut engine: EngineHandle, mut cmds: mpsc::UnboundedReceiver<Cmd>) {
        let _ = self.ops.send(Op::ListModels).await;
        loop {
            tokio::select! {
                cmd = cmds.recv() => match cmd {
                    Some(cmd) => self.on_cmd(cmd).await,
                    None => break,
                },
                ev = engine.events.recv() => match ev {
                    Ok(ev) => self.on_event(ev).await,
                    Err(RecvError::Lagged(n)) => tracing::warn!(n, "acp session lagged behind the engine"),
                    Err(RecvError::Closed) => break,
                },
            }
        }
        if let Some(id) = self.prompt.take().or(self.queued.take().map(|q| q.0)) {
            self.peer
                .respond(id, Err(RpcError::internal("the session closed")));
        }
        engine.shutdown().await;
    }

    fn update(&self, update: Value) {
        self.peer.notify(
            "session/update",
            json!({"sessionId": self.id, "update": update}),
        );
    }

    async fn on_cmd(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Prompt { id, items } => {
                if self.prompt.is_some() || self.queued.is_some() {
                    self.peer.respond(
                        id,
                        Err(RpcError::invalid(
                            "a prompt is already running in this session · cancel it first",
                        )),
                    );
                } else if self.ready {
                    self.submit(id, items).await;
                } else {
                    self.queued = Some((id, items));
                }
            }
            Cmd::Cancel => {
                if let Some((id, _)) = self.queued.take() {
                    self.respond_stop(id, "cancelled");
                } else if self.prompt.is_some() {
                    self.cancelled = true;
                    let _ = self.ops.send(Op::Interrupt).await;
                }
            }
            Cmd::Mode(mode) => {
                let _ = self.ops.send(Op::SetPermissionMode { mode }).await;
            }
            Cmd::Model { value, reply } => {
                let result = match value.split_once('/') {
                    Some((provider, model)) if self.catalog.iter().any(|(v, _)| *v == value) => {
                        let _ = self
                            .ops
                            .send(Op::SetModel {
                                provider: ProviderId(provider.to_owned()),
                                model: ModelId(model.to_owned()),
                            })
                            .await;
                        self.current_model = Some(value);
                        Ok(self.config_options())
                    }
                    _ => Err(RpcError::invalid(format!(
                        "unknown model `{value}` · pick one the session lists"
                    ))),
                };
                let _ = reply.send(result);
            }
        }
    }

    async fn submit(&mut self, id: Value, items: Vec<InputItem>) {
        self.prompt = Some(id);
        self.started = false;
        self.cancelled = false;
        self.error = None;
        let op = Op::UserTurn {
            items,
            overrides: TurnOverrides::default(),
        };
        if self.ops.send(op).await.is_err() {
            self.fail("the engine stopped");
        }
    }

    fn respond_stop(&self, id: Value, reason: &str) {
        self.peer.respond(id, Ok(json!({"stopReason": reason})));
    }

    fn fail(&mut self, message: &str) {
        if let Some(id) = self.prompt.take() {
            self.peer.respond(id, Err(RpcError::internal(message)));
        }
    }

    async fn on_event(&mut self, ev: Event) {
        if mcp_settled(&ev) && !self.ready {
            self.ready = true;
            if let Some((id, items)) = self.queued.take() {
                self.submit(id, items).await;
            }
        }
        match ev {
            Event::TurnStarted { .. } => self.started = true,
            Event::AssistantDelta { text } => {
                self.update(chunk("agent_message_chunk", &text));
            }
            Event::ReasoningDelta { text } => {
                self.update(chunk("agent_thought_chunk", &text));
            }
            Event::ToolCallBegin { call } => {
                self.announce(&call.call_id, &call.name, &call.summary, "in_progress");
            }
            Event::ToolCallOutputDelta { call_id, chunk } => {
                self.output.entry(call_id.0).or_default().push_str(&chunk);
            }
            Event::ToolCallEnd { call_id, result } => {
                let streamed = self.output.remove(&call_id.0).unwrap_or_default();
                let detail = result.detail.as_deref().unwrap_or(&streamed);
                let content = if detail.trim().is_empty() {
                    text_content(&result.summary)
                } else {
                    tool_content(detail)
                };
                self.update(json!({
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": call_id.0,
                    "status": if result.ok { "completed" } else { "failed" },
                    "content": [content],
                }));
            }
            Event::ApprovalRequest {
                call_id,
                request,
                always,
            } => self.ask(call_id, request, &always),
            Event::Questions { call_id, .. } => {
                // ACP has no questions yet: the model decides and says what it assumed.
                let _ = self
                    .ops
                    .send(Op::AnswerQuestions {
                        call_id,
                        answers: Vec::new(),
                        stop_asking: true,
                    })
                    .await;
            }
            Event::Plan { checklist } => self.update(convert::plan(&checklist)),
            Event::ContextUpdate {
                used,
                budget,
                window,
                ..
            } => {
                self.update(json!({
                    "sessionUpdate": "usage_update",
                    "used": used,
                    "size": window.unwrap_or(budget),
                }));
            }
            Event::Compacting { reason } => {
                let text = format!("\n(summarizing earlier conversation: {reason})\n");
                self.update(chunk("agent_thought_chunk", &text));
            }
            Event::PermissionMode { mode } => {
                self.update(json!({"sessionUpdate": "current_mode_update", "currentModeId": mode}));
            }
            Event::ModelCatalog { providers, current } => {
                self.catalog = choices(&providers);
                if current.is_some() {
                    self.current_model = current;
                }
                if !self.catalog.is_empty() {
                    self.update(json!({
                        "sessionUpdate": "config_option_update",
                        "configOptions": self.config_options(),
                    }));
                }
            }
            Event::Warning { message } => {
                tracing::warn!(%message, "engine warning");
                // Out of the answer itself, but where the user sees it during a turn.
                if self.started && self.prompt.is_some() {
                    self.update(chunk("agent_thought_chunk", &format!("\n⚠ {message}\n")));
                }
            }
            Event::Error { error } => {
                tracing::warn!(message = %error.message, "engine error");
                if self.prompt.is_some() && !self.started {
                    // No turn will start, so nothing else answers the prompt.
                    self.fail(&error.message);
                } else {
                    self.error = Some(error.message);
                }
            }
            Event::TurnComplete { .. } => {
                if let Some(id) = self.prompt.take() {
                    self.respond_stop(id, "end_turn");
                }
                self.announced.clear();
            }
            Event::TurnAborted { reason, .. } => {
                if self.cancelled {
                    if let Some(id) = self.prompt.take() {
                        self.respond_stop(id, "cancelled");
                    }
                } else {
                    let message = self.error.take().unwrap_or(reason);
                    self.fail(&message);
                }
                self.announced.clear();
            }
            _ => {}
        }
    }

    fn announce(&mut self, call_id: &CallId, name: &str, title: &str, status: &str) {
        if !self.announced.insert(call_id.0.clone()) {
            return;
        }
        let kind = convert::tool_kind(name);
        self.update(convert::tool_call(&call_id.0, title, kind, status));
    }

    /// Ask the editor, without blocking the session: the user may cancel meanwhile.
    fn ask(&mut self, call_id: CallId, request: ApprovalRequest, always: &[String]) {
        let (name, title, detail) = match &request {
            ApprovalRequest::Shell {
                command,
                outside_sandbox,
                ..
            } => {
                let note = if *outside_sandbox {
                    " (outside the sandbox)"
                } else {
                    ""
                };
                (
                    "shell",
                    format!("Run `{command}`{note}"),
                    convert::fenced(command, "sh"),
                )
            }
            ApprovalRequest::Edit { path, diff } => (
                "edit",
                format!("Edit {path}"),
                convert::fenced(diff, "diff"),
            ),
            ApprovalRequest::Tool { name, arguments } => (
                name.as_str(),
                format!("Use {name}"),
                convert::fenced(arguments, "json"),
            ),
        };
        self.announce(&call_id, name, &title, "pending");
        let req = json!({
            "sessionId": self.id,
            "toolCall": {
                "toolCallId": call_id.0,
                "title": title,
                "content": [text_content(&detail)],
            },
            "options": options(always),
        });
        let peer = self.peer.clone();
        let ops = self.ops.clone();
        tokio::spawn(async move {
            let answer = peer.request("session/request_permission", req).await;
            let decision = match answer
                .ok()
                .and_then(|v| serde_json::from_value::<PermissionResponse>(v).ok())
                .map(|r| r.outcome)
            {
                Some(PermissionOutcome::Selected { option_id }) => match option_id.as_str() {
                    "allow" => ApprovalDecision::AllowOnce,
                    "allow-session" => ApprovalDecision::AllowSession,
                    "allow-always" => ApprovalDecision::AlwaysAllow {
                        scope: WriteScope::Project,
                    },
                    _ => ApprovalDecision::Deny,
                },
                _ => ApprovalDecision::Deny,
            };
            let _ = ops.send(Op::ApprovalResponse { call_id, decision }).await;
        });
    }

    /// The session's config options: the model, as a select.
    fn config_options(&self) -> Value {
        let Some(current) = self
            .current_model
            .clone()
            .or_else(|| self.catalog.first().map(|(v, _)| v.clone()))
        else {
            return json!([]);
        };
        let options: Vec<Value> = self
            .catalog
            .iter()
            .map(|(value, name)| json!({"value": value, "name": name}))
            .collect();
        json!([{
            "id": MODEL_OPTION,
            "name": "Model",
            "category": "model",
            "type": "select",
            "currentValue": current,
            "options": options,
        }])
    }
}

/// The models of the providers that can be used now, as `(value, name)`.
fn choices(providers: &[ProviderModels]) -> Vec<(String, String)> {
    providers
        .iter()
        .filter(|p| p.available)
        .flat_map(|p| {
            p.models.iter().map(move |m| {
                let value = format!("{}/{}", p.id, m.id);
                let name = m.alias.clone().unwrap_or_else(|| value.clone());
                (value, name)
            })
        })
        .collect()
}

/// The choices for an approval. "Always allow" is offered when a rule can allow the call,
/// and names the rule it saves to `[permissions] allow`.
fn options(always: &[String]) -> Value {
    let mut out = vec![
        json!({"optionId": "allow", "name": "Allow", "kind": "allow_once"}),
        json!({"optionId": "allow-session", "name": "Allow for this session", "kind": "allow_always"}),
    ];
    if !always.is_empty() {
        let rules = always
            .iter()
            .map(|r| format!("`{r}`"))
            .collect::<Vec<_>>()
            .join(", ");
        out.push(json!({
            "optionId": "allow-always",
            "name": format!("Always allow ({rules})"),
            "kind": "allow_always",
        }));
    }
    out.push(json!({"optionId": "deny", "name": "Deny", "kind": "reject_once"}));
    Value::Array(out)
}
