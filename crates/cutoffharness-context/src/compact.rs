//! Compaction: old history is summarized into one message
//! (docs/plan/05-context-management.md#compaction-summarization).
//!
//! This module picks the cut, renders what is summarized as plain text, and splices the
//! summary in. The model call itself is the engine's job.

use cutoffharness_providers::{Message, ToolCall};

/// Starts every summary message.
pub const SUMMARY_MARKER: &str = "[compaction summary]";

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
    matches!(m, Message::User { content } if content.starts_with(SUMMARY_MARKER))
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
        Message::User { content } if is_summary(m) => {
            format!("### Summary of the session before this point\n{content}")
        }
        Message::User { content } => format!("### User\n{content}"),
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
            Message::User { .. } if is_summary(m) => {
                lines.push("- (an earlier summary, not kept)".into());
            }
            Message::User { content } => {
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
pub fn summary_message(summary: &str, request: Option<&str>) -> Message {
    let mut content = format!(
        "{SUMMARY_MARKER}\nEarlier parts of this conversation were summarized to save \
         context. Files may have changed since; read them again before editing.\n\n{}",
        summary.trim()
    );
    if let Some(r) = request {
        content.push_str("\n\n## The user's current request (verbatim)\n");
        content.push_str(r);
    }
    Message::User { content }
}

/// The latest user request before `cut`, when the cut leaves no user message after it.
pub fn carried_request(history: &[Message], cut: usize) -> Option<&str> {
    let kept_user = history[cut.min(history.len())..]
        .iter()
        .any(|m| matches!(m, Message::User { .. }));
    if kept_user {
        return None;
    }
    history[..cut].iter().rev().find_map(|m| match m {
        Message::User { content } if !is_summary(m) => Some(content.as_str()),
        _ => None,
    })
}

/// Replace `history[..cut]` with `summary`.
pub fn apply(history: &mut Vec<Message>, cut: usize, summary: Message) {
    let cut = cut.min(history.len());
    history.splice(..cut, [summary]);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(s: &str) -> Message {
        Message::User { content: s.into() }
    }
    fn asst(s: &str) -> Message {
        Message::Assistant {
            content: s.into(),
            reasoning: None,
            tool_calls: Vec::new(),
        }
    }
    fn chars(h: &[Message]) -> u32 {
        h.iter()
            .map(|m| match m {
                Message::User { content }
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
        let summary = summary_message("did things", carried_request(&h, cut));
        apply(&mut h, cut, summary);
        assert_eq!(h.len(), 3);
        assert!(is_summary(&h[0]));
        let Message::User { content } = &h[0] else {
            panic!("summary is a user message")
        };
        assert!(content.ends_with("fix the bug"));
        // An over-budget single step is still kept.
        assert_eq!(cut_point(&h, KEEP_TURNS, 10, chars), Some(2));
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
            },
        ];
        let s = fallback_summary(&h);
        assert!(s.contains("- user: fix the parser\n"));
        assert!(s.contains(r#"  - read {"path":"src/parse.rs"}"#));
    }

    #[test]
    fn transcript_drops_oldest_but_keeps_the_summary() {
        let h = vec![
            summary_message("before", None),
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
