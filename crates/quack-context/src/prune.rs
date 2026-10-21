//! Epoch-based pruning rules (docs/plan/05-context-management.md#pruning-rules-applied-at-epoch-boundaries).
//!
//! Rules only ever replace the content of old tool results. User messages and the model's
//! own text are never touched. Each rule yields [`PruneOp`]s, which the engine records in the
//! session log so a resume rebuilds the same history.

use std::collections::HashMap;

use quack_providers::{Message, ToolCall};
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
        paths: Vec<String>,
    },
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rule {
    /// Rule 1: the file was read again or changed later.
    SupersededRead,
    /// Rule 2: the file changed on disk (outside the session) since it was read.
    StaleRead,
    /// Rule 3: a large old output, cut to its head and tail.
    LargeOutput,
    /// Rule 4: an old read of a whole file, replaced by the file's outline.
    OldRead,
    /// Rule 5: a failed call followed by a successful one with the same aim.
    FailedAttempt,
}

/// What the rules need to know beyond the history.
pub struct Hooks<'a> {
    /// What each tool call did.
    pub classify: &'a dyn Fn(&ToolCall) -> Access,
    /// The file changed on disk since the session last read or wrote it.
    pub stale: &'a dyn Fn(&str) -> bool,
    /// The outline of a file's text (`path`, `text`), when its language has one.
    pub outline: &'a dyn Fn(&str, &str) -> Option<String>,
}

impl std::fmt::Debug for Hooks<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Hooks")
    }
}

/// Failed results this short are collapsed too: an error chain is noise however long.
const MIN_FAILED_CHARS: usize = 200;

/// Replace the content of the tool result at `index`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PruneOp {
    pub index: usize,
    pub rule: Rule,
    pub content: String,
}

/// Plan pruning for everything before `protect_from`; later messages are kept as they are.
pub fn plan(history: &[Message], protect_from: usize, hooks: &Hooks<'_>) -> Vec<PruneOp> {
    let results: HashMap<&str, usize> = history
        .iter()
        .enumerate()
        .filter_map(|(i, m)| match m {
            Message::Tool { call_id, .. } => Some((call_id.as_str(), i)),
            _ => None,
        })
        .collect();
    let content = |i: usize| match history.get(i) {
        Some(Message::Tool { content, .. }) => Some(content.as_str()),
        _ => None,
    };
    // (index of the result, the call, what it did), in history order.
    let calls: Vec<(usize, &ToolCall, Access)> = history
        .iter()
        .flat_map(|m| match m {
            Message::Assistant { tool_calls, .. } => tool_calls.as_slice(),
            _ => &[],
        })
        .filter_map(|c| Some((*results.get(c.id.as_str())?, c, (hooks.classify)(c))))
        .collect();
    let accesses: Vec<(usize, Access)> = calls.iter().map(|(i, _, a)| (*i, a.clone())).collect();

    let mut ops = Vec::new();
    for (n, (index, call, access)) in calls.iter().enumerate() {
        if *index >= protect_from {
            continue;
        }
        let Some(content) = content(*index) else {
            continue;
        };
        if content.starts_with(MARKER) {
            continue;
        }
        // Rule 5, for short results too: a failure fixed by a later call.
        if content.len() >= MIN_FAILED_CHARS
            && failed(content)
            && let Some(aim) = aim(call)
            && calls[n + 1..]
                .iter()
                .any(|(j, c, _)| aim_matches(c, &aim) && content_ok(content_of(history, *j)))
        {
            ops.push(PruneOp {
                index: *index,
                rule: Rule::FailedAttempt,
                content: format!(
                    "{MARKER}: this `{}` call failed; a later one on the same {} succeeded.]",
                    call.name, aim.1
                ),
            });
            continue;
        }
        if content.len() < MIN_CHARS {
            continue;
        }
        if let Access::Read { path, lines } = access {
            if let Some(why) = superseded(path, *lines, &accesses[n + 1..]) {
                ops.push(PruneOp {
                    index: *index,
                    rule: Rule::SupersededRead,
                    content: format!(
                        "{MARKER}: this read of {path} is out of date; the file was {why} later. \
                         Read it again if you need it.]"
                    ),
                });
                continue;
            }
            if (hooks.stale)(path) {
                ops.push(PruneOp {
                    index: *index,
                    rule: Rule::StaleRead,
                    content: format!(
                        "{MARKER}: this read of {path} is out of date; the file changed on disk \
                         since. Read it again if you need it.]"
                    ),
                });
                continue;
            }
            if lines.is_none()
                && content.len() > LARGE_CHARS
                && let Some(outline) = (hooks.outline)(path, &unnumber(content))
            {
                ops.push(PruneOp {
                    index: *index,
                    rule: Rule::OldRead,
                    content: format!(
                        "{MARKER}: an old read of {path}; only its outline is kept. Read the \
                         lines you need again.]\n{}",
                        outline.trim_end()
                    ),
                });
                continue;
            }
        }
        if content.len() > LARGE_CHARS
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

fn content_of(history: &[Message], i: usize) -> &str {
    match history.get(i) {
        Some(Message::Tool { content, .. }) => content,
        _ => "",
    }
}

/// Whether a tool result reports a failure: a tool error, or a command that did not exit 0.
fn failed(content: &str) -> bool {
    let first = content.lines().next().unwrap_or_default();
    first.starts_with("Error:")
        || first.starts_with("timed out after")
        || first == "killed by a signal"
        || first
            .strip_prefix("exit code ")
            .is_some_and(|c| c.trim() != "0")
}

fn content_ok(content: &str) -> bool {
    !content.is_empty() && !failed(content) && !content.starts_with(MARKER)
}

/// What a call aims at: its tool and its target (the command, or the file), when it has
/// one. `(key, description)`.
fn aim(call: &ToolCall) -> Option<(String, &'static str)> {
    let args: serde_json::Value = serde_json::from_str(&call.arguments).ok()?;
    let text = |k: &str| args.get(k).and_then(serde_json::Value::as_str);
    if let Some(cmd) = text("command").or_else(|| text("cmd")) {
        let cmd: Vec<&str> = cmd.split_whitespace().collect();
        return Some((format!("{}:{}", call.name, cmd.join(" ")), "command"));
    }
    ["path", "file_path", "absolute_path", "file", "filename"]
        .iter()
        .find_map(|k| text(k))
        .map(|p| (format!("{}:{}", call.name, p.trim()), "file"))
}

fn aim_matches(call: &ToolCall, want: &(String, &'static str)) -> bool {
    aim(call).is_some_and(|a| a.0 == want.0)
}

/// File text from a `read` result: line numbers (`  12\t`, or hashline `12:ab|`) taken off,
/// notes after the lines dropped.
fn unnumber(content: &str) -> String {
    let mut out = String::new();
    for line in content.lines() {
        let digits = line
            .trim_start()
            .chars()
            .take_while(char::is_ascii_digit)
            .count();
        let rest = &line.trim_start()[digits..];
        let code = if digits == 0 {
            None
        } else if let Some(code) = rest.strip_prefix('\t') {
            Some(code)
        } else if let Some(tagged) = rest.strip_prefix(':') {
            tagged.split_once('|').map(|(_, code)| code)
        } else {
            None
        };
        match code {
            Some(c) => {
                out.push_str(c.strip_suffix("… [line truncated]").unwrap_or(c));
                out.push('\n');
            }
            None if line.trim().is_empty() => {}
            // The `(lines 1-200 of 900; …)` note, or anything not a numbered line.
            None => break,
        }
    }
    out
}

/// How a later access made this read stale, if one did.
fn superseded(
    path: &str,
    lines: Option<(u64, u64)>,
    later: &[(usize, Access)],
) -> Option<&'static str> {
    later.iter().find_map(|(_, a)| match a {
        Access::Write { paths } if paths.iter().any(|p| p == path) => Some("changed"),
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
    // Signed reasoning after an edited message would no longer verify.
    if !ops.is_empty() {
        quack_providers::unseal(history);
    }
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
            sealed: None,
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
            "edit" => Access::Write { paths: vec![path] },
            _ => Access::Other,
        }
    }

    fn hooks() -> Hooks<'static> {
        Hooks {
            classify: &classify,
            stale: &|_| false,
            outline: &|_, _| None,
        }
    }

    fn shell(id: &str, cmd: &str) -> Message {
        Message::Assistant {
            content: String::new(),
            reasoning: None,
            tool_calls: vec![ToolCall {
                id: id.into(),
                name: "shell".into(),
                arguments: serde_json::json!({ "command": cmd }).to_string(),
            }],
            sealed: None,
        }
    }

    #[test]
    fn an_edit_unseals_the_history() {
        let mut history = vec![
            Message::Assistant {
                content: String::new(),
                reasoning: None,
                tool_calls: Vec::new(),
                sealed: Some(quack_providers::Sealed {
                    provider: "p".into(),
                    model: "m".into(),
                    fingerprint: String::new(),
                    items: Vec::new(),
                }),
            },
            Message::Tool {
                call_id: "a".into(),
                content: "long".into(),
            },
        ];
        apply(&mut history, &[]);
        assert!(matches!(
            &history[0],
            Message::Assistant {
                sealed: Some(_),
                ..
            }
        ));
        let op = PruneOp {
            index: 1,
            rule: Rule::LargeOutput,
            content: "[pruned]".into(),
        };
        apply(&mut history, &[op]);
        assert!(matches!(
            &history[0],
            Message::Assistant { sealed: None, .. }
        ));
    }

    #[test]
    fn stale_reads_and_old_reads_become_outlines() {
        let numbered: String = (1..=400).map(|n| format!("{n:>4}\tline {n}\n")).collect();
        let h = vec![
            call("1", "read", "a.rs"),
            result("1", numbered.clone()),
            call("2", "read", "b.rs"),
            result("2", numbered),
        ];
        let stale = |p: &str| p == "a.rs";
        let outline = |p: &str, text: &str| {
            assert!(text.starts_with("line 1\nline 2\n"), "{text:.40}");
            Some(format!("{p} outline"))
        };
        let ops = plan(
            &h,
            h.len(),
            &Hooks {
                classify: &classify,
                stale: &stale,
                outline: &outline,
            },
        );
        assert_eq!(
            ops.iter().map(|o| (o.index, o.rule)).collect::<Vec<_>>(),
            [(1, Rule::StaleRead), (3, Rule::OldRead)]
        );
        assert!(
            ops[1].content.ends_with("b.rs outline"),
            "{}",
            ops[1].content
        );
    }

    #[test]
    fn failed_attempts_collapse_once_fixed() {
        let err = format!(
            "exit code 101\n{}",
            "error[E0308]: mismatched types\n".repeat(20)
        );
        let h = vec![
            shell("1", "cargo  test"),
            result("1", err.clone()),
            shell("2", "cargo build"),
            result("2", err.clone()),
            shell("3", "cargo test"),
            result("3", "exit code 0\nok".into()),
        ];
        let ops = plan(&h, h.len(), &hooks());
        assert_eq!(
            ops.iter().map(|o| (o.index, o.rule)).collect::<Vec<_>>(),
            [(1, Rule::FailedAttempt)],
            "only the command that later passed"
        );
        assert!(ops[0].content.contains("same command succeeded"));
        assert!(failed("Error: no match") && !failed("exit code 0\n") && failed("exit code 2"));
    }

    #[test]
    fn unnumbering_reads() {
        assert_eq!(
            unnumber("   1\tfn a() {}\n   2\t\n\n(lines 1-2 of 9; …)\n"),
            "fn a() {}\n\n"
        );
        assert_eq!(unnumber("1:ab|x\n2:cd|y\n"), "x\ny\n");
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
        let ops = plan(&h, h.len(), &hooks());
        assert_eq!(
            ops.iter().map(|o| (o.index, o.rule)).collect::<Vec<_>>(),
            [(2, Rule::SupersededRead), (4, Rule::SupersededRead)]
        );
        assert!(ops[0].content.contains("a.rs") && ops[0].content.contains("changed"));
        assert!(ops[1].content.contains("read again"));
        let removed = apply(&mut h, &ops);
        assert!(removed > 1500);
        // Idempotent: pruned results are not pruned again, and the protected tail is kept.
        assert!(plan(&h, h.len(), &hooks()).is_empty());
        assert!(plan(&h, 4, &hooks()).is_empty());
    }

    #[test]
    fn large_old_outputs_keep_head_and_tail() {
        let body: String = (1..=500).map(|n| format!("line {n}\n")).collect();
        let h = vec![call("1", "shell", "-"), result("1", body.clone())];
        let ops = plan(&h, 2, &hooks());
        assert_eq!(ops.len(), 1);
        let c = &ops[0].content;
        assert!(c.starts_with(MARKER) && c.contains("450 lines omitted"));
        assert!(c.contains("line 1\n") && c.contains("line 500") && !c.contains("line 200\n"));
        assert!(plan(&h, 1, &hooks()).is_empty(), "protected");
    }

    #[test]
    fn ranged_reads_only_cover_their_range() {
        assert!(covers(None, Some((1, 5))));
        assert!(covers(Some((1, 10)), Some((2, 5))));
        assert!(!covers(Some((3, 10)), Some((2, 5))));
        assert!(!covers(Some((1, 10)), None));
    }
}
