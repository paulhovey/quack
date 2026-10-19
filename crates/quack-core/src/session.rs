//! Session persistence: what the engine records, and rebuilding history on resume.
//!
//! Record types: `session_start`, `resume`, `message` (one chat message, exactly as sent),
//! `tool` (UI summary of a tool call), `usage` (with `cost` when priced), `prune` (tool results replaced),
//! `compaction` (history before `cut` replaced by `message`), `rewind` (history cut back
//! to its first `to` messages), `approval` and `questions` (what the user answered; replay
//! skips them, since the tool results carry the answers).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use quack_config::{Loaded, expand_home};
use quack_context::{compact, prune};
use quack_protocol::{ReplayItem, Usage};
use quack_providers::Message;
use quack_store::session::{IndexEntry, ProjectSessions, Record, SessionLog};
use serde_json::json;

/// Which session to start with.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Resume {
    #[default]
    New,
    /// The most recent session in this project.
    Latest,
    /// By id or unique id prefix.
    Id(String),
}

pub(crate) type SharedLog = Arc<Mutex<SessionLog>>;

pub(crate) fn project_sessions(config: &Loaded) -> ProjectSessions {
    let configured = &config.settings.sessions.path;
    let root = if configured.is_empty() {
        config.dirs.user_data.join("sessions")
    } else {
        expand_home(configured, &|k| std::env::var_os(k))
    };
    ProjectSessions::new(&root, &config.dirs.workspace_root)
}

/// Sessions of this workspace, most recent first (for `quack sessions`).
pub fn list_sessions(config: &Loaded) -> (PathBuf, Vec<IndexEntry>) {
    let ps = project_sessions(config);
    let list = ps.list();
    (ps.dir, list)
}

pub(crate) fn find(config: &Loaded, resume: &Resume) -> Result<Option<IndexEntry>, String> {
    let ps = project_sessions(config);
    match resume {
        Resume::New => Ok(None),
        Resume::Latest => ps
            .latest()
            .map(Some)
            .ok_or_else(|| "no saved sessions in this project yet".to_owned()),
        Resume::Id(id) => ps.find(id).map(Some),
    }
}

/// Rebuild model history and the display transcript from a session's records.
pub(crate) fn replay(records: &[Record]) -> (Vec<Message>, Vec<ReplayItem>, u32) {
    let mut history = Vec::new();
    let mut items = Vec::new();
    let mut turns: u32 = 0;
    // `(history index, items length)` at each user message, to cut the transcript on rewind.
    let mut marks: Vec<(usize, usize)> = Vec::new();
    for r in records {
        match r.kind.as_str() {
            "message" => {
                let Some(m) = r
                    .data
                    .get("message")
                    .and_then(|v| serde_json::from_value::<Message>(v.clone()).ok())
                else {
                    continue;
                };
                match &m {
                    Message::User { content } => {
                        turns += 1;
                        marks.push((history.len(), items.len()));
                        items.push(ReplayItem::User {
                            text: content.clone(),
                        });
                    }
                    Message::Assistant { content, .. } if !content.trim().is_empty() => {
                        items.push(ReplayItem::Assistant {
                            text: content.clone(),
                        });
                    }
                    _ => {}
                }
                history.push(m);
            }
            "tool" => {
                let s = |k: &str| {
                    r.data
                        .get(k)
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_owned()
                };
                items.push(ReplayItem::Tool {
                    summary: s("summary"),
                    ok: r
                        .data
                        .get("ok")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false),
                    result: s("result"),
                });
            }
            "prune" => {
                if let Some(ops) = r
                    .data
                    .get("ops")
                    .and_then(|v| serde_json::from_value::<Vec<prune::PruneOp>>(v.clone()).ok())
                {
                    prune::apply(&mut history, &ops);
                }
            }
            "compaction" => {
                let cut = r.data.get("cut").and_then(serde_json::Value::as_u64);
                let msg = r
                    .data
                    .get("message")
                    .and_then(|v| serde_json::from_value::<Message>(v.clone()).ok());
                if let (Some(cut), Some(msg)) = (cut, msg) {
                    let cut = usize::try_from(cut)
                        .unwrap_or(usize::MAX)
                        .min(history.len());
                    compact::apply(&mut history, cut, msg);
                    marks.retain(|(i, _)| *i >= cut);
                    for (i, _) in &mut marks {
                        *i = *i - cut + 1;
                    }
                    items.push(ReplayItem::Compacted);
                }
            }
            "rewind" => {
                let Some(to) = r
                    .data
                    .get("to")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|t| usize::try_from(t).ok())
                else {
                    continue;
                };
                if let Some(&(_, len)) = marks.iter().find(|(i, _)| *i == to) {
                    items.truncate(len);
                }
                let dropped = marks.iter().filter(|(i, _)| *i >= to).count();
                marks.retain(|(i, _)| *i < to);
                turns = turns.saturating_sub(u32::try_from(dropped).unwrap_or(u32::MAX));
                history.truncate(to);
            }
            // The live session repaired its history when it resumed; do the same at the
            // same point so later prune and compaction records line up.
            "resume" => repair_dangling_calls(&mut history),
            _ => {}
        }
    }
    repair_dangling_calls(&mut history);
    (history, items, turns)
}

/// A crash between a tool call and its result leaves a call without a result, which
/// servers reject. Give such calls a result saying what happened.
fn repair_dangling_calls(history: &mut Vec<Message>) {
    let answered: std::collections::HashSet<String> = history
        .iter()
        .filter_map(|m| match m {
            Message::Tool { call_id, .. } => Some(call_id.clone()),
            _ => None,
        })
        .collect();
    let missing: Vec<String> = history
        .iter()
        .flat_map(|m| match m {
            Message::Assistant { tool_calls, .. } => {
                tool_calls.iter().map(|c| c.id.clone()).collect()
            }
            _ => Vec::new(),
        })
        .filter(|id| !answered.contains(id))
        .collect();
    for id in missing {
        // Insert right after the assistant message that made the call.
        let at = history
            .iter()
            .position(|m| matches!(m, Message::Assistant { tool_calls, .. } if tool_calls.iter().any(|c| c.id == id)))
            .map_or(history.len(), |i| i + 1);
        history.insert(
            at,
            Message::Tool {
                call_id: id,
                content: "Not run: the session ended before this call completed.".into(),
            },
        );
    }
}

/// Append a record, logging (not failing) on I/O errors: losing the log must not stop work.
pub(crate) fn record(log: Option<&SharedLog>, kind: &str, payload: serde_json::Value) {
    if let Some(log) = log
        && let Ok(mut l) = log.lock()
        && let Err(e) = l.append(kind, payload)
    {
        tracing::warn!(error = %e, "could not write session log");
    }
}

pub(crate) fn record_message(log: Option<&SharedLog>, m: &Message) {
    record(log, "message", json!({ "message": m }));
}

/// One model call's usage, and its price when the model has a `cost`.
pub(crate) fn record_usage(log: Option<&SharedLog>, u: Option<&Usage>, cost: Option<f64>) {
    let Some(u) = u else { return };
    match cost {
        Some(c) => record(log, "usage", json!({ "usage": u, "cost": c })),
        None => record(log, "usage", json!({ "usage": u })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quack_providers::ToolCall;

    fn rec(kind: &str, data: serde_json::Value) -> Record {
        Record {
            kind: kind.into(),
            ts: String::new(),
            data: data.as_object().cloned().unwrap_or_default(),
        }
    }

    #[test]
    fn replay_applies_prune_and_compaction_in_order() {
        let msg = |m: Message| rec("message", json!({ "message": m }));
        let user = |s: &str| Message::User { content: s.into() };
        let records = vec![
            msg(user("one")),
            msg(Message::Tool {
                call_id: "a".into(),
                content: "long output".into(),
            }),
            rec(
                "prune",
                json!({ "ops": [{ "index": 1, "rule": "large_output", "content": "[pruned]" }] }),
            ),
            msg(user("two")),
            rec(
                "compaction",
                json!({ "cut": 2, "message": user("[compaction summary] S") }),
            ),
            msg(user("three")),
        ];
        let (history, items, turns) = replay(&records);
        assert_eq!(
            history,
            [user("[compaction summary] S"), user("two"), user("three")]
        );
        assert_eq!(turns, 3);
        assert_eq!(items[2], ReplayItem::Compacted);
    }

    #[test]
    fn replay_applies_rewind_to_history_and_transcript() {
        let msg = |m: Message| rec("message", json!({ "message": m }));
        let user = |s: &str| Message::User { content: s.into() };
        let asst = |s: &str| Message::Assistant {
            content: s.into(),
            reasoning: None,
            tool_calls: Vec::new(),
        };
        let records = vec![
            msg(user("one")),
            msg(asst("a1")),
            msg(user("two")),
            msg(asst("a2")),
            rec("rewind", json!({ "to": 2 })),
            msg(user("two again")),
        ];
        let (history, items, turns) = replay(&records);
        assert_eq!(history, [user("one"), asst("a1"), user("two again")]);
        assert_eq!(turns, 2);
        assert_eq!(
            items,
            [
                ReplayItem::User { text: "one".into() },
                ReplayItem::Assistant { text: "a1".into() },
                ReplayItem::User {
                    text: "two again".into()
                },
            ]
        );
    }

    #[test]
    fn dangling_calls_get_results() {
        let mut h = vec![
            Message::User {
                content: "go".into(),
            },
            Message::Assistant {
                content: String::new(),
                reasoning: None,
                tool_calls: vec![ToolCall {
                    id: "a".into(),
                    name: "shell".into(),
                    arguments: "{}".into(),
                }],
            },
        ];
        repair_dangling_calls(&mut h);
        assert!(matches!(&h[2], Message::Tool { call_id, .. } if call_id == "a"));
    }
}
