//! Compaction: old history is summarized into one message
//! (docs/plan/05-context-management.md#compaction-summarization).
//!
//! This module picks the cut, renders what is summarized as plain text, and splices the
//! summary in. The model call itself is the engine's job.

use quack_providers::{Message, ToolCall};

/// Starts every summary message.
pub const SUMMARY_MARKER: &str = "[compaction summary]";
/// Heads the user's request, carried verbatim through every compaction.
const REQUEST_HEADING: &str = "## The user's current request (verbatim)";
/// Heads the files the session changed, listed by the engine rather than the summarizer.
const FILES_HEADING: &str = "## Files changed in this session";

/// Used when the profile has no `compact.md`.
pub const DEFAULT_PROMPT: &str = "\
You summarize a coding session so the work can continue in a fresh context window. \
You will get the transcript of the session so far. Write a summary in markdown with these \
sections:

## Goal
What the user asked for, in their words where it matters.

## Done so far
Decisions made, findings, and what worked or failed.

## Files
Each file created or changed, with its current state.

## Open problems
Errors not yet fixed, questions not yet answered.

## Next steps
What to do next to finish the task.

Keep exact file paths, function names, commands and error messages that still matter. \
Leave out anything that no longer matters. Do not invent anything. Reply with the summary only.";

/// Turns kept verbatim by automatic compaction when they fit: the current one plus two
/// earlier ones.
pub const KEEP_TURNS: usize = 3;
/// Tool results are cut to this many characters in the transcript sent for summarizing.
const TOOL_CHARS: usize = 1_500;
const ARGS_CHARS: usize = 300;

/// Where to cut: everything before the returned index gets summarized. Keeps up to
/// `keep_turns` whole turns (each starting at a user message) that fit in `keep_tokens`;
/// falls back to a cut inside the current turn when that turn alone is over `keep_tokens`.
/// `None` when there is nothing to summarize.
pub fn cut_point(
    history: &[Message],
    keep_turns: usize,
    keep_tokens: u32,
    estimate: impl Fn(&[Message]) -> u32,
) -> Option<usize> {
    let turns: Vec<usize> = history
        .iter()
        .enumerate()
        .filter(|(_, m)| matches!(m, Message::User { .. }) && !is_summary(m))
        .map(|(i, _)| i)
        .collect();
    for k in (1..=keep_turns.max(1).min(turns.len())).rev() {
        let cut = turns[turns.len() - k];
        if cut > 0 && estimate(&history[cut..]) <= keep_tokens {
            return Some(cut);
        }
    }
    if turns
        .last()
        .is_some_and(|&t| estimate(&history[t..]) <= keep_tokens)
    {
        // Only the current turn, and it fits.
        return None;
    }
    // The current turn alone is too big: keep its most recent steps that fit, at least one.
    let after = turns.last().map_or(0, |t| t + 1);
    let steps: Vec<usize> = crate::boundaries(history)
        .filter(|&i| i >= after && i > 0)
        .collect();
    steps
        .iter()
        .find(|&&i| estimate(&history[i..]) <= keep_tokens)
        .or(steps.last())
        .copied()
}

pub fn is_summary(m: &Message) -> bool {
    matches!(m, Message::User { content, .. } if content.starts_with(SUMMARY_MARKER))
}

/// The part being summarized as plain text, which any model can read regardless of its
/// tool-call template. Long tool results are cut; when the whole text is over `max_chars`,
/// the oldest messages are dropped (an earlier summary is always kept).
pub fn transcript(history: &[Message], max_chars: usize) -> String {
    let mut parts: Vec<String> = history.iter().map(render).collect();
    let mut total: usize = parts.iter().map(|p| p.len() + 2).sum();
    let first = usize::from(history.first().is_some_and(is_summary));
    let mut dropped = 0;
    while total > max_chars && parts.len() > first + 1 {
        total -= parts.remove(first).len() + 2;
        dropped += 1;
    }
    if dropped > 0 {
        parts.insert(
            first,
            format!("[{dropped} older messages left out to fit the summarizer's window]"),
        );
    }
    parts.join("\n\n")
}

fn render(m: &Message) -> String {
    match m {
        Message::System { content } => format!("### System\n{content}"),
        Message::User { content, .. } if is_summary(m) => {
            format!("### Summary of the session before this point\n{content}")
        }
        Message::User { content, .. } => format!("### User\n{content}"),
        Message::Assistant {
            content,
            tool_calls,
            ..
        } => {
            let mut s = "### Assistant".to_owned();
            if !content.trim().is_empty() {
                s.push('\n');
                s.push_str(content.trim());
            }
            for ToolCall {
                name, arguments, ..
            } in tool_calls
            {
                s.push_str(&format!("\n→ {name} {}", clip(arguments, ARGS_CHARS)));
            }
            s
        }
        Message::Tool { content, .. } => format!("### Tool result\n{}", clip(content, TOOL_CHARS)),
    }
}

fn clip(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_owned();
    }
    let mut head = max * 2 / 3;
    while !s.is_char_boundary(head) {
        head -= 1;
    }
    let mut tail = s.len() - (max - head);
    while !s.is_char_boundary(tail) {
        tail += 1;
    }
    format!(
        "{}\n[… {} characters cut …]\n{}",
        &s[..head],
        tail - head,
        &s[tail..]
    )
}

/// A summary written without a model, for when the model cannot write one: the user's
/// requests and the tool calls, most recent last.
pub fn fallback_summary(history: &[Message]) -> String {
    const MAX_LINES: usize = 40;
    let mut lines: Vec<String> = Vec::new();
    for m in history {
        match m {
            Message::User { content, .. } if is_summary(m) => {
                // A model wrote it, so it says more than this list can; keep it whole.
                lines.push(format!("- earlier summary:\n{}", summary_body(content)));
            }
            Message::User { content, .. } => {
                let first = content.lines().next().unwrap_or_default();
                lines.push(format!("- user: {}", clip_line(first, 200)));
            }
            Message::Assistant { tool_calls, .. } => lines.extend(
                tool_calls
                    .iter()
                    .map(|c| format!("  - {} {}", c.name, clip_line(&c.arguments, 120))),
            ),
            _ => {}
        }
    }
    let dropped = lines.len().saturating_sub(MAX_LINES);
    let mut out = String::from(
        "No summary could be written, so this is a plain list of what happened, oldest first.\n",
    );
    if dropped > 0 {
        out.push_str(&format!("- ({dropped} earlier entries left out)\n"));
    }
    out.push_str(&lines[dropped..].join("\n"));
    out
}

fn clip_line(s: &str, max: usize) -> String {
    let s = s.replace('\n', " ");
    match s.char_indices().nth(max) {
        Some((at, _)) => format!("{}…", &s[..at]),
        None => s,
    }
}

/// The message that replaces the summarized span. When the cut falls inside the current
/// turn, the user's request is carried over verbatim so it is never lost to a summary.
/// `changed` lists the files the session changed (from the engine's checkpoints), so the
/// model knows its work is on disk without reading everything again.
pub fn summary_message(summary: &str, request: Option<&str>, changed: &[String]) -> Message {
    let mut content = format!(
        "{SUMMARY_MARKER}\nEarlier parts of this conversation were summarized to save \
         context. The work it describes is on disk. Read a file again only for lines you need \
         and no longer have: an edit whose text doesn't match the file is refused and says \
         why.\n\n{}",
        summary.trim()
    );
    if !changed.is_empty() {
        content.push_str(&format!("\n\n{FILES_HEADING}\n"));
        content.push_str(
            &changed
                .iter()
                .map(|f| format!("- {f}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
    if let Some(r) = request {
        content.push_str(&format!("\n\n{REQUEST_HEADING}\n"));
        content.push_str(r);
    }
    Message::User {
        content,
        images: Vec::new(),
    }
}

/// The latest user request before `cut`, when the cut leaves no user message after it.
/// After an earlier compaction that request lives in the summary, so it is taken from there.
pub fn carried_request(history: &[Message], cut: usize) -> Option<&str> {
    let kept_user = history[cut.min(history.len())..]
        .iter()
        .any(|m| matches!(m, Message::User { .. }));
    if kept_user {
        return None;
    }
    history[..cut].iter().rev().find_map(|m| match m {
        Message::User { content, .. } if is_summary(m) => content
            .split_once(&format!("\n\n{REQUEST_HEADING}\n"))
            .map(|(_, request)| request),
        Message::User { content, .. } => Some(content.as_str()),
        _ => None,
    })
}

/// What the summarizer wrote, without the header and the sections the engine adds.
fn summary_body(content: &str) -> &str {
    let body = content.split_once("\n\n").map_or("", |(_, rest)| rest);
    let end = [FILES_HEADING, REQUEST_HEADING]
        .iter()
        .filter_map(|h| body.find(&format!("\n\n{h}\n")))
        .min()
        .unwrap_or(body.len());
    body[..end].trim()
}

/// Why a summarizer's reply can't stand in for the history, if it can't. Some models
/// answer a request without tools with a tool call written out as text (DeepSeek's DSML,
/// Qwen's `<tool_call>`, Mistral's `[TOOL_CALLS]`, Kimi's sections, ...); taken as the summary, it would leave
/// nothing about the task.
pub fn unusable_summary(text: &str) -> Option<&'static str> {
    const CALL_MARKUP: [&str; 9] = [
        "｜DSML｜",
        "<tool_call>",
        "<|tool_call",
        "<｜tool▁call",
        "<function=",
        "<invoke name=",
        "[TOOL_CALLS]",
        "<|python_tag|>",
        "<|tool_calls_section_begin|>",
    ];
    let text = text.trim();
    if text.chars().filter(|c| c.is_alphanumeric()).count() < 10 {
        return Some("the summary is empty or too short");
    }
    // Prose may mention markup in passing; a reply that opens with it is a call.
    let head: String = text.chars().take(200).collect();
    if CALL_MARKUP.iter().any(|m| head.contains(m)) {
        return Some("the model answered with a tool call instead of a summary");
    }
    None
}

/// Replace `history[..cut]` with `summary`.
pub fn apply(history: &mut Vec<Message>, cut: usize, summary: Message) {
    let cut = cut.min(history.len());
    history.splice(..cut, [summary]);
    // The kept turns' signed reasoning was made after the summarized ones.
    quack_providers::unseal(history);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(s: &str) -> Message {
        Message::User {
            content: s.into(),
            images: Vec::new(),
        }
    }
    fn asst(s: &str) -> Message {
        Message::Assistant {
            content: s.into(),
            reasoning: None,
            tool_calls: Vec::new(),
            sealed: None,
        }
    }
    fn chars(h: &[Message]) -> u32 {
        h.iter()
            .map(|m| match m {
                Message::User { content, .. }
                | Message::Assistant { content, .. }
                | Message::Tool { content, .. }
                | Message::System { content } => content.len() as u32,
            })
            .sum()
    }

    #[test]
    fn keeps_whole_recent_turns_when_they_fit() {
        let h: Vec<Message> = (0..5)
            .flat_map(|n| [user(&format!("q{n}")), asst(&"a".repeat(100))])
            .collect();
        // Three turns fit.
        assert_eq!(cut_point(&h, KEEP_TURNS, 1000, chars), Some(4));
        // Only one fits.
        assert_eq!(cut_point(&h, KEEP_TURNS, 150, chars), Some(8));
        // Manual compaction keeps only the current turn.
        assert_eq!(cut_point(&h, 1, 1000, chars), Some(8));
        // A single turn: nothing before it, and it fits.
        assert_eq!(cut_point(&h[..2], KEEP_TURNS, 1000, chars), None);
    }

    #[test]
    fn cuts_inside_a_long_turn_and_carries_the_request() {
        let mut h = vec![user("fix the bug")];
        for _ in 0..6 {
            h.push(asst(&"x".repeat(100)));
        }
        let cut = cut_point(&h, KEEP_TURNS, 250, chars).expect("cut");
        assert_eq!(cut, 5, "keeps the last two steps");
        assert_eq!(carried_request(&h, cut), Some("fix the bug"));
        let summary = summary_message("did things", carried_request(&h, cut), &[]);
        apply(&mut h, cut, summary);
        assert_eq!(h.len(), 3);
        assert!(is_summary(&h[0]));
        let Message::User { content, .. } = &h[0] else {
            panic!("summary is a user message")
        };
        assert!(content.ends_with("fix the bug"));
        // An over-budget single step is still kept.
        assert_eq!(cut_point(&h, KEEP_TURNS, 10, chars), Some(2));
    }

    #[test]
    fn the_request_survives_every_compaction() {
        let mut h = vec![user("add partial shipments")];
        for round in 0..3 {
            for _ in 0..6 {
                h.push(asst(&"x".repeat(100)));
            }
            let cut = cut_point(&h, KEEP_TURNS, 250, chars).expect("cut");
            let request = carried_request(&h, cut);
            assert_eq!(request, Some("add partial shipments"), "round {round}");
            let summary = summary_message(&format!("round {round}"), request, &["a.py".into()]);
            apply(&mut h, cut, summary);
        }
        let Message::User { content, .. } = &h[0] else {
            panic!("summary is a user message")
        };
        assert_eq!(content.matches("add partial shipments").count(), 1);
        assert!(content.contains("round 2") && !content.contains("round 1"));
    }

    #[test]
    fn the_summary_lists_changed_files_and_does_not_ask_for_rereads() {
        let m = summary_message(
            "did things",
            Some("fix it"),
            &["src/a.rs".into(), "b.py".into()],
        );
        let Message::User { content, .. } = &m else {
            panic!("summary is a user message")
        };
        assert!(content.contains("## Files changed in this session\n- src/a.rs\n- b.py"));
        assert!(!content.contains("read them again"));
        assert_eq!(summary_body(content), "did things");
    }

    #[test]
    fn tool_calls_and_empty_replies_are_not_summaries() {
        let dsml = "<｜DSML｜ calls>\n<｜DSML｜ invoke name=\"read\">\n<｜DSML｜ parameter \
                    name=\"path\" string=\"true\">orderly/shop.py</｜DSML｜ parameter>";
        assert!(unusable_summary(dsml).is_some());
        assert!(unusable_summary("<tool_call>{\"name\":\"read\"}</tool_call>").is_some());
        assert!(unusable_summary("[TOOL_CALLS][{\"name\":\"grep\"}]").is_some());
        assert!(unusable_summary("  ok  ").is_some());
        let prose = format!(
            "## Goal\nFix the parser.\n\n## Done so far\n{}Found that the model emitted \
             `<tool_call>` tags in text.",
            "Read src/parse.rs and ran the tests. ".repeat(8)
        );
        assert_eq!(unusable_summary(&prose), None);
    }

    #[test]
    fn fallback_keeps_an_earlier_summary() {
        let h = vec![
            summary_message(
                "## Goal\nship parts of orders",
                Some("the request"),
                &["x.py".into()],
            ),
            user("go on"),
        ];
        let s = fallback_summary(&h);
        assert!(
            s.contains("- earlier summary:\n## Goal\nship parts of orders\n"),
            "{s}"
        );
        assert!(!s.contains("the request") && !s.contains("x.py"));
    }

    #[test]
    fn fallback_lists_requests_and_calls() {
        let h = vec![
            user("fix the parser\nmore detail"),
            Message::Assistant {
                content: String::new(),
                reasoning: None,
                tool_calls: vec![ToolCall {
                    id: "1".into(),
                    name: "read".into(),
                    arguments: r#"{"path":"src/parse.rs"}"#.into(),
                }],
                sealed: None,
            },
        ];
        let s = fallback_summary(&h);
        assert!(s.contains("- user: fix the parser\n"));
        assert!(s.contains(r#"  - read {"path":"src/parse.rs"}"#));
    }

    #[test]
    fn transcript_drops_oldest_but_keeps_the_summary() {
        let h = vec![
            summary_message("before", None, &[]),
            user(&"a".repeat(500)),
            asst(&"b".repeat(500)),
            user("latest"),
        ];
        let t = transcript(&h, 700);
        assert!(t.contains("before") && t.contains("latest"));
        assert!(t.contains("2 older messages left out"));
        assert!(!t.contains("aaaa"));
        assert!(clip(&"é".repeat(2000), 100).len() < 200);
    }
}
