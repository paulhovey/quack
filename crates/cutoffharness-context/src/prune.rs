//! Epoch-based pruning rules (docs/plan/05-context-management.md#pruning-rules-applied-at-epoch-boundaries).
//!
//! Rules only ever replace the content of old tool results. User messages and the model's
//! own text are never touched. Each rule yields [`PruneOp`]s, which the engine records in the
//! session log so a resume rebuilds the same history.

use std::collections::HashMap;

use cutoffharness_providers::{Message, ToolCall};
use serde::{Deserialize, Serialize};

/// Every pruned result starts with this, so a result is never pruned twice.
pub const MARKER: &str = "[pruned";

/// Tool results shorter than this are left alone: the stub would save almost nothing.
const MIN_CHARS: usize = 400;
/// Old outputs longer than this (about 1,000 tokens) are cut to their head and tail.
const LARGE_CHARS: usize = 3_500;
const KEEP_LINES: usize = 25;
const KEEP_CHARS: usize = 1_200;

/// What a tool call did to a file, as far as pruning cares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Access {
    /// `lines` is `(first, last)`, 1-based and inclusive; `None` means the whole file.
    Read {
        path: String,
        lines: Option<(u64, u64)>,
    },
    Write {
        path: String,
    },
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rule {
    /// Rule 1: the file was read again or changed later.
    SupersededRead,
    /// Rule 3: a large old output, cut to its head and tail.
    LargeOutput,
}

/// Replace the content of the tool result at `index`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PruneOp {
    pub index: usize,
    pub rule: Rule,
    pub content: String,
}

/// Plan pruning for everything before `protect_from`; later messages are kept as they are.
/// `classify` says what each tool call did.
pub fn plan(
    history: &[Message],
    protect_from: usize,
    classify: impl Fn(&ToolCall) -> Access,
) -> Vec<PruneOp> {
    let results: HashMap<&str, usize> = history
        .iter()
        .enumerate()
        .filter_map(|(i, m)| match m {
            Message::Tool { call_id, .. } => Some((call_id.as_str(), i)),
            _ => None,
        })
        .collect();
    // (index of the result, what the call did), in history order.
    let accesses: Vec<(usize, Access)> = history
        .iter()
        .flat_map(|m| match m {
            Message::Assistant { tool_calls, .. } => tool_calls.as_slice(),
            _ => &[],
        })
        .filter_map(|c| Some((*results.get(c.id.as_str())?, classify(c))))
        .collect();

    let mut ops = Vec::new();
    for (n, (index, access)) in accesses.iter().enumerate() {
        if *index >= protect_from {
            continue;
        }
        let Some(Message::Tool { content, .. }) = history.get(*index) else {
            continue;
        };
        if content.starts_with(MARKER) || content.len() < MIN_CHARS {
            continue;
        }
        if let Access::Read { path, lines } = access
            && let Some(why) = superseded(path, *lines, &accesses[n + 1..])
        {
            ops.push(PruneOp {
                index: *index,
                rule: Rule::SupersededRead,
                content: format!(
                    "{MARKER}: this read of {path} is out of date; the file was {why} later. \
                     Read it again if you need it.]"
                ),
            });
        } else if content.len() > LARGE_CHARS
            && let Some(short) = head_and_tail(content)
        {
            ops.push(PruneOp {
                index: *index,
                rule: Rule::LargeOutput,
                content: short,
            });
        }
    }
    ops
}

/// How a later access made this read stale, if one did.
fn superseded(
    path: &str,
    lines: Option<(u64, u64)>,
    later: &[(usize, Access)],
) -> Option<&'static str> {
    later.iter().find_map(|(_, a)| match a {
        Access::Write { path: p } if p == path => Some("changed"),
        Access::Read { path: p, lines: l } if p == path && covers(*l, lines) => Some("read again"),
        _ => None,
    })
}

fn covers(outer: Option<(u64, u64)>, inner: Option<(u64, u64)>) -> bool {
    match (outer, inner) {
        (None, _) => true,
        (Some(_), None) => false,
        (Some((a, b)), Some((c, d))) => a <= c && b >= d,
    }
}

/// The first and last lines of a long output with a note in between, or `None` when that
/// would not make it meaningfully shorter.
fn head_and_tail(content: &str) -> Option<String> {
    let lines: Vec<&str> = content.lines().collect();
    let clip = |s: String, from_end: bool| -> String {
        if s.len() <= KEEP_CHARS {
            return s;
        }
        if from_end {
            let mut at = s.len() - KEEP_CHARS;
            while !s.is_char_boundary(at) {
                at += 1;
            }
            s[at..].to_owned()
        } else {
            let mut at = KEEP_CHARS;
            while !s.is_char_boundary(at) {
                at -= 1;
            }
            s[..at].to_owned()
        }
    };
    let (head, tail, elided) = if lines.len() > KEEP_LINES * 2 {
        (
            lines[..KEEP_LINES].join("\n"),
            lines[lines.len() - KEEP_LINES..].join("\n"),
            format!("{} lines", lines.len() - KEEP_LINES * 2),
        )
    } else {
        // Few but very long lines: cut by characters instead.
        (
            content.to_owned(),
            content.to_owned(),
            "the middle".to_owned(),
        )
    };
    let (head, tail) = (clip(head, false), clip(tail, true));
    let out =
        format!("{MARKER}: this old output was shortened; {elided} omitted]\n{head}\n[…]\n{tail}");
    (out.len() * 2 < content.len()).then_some(out)
}

/// Apply ops to history. Ops that no longer fit (index out of range or not a tool result)
/// are skipped. Returns the number of characters removed.
pub fn apply(history: &mut [Message], ops: &[PruneOp]) -> usize {
    let mut removed = 0;
    for op in ops {
        if let Some(Message::Tool { content, .. }) = history.get_mut(op.index) {
            removed += content.len().saturating_sub(op.content.len());
            content.clone_from(&op.content);
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(id: &str, name: &str, path: &str) -> Message {
        Message::Assistant {
            content: String::new(),
            reasoning: None,
            tool_calls: vec![ToolCall {
                id: id.into(),
                name: name.into(),
                arguments: format!(r#"{{"path":"{path}"}}"#),
            }],
        }
    }

    fn result(id: &str, content: String) -> Message {
        Message::Tool {
            call_id: id.into(),
            content,
        }
    }

    fn classify(c: &ToolCall) -> Access {
        let path = c.arguments[9..c.arguments.len() - 2].to_owned();
        match c.name.as_str() {
            "read" => Access::Read { path, lines: None },
            "edit" => Access::Write { path },
            _ => Access::Other,
        }
    }

    #[test]
    fn reads_superseded_by_edits_and_rereads() {
        let body = "x\n".repeat(500);
        let mut h = vec![
            Message::User {
                content: "go".into(),
            },
            call("1", "read", "a.rs"),
            result("1", body.clone()),
            call("2", "read", "b.rs"),
            result("2", body.clone()),
            call("3", "edit", "a.rs"),
            result("3", "ok".into()),
            call("4", "read", "b.rs"),
            result("4", body.clone()),
        ];
        let ops = plan(&h, h.len(), classify);
        assert_eq!(
            ops.iter().map(|o| (o.index, o.rule)).collect::<Vec<_>>(),
            [(2, Rule::SupersededRead), (4, Rule::SupersededRead)]
        );
        assert!(ops[0].content.contains("a.rs") && ops[0].content.contains("changed"));
        assert!(ops[1].content.contains("read again"));
        let removed = apply(&mut h, &ops);
        assert!(removed > 1500);
        // Idempotent: pruned results are not pruned again, and the protected tail is kept.
        assert!(plan(&h, h.len(), classify).is_empty());
        assert!(plan(&h, 4, classify).is_empty());
    }

    #[test]
    fn large_old_outputs_keep_head_and_tail() {
        let body: String = (1..=500).map(|n| format!("line {n}\n")).collect();
        let h = vec![call("1", "shell", "-"), result("1", body.clone())];
        let ops = plan(&h, 2, classify);
        assert_eq!(ops.len(), 1);
        let c = &ops[0].content;
        assert!(c.starts_with(MARKER) && c.contains("450 lines omitted"));
        assert!(c.contains("line 1\n") && c.contains("line 500") && !c.contains("line 200\n"));
        assert!(plan(&h, 1, classify).is_empty(), "protected");
    }

    #[test]
    fn ranged_reads_only_cover_their_range() {
        assert!(covers(None, Some((1, 5))));
        assert!(covers(Some((1, 10)), Some((2, 5))));
        assert!(!covers(Some((3, 10)), Some((2, 5))));
        assert!(!covers(Some((1, 10)), None));
    }
}
