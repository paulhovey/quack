//! One user turn: stream the model, run its tool calls, loop until it answers
//! (docs/plan/04-agent-loop-and-tools.md#turn-lifecycle).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use cutoffharness_protocol::{
    ApprovalDecision, CallId, EngineError, EngineErrorKind, Event, ToolCallInfo, ToolResultSummary,
    TurnId, Usage,
};
use cutoffharness_providers::{
    ChatRequest, ErrorKind, Message, ResponseBuilder, StreamEvent, ToolCall, ToolSpec, toolparse,
};
use cutoffharness_tools::{Decision, Permissions, ToolOutput};
use serde_json::Value;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

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
    pub approvals: Approvals,
    pub tx: broadcast::Sender<Event>,
    pub cancel: CancellationToken,
    pub log: Option<SharedLog>,
}

impl TurnCtx {
    /// Add to history and the session log together, so the log is the source of truth.
    fn push(&self, history: &mut Vec<Message>, m: Message) {
        session::record_message(self.log.as_ref(), &m);
        history.push(m);
    }

    fn emit(&self, e: Event) {
        let _ = self.tx.send(e);
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

enum StepEnd {
    Done,
    Aborted(String),
}

/// Run the turn; returns the updated history.
pub(crate) async fn run(ctx: TurnCtx, mut history: Vec<Message>, prompt: String) -> Vec<Message> {
    ctx.emit(Event::TurnStarted { turn: ctx.turn });
    ctx.push(&mut history, Message::User { content: prompt });
    let mut total = Usage::default();
    let mut recent: Vec<(String, String)> = Vec::new();

    let end = 'steps: {
        for _ in 0..MAX_STEPS {
            let (msg, usage) = match stream_step(&ctx, &history).await {
                Ok(r) => r,
                Err(StepEnd::Aborted(reason)) => break 'steps StepEnd::Aborted(reason),
                Err(StepEnd::Done) => break 'steps StepEnd::Done,
            };
            if let Some(u) = usage {
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
            ctx.push(&mut history, msg);
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
        StepEnd::Aborted(reason) => ctx.emit(Event::TurnAborted {
            turn: ctx.turn,
            reason,
        }),
    }
    history
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
    let mut sampling = s.profile.file.defaults.sampling.clone();
    let m = &s.model.sampling;
    sampling.temperature = m.temperature.or(sampling.temperature);
    sampling.top_p = m.top_p.or(sampling.top_p);
    sampling.top_k = m.top_k.or(sampling.top_k);
    sampling.min_p = m.min_p.or(sampling.min_p);
    sampling.repetition_penalty = m.repetition_penalty.or(sampling.repetition_penalty);
    let req = ChatRequest {
        model: s.model.id.clone(),
        messages,
        tools: s
            .surface
            .tools
            .iter()
            .map(|t| ToolSpec {
                name: t.name.clone(),
                description: t.description.clone(),
                parameters: t.schema.clone(),
            })
            .collect(),
        max_tokens: s.model.max_output,
        sampling,
        reasoning_effort: Some(s.model.reasoning.effort.clone()).filter(|e| !e.is_empty()),
        extra_body: serde_json::to_value(&s.model.extra_body)
            .ok()
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default(),
    };

    let reasoning = s
        .profile
        .file
        .defaults
        .reasoning_format
        .unwrap_or(s.model.reasoning.format);
    let mut rx = s.provider.stream(req, reasoning, ctx.cancel.child_token());
    let mut b = ResponseBuilder::default();
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
                match &ev {
                    StreamEvent::TextDelta(t) => {
                        ctx.emit(Event::AssistantDelta { text: t.clone() })
                    }
                    StreamEvent::ReasoningDelta(t) => {
                        ctx.emit(Event::ReasoningDelta { text: t.clone() })
                    }
                    _ => {}
                }
                b.push(&ev);
            }
            Err(e) => {
                let hint = match e.kind {
                    ErrorKind::ContextLengthExceeded => {
                        " · the conversation is too long for this model; compaction lands in M2"
                    }
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
    let usage = b.usage;
    let mut msg = b.into_message();
    if let Message::Assistant {
        tool_calls,
        content,
        ..
    } = &mut msg
    {
        // Fallback chain: a model that printed its tool call as text still gets it run.
        if tool_calls.is_empty() {
            let names: Vec<&str> = s.surface.tools.iter().map(|t| t.name.as_str()).collect();
            let format = toolparse::TextFormat::parse(&s.model.tool_calling.text_format);
            if let Some((rest, calls)) = toolparse::extract(content, format, &names) {
                tracing::debug!(count = calls.len(), "promoted text tool calls");
                ctx.emit(Event::AssistantRewrite { text: rest.clone() });
                *content = rest;
                *tool_calls = calls;
            }
        }
        dedupe_ids(tool_calls, history);
    }
    Ok((msg, usage))
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
fn parse_args(raw: &str) -> Result<Value, String> {
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
    let Some((internal, args)) = s.surface.map_call(&call.name, args) else {
        let names: Vec<&str> = s.surface.tools.iter().map(|t| t.name.as_str()).collect();
        return reject(
            ctx,
            call,
            format!(
                "unknown tool `{}`; available tools: {}",
                call.name,
                names.join(", ")
            ),
        );
    };
    let Some(tool) = s.registry.get(&internal) else {
        return reject(ctx, call, format!("tool `{internal}` is not available"));
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
                        p.allow_session(subject);
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
    let out = tool.run(args, &tctx).await;
    drop(tctx);
    let _ = forward.await;
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
