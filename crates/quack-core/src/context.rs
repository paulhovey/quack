//! Keeping requests inside the budget: pruning, then compaction
//! (docs/plan/05-context-management.md). Both are recorded in the session log, so a resume
//! rebuilds the same history.

use quack_config::settings::CacheStrategy;
use quack_context::compact;
use quack_context::prune::{self, Access};
use quack_protocol::{ContextReport, ContextSegment, Event, PruneReport};
use quack_providers::{Message, StreamEvent, ToolCall, tokens};
use quack_tools::Subject;
use serde_json::{Value, json};

use crate::session;
use crate::startup::Setup;
use crate::turn::{self, TurnCtx};

/// Share of the budget that history kept verbatim may use after a compaction.
const KEEP_RATIO: f64 = 0.35;
/// Messages at the end of history that pruning leaves alone.
const PROTECT_MESSAGES: usize = 8;
/// Assumed budget when the model's window is unknown.
const FALLBACK_BUDGET: u32 = 32_000;

/// Before each request: prune at the threshold (or every step with `aggressive`), and
/// compact when pruning was not enough. Failures are warnings; the request still goes out.
pub(crate) async fn fit(ctx: &TurnCtx, history: &mut Vec<Message>) {
    let s = &ctx.setup;
    let Some(budget) = s.budget else { return };
    let threshold = (f64::from(budget) * s.context.compaction_threshold.clamp(0.1, 1.0)) as u32;
    let aggressive = s.context.cache_strategy == CacheStrategy::Aggressive;
    if !aggressive && turn::estimate(s, history) <= threshold {
        return;
    }
    prune_now(ctx, history);
    if turn::estimate(s, history) > threshold {
        let reason = format!("context is over {}% of the budget", percent(s));
        if let Err(e) = compact_now(ctx, history, compact::KEEP_TURNS, None, &reason).await {
            ctx.emit(Event::Warning {
                message: format!("could not compact: {e}"),
            });
        }
    }
}

fn percent(s: &Setup) -> u32 {
    (s.context.compaction_threshold * 100.0).round() as u32
}

/// Apply the pruning rules to everything but the most recent messages.
pub(crate) fn prune_now(ctx: &TurnCtx, history: &mut [Message]) {
    let protect_from = history.len().saturating_sub(PROTECT_MESSAGES);
    let s = &ctx.setup;
    let root = &s.tool_ctx.root;
    let hooks = prune::Hooks {
        classify: &|c| classify(s, c),
        stale: &|p| s.tool_ctx.reads.is_stale(std::path::Path::new(p)),
        outline: &|p, text| {
            let path = std::path::Path::new(p);
            quack_tools::outline::outline(path, text)
                .filter(|o| !o.symbols.is_empty())
                .map(|o| o.render(&quack_tools::paths::display(root, path)))
        },
    };
    let ops = prune::plan(history, protect_from, &hooks);
    if ops.is_empty() {
        return;
    }
    let removed = prune::apply(history, &ops);
    session::record(ctx.log.as_ref(), "prune", json!({ "ops": ops }));
    let items = ops
        .iter()
        .map(|o| o.content.lines().next().unwrap_or_default().to_owned())
        .collect();
    ctx.emit(Event::ContextPruned {
        report: PruneReport {
            removed_tokens: u32::try_from(removed * 2 / 7).unwrap_or(u32::MAX),
            items,
        },
    });
}

/// What a call did to files, through the same mapping and tools that ran it.
fn classify(s: &Setup, call: &ToolCall) -> Access {
    let Some((internal, args)) = turn::parse_args(&call.arguments)
        .ok()
        .and_then(|a| s.surface.map_call(&call.name, a))
    else {
        return Access::Other;
    };
    let Some(tool) = s.registry.get(&internal) else {
        return Access::Other;
    };
    match tool.subject(&args, &s.tool_ctx) {
        // `glob`, `grep` and `list_dir` are reads too, but of many files.
        // A symbol read is a part, and a large file read without a range was its outline:
        // neither makes an earlier read of that file out of date.
        Ok(Subject::Read(p))
            if internal == "read"
                && args.get("symbol").is_none()
                && (read_lines(&args).is_some() || !outlined(s, &p)) =>
        {
            Access::Read {
                path: p.display().to_string(),
                lines: read_lines(&args),
            }
        }
        Ok(sub @ (Subject::Edit(_) | Subject::Edits(_))) => Access::Write {
            paths: sub
                .edited()
                .iter()
                .map(|p| p.display().to_string())
                .collect(),
        },
        _ => Access::Other,
    }
}

/// Whether a whole-file `read` of `p` returns its outline instead.
fn outlined(s: &Setup, p: &std::path::Path) -> bool {
    let above = s.tool_ctx.outline_above;
    above > 0
        && quack_tools::outline::supported(p)
        && std::fs::metadata(p).is_ok_and(|m| m.len() as usize > above)
}

fn read_lines(args: &Value) -> Option<(u64, u64)> {
    let n = |k: &str| args.get(k).and_then(Value::as_u64);
    match (n("offset"), n("limit")) {
        (None | Some(0 | 1), None) => None,
        (offset, limit) => {
            let first = offset.unwrap_or(1).max(1);
            Some((first, limit.map_or(u64::MAX, |l| first + l.max(1) - 1)))
        }
    }
}

/// Summarize history before the cut point with the session's model and splice the summary
/// in. `keep_turns` recent turns stay verbatim when they fit.
pub(crate) async fn compact_now(
    ctx: &TurnCtx,
    history: &mut Vec<Message>,
    keep_turns: usize,
    focus: Option<&str>,
    reason: &str,
) -> Result<(), String> {
    let s = &ctx.setup;
    let budget = s.budget.unwrap_or(FALLBACK_BUDGET);
    let keep = (f64::from(budget) * KEEP_RATIO) as u32;
    let scale = |h: &[Message]| {
        let raw = tokens::estimate(&serde_json::to_string(h).unwrap_or_default());
        s.estimator.lock().map_or(raw, |e| e.scale(raw))
    };
    let cut = compact::cut_point(history, keep_turns, keep, scale)
        .ok_or("nothing to compact yet: the conversation is only the current turn")?;
    ctx.emit(Event::Compacting {
        reason: reason.to_owned(),
    });

    let mut prompt = s
        .profile
        .compact_prompt
        .clone()
        .unwrap_or_else(|| compact::DEFAULT_PROMPT.to_owned());
    if let Some(f) = focus.map(str::trim).filter(|f| !f.is_empty()) {
        prompt.push_str(&format!("\n\nPay particular attention to: {f}"));
    }
    // About 60% of the budget in characters, leaving room for the prompt and the reply.
    let text = compact::transcript(&history[..cut], budget as usize * 2);
    let messages = vec![
        Message::System { content: prompt },
        Message::User { content: text },
    ];
    let mut req = turn::request(s, messages, false);
    // A looping model (small ones sometimes think forever) fails fast instead.
    let cap = (budget / 4).clamp(1_024, 8_192);
    req.max_tokens = Some(s.model.max_output.map_or(cap, |m| m.min(cap)));
    let summary = match summarize(ctx, req).await {
        Ok(text) => text,
        Err(e) if ctx.cancel.is_cancelled() => return Err(e),
        Err(e) => {
            ctx.emit(Event::Warning {
                message: format!(
                    "the model could not write a summary ({e}); keeping a plain list of \
                     earlier requests and tool calls instead"
                ),
            });
            compact::fallback_summary(&history[..cut])
        }
    };

    let request = compact::carried_request(history, cut).map(str::to_owned);
    let message = compact::summary_message(&summary, request.as_deref());
    compact::apply(history, cut, message.clone());
    session::record(
        ctx.log.as_ref(),
        "compaction",
        json!({ "cut": cut, "message": message, "reason": reason }),
    );
    ctx.emit(Event::Compacted {
        summary_tokens: tokens::estimate(&summary),
        replaced: u32::try_from(cut).unwrap_or(u32::MAX),
    });
    ctx.context_estimate(history);
    Ok(())
}

/// Stream a request with no tools and return its text.
async fn summarize(ctx: &TurnCtx, req: quack_providers::ChatRequest) -> Result<String, String> {
    let s = &ctx.setup;
    let mut rx = s
        .provider
        .stream(req, turn::reasoning_format(s), ctx.cancel.child_token());
    let mut text = String::new();
    loop {
        let item = tokio::select! {
            () = ctx.cancel.cancelled() => return Err("interrupted".into()),
            item = rx.recv() => item,
        };
        match item {
            None => break,
            Some(Ok(StreamEvent::TextDelta(t))) => text.push_str(&t),
            Some(Ok(_)) => {}
            Some(Err(e)) => return Err(format!("{}: {e}", s.provider.id)),
        }
    }
    let text = text.trim();
    if text.is_empty() {
        return Err("the model returned an empty summary".into());
    }
    Ok(text.to_owned())
}

/// Requests estimated above this get pruned, then compacted.
pub(crate) fn threshold(s: &Setup) -> Option<u32> {
    s.budget
        .map(|b| (f64::from(b) * s.context.compaction_threshold.clamp(0.1, 1.0)) as u32)
}

/// What is using the context. `history` is `None` while a turn holds it.
pub(crate) fn report(s: &Setup, history: Option<&[Message]>) -> ContextReport {
    let ratio = s.estimator.lock().map_or(1.0, |e| e.ratio());
    let tok = |text: &str| (f64::from(tokens::estimate(text)) * ratio).round() as u32;
    let mut segments = vec![
        ContextSegment {
            name: "System prompt".into(),
            tokens: tok(&s.system),
            detail: format!("profile {}, with instructions", s.profile.id),
        },
        ContextSegment {
            name: "Tool definitions".into(),
            tokens: tok(
                &serde_json::to_string(&turn::request(s, Vec::new(), true).tools)
                    .unwrap_or_default(),
            ),
            detail: format!("{} tools", s.surface.tools.len()),
        },
    ];
    let mut note = None;
    match history {
        None => note = Some("a turn is running; the conversation is shown between turns".into()),
        Some(history) => {
            let mut groups: [(&str, u32, usize, usize); 4] = [
                ("Summary", 0, 0, 0),
                ("Your messages", 0, 0, 0),
                ("Model replies and tool calls", 0, 0, 0),
                ("Tool results", 0, 0, 0),
            ];
            for m in history {
                let g = match m {
                    _ if compact::is_summary(m) => 0,
                    Message::User { .. } | Message::System { .. } => 1,
                    Message::Assistant { .. } => 2,
                    Message::Tool { .. } => 3,
                };
                groups[g].1 += tok(&serde_json::to_string(m).unwrap_or_default());
                groups[g].2 += 1;
                if matches!(m, Message::Tool { content, .. } if content.starts_with(prune::MARKER))
                {
                    groups[g].3 += 1;
                }
            }
            for (i, (name, tokens, count, pruned)) in groups.into_iter().enumerate() {
                if count == 0 {
                    continue;
                }
                let detail = match i {
                    0 => "earlier conversation, summarized".to_owned(),
                    3 if pruned > 0 => format!("{count} results, {pruned} pruned"),
                    _ => format!("{count} messages"),
                };
                segments.push(ContextSegment {
                    name: name.into(),
                    tokens,
                    detail,
                });
            }
        }
    }
    let threshold = threshold(s);
    ContextReport {
        model: s.model_ref.clone(),
        window: s.model.context_window,
        budget: s.budget,
        threshold,
        used: history.map_or(0, |h| turn::estimate(s, h)),
        strategy: format!("{:?}", s.context.cache_strategy).to_lowercase(),
        segments,
        note,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_ranges() {
        assert_eq!(read_lines(&json!({})), None);
        assert_eq!(read_lines(&json!({"offset": 1})), None);
        assert_eq!(read_lines(&json!({"offset": 10})), Some((10, u64::MAX)));
        assert_eq!(
            read_lines(&json!({"offset": 10, "limit": 5})),
            Some((10, 14))
        );
        assert_eq!(read_lines(&json!({"limit": 5})), Some((1, 5)));
    }
}
