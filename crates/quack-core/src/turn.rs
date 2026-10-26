//! One user turn: stream the model, run its tool calls, loop until it answers
//! (docs/plan/04-agent-loop-and-tools.md#turn-lifecycle).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use quack_config::hooks::HookEvent;
use quack_protocol::{
    ApprovalDecision, CallId, EngineError, EngineErrorKind, Event, Question, ToolCallInfo,
    ToolResultSummary, TurnId, Usage, WriteScope,
};
use quack_providers::{
    ChatRequest, ErrorKind, Message, ResponseBuilder, StreamEvent, ToolCall, ToolSpec, tokens,
    toolparse,
};
use quack_store::checkpoint::Checkpoints;
use quack_tools::{Answers, Decision, Permissions, Subject, ToolOutput};
use serde_json::{Value, json};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use quack_context::compact;

use crate::hooks::{self, Hooks, Sink, Target, Verdict};
use crate::session::{self, SharedLog};
use crate::startup::Setup;
use crate::stats::Stats;

/// Steps (model calls) per turn before pausing.
pub(crate) const MAX_STEPS: usize = 100;
/// Identical consecutive calls before the model is told to change approach.
const REPEAT_LIMIT: usize = 3;

pub(crate) type Approvals = Arc<Mutex<HashMap<String, oneshot::Sender<ApprovalDecision>>>>;
/// `ask_user` calls waiting for the user's answers, by call id.
pub(crate) type Questions = Arc<Mutex<HashMap<String, oneshot::Sender<Answers>>>>;

pub(crate) struct TurnCtx {
    pub turn: TurnId,
    pub setup: Arc<Setup>,
    pub perms: Arc<Mutex<Permissions>>,
    pub nested: Arc<quack_instructions::agents_md::Nested>,
    pub approvals: Approvals,
    pub questions: Questions,
    pub tx: broadcast::Sender<Event>,
    pub cancel: CancellationToken,
    pub log: Option<SharedLog>,
    /// The session's own log. For a sub-agent, whose messages go to its own `log`, its
    /// usage is also recorded here (`agent_usage`), so `/stats` counts it after a resume.
    pub main_log: Option<SharedLog>,
    /// Pre-images of edited files, for `/undo`.
    pub checkpoints: Option<Arc<Checkpoints>>,
    /// The session's usage, fed by `emit`.
    pub stats: Arc<Mutex<Stats>>,
    pub hooks: Arc<Hooks>,
    /// The session id, for hooks.
    pub session: String,
    /// The sub-agent run this turn is (`explore-2`); `None` for the session's own turns.
    pub agent: Option<String>,
    /// Model calls before the turn stops.
    pub max_steps: usize,
    /// Sub-agent setups, built on first use.
    pub pool: Arc<crate::agent::Pool>,
    /// One approval or question on screen at a time, while sub-agents run side by side.
    pub prompt_lock: Arc<tokio::sync::Mutex<()>>,
}

/// What the user sent, ready for the model, and what they typed (which hooks see).
pub(crate) struct Prompt {
    pub text: String,
    pub typed: String,
    /// Images the user attached, encoded and ready for the wire.
    pub images: Vec<quack_protocol::Image>,
    /// Context from `session_start` hooks, for the session's first prompt.
    pub start: Option<tokio::task::JoinHandle<Vec<String>>>,
}

impl TurnCtx {
    /// For an append-only provider, end the step's last tool result with the plan and working
    /// file reminders, where they stay: the next request extends this one instead of rewriting its end.
    fn keep_plan(&self, content: &mut String, last: bool) {
        if last && self.setup.provider.append_only() {
            if let Some(wf) = working_files_reminder(self) {
                content.push_str("\n\n");
                content.push_str(&wf);
            }
            if let Some(plan) = self.setup.tool_ctx.plan.reminder() {
                content.push_str("\n\n");
                content.push_str(&plan);
            }
        }
    }

    /// Add to history and the session log together, so the log is the source of truth.
    fn push(&self, history: &mut Vec<Message>, m: Message) {
        session::record_message(self.log.as_ref(), &m);
        history.push(m);
    }

    pub(crate) fn emit(&self, e: Event) {
        if let Ok(mut s) = self.stats.lock() {
            s.observe(&e);
        }
        // A sub-agent's events go to clients wrapped, except the approvals they answer.
        let e = match &self.agent {
            Some(id) if !matches!(e, Event::ApprovalRequest { .. }) => Event::Agent {
                id: id.clone(),
                inner: Box::new(e),
            },
            _ => e,
        };
        let _ = self.tx.send(e);
    }

    /// A call id as clients and the approval table see it. A sub-agent's model numbers its
    /// calls from scratch, so its ids get the run's id in front.
    fn key(&self, id: &str) -> String {
        match &self.agent {
            Some(a) => format!("{a}/{id}"),
            None => id.to_owned(),
        }
    }

    /// Emit the corrected estimate of what the next request will cost.
    pub(crate) fn context_estimate(&self, history: &[Message]) {
        self.emit(Event::ContextUpdate {
            used: estimate(&self.setup, history),
            budget: self.setup.budget.unwrap_or(0),
            window: self.setup.model.context_window,
            cache_hit_tokens: None,
            threshold: crate::context::threshold(&self.setup),
        });
    }

    /// Run the hooks for `event` that match `target`, reporting through this turn.
    pub(crate) async fn hooks(
        &self,
        event: HookEvent,
        fields: Value,
        target: Target<'_>,
    ) -> hooks::Outcome {
        if !self.hooks.any(event) {
            return hooks::Outcome::default();
        }
        let emit = |e: Event| self.emit(e);
        let sink = Sink {
            emit: &emit,
            log: self.log.as_ref(),
            session: &self.session,
        };
        self.hooks
            .run(event, fields, target, &sink, &self.cancel)
            .await
    }

    /// `turn_end` hooks, in the background: nothing waits for them.
    fn turn_end(&self, fields: Value) {
        if self.agent.is_some() || !self.hooks.any(HookEvent::TurnEnd) {
            return;
        }
        let (hooks, tx, log) = (self.hooks.clone(), self.tx.clone(), self.log.clone());
        let session = self.session.clone();
        tokio::spawn(async move {
            let emit = move |e: Event| {
                let _ = tx.send(e);
            };
            let sink = Sink {
                emit: &emit,
                log: log.as_ref(),
                session: &session,
            };
            let cancel = CancellationToken::new();
            hooks
                .run(HookEvent::TurnEnd, fields, Target::None, &sink, &cancel)
                .await;
        });
    }

    fn error(&self, kind: EngineErrorKind, message: impl Into<String>) {
        self.emit(Event::Error {
            error: EngineError {
                kind,
                message: message.into(),
            },
        });
    }
}

/// Sent once per turn after an empty reply.
pub(crate) const EMPTY_NUDGE: &str =
    "Your last reply was empty. Continue: call a tool if you need one, otherwise answer.";

enum StepEnd {
    Done,
    Aborted(String),
    /// The server said the request is too long for the model.
    Overflow(String),
    /// The reply stalled or the connection dropped before it finished; nothing went into the
    /// history, so the step can be sent again.
    Dropped(String),
}

/// Step-level retries after a dropped reply, on top of the provider's own retries (which stop
/// once a reply has started).
const MAX_DROPPED_RETRIES: u32 = 2;

/// Run the turn; returns the updated history.
pub(crate) async fn run(ctx: TurnCtx, mut history: Vec<Message>, prompt: Prompt) -> Vec<Message> {
    ctx.emit(Event::TurnStarted { turn: ctx.turn });
    let mut extra = match prompt.start {
        Some(start) => start.await.unwrap_or_default(),
        None => Vec::new(),
    };
    // A sub-agent's prompt comes from the model, not the user.
    let hooked = if ctx.agent.is_some() {
        hooks::Outcome::default()
    } else {
        ctx.hooks(
            HookEvent::UserPrompt,
            json!({ "prompt": prompt.typed }),
            Target::Prompt(&prompt.typed),
        )
        .await
    };
    if let Some((id, reason)) = hooked.denied() {
        let reason = if reason.is_empty() {
            String::new()
        } else {
            format!(": {reason}")
        };
        ctx.emit(Event::Warning {
            message: format!("hook `{id}` blocked this prompt{reason}; nothing was sent"),
        });
        ctx.emit(Event::TurnAborted {
            turn: ctx.turn,
            reason: format!("blocked by hook `{id}`"),
        });
        ctx.turn_end(json!({ "turn": ctx.turn.0, "outcome": "blocked" }));
        return history;
    }
    extra.extend(hooked.context);
    // Hook context rides on the user message, after the cached prefix, and is kept with
    // it, so a resumed session sends the same request.
    let content = if extra.is_empty() {
        prompt.text
    } else {
        format!("{}\n\n{}", prompt.text, extra.join("\n\n"))
    };
    ctx.push(
        &mut history,
        Message::User {
            content,
            images: prompt.images,
        },
    );
    let mut total = Usage::default();
    let mut recent: Vec<(String, String)> = Vec::new();
    let mut nudged = false;
    let mut overflowed = false;
    let mut dropped = 0;

    let end = 'steps: {
        for _ in 0..ctx.max_steps {
            crate::context::fit(&ctx, &mut history).await;
            let estimated = raw_estimate(&ctx.setup, &history);
            let (msg, usage) = match stream_step(&ctx, &history).await {
                Ok(r) => {
                    dropped = 0;
                    r
                }
                Err(StepEnd::Aborted(reason)) => break 'steps StepEnd::Aborted(reason),
                Err(StepEnd::Done) => break 'steps StepEnd::Done,
                Err(StepEnd::Dropped(message)) => {
                    if dropped < MAX_DROPPED_RETRIES {
                        dropped += 1;
                        ctx.emit(Event::Warning {
                            message: format!("{message} · sending the step again"),
                        });
                        continue;
                    }
                    ctx.error(EngineErrorKind::Provider, message);
                    break 'steps StepEnd::Aborted("provider error".into());
                }
                Err(StepEnd::Overflow(message)) => {
                    // Compact once and retry; a second overflow ends the turn.
                    let compacted = !overflowed
                        && crate::context::compact_now(
                            &ctx,
                            &mut history,
                            compact::KEEP_TURNS,
                            None,
                            "the server said the request is too long",
                        )
                        .await
                        .is_ok();
                    overflowed = true;
                    if compacted {
                        continue;
                    }
                    ctx.error(
                        EngineErrorKind::Provider,
                        format!(
                            "{message} · the conversation is too long for this model; \
                             try /compact or /new"
                        ),
                    );
                    break 'steps StepEnd::Aborted("context overflow".into());
                }
            };
            if let Some(u) = usage {
                if let Ok(mut e) = ctx.setup.estimator.lock() {
                    e.observe(estimated, u.prompt_tokens);
                }
                total.prompt_tokens = u.prompt_tokens;
                total.completion_tokens += u.completion_tokens;
                total.cached_prompt_tokens = u.cached_prompt_tokens;
                ctx.emit(Event::ContextUpdate {
                    used: u.prompt_tokens + u.completion_tokens,
                    budget: ctx.setup.budget.unwrap_or(0),
                    window: ctx.setup.model.context_window,
                    cache_hit_tokens: u.cached_prompt_tokens,
                    threshold: crate::context::threshold(&ctx.setup),
                });
            }
            let calls = match &msg {
                Message::Assistant { tool_calls, .. } => tool_calls.clone(),
                _ => Vec::new(),
            };
            let empty =
                matches!(&msg, Message::Assistant { content, .. } if content.trim().is_empty());
            let reported = usage.is_some();
            // An empty reply (common with small models after a tool result) gets one nudge.
            // The empty message itself is dropped: some chat templates reject it.
            if calls.is_empty() && empty && !nudged {
                nudged = true;
                ctx.emit(Event::Warning {
                    message: "the model returned an empty response; asking it to continue".into(),
                });
                ctx.push(
                    &mut history,
                    Message::User {
                        content: EMPTY_NUDGE.into(),
                        images: Vec::new(),
                    },
                );
                continue;
            }
            ctx.push(&mut history, msg);
            if !reported {
                // Servers that do not report usage still get a live context figure.
                ctx.context_estimate(&history);
            }
            if calls.is_empty() {
                if empty {
                    ctx.emit(Event::Warning {
                        message: "the model returned an empty response".into(),
                    });
                }
                break 'steps StepEnd::Done;
            }
            let mut i = 0;
            while i < calls.len() {
                if ctx.cancel.is_cancelled() {
                    // Every tool call needs a result, or the next request is invalid.
                    for c in &calls[i..] {
                        ctx.push(
                            &mut history,
                            Message::Tool {
                                call_id: c.id.clone(),
                                content: "Not run: interrupted by the user.".into(),
                                images: Vec::new(),
                            },
                        );
                    }
                    break 'steps StepEnd::Aborted("interrupted".into());
                }
                // Read-only sub-agents started together run side by side.
                let together = calls[i..].iter().take_while(|c| parallel(&ctx, c)).count();
                if together > 1 {
                    let batch = &calls[i..i + together];
                    let outs =
                        futures::future::join_all(batch.iter().map(|c| run_call(&ctx, c))).await;
                    let end = i + together == calls.len();
                    for (n, (call, out)) in batch.iter().zip(outs).enumerate() {
                        let ToolOutput {
                            model: mut content,
                            images,
                            ..
                        } = out;
                        ctx.keep_plan(&mut content, end && n + 1 == together);
                        ctx.push(
                            &mut history,
                            Message::Tool {
                                call_id: call.id.clone(),
                                content,
                                images,
                            },
                        );
                    }
                    i += together;
                    continue;
                }
                let call = &calls[i];
                i += 1;
                recent.push((call.name.clone(), call.arguments.clone()));
                let mut out = run_call(&ctx, call).await;
                let repeats = recent
                    .iter()
                    .rev()
                    .take_while(|c| **c == recent[recent.len() - 1])
                    .count();
                let same_count = recent
                    .iter()
                    .filter(|c| *c == &recent[recent.len() - 1])
                    .count();
                if repeats >= REPEAT_LIMIT {
                    out.model.push_str(&format!(
                        "\n\nNote: this exact call has now been made {repeats} times in a row. \
                         Try a different approach."
                    ));
                } else if same_count >= 2 && edits(&ctx.setup, call) {
                    out.model.push_str(&format!(
                        "\n\nNote: this exact edit has already been attempted in this turn ({same_count} times total). \
                         Repeating identical changes without modifying arguments will not fix the problem. \
                         Read the file to check its actual state or try a different fix."
                    ));
                }
                ctx.keep_plan(&mut out.model, i == calls.len());
                ctx.push(
                    &mut history,
                    Message::Tool {
                        call_id: call.id.clone(),
                        content: out.model,
                        images: out.images,
                    },
                );
            }
            // Tool output can be large; show its weight before the next request.
            ctx.context_estimate(&history);
        }
        ctx.emit(Event::Warning {
            message: format!(
                "stopped after {} steps; send a message to continue",
                ctx.max_steps
            ),
        });
        StepEnd::Done
    };

    match end {
        StepEnd::Done => {
            ctx.emit(Event::TurnComplete {
                turn: ctx.turn,
                usage: total,
            });
            ctx.turn_end(json!({ "turn": ctx.turn.0, "outcome": "complete", "usage": total }));
        }
        StepEnd::Aborted(reason) | StepEnd::Overflow(reason) | StepEnd::Dropped(reason) => {
            ctx.turn_end(json!({ "turn": ctx.turn.0, "outcome": "aborted", "reason": reason }));
            ctx.emit(Event::TurnAborted {
                turn: ctx.turn,
                reason,
            });
        }
    }
    history
}

/// The next request's prompt tokens: the byte estimate corrected by reported usage.
pub(crate) fn estimate(s: &Setup, history: &[Message]) -> u32 {
    let raw = raw_estimate(s, history);
    s.estimator.lock().map_or(raw, |e| e.scale(raw))
}

/// Uncorrected estimate of a request's prompt: system prompt, history and tool definitions.
/// Image payloads are counted as their rough token cost, not their base64 bytes, which
/// would count a screenshot as tens of thousands of tokens.
fn raw_estimate(s: &Setup, history: &[Message]) -> u32 {
    let json = serde_json::to_string(history).unwrap_or_default();
    let image_bytes: usize = history
        .iter()
        .flat_map(|m| match m {
            Message::User { images, .. } | Message::Tool { images, .. } => images.as_slice(),
            _ => &[],
        })
        .map(|i| i.data.len())
        .sum();
    let image_tokens: u32 = history
        .iter()
        .flat_map(|m| match m {
            Message::User { images, .. } | Message::Tool { images, .. } => images.as_slice(),
            _ => &[],
        })
        .map(quack_protocol::Image::tokens)
        .sum();
    let history = tokens::estimate(&json[..json.len().saturating_sub(image_bytes)])
        .saturating_add(image_tokens);
    let tools: usize = tool_specs(s)
        .iter()
        .map(|t| t.description.len() + t.name.len() + t.parameters.to_string().len())
        .sum();
    tokens::estimate(&s.system)
        .saturating_add(tokens::estimate(&s.session_context))
        .saturating_add(history)
        .saturating_add(u32::try_from(tools * 2 / 7).unwrap_or(u32::MAX))
}

/// The model entry's sampling settings over the profile's defaults.
pub(crate) fn sampling(s: &Setup) -> quack_config::provider::Sampling {
    let mut sampling = s.profile.file.defaults.sampling.clone();
    let m = &s.model.sampling;
    sampling.temperature = m.temperature.or(sampling.temperature);
    sampling.top_p = m.top_p.or(sampling.top_p);
    sampling.top_k = m.top_k.or(sampling.top_k);
    sampling.min_p = m.min_p.or(sampling.min_p);
    sampling.repetition_penalty = m.repetition_penalty.or(sampling.repetition_penalty);
    sampling
}

/// A request to the session's model with its sampling and extra parameters; with the
/// session's tools when `tools` is set.
pub(crate) fn request(s: &Setup, mut messages: Vec<Message>, tools: bool) -> ChatRequest {
    if !s.tool_ctx.vision {
        without_images(&mut messages);
    }
    ChatRequest {
        model: s.model.id.clone(),
        messages,
        tools: if tools { tool_specs(s) } else { Vec::new() },
        context_window: s.model.context_window,
        max_tokens: s.model.max_output,
        sampling: sampling(s),
        reasoning_effort: Some(s.model.reasoning.effort.clone()).filter(|e| !e.is_empty()),
        preserve_reasoning: s.model.reasoning.preserve_in_history,
        extra_body: serde_json::to_value(&s.model.extra_body)
            .ok()
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default(),
        cache_key: None,
        tool_mode: Some(s.model.tool_calling.mode),
    }
}

/// Images in the history from a model that took them (before a `/models` switch, a
/// resume on another model, or a planner handoff) become a note, since a model without
/// vision would have the whole request rejected. The history itself keeps them.
fn without_images(messages: &mut [Message]) {
    for m in messages {
        if let Message::User { content, images }
        | Message::Tool {
            content, images, ..
        } = m
            && !images.is_empty()
        {
            let n = images.len();
            images.clear();
            if !content.is_empty() {
                content.push_str("\n\n");
            }
            content.push_str(&format!(
                "[{n} image{} not sent: this model takes no images]",
                if n == 1 { "" } else { "s" }
            ));
        }
    }
}

/// The profile's tools, then the MCP tools offered right now.
fn tool_specs(s: &Setup) -> Vec<ToolSpec> {
    let mut specs: Vec<ToolSpec> = s
        .surface
        .tools
        .iter()
        .map(|t| ToolSpec {
            name: t.name.clone(),
            description: t.description.clone(),
            parameters: t.schema.clone(),
        })
        .collect();
    let mcp = if s.mcp_tools {
        s.mcp.offered(s.budget)
    } else {
        Vec::new()
    };
    specs.extend(mcp.into_iter().map(|t| ToolSpec {
        name: t.name().to_owned(),
        description: t.description(&s.tool_ctx),
        parameters: t.schema(),
    }));
    specs
}

/// Every tool name the model may call right now.
fn tool_names(s: &Setup) -> Vec<String> {
    tool_specs(s).into_iter().map(|t| t.name).collect()
}

pub(crate) fn reasoning_format(s: &Setup) -> quack_config::provider::ReasoningFormat {
    s.profile
        .file
        .defaults
        .reasoning_format
        .unwrap_or(s.model.reasoning.format)
}

/// Put the session context (`context.md`: workspace, date, instructions) at the start of
/// the first user message. Keeping it out of the system prompt keeps the system prompt and
/// the tool block, which chat templates render after it, the same in every session and
/// project, so the server's prompt cache reuses them. The history keeps what the user typed.
pub(crate) fn with_session_context(s: &Setup, messages: &mut [Message]) {
    if s.session_context.is_empty() {
        return;
    }
    if let Some(Message::User { content, .. }) = messages
        .iter_mut()
        .find(|m| matches!(m, Message::User { .. }))
    {
        *content = format!("{}\n\n{content}", s.session_context);
    }
}

/// The files the session changed, as shown to the model, first change first.
pub(crate) fn changed_files(ctx: &TurnCtx) -> Vec<String> {
    let mut files = Vec::new();
    for c in ctx.checkpoints.iter().flat_map(|cp| cp.changes()) {
        let shown = quack_tools::paths::display(&ctx.setup.tool_ctx.root, &c.path);
        if !files.contains(&shown) {
            files.push(shown);
        }
    }
    files
}

fn working_files_reminder(ctx: &TurnCtx) -> Option<String> {
    let files = changed_files(ctx);
    if files.is_empty() {
        return None;
    }
    Some(format!(
        "[Modified files in this session: {}]",
        files.join(", ")
    ))
}

/// One model call. Streams deltas to clients and returns the assembled message.
async fn stream_step(
    ctx: &TurnCtx,
    history: &[Message],
) -> Result<(Message, Option<Usage>), StepEnd> {
    let s = &ctx.setup;
    let mut messages = Vec::with_capacity(history.len() + 1);
    if !s.system.is_empty() {
        messages.push(Message::System {
            content: s.system.clone(),
        });
    }
    messages.extend_from_slice(history);
    with_session_context(s, &mut messages);
    // The plan and modified-files reminders ride on the last message of this request only,
    // so the history (and the cached prefix before it) stays the same. An append-only provider
    // has it in the history already (`TurnCtx::keep_plan`).
    if !s.provider.append_only() {
        if let Some(wf) = working_files_reminder(ctx)
            && let Some(Message::Tool { content, .. } | Message::User { content, .. }) =
                messages.last_mut()
        {
            content.push_str("\n\n");
            content.push_str(&wf);
        }
        if let Some(plan) = s.tool_ctx.plan.reminder()
            && let Some(Message::Tool { content, .. } | Message::User { content, .. }) =
                messages.last_mut()
        {
            content.push_str("\n\n");
            content.push_str(&plan);
        }
    }
    let mut req = request(s, messages, true);
    // Sub-agents have their own prefix, so their own key.
    req.cache_key = Some(match &ctx.agent {
        Some(agent) => format!("{}/{agent}", ctx.session),
        None => ctx.session.clone(),
    });
    let fingerprint = quack_providers::fingerprint(&req);
    let mut rx = s
        .provider
        .stream(req, reasoning_format(s), ctx.cancel.child_token());
    let mut b = ResponseBuilder::default();
    let sent = std::time::Instant::now();
    let mut first: Option<std::time::Instant> = None;
    let format = toolparse::TextFormat::parse(&s.model.tool_calling.text_format);
    let mut gate = toolparse::TextGate::new(format);
    loop {
        let item = tokio::select! {
            () = ctx.cancel.cancelled() => None,
            item = rx.recv() => Some(item),
        };
        let Some(item) = item else {
            // Interrupted. The partial text is not kept in history: an assistant message cut
            // mid-sentence confuses some chat templates. The session log will record it.
            return Err(StepEnd::Aborted("interrupted".into()));
        };
        let Some(item) = item else { break };
        match item {
            Ok(ev) => {
                if first.is_none()
                    && matches!(
                        ev,
                        StreamEvent::TextDelta(_)
                            | StreamEvent::ReasoningDelta(_)
                            | StreamEvent::ToolCallStart { .. }
                    )
                {
                    first = Some(std::time::Instant::now());
                }
                match &ev {
                    StreamEvent::TextDelta(t) => {
                        let text = gate.feed(t);
                        if !text.is_empty() {
                            ctx.emit(Event::AssistantDelta { text });
                        }
                    }
                    StreamEvent::ReasoningDelta(t) => {
                        ctx.emit(Event::ReasoningDelta { text: t.clone() })
                    }
                    _ => {}
                }
                b.push(&ev);
            }
            Err(e) => {
                if e.kind == ErrorKind::ContextLengthExceeded {
                    return Err(StepEnd::Overflow(format!("{}: {e}", s.provider.id)));
                }
                if matches!(e.kind, ErrorKind::Timeout | ErrorKind::Network) {
                    return Err(StepEnd::Dropped(format!("{}: {e}", s.provider.id)));
                }
                let hint = match e.kind {
                    ErrorKind::Auth => " · check the provider's api_key",
                    _ => "",
                };
                ctx.error(
                    EngineErrorKind::Provider,
                    format!("{}: {e}{hint}", s.provider.id),
                );
                return Err(StepEnd::Aborted("provider error".into()));
            }
        }
    }
    let tail = gate.finish();
    if !tail.is_empty() {
        ctx.emit(Event::AssistantDelta { text: tail });
    }
    let usage = b.usage;
    let cost = usage.as_ref().and_then(|u| {
        let c = s.model.cost.as_ref()?;
        Some(c.of(
            u.prompt_tokens,
            u.cached_prompt_tokens.unwrap_or(0),
            u.cache_write_tokens.unwrap_or(0),
            u.completion_tokens,
        ))
    });
    session::record_usage(ctx.log.as_ref(), usage.as_ref(), cost);
    if let (Some(agent), Some(u)) = (&ctx.agent, &usage) {
        session::record(
            ctx.main_log.as_ref(),
            "agent_usage",
            json!({ "agent": agent, "usage": u, "cost": cost }),
        );
    }
    if let Ok(mut st) = ctx.stats.lock() {
        st.request(usage.as_ref(), cost, sent.elapsed());
    }
    let mut msg = b.into_message();
    if let Some(first) = first {
        emit_speed(ctx, sent, first, usage, &msg);
    }
    if let Message::Assistant {
        tool_calls,
        content,
        sealed,
        ..
    } = &mut msg
    {
        if let Some(sd) = sealed {
            sd.provider.clone_from(&s.provider.id);
            sd.model.clone_from(&s.model.id);
            sd.fingerprint = fingerprint;
        }
        // Fallback chain: a model that printed its tool call as text still gets it run.
        let names = tool_names(s);
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let promoted = tool_calls.is_empty()
            && toolparse::extract(content, format, &names).is_some_and(|(rest, calls)| {
                tracing::debug!(count = calls.len(), "promoted text tool calls");
                *content = rest;
                *tool_calls = calls;
                true
            });
        if promoted {
            // The reply no longer matches what the server sent.
            *sealed = None;
        }
        if promoted || gate.hid() {
            // Promoted: show the text without the calls. Otherwise it only looked like a
            // call, so show what the gate held back.
            ctx.emit(Event::AssistantRewrite {
                text: content.clone(),
            });
        }
        dedupe_ids(tool_calls, history);
    }
    Ok((msg, usage))
}

/// Report time to first token and generation speed. Output tokens come from the server's
/// usage, or are estimated from the reply when it reports none.
fn emit_speed(
    ctx: &TurnCtx,
    sent: std::time::Instant,
    first: std::time::Instant,
    usage: Option<Usage>,
    msg: &Message,
) {
    let generating = first.elapsed().as_secs_f32();
    let out = usage
        .map(|u| u.completion_tokens)
        .unwrap_or_else(|| match msg {
            Message::Assistant {
                content,
                reasoning,
                tool_calls,
                ..
            } => {
                let calls: usize = tool_calls.iter().map(|c| c.arguments.len()).sum();
                tokens::estimate(content)
                    + reasoning.as_deref().map_or(0, tokens::estimate)
                    + u32::try_from(calls * 2 / 7).unwrap_or(0)
            }
            _ => 0,
        });
    // Very short replies give meaningless rates.
    let tokens_per_sec = (out >= 8 && generating > 0.05).then(|| out as f32 / generating);
    ctx.emit(Event::Speed {
        ttft_ms: u32::try_from((first - sent).as_millis()).unwrap_or(u32::MAX),
        tokens_per_sec,
    });
}

/// Servers that omit ids get `call_N`, which can collide across steps.
fn dedupe_ids(calls: &mut [ToolCall], history: &[Message]) {
    let mut used: HashSet<String> = history
        .iter()
        .filter_map(|m| match m {
            Message::Tool { call_id, .. } => Some(call_id.clone()),
            _ => None,
        })
        .collect();
    for c in calls {
        let mut n = 1;
        let base = c.id.clone();
        while used.contains(&c.id) {
            c.id = format!("{base}_{n}");
            n += 1;
        }
        used.insert(c.id.clone());
    }
}

/// Lenient argument parsing: empty means `{}`, a JSON string holding an object is unwrapped.
pub(crate) fn parse_args(raw: &str) -> Result<Value, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(Value::Object(Default::default()));
    }
    let v: Value = serde_json::from_str(raw)
        .or_else(|_| serde_json::from_str(&format!("{raw}}}")))
        .map_err(|e| format!("arguments are not valid JSON ({e}): {raw}"))?;
    match v {
        Value::Object(_) => Ok(v),
        Value::String(s) => parse_args(&s),
        other => Err(format!("arguments must be a JSON object, got {other}")),
    }
}

fn begin(ctx: &TurnCtx, call: &ToolCall, summary: &str) {
    ctx.emit(Event::ToolCallBegin {
        call: ToolCallInfo {
            call_id: CallId(ctx.key(&call.id)),
            name: call.name.clone(),
            summary: summary.to_owned(),
        },
    });
}

fn finish(ctx: &TurnCtx, call: &ToolCall, summary: &str, mut out: ToolOutput) -> ToolOutput {
    let kept = &ctx.setup.tool_ctx.observations;
    if let Some(full) = out.full.take()
        && kept.enabled()
    {
        let handle = session::observe(ctx.log.as_ref(), kept, &full);
        out.model.push('\n');
        out.model
            .push_str(&quack_tools::recall::hint(&handle, full.lines().count()));
    }
    session::record(
        ctx.log.as_ref(),
        "tool",
        serde_json::json!({
            "call_id": call.id,
            "summary": summary,
            "ok": out.ok,
            "result": out.summary,
        }),
    );
    ctx.emit(Event::ToolCallEnd {
        call_id: CallId(ctx.key(&call.id)),
        result: ToolResultSummary {
            ok: out.ok,
            summary: out.summary.clone(),
            detail: out.detail.clone(),
        },
    });
    out
}

/// A call that fails before it reaches a tool.
fn reject(ctx: &TurnCtx, call: &ToolCall, message: String) -> ToolOutput {
    begin(ctx, call, &call.name);
    finish(ctx, call, &call.name, ToolOutput::error(message))
}

/// Show the user an `ask_user` call's questions and wait for the answers; `None` when
/// the turn is interrupted.
async fn ask(ctx: &TurnCtx, call_id: &CallId, questions: Vec<Question>) -> Option<Answers> {
    let _one_at_a_time = ctx.prompt_lock.lock().await;
    let (otx, orx) = oneshot::channel();
    if let Ok(mut pending) = ctx.questions.lock() {
        pending.insert(call_id.0.clone(), otx);
    }
    ctx.emit(Event::Questions {
        call_id: call_id.clone(),
        questions: questions.clone(),
    });
    let asked = std::time::Instant::now();
    let answer = tokio::select! {
        () = ctx.cancel.cancelled() => None,
        a = orx => a.ok(),
    };
    if let Ok(mut st) = ctx.stats.lock() {
        st.waited(asked.elapsed());
    }
    if let Ok(mut pending) = ctx.questions.lock() {
        pending.remove(&call_id.0);
    }
    session::record(
        ctx.log.as_ref(),
        "questions",
        json!({
            "call_id": call_id.0,
            "questions": questions,
            "answers": answer.as_ref().map(|a| &a.answers),
            "stop_asking": answer.as_ref().is_some_and(|a| a.stop_asking),
        }),
    );
    answer
}

/// The internal tool and arguments behind a call, through the profile's names.
pub(crate) fn internal_call(s: &Setup, call: &ToolCall) -> Option<(String, Value)> {
    parse_args(&call.arguments)
        .ok()
        .and_then(|a| s.surface.map_call(&call.name, a))
}

/// Whether a call changes files, whatever the profile calls the tool.
fn edits(s: &Setup, call: &ToolCall) -> bool {
    internal_call(s, call)
        .and_then(|(name, args)| s.registry.get(&name)?.subject(&args, &s.tool_ctx).ok())
        .is_some_and(|sub| matches!(sub, Subject::Edit(_) | Subject::Edits(_)))
}

/// Whether `call` starts a read-only sub-agent, which may run beside others.
fn parallel(ctx: &TurnCtx, call: &ToolCall) -> bool {
    let s = &ctx.setup;
    let Some((internal, args)) = internal_call(s, call) else {
        return false;
    };
    internal == "task"
        && s.registry
            .get("task")
            .and_then(|t| t.subject(&args, &s.tool_ctx).ok())
            == Some(Subject::Agent { read_only: true })
}

/// Run one tool call through mapping, permissions and approval.
async fn run_call(ctx: &TurnCtx, call: &ToolCall) -> ToolOutput {
    let s = &ctx.setup;
    let call_id = CallId(ctx.key(&call.id));
    let plan_version = s.tool_ctx.plan.version();
    let args = match parse_args(&call.arguments) {
        Ok(a) => a,
        Err(e) => return reject(ctx, call, e),
    };
    let (tool, mut args) = match s.surface.map_call(&call.name, args.clone()) {
        Some((internal, args)) => match s.registry.get(&internal) {
            Some(tool) => (tool, args),
            None => return reject(ctx, call, format!("tool `{internal}` is not available")),
        },
        None => match s.mcp.find(&call.name, s.budget).filter(|_| s.mcp_tools) {
            Some(tool) => (tool, args),
            None => {
                return reject(
                    ctx,
                    call,
                    format!(
                        "unknown tool `{}`; available tools: {}",
                        call.name,
                        tool_names(s).join(", ")
                    ),
                );
            }
        },
    };

    let mut tctx = s.tool_ctx.clone();
    tctx.cancel = ctx.cancel.child_token();
    // Yolo and `/sandbox off` run commands outside the sandbox.
    if ctx.perms.lock().is_ok_and(|p| {
        p.sandbox_off || (p.mode == quack_config::settings::PermissionMode::Yolo && !p.plan)
    }) {
        tctx.sandbox = quack_tools::SandboxState::Off;
    }
    let summary = tool.summary(&args, &tctx);
    begin(ctx, call, &summary);
    let done = |out: ToolOutput| finish(ctx, call, &summary, out);

    args = match prepare(tool.as_ref(), args, &tctx).await {
        Ok(a) => a,
        Err(e) => return done(ToolOutput::error(e)),
    };
    let mut subject = match tool.subject(&args, &tctx) {
        Ok(sub) => sub,
        Err(e) => return done(ToolOutput::error(e)),
    };
    let root = &s.tool_ctx.root;
    let mut hooked = None;
    if ctx.hooks.any(HookEvent::PreTool) {
        let target = hooks::call_target(&subject, &args, root);
        let h = ctx
            .hooks(
                HookEvent::PreTool,
                json!({ "tool": tool.name(), "args": args, "target": target }),
                Target::Call {
                    tool: tool.name(),
                    target: &target,
                },
            )
            .await;
        if let Some((id, reason)) = h.denied() {
            let reason = if reason.is_empty() {
                "no reason given"
            } else {
                reason
            };
            return done(ToolOutput::error(format!(
                "blocked by hook `{id}`: {reason}"
            )));
        }
        if let Some(updated) = h.updated_args.clone() {
            args = match prepare(tool.as_ref(), updated, &tctx).await {
                Ok(a) => a,
                Err(e) => return done(ToolOutput::error(e)),
            };
            subject = match tool.subject(&args, &tctx) {
                Ok(sub) => sub,
                Err(e) => return done(ToolOutput::error(e)),
            };
        }
        hooked = h.verdict.map(|(v, _)| v);
    }
    let decision = ctx
        .perms
        .lock()
        .map(|p| p.check(&subject))
        .unwrap_or(Decision::Ask);
    // A deny rule beats a hook's allow, and a command outside the sandbox always asks.
    let decision = match (hooked, decision) {
        (_, deny @ Decision::Deny(_)) => deny,
        (Some(Verdict::Ask), _) => Decision::Ask,
        (Some(Verdict::Allow), Decision::Ask)
            if !matches!(subject, Subject::ShellOutsideSandbox(_)) =>
        {
            Decision::Allow
        }
        (_, d) => d,
    };
    match decision {
        Decision::Allow => {}
        Decision::Deny(reason) => {
            return done(ToolOutput::error(format!("permission denied: {reason}")));
        }
        Decision::Ask => {
            // If the call would fail anyway (e.g. old_string not found), skip asking.
            let request = match tool.preview(&args, &tctx) {
                Ok(r) => r,
                Err(e) => return done(ToolOutput::error(e)),
            };
            let always = ctx
                .perms
                .lock()
                .map(|p| p.rules_for(&subject))
                .unwrap_or_default();
            let _one_at_a_time = ctx.prompt_lock.lock().await;
            let (otx, orx) = oneshot::channel();
            if let Ok(mut pending) = ctx.approvals.lock() {
                pending.insert(call_id.0.clone(), otx);
            }
            ctx.emit(Event::ApprovalRequest {
                call_id: call_id.clone(),
                request,
                always: always.clone(),
            });
            let asked = std::time::Instant::now();
            let answer = tokio::select! {
                () = ctx.cancel.cancelled() => None,
                a = orx => a.ok(),
            };
            if let Ok(mut st) = ctx.stats.lock() {
                st.waited(asked.elapsed());
            }
            if let Ok(mut pending) = ctx.approvals.lock() {
                pending.remove(&call_id.0);
            }
            session::record(
                ctx.log.as_ref(),
                "approval",
                serde_json::json!({ "call_id": call.id, "decision": answer }),
            );
            match answer {
                None => return done(ToolOutput::error("interrupted by the user")),
                Some(ApprovalDecision::AllowOnce) => {}
                Some(ApprovalDecision::AllowSession) => {
                    if let Ok(mut p) = ctx.perms.lock() {
                        p.allow_session(subject.clone());
                    }
                }
                Some(ApprovalDecision::AlwaysAllow { scope }) => {
                    if let Ok(mut p) = ctx.perms.lock() {
                        p.allow_session(subject.clone());
                    }
                    save_always(ctx, &scope, &always);
                }
                Some(ApprovalDecision::Deny) => {
                    return done(ToolOutput::error(
                        "the user declined this action. Ask them how to proceed if unsure.",
                    ));
                }
                Some(ApprovalDecision::DenyWithFeedback { feedback }) => {
                    return done(ToolOutput::error(format!(
                        "the user declined this action and said: {feedback}"
                    )));
                }
            }
        }
    }

    let (otx, mut orx) = mpsc::unbounded_channel::<String>();
    tctx.on_output = Some(otx);
    let tx = ctx.tx.clone();
    let id = call_id.clone();
    let forward = tokio::spawn(async move {
        while let Some(chunk) = orx.recv().await {
            let _ = tx.send(Event::ToolCallOutputDelta {
                call_id: id.clone(),
                chunk,
            });
        }
    });
    // An undo point for each file the call changes.
    let checkpoints: Vec<_> = match &ctx.checkpoints {
        Some(cp) => subject
            .edited()
            .iter()
            .filter_map(|p| match cp.before(ctx.turn.0, &call_id.0, p) {
                Ok(seq) => Some((cp, seq, p)),
                Err(e) => {
                    tracing::warn!(error = %e, path = %p.display(), "no undo point for this edit");
                    None
                }
            })
            .collect(),
        None => Vec::new(),
    };
    // Diagnostics before an edit, to report only what the edit changed.
    let lsp = tctx.lsp.clone();
    let lsp_before = match (&subject, &lsp) {
        (Subject::Edit(p), Some(m)) => m.before_edit(p).await,
        _ => None,
    };
    let post_args = ctx.hooks.any(HookEvent::PostTool).then(|| args.clone());
    let (atx, mut arx) = mpsc::unbounded_channel();
    tctx.ask = Some(atx);
    let (stx, mut srx) = mpsc::unbounded_channel();
    tctx.spawn = Some(stx);
    // The call runs until done; questions it asks on the way go to the user, and
    // sub-agents it starts run here.
    let mut out = {
        let run = tool.run(args, &tctx);
        tokio::pin!(run);
        loop {
            tokio::select! {
                o = &mut run => break o,
                Some((questions, reply)) = arx.recv() => {
                    // Dropping `reply` when interrupted lets the call see the cancellation.
                    if let Some(a) = ask(ctx, &call_id, questions).await {
                        let _ = reply.send(a);
                    }
                }
                Some((request, reply)) = srx.recv() => {
                    let _ = reply.send(crate::agent::run(ctx, &call_id.0, request).await);
                }
            }
        }
    };
    if s.tool_ctx.plan.version() != plan_version {
        ctx.emit(Event::Plan {
            checklist: s.tool_ctx.plan.checklist(),
        });
    }
    drop(tctx);
    let _ = forward.await;
    for (cp, seq, p) in checkpoints {
        if let Err(e) = cp.after(seq, p, out.ok) {
            tracing::warn!(error = %e, "could not record the edit for undo");
        }
    }
    if out.ok
        && let Some(m) = &lsp
    {
        match (&subject, lsp_before) {
            (Subject::Read(p), _) => m.touch(p),
            (Subject::Edit(p), Some(before)) => {
                let root = ctx.setup.tool_ctx.root.clone();
                let display = move |f: &std::path::Path| quack_tools::paths::display(&root, f);
                let report = tokio::select! {
                    r = m.after_edit(p, &before, &display) => r,
                    () = ctx.cancel.cancelled() => None,
                };
                if let Some(text) = report {
                    if let Some(n) = text
                        .strip_prefix("diagnostics (")
                        .and_then(|t| t.split(' ').next())
                    {
                        out.summary.push_str(&format!(" · {n} new diagnostic(s)"));
                    }
                    out.model.push_str("\n\n");
                    out.model.push_str(&text);
                    out.detail = Some(match out.detail.take() {
                        Some(d) => format!("{d}\n{text}"),
                        None => text,
                    });
                }
            }
            // Several files at once (a rename, a patch): the servers get the new text now.
            (Subject::Edits(ps), _) => {
                for p in ps {
                    m.touch(p);
                }
            }
            _ => {}
        }
    }
    // AGENTS.md files below the root arrive with the first result that touches them.
    if out.ok
        && let Subject::Read(p) | Subject::Edit(p) = &subject
        && let Some(extra) = ctx.nested.attach(p)
    {
        out.model.push_str("\n\n");
        out.model.push_str(&extra);
    }
    if let Some(args) = post_args
        && !ctx.cancel.is_cancelled()
    {
        let target = hooks::call_target(&subject, &args, root);
        let h = ctx
            .hooks(
                HookEvent::PostTool,
                json!({
                    "tool": tool.name(),
                    "args": args,
                    "target": target,
                    "ok": out.ok,
                    "output": out.model,
                }),
                Target::Call {
                    tool: tool.name(),
                    target: &target,
                },
            )
            .await;
        if let Some(text) = h.context_text() {
            out.model.push_str("\n\n");
            out.model.push_str(&text);
        }
    }
    done(out)
}

/// A tool's work before the permission check, stopped by an interrupt.
async fn prepare(
    tool: &dyn quack_tools::Tool,
    args: Value,
    tctx: &quack_tools::ToolCtx,
) -> Result<Value, String> {
    tokio::select! {
        r = tool.prepare(args, tctx) => r,
        () = tctx.cancel.cancelled() => Err("interrupted by the user".into()),
    }
}

/// "Always allow": save the call's rules to config, so later sessions allow it too.
pub(crate) fn save_always(ctx: &TurnCtx, scope: &WriteScope, rules: &[String]) {
    if rules.is_empty() || *scope == WriteScope::Session {
        return;
    }
    let project = *scope == WriteScope::Project;
    match ctx.pool.config.save_allow_rules(project, rules) {
        Ok(path) => ctx.emit(Event::SettingSaved {
            key: "permissions.allow".into(),
            value: rules.join(", "),
            path: path.display().to_string(),
            note: None,
        }),
        Err(e) => ctx.emit(Event::Warning {
            message: format!("allowed for this session only; cannot save the rule: {e}"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn images_become_a_note_for_a_model_without_vision() {
        let img = || quack_protocol::Image {
            media_type: "image/png".into(),
            data: "eHk=".into(),
            width: Some(2),
            height: Some(1),
        };
        let mut messages = vec![
            Message::User {
                content: String::new(),
                images: vec![img(), img()],
            },
            Message::Tool {
                call_id: "c1".into(),
                content: "[image: pic.png]".into(),
                images: vec![img()],
            },
        ];
        without_images(&mut messages);
        let Message::User { content, images } = &messages[0] else {
            panic!("user message")
        };
        assert!(images.is_empty());
        assert_eq!(content, "[2 images not sent: this model takes no images]");
        let Message::Tool {
            content, images, ..
        } = &messages[1]
        else {
            panic!("tool message")
        };
        assert!(images.is_empty());
        assert_eq!(
            content,
            "[image: pic.png]\n\n[1 image not sent: this model takes no images]"
        );
    }

    #[test]
    fn lenient_args() {
        assert_eq!(parse_args(""), Ok(serde_json::json!({})));
        assert_eq!(
            parse_args(r#""{\"a\":1}""#),
            Ok(serde_json::json!({"a": 1}))
        );
        assert_eq!(parse_args(r#"{"a":1"#), Ok(serde_json::json!({"a": 1})));
        assert!(parse_args("[1]").is_err());
    }

    #[test]
    fn duplicate_ids_are_renamed() {
        let history = vec![Message::Tool {
            call_id: "call_0".into(),
            content: String::new(),
            images: Vec::new(),
        }];
        let mut calls = vec![ToolCall {
            id: "call_0".into(),
            name: "read".into(),
            arguments: "{}".into(),
        }];
        dedupe_ids(&mut calls, &history);
        assert_eq!(calls[0].id, "call_0_1");
    }
}
