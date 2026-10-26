//! Compaction: old history is summarized into one message
//! (docs/plan/05-context-management.md#compaction-summarization).
//!
//! This module picks the cut, renders what is summarized as plain text, and splices the
//! summary in. The model call itself is the engine's job.

use quack_providers::{Message, ToolCall};

/// Starts every summary message.
pub const SUMMARY_MARKER: &str = "[compaction summary]";
/// Heads what the user said, carried verbatim through every compaction.
const REQUEST_HEADING: &str = "## What the user said, oldest first (verbatim)";
/// The heading summaries written before 2026-10-25 used, over the latest request alone.
const OLD_REQUEST_HEADING: &str = "## The user's current request (verbatim)";
/// Heads each earlier user message under `REQUEST_HEADING`.
const EARLIER: &str = "### Earlier message";
/// Heads the request the current turn answers under `REQUEST_HEADING`.
const CURRENT: &str = "### Current request";
/// Each earlier user message is carried cut to this many characters (the current request
/// is carried whole).
const CARRIED_CHARS: usize = 2_000;
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
    // The current turn alone is too big: keep its most recent steps that fit, at least one,
    // or the whole turn when it has no steps yet (one long message).
    let after = turns.last().map_or(0, |t| t + 1);
    let steps: Vec<usize> = crate::boundaries(history)
        .filter(|&i| i >= after && i > 0)
        .collect();
    steps
        .iter()
        .find(|&&i| estimate(&history[i..]) <= keep_tokens)
        .or(steps.last())
        .or(turns.last().filter(|&&t| t > 0))
        .copied()
}

pub fn is_summary(m: &Message) -> bool {
    matches!(m, Message::User { content, .. } if content.starts_with(SUMMARY_MARKER))
}

/// The part being summarized as plain text, which any model can read regardless of its
/// tool-call template. Long tool results are cut; when the whole text is over `max_chars`,
/// the oldest messages are dropped (an earlier summary is always kept).
pub fn transcript(history: &[Message], max_chars: usize) -> String {
    let mut parts: Vec<(bool, String)> = history
        .iter()
        .map(|m| (matches!(m, Message::User { .. }), render(m)))
        .collect();
    let mut total: usize = parts.iter().map(|(_, p)| p.len() + 2).sum();
    let first = usize::from(history.first().is_some_and(is_summary));
    let mut dropped = 0;
    while total > max_chars && parts.len() > first + 1 {
        // What the user said goes last: it is where the requirements are.
        let at = (first..parts.len() - 1)
            .find(|&i| !parts[i].0)
            .unwrap_or(first);
        total -= parts.remove(at).1.len() + 2;
        dropped += 1;
    }
    let mut parts: Vec<String> = parts.into_iter().map(|(_, p)| p).collect();
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

/// The message that replaces the summarized span. What the user said in it is carried
/// over verbatim (`carried`), so a summary can never paraphrase a requirement away.
/// `changed` lists the files the session changed (from the engine's checkpoints), so the
/// model knows its work is on disk without reading everything again.
pub fn summary_message(summary: &str, carried: &Carried, changed: &[String]) -> Message {
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
    if carried.is_empty() {
        return Message::User {
            content,
            images: Vec::new(),
        };
    }
    content.push_str(&format!("\n\n{REQUEST_HEADING}"));
    if carried.left_out > 0 {
        content.push_str(&format!(
            "\n({} older messages left out to fit)",
            carried.left_out
        ));
    }
    for e in &carried.earlier {
        content.push_str(&format!("\n\n{EARLIER}\n{e}"));
    }
    if let Some(r) = &carried.current {
        content.push_str(&format!("\n\n{CURRENT}\n{r}"));
    }
    Message::User {
        content,
        images: Vec::new(),
    }
}

/// What the user said in the summarized span, carried verbatim through every compaction.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Carried {
    /// Earlier messages, oldest first, each cut to `CARRIED_CHARS`.
    pub earlier: Vec<String>,
    /// How many of the oldest were left out to fit.
    pub left_out: usize,
    /// The request the current turn answers, whole, when the cut leaves no user message
    /// after it.
    pub current: Option<String>,
}

impl Carried {
    pub fn is_empty(&self) -> bool {
        self.earlier.is_empty() && self.current.is_none()
    }
}

/// Summaries this long need no length limit in the prompt.
const UNLIMITED_WORDS: u32 = 2_000;

/// Added to the summary prompt: a length to aim for when the summary has `tokens` to fit
/// in, and when what the user said is carried beside it, not to restate it.
pub fn length_note(tokens: u32, carried: bool) -> String {
    let mut note = String::new();
    let words = (tokens * 3 / 4 / 50 * 50).max(100);
    if words < UNLIMITED_WORDS {
        note.push_str(&format!(
            "\n\nKeep the summary under {words} words: it must leave room to work in a small \
             context window."
        ));
    }
    if carried {
        note.push_str(
            "\n\nThe user's messages are kept word for word beside the summary, so under Goal \
             name the task in a sentence instead of restating them.",
        );
    }
    note
}

/// What the user said before `cut`: every user message, and what earlier summaries carried,
/// in order. `clean` takes off what the engine added to a user message (plan-mode notes),
/// or returns `None` for a message the engine wrote itself. Earlier messages are left out,
/// oldest first, to keep the whole within `max_chars`; the current request always stays.
pub fn carried<'a>(
    history: &'a [Message],
    cut: usize,
    max_chars: usize,
    clean: impl Fn(&'a str) -> Option<&'a str>,
) -> Carried {
    let cut = cut.min(history.len());
    // (text, whether it may still need cutting)
    let mut said: Vec<(String, bool)> = Vec::new();
    let mut left_out = 0;
    for m in &history[..cut] {
        match m {
            Message::User { content, .. } if is_summary(m) => {
                let (entries, n) = parse_carried(content);
                said.extend(entries);
                left_out += n;
            }
            Message::User { content, .. } => {
                if let Some(text) = clean(content).map(str::trim).filter(|t| !t.is_empty()) {
                    said.push((text.to_owned(), true));
                }
            }
            _ => {}
        }
    }
    let kept_user = history[cut..]
        .iter()
        .any(|m| matches!(m, Message::User { .. }) && !is_summary(m));
    let current = if kept_user {
        None
    } else {
        said.pop().map(|(text, _)| text)
    };
    let mut earlier: Vec<String> = said
        .into_iter()
        .map(|(text, fresh)| {
            if fresh {
                clip(&text, CARRIED_CHARS)
            } else {
                text
            }
        })
        .collect();
    let room = max_chars.saturating_sub(current.as_ref().map_or(0, String::len));
    let mut total: usize = earlier.iter().map(|e| e.len() + EARLIER.len() + 3).sum();
    while total > room && !earlier.is_empty() {
        total -= earlier.remove(0).len() + EARLIER.len() + 3;
        left_out += 1;
    }
    Carried {
        earlier,
        left_out,
        current,
    }
}

/// The messages an earlier summary carried, and how many it had already left out. Entries
/// marked `true` were the current request then and are cut if they become earlier ones.
fn parse_carried(content: &str) -> (Vec<(String, bool)>, usize) {
    if let Some((_, old)) = content.split_once(&format!("\n\n{OLD_REQUEST_HEADING}\n")) {
        return (vec![(old.trim().to_owned(), true)], 0);
    }
    let Some((_, section)) = content.split_once(&format!("\n\n{REQUEST_HEADING}")) else {
        return (Vec::new(), 0);
    };
    let mut marks: Vec<(usize, &str, bool)> = [(EARLIER, false), (CURRENT, true)]
        .iter()
        .flat_map(|&(h, current)| {
            let mark = format!("\n\n{h}\n");
            section
                .match_indices(&mark)
                .map(|(i, _)| (i, h, current))
                .collect::<Vec<_>>()
        })
        .collect();
    marks.sort_unstable_by_key(|&(i, ..)| i);
    let left_out = section[..marks.first().map_or(section.len(), |m| m.0)]
        .trim()
        .strip_prefix('(')
        .and_then(|s| s.split_once(' '))
        .and_then(|(n, _)| n.parse().ok())
        .unwrap_or(0);
    let entries = marks
        .iter()
        .enumerate()
        .map(|(k, &(i, h, current))| {
            let start = i + h.len() + 3;
            let end = marks.get(k + 1).map_or(section.len(), |m| m.0);
            (section[start..end].trim().to_owned(), current)
        })
        .collect();
    (entries, left_out)
}

/// What the summarizer wrote, without the header and the sections the engine adds.
fn summary_body(content: &str) -> &str {
    let body = content.split_once("\n\n").map_or("", |(_, rest)| rest);
    let end = [FILES_HEADING, REQUEST_HEADING, OLD_REQUEST_HEADING]
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
    fn current(s: &str) -> Carried {
        Carried {
            current: Some(s.into()),
            ..Carried::default()
        }
    }
    fn body(m: &Message) -> &str {
        let Message::User { content, .. } = m else {
            panic!("summary is a user message")
        };
        content
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
        // The current turn is one message over the budget, with no steps to cut between:
        // everything before it goes.
        let mut long = h[..8].to_vec();
        long.push(user(&"q".repeat(500)));
        assert_eq!(cut_point(&long, KEEP_TURNS, 150, chars), Some(8));
    }

    #[test]
    fn a_small_room_asks_for_a_short_summary() {
        let note = length_note(1_000, true);
        assert!(note.contains("under 750 words"), "{note}");
        assert!(note.contains("name the task in a sentence"));
        // Large windows get no limit, and nothing carried, nothing said about it.
        assert_eq!(length_note(20_000, false), "");
        assert!(length_note(10, false).contains("under 100 words"));
    }

    #[test]
    fn cuts_inside_a_long_turn_and_carries_the_request() {
        let mut h = vec![user("fix the bug")];
        for _ in 0..6 {
            h.push(asst(&"x".repeat(100)));
        }
        let cut = cut_point(&h, KEEP_TURNS, 250, chars).expect("cut");
        assert_eq!(cut, 5, "keeps the last two steps");
        let c = carried(&h, cut, 10_000, Some);
        assert_eq!(c.current.as_deref(), Some("fix the bug"));
        let summary = summary_message("did things", &c, &[]);
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
            let c = carried(&h, cut, 10_000, Some);
            assert_eq!(
                c.current.as_deref(),
                Some("add partial shipments"),
                "round {round}"
            );
            let summary = summary_message(&format!("round {round}"), &c, &["a.py".into()]);
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
            &current("fix it"),
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
                &current("the request"),
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
            summary_message("before", &Carried::default(), &[]),
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

    #[test]
    fn earlier_requirements_survive_every_compaction() {
        // Turn 1 sets a rule, turn 2 asks for the work; a long turn 2 then compacts three
        // times, each cut inside it.
        let mut h = vec![
            user("use pnpm, never npm"),
            asst("ok"),
            user("add partial shipments"),
        ];
        for round in 0..3 {
            for _ in 0..6 {
                h.push(asst(&"x".repeat(100)));
            }
            let cut = cut_point(&h, KEEP_TURNS, 250, chars).expect("cut");
            let c = carried(&h, cut, 10_000, Some);
            assert_eq!(c.earlier, ["use pnpm, never npm"], "round {round}");
            assert_eq!(c.current.as_deref(), Some("add partial shipments"));
            apply(
                &mut h,
                cut,
                summary_message(&format!("round {round}"), &c, &[]),
            );
        }
        let content = body(&h[0]);
        assert_eq!(
            content.matches("use pnpm, never npm").count(),
            1,
            "{content}"
        );
        assert!(content.ends_with(
            "### Earlier message\nuse pnpm, never npm\n\n### Current request\nadd partial shipments"
        ));
        assert_eq!(summary_body(content), "round 2");

        // A new turn: the old current request becomes an earlier message, and nothing is
        // carried as current because the new request is kept verbatim.
        h.push(user("now add refunds"));
        h.push(asst("ok"));
        let c = carried(&h, 1, 10_000, Some);
        assert_eq!(c.earlier, ["use pnpm, never npm", "add partial shipments"]);
        assert_eq!(c.current, None);
    }

    #[test]
    fn carrying_skips_engine_text_and_keeps_to_the_budget() {
        let note = "\n\n[Plan mode] plan first";
        let h = vec![
            user("rule one"),
            user("Continue."),
            user(&format!("rule two{note}")),
            user(&"p".repeat(5_000)),
            user("the request"),
            asst("x"),
        ];
        fn clean(t: &str) -> Option<&str> {
            (t != "Continue.").then(|| t.strip_suffix("\n\n[Plan mode] plan first").unwrap_or(t))
        }
        let c = carried(&h, h.len(), 100_000, clean);
        assert_eq!(c.earlier.len(), 3);
        assert_eq!(c.earlier[..2], ["rule one", "rule two"]);
        assert!(c.earlier[2].len() < 2_100 && c.earlier[2].contains("characters cut"));
        assert_eq!(c.current.as_deref(), Some("the request"));

        // Over the budget, the oldest go first and are counted; the request stays whole.
        let c = carried(&h, h.len(), 2_080, clean);
        assert_eq!(c.left_out, 2);
        assert_eq!(c.earlier.len(), 1);
        assert_eq!(c.current.as_deref(), Some("the request"));
        // The count carries through the next summary.
        let next = vec![summary_message("s", &c, &[]), user("more"), asst("y")];
        let again = carried(&next, 1, 100_000, Some);
        assert_eq!(again.left_out, 2);
        // "more" is kept after the cut, so the old request is an earlier message now.
        assert_eq!(again.earlier[0], c.earlier[0]);
        assert_eq!(again.earlier[1], "the request");
        assert_eq!(again.current, None);
        assert_eq!(carried(&next, 3, 100_000, Some).earlier.len(), 2);
    }

    #[test]
    fn summaries_from_before_the_change_still_carry_their_request() {
        let old = user(
            "[compaction summary]\nintro\n\ndid things\n\n## The user's current request \
             (verbatim)\nfix the bug",
        );
        assert_eq!(summary_body(body(&old)), "did things");
        let h = vec![old, asst("x")];
        let c = carried(&h, 2, 10_000, Some);
        assert_eq!(c.current.as_deref(), Some("fix the bug"));
    }

    #[test]
    fn transcript_keeps_user_messages_longest() {
        let h = vec![
            user("rule: tabs"),
            asst(&"b".repeat(500)),
            asst(&"c".repeat(500)),
            user("latest"),
        ];
        let t = transcript(&h, 700);
        assert!(t.contains("rule: tabs") && t.contains("latest"), "{t}");
        assert!(t.contains("1 older messages left out"));
        assert!(!t.contains("bbbb") && t.contains("cccc"));
    }
}
