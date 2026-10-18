//! One user turn: stream the model, run its tool calls, loop until it answers
//! (docs/plan/04-agent-loop-and-tools.md#turn-lifecycle).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use cutoffharness_protocol::{
    ApprovalDecision, CallId, EngineError, EngineErrorKind, Event, ToolCallInfo, ToolResultSummary,
    TurnId, Usage,
};
use cutoffharness_providers::{
    ChatRequest, ErrorKind, Message, ResponseBuilder, StreamEvent, ToolCall, ToolSpec, tokens,
    toolparse,
};
use cutoffharness_store::checkpoint::Checkpoints;
use cutoffharness_tools::{Decision, Permissions, Subject, ToolOutput};
use serde_json::Value;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use cutoffharness_context::compact;

use crate::session::{self, SharedLog};
use crate::startup::Setup;

/// Steps (model calls) per turn before pausing.
const MAX_STEPS: usize = 100;
/// Identical consecutive calls before the model is told to change approach.
const REPEAT_LIMIT: usize = 3;

pub(crate) type Approvals = Arc<Mutex<HashMap<String, oneshot::Sender<ApprovalDecision>>>>;

pub(crate) struct TurnCtx {
    pub turn: TurnId,
    pub setup: Arc<Setup>,
    pub perms: Arc<Mutex<Permissions>>,
    pub nested: Arc<cutoffharness_instructions::agents_md::Nested>,
    pub approvals: Approvals,
    pub tx: broadcast::Sender<Event>,
    pub cancel: CancellationToken,
    pub log: Option<SharedLog>,
    /// Pre-images of edited files, for `/undo`.
    pub checkpoints: Option<Arc<Checkpoints>>,
}

impl TurnCtx {
    /// Add to history and the session log together, so the log is the source of truth.
    fn push(&self, history: &mut Vec<Message>, m: Message) {
        session::record_message(self.log.as_ref(), &m);
        history.push(m);
    }

    pub(crate) fn emit(&self, e: Event) {
        let _ = self.tx.send(e);
    }

    /// Emit the corrected estimate of what the next request will cost.
    pub(crate) fn context_estimate(&self, history: &[Message]) {
        self.emit(Event::ContextUpdate {
            used: estimate(&self.setup, history),
            budget: self.setup.budget.unwrap_or(0),
            cache_hit_tokens: None,
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
const EMPTY_NUDGE: &str =
    "Your last reply was empty. Continue: call a tool if you need one, otherwise answer.";

enum StepEnd {
    Done,
    Aborted(String),
    /// The server said the request is too long for the model.
    Overflow(String),
}

/// Run the turn; returns the updated history.
pub(crate) async fn run(ctx: TurnCtx, mut history: Vec<Message>, prompt: String) -> Vec<Message> {
    ctx.emit(Event::TurnStarted { turn: ctx.turn });
    ctx.push(&mut history, Message::User { content: prompt });
    let mut total = Usage::default();
    let mut recent: Vec<(String, String)> = Vec::new();
    let mut nudged = false;
    let mut overflowed = false;

    let end = 'steps: {
        for _ in 0..MAX_STEPS {
            crate::context::fit(&ctx, &mut history).await;
            let estimated = raw_estimate(&ctx.setup, &history);
            let (msg, usage) = match stream_step(&ctx, &history).await {
                Ok(r) => r,
                Err(StepEnd::Aborted(reason)) => break 'steps StepEnd::Aborted(reason),
                Err(StepEnd::Done) => break 'steps StepEnd::Done,
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
                session::record_usage(ctx.log.as_ref(), &u);
                total.prompt_tokens = u.prompt_tokens;
                total.completion_tokens += u.completion_tokens;
                total.cached_prompt_tokens = u.cached_prompt_tokens;
                ctx.emit(Event::ContextUpdate {
                    used: u.prompt_tokens + u.completion_tokens,
                    budget: ctx.setup.budget.unwrap_or(0),
                    cache_hit_tokens: u.cached_prompt_tokens,
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
            for (i, call) in calls.iter().enumerate() {
                if ctx.cancel.is_cancelled() {
                    // Every tool call needs a result, or the next request is invalid.
                    for c in &calls[i..] {
                        ctx.push(
                            &mut history,
                            Message::Tool {
                                call_id: c.id.clone(),
                                content: "Not run: interrupted by the user.".into(),
                            },
                        );
                    }
                    break 'steps StepEnd::Aborted("interrupted".into());
                }
                recent.push((call.name.clone(), call.arguments.clone()));
                let mut out = run_call(&ctx, call).await;
                let repeats = recent
                    .iter()
                    .rev()
                    .take_while(|c| **c == recent[recent.len() - 1])
                    .count();
                if repeats >= REPEAT_LIMIT {
                    out.model.push_str(&format!(
                        "\n\nNote: this exact call has now been made {repeats} times in a row. \
                         Try a different approach."
                    ));
                }
                ctx.push(
                    &mut history,
                    Message::Tool {
                        call_id: call.id.clone(),
                        content: out.model,
                    },
                );
            }
            // Tool output can be large; show its weight before the next request.
            ctx.context_estimate(&history);
        }
        ctx.emit(Event::Warning {
            message: format!("stopped after {MAX_STEPS} steps; send a message to continue"),
        });
        StepEnd::Done
    };

    match end {
        StepEnd::Done => ctx.emit(Event::TurnComplete {
            turn: ctx.turn,
            usage: total,
        }),
        StepEnd::Aborted(reason) | StepEnd::Overflow(reason) => ctx.emit(Event::TurnAborted {
            turn: ctx.turn,
            reason,
        }),
    }
    history
}

/// The next request's prompt tokens: the byte estimate corrected by reported usage.
pub(crate) fn estimate(s: &Setup, history: &[Message]) -> u32 {
    let raw = raw_estimate(s, history);
    s.estimator.lock().map_or(raw, |e| e.scale(raw))
}

/// Uncorrected estimate of a request's prompt: system prompt, history and tool definitions.
fn raw_estimate(s: &Setup, history: &[Message]) -> u32 {
    let history = serde_json::to_string(history).unwrap_or_default();
    let tools: usize = tool_specs(s)
        .iter()
        .map(|t| t.description.len() + t.name.len() + t.parameters.to_string().len())
        .sum();
    tokens::estimate(&s.system)
        .saturating_add(tokens::estimate(&history))
        .saturating_add(u32::try_from(tools * 2 / 7).unwrap_or(u32::MAX))
}

/// A request to the session's model with its sampling and extra parameters; with the
/// session's tools when `tools` is set.
pub(crate) fn request(s: &Setup, messages: Vec<Message>, tools: bool) -> ChatRequest {
    let mut sampling = s.profile.file.defaults.sampling.clone();
    let m = &s.model.sampling;
    sampling.temperature = m.temperature.or(sampling.temperature);
    sampling.top_p = m.top_p.or(sampling.top_p);
    sampling.top_k = m.top_k.or(sampling.top_k);
    sampling.min_p = m.min_p.or(sampling.min_p);
    sampling.repetition_penalty = m.repetition_penalty.or(sampling.repetition_penalty);
    ChatRequest {
        model: s.model.id.clone(),
        messages,
        tools: if tools { tool_specs(s) } else { Vec::new() },
        context_window: s.model.context_window,
        max_tokens: s.model.max_output,
        sampling,
        reasoning_effort: Some(s.model.reasoning.effort.clone()).filter(|e| !e.is_empty()),
        extra_body: serde_json::to_value(&s.model.extra_body)
            .ok()
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default(),
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
    specs.extend(s.mcp.offered(s.budget).into_iter().map(|t| ToolSpec {
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

pub(crate) fn reasoning_format(s: &Setup) -> cutoffharness_config::provider::ReasoningFormat {
    s.profile
        .file
        .defaults
        .reasoning_format
        .unwrap_or(s.model.reasoning.format)
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
    let req = request(s, messages, true);
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
    let mut msg = b.into_message();
    if let Some(first) = first {
        emit_speed(ctx, sent, first, usage, &msg);
    }
    if let Message::Assistant {
        tool_calls,
        content,
        ..
    } = &mut msg
    {
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
            call_id: CallId(call.id.clone()),
            name: call.name.clone(),
            summary: summary.to_owned(),
        },
    });
}

fn finish(ctx: &TurnCtx, call: &ToolCall, summary: &str, out: ToolOutput) -> ToolOutput {
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
        call_id: CallId(call.id.clone()),
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

/// Run one tool call through mapping, permissions and approval.
async fn run_call(ctx: &TurnCtx, call: &ToolCall) -> ToolOutput {
    let s = &ctx.setup;
    let call_id = CallId(call.id.clone());
    let args = match parse_args(&call.arguments) {
        Ok(a) => a,
        Err(e) => return reject(ctx, call, e),
    };
    let (tool, args) = match s.surface.map_call(&call.name, args.clone()) {
        Some((internal, args)) => match s.registry.get(&internal) {
            Some(tool) => (tool, args),
            None => return reject(ctx, call, format!("tool `{internal}` is not available")),
        },
        None => match s.mcp.find(&call.name, s.budget) {
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
    let summary = tool.summary(&args, &tctx);
    begin(ctx, call, &summary);
    let done = |out: ToolOutput| finish(ctx, call, &summary, out);

    let subject = match tool.subject(&args, &tctx) {
        Ok(sub) => sub,
        Err(e) => return done(ToolOutput::error(e)),
    };
    let decision = ctx
        .perms
        .lock()
        .map(|p| p.check(&subject))
        .unwrap_or(Decision::Ask);
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
            let (otx, orx) = oneshot::channel();
            if let Ok(mut pending) = ctx.approvals.lock() {
                pending.insert(call.id.clone(), otx);
            }
            ctx.emit(Event::ApprovalRequest {
                call_id: call_id.clone(),
                request,
            });
            let answer = tokio::select! {
                () = ctx.cancel.cancelled() => None,
                a = orx => a.ok(),
            };
            if let Ok(mut pending) = ctx.approvals.lock() {
                pending.remove(&call.id);
            }
            session::record(
                ctx.log.as_ref(),
                "approval",
                serde_json::json!({ "call_id": call.id, "decision": answer }),
            );
            match answer {
                None => return done(ToolOutput::error("interrupted by the user")),
                Some(ApprovalDecision::AllowOnce) => {}
                Some(ApprovalDecision::AllowSession | ApprovalDecision::AlwaysAllow { .. }) => {
                    if let Ok(mut p) = ctx.perms.lock() {
                        p.allow_session(subject.clone());
                    }
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
    let checkpoint = match (&subject, &ctx.checkpoints) {
        (Subject::Edit(p), Some(cp)) => match cp.before(ctx.turn.0, &call.id, p) {
            Ok(seq) => Some((cp, seq, p)),
            Err(e) => {
                tracing::warn!(error = %e, path = %p.display(), "no undo point for this edit");
                None
            }
        },
        _ => None,
    };
    let mut out = tool.run(args, &tctx).await;
    drop(tctx);
    let _ = forward.await;
    if let Some((cp, seq, p)) = checkpoint
        && let Err(e) = cp.after(seq, p, out.ok)
    {
        tracing::warn!(error = %e, "could not record the edit for undo");
    }
    // AGENTS.md files below the root arrive with the first result that touches them.
    if out.ok
        && let Subject::Read(p) | Subject::Edit(p) = &subject
        && let Some(extra) = ctx.nested.attach(p)
    {
        out.model.push_str("\n\n");
        out.model.push_str(&extra);
    }
    done(out)
}

#[cfg(test)]
mod tests {
    use super::*;

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
