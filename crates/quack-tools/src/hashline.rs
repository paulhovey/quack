//! `edit_hashline` (internal `edit.hashline`): line-number edits checked against a file tag.
//! With `[tools] edit_format = "hashline"`, `read` and `grep` put `[path#tag]` above a
//! file's lines (4 hex characters hashed from the whole file) and number the lines as
//! usual. An edit names lines by number and passes the tag of the output they come from.
//! Every version the model was shown is kept (`ReadTracker::views`), so numbers from an
//! earlier read are moved to where those lines are now; if the lines changed since, the
//! edit is refused with a fresh view of that area, so retries converge.
//!
//! The model never retypes old text, so nothing has to match exactly, and reads cost
//! what plain numbered reads do. The format follows oh-my-pi's hashline mode (MIT).

use std::path::Path;

use futures::future::BoxFuture;
use quack_protocol::ApprovalRequest;
use serde_json::{Value, json};

use crate::paths;
use crate::registry::{Subject, Tool, ToolCtx, ToolKind, ToolOutput, opt_bool, opt_str};
use crate::text::{TextFile, diff_stats, io_msg, unified_diff};

const PATH: &[&str] = &["path", "file_path", "filename", "file"];
const TAG: &[&str] = &["tag", "hash", "file_tag", "version", "snapshot"];
const BLOCK: &[&str] = &["block", "block_start", "around", "construct", "symbol"];
const AFTER_BLOCK: &[&str] = &["after_block", "block_after", "insert_after_block"];
/// Lines of fresh context shown around an edit or a refused range.
const CONTEXT: usize = 3;

/// The file's tag: 4 hex characters of FNV-1a over its LF-normalized text.
pub(crate) fn file_tag(text: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{:04x}", (h ^ (h >> 32)) & 0xffff)
}

pub(crate) fn header(shown: &str, tag: &str) -> String {
    format!("[{shown}#{tag}]")
}

/// Lines `from..to` (0-based, exclusive end), numbered as `read` numbers them.
pub(crate) fn numbered(lines: &[&str], from: usize, to: usize) -> String {
    let to = to.min(lines.len());
    let width = to.to_string().len().max(4);
    (from..to)
        .map(|i| format!("{:>width$}\t{}\n", i + 1, lines[i]))
        .collect()
}

/// The file's lines, with the empty piece after a final newline left out.
pub(crate) fn split(text: &str) -> Vec<&str> {
    let mut lines: Vec<&str> = text.split('\n').collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    lines
}

/// A line number as models write it: `12` or `"12"`, and also `"12:k3x"` (an older
/// per-line tag), `"src/a.py:12"` (copied from grep) or `"12|text"`.
fn line_no(v: &Value) -> Option<usize> {
    match v {
        Value::Number(n) => n.as_u64().and_then(|n| usize::try_from(n).ok()),
        Value::String(s) => s
            .split('|')
            .next()?
            .split(':')
            .map(str::trim)
            .find(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))?
            .parse()
            .ok(),
        _ => None,
    }
}

/// `start` with an optional end: `12`, `"12-14"`, `"12..14"`, `[12, 14]`.
fn line_range(v: &Value) -> Option<(usize, Option<usize>)> {
    match v {
        Value::Array(a) => Some((line_no(a.first()?)?, a.get(1).and_then(line_no))),
        Value::String(s) => {
            let s = s.trim();
            for sep in ["..=", "..", "-", "–", ","] {
                if let Some((a, b)) = s.split_once(sep)
                    && let (Some(a), Some(b)) = (line_no(&json!(a)), line_no(&json!(b)))
                {
                    return Some((a, Some(b)));
                }
            }
            Some((line_no(v)?, None))
        }
        _ => Some((line_no(v)?, None)),
    }
}

fn find<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().find_map(|k| v.get(*k)).filter(|v| !v.is_null())
}

/// Where an insert goes, in the version its numbers come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Gap {
    /// After this 1-based line; 0 is the top of the file.
    After(usize),
    End,
}

/// One edit as the model wrote it, numbered against the version it read.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Spec {
    /// Replace lines `start..=end` (1-based) with `text`; empty `text` deletes them.
    Replace {
        start: usize,
        end: usize,
        text: Vec<String>,
    },
    Insert {
        at: Gap,
        text: Vec<String>,
    },
    /// Replace (`Some`) or delete (`None`) the construct around `anchor`: the
    /// function, class or other definition whose outline span holds that line.
    Block {
        anchor: usize,
        text: Option<Vec<String>>,
    },
    /// Insert after the construct around `anchor`.
    AfterBlock {
        anchor: usize,
        text: Vec<String>,
    },
}

/// Action names, and the field each one's lines go in when a model writes the action as
/// a key (`{"insert_after": 12}`) instead of `"action": "insert_after"`.
const KEYED: &[(&str, &str)] = &[
    ("insert_after", "after"),
    ("insert_before", "before"),
    ("insert", "after"),
    ("append", "after"),
    ("replace", "start"),
    ("delete", "start"),
    ("after_block", "after_block"),
    ("insert_after_block", "after_block"),
    ("replace_block", "block"),
    ("delete_block", "block"),
    ("cut_block", "block"),
];
const START: &[&str] = &["start", "start_line", "line", "from", "range", "anchor"];
const END: &[&str] = &["end", "end_line", "to"];
const AFTER: &[&str] = &["after", "after_line", "anchor", "line", "start"];
const CONTENT: &[&str] = &["content", "text", "new_text", "new", "code", "lines"];

fn gap(v: &Value) -> Option<Gap> {
    if let Some(s) = v.as_str() {
        match s.trim().to_ascii_lowercase().as_str() {
            "$" | "end" | "eof" | "bottom" => return Some(Gap::End),
            "^" | "top" | "bof" => return Some(Gap::After(0)),
            _ => {}
        }
    }
    line_no(v).map(Gap::After)
}

/// `content` as lines: a string (one final newline dropped) or an array of lines.
fn content(v: &Value) -> Option<Vec<String>> {
    let text = match find(v, CONTENT)? {
        Value::String(s) => s.replace("\r\n", "\n"),
        Value::Array(a) => a
            .iter()
            .map(|l| l.as_str().map_or_else(|| l.to_string(), str::to_owned))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => return None,
    };
    if text.is_empty() {
        return Some(Vec::new());
    }
    let text = text.strip_suffix('\n').unwrap_or(&text);
    Some(unnumber(text.split('\n').map(str::to_owned).collect()))
}

/// Lines copied from `read` with their numbers still on (`  12\tcode`, or `12|code` and
/// `12:k3x|code` from older output) lose them, but only when every line has one and the
/// numbers count up by one, so code that starts with a number is left alone.
fn unnumber(rows: Vec<String>) -> Vec<String> {
    let parsed: Option<Vec<(usize, &str)>> = rows
        .iter()
        .map(|row| {
            let t = row.trim_start();
            let digits = t.bytes().take_while(u8::is_ascii_digit).count();
            let n = t[..digits].parse().ok()?;
            let rest = &t[digits..];
            let code = rest
                .strip_prefix('\t')
                .or_else(|| rest.strip_prefix('|'))
                .or_else(|| {
                    let (tag, code) = rest.strip_prefix(':')?.split_once('|')?;
                    (tag.len() <= 4).then_some(code)
                })?;
            Some((n, code))
        })
        .collect();
    match parsed {
        Some(p) if p.windows(2).all(|w| w[1].0 == w[0].0 + 1) => {
            p.into_iter().map(|(_, code)| code.to_owned()).collect()
        }
        _ => rows,
    }
}

fn parse_spec(v: &Value) -> Result<Spec, String> {
    let keyed = KEYED.iter().find_map(|(action, field)| {
        let lines = v.get(*action).filter(|a| !a.is_null() && !a.is_boolean())?;
        let mut v = v.clone();
        let obj = v.as_object_mut()?;
        obj.insert("action".into(), json!(action));
        obj.insert((*field).into(), lines.clone());
        Some(v)
    });
    let v = keyed.as_ref().unwrap_or(v);
    let explicit = opt_str(v, &["action", "op", "operation", "type"]);
    let action = explicit.unwrap_or_else(|| {
        if find(v, &["after", "after_line", "before"]).is_some() {
            "insert"
        } else if find(v, AFTER_BLOCK).is_some() {
            "insert_after_block"
        } else if v.get("delete").and_then(Value::as_bool) == Some(true) {
            "delete"
        } else if find(v, BLOCK).is_some() {
            "replace_block"
        } else {
            "replace"
        }
    });
    // `"end": "block"` says the range runs to the end of the construct `start` is in.
    let end_is_block = find(v, END).is_some_and(|e| {
        e.as_str().is_some_and(|s| {
            s.trim().eq_ignore_ascii_case("block") || s.trim().eq_ignore_ascii_case("symbol")
        })
    });
    match action {
        "block" | "replace_block" | "delete_block" | "cut_block" | "block_delete" => {
            block_spec(v, action != "replace_block" && action != "block")
        }
        "insert_after_block" | "after_block" | "append_block" | "block_after" => {
            let anchor = find(v, AFTER_BLOCK)
                .or_else(|| find(v, BLOCK))
                .and_then(line_no)
                .ok_or("`after_block` (a line in the construct to insert after) is required")?;
            if anchor == 0 {
                return Err("line numbers start at 1".into());
            }
            let text = content(v)
                .map(|t| if t.is_empty() { vec![String::new()] } else { t })
                .ok_or("`content` (the lines to insert) is required")?;
            Ok(Spec::AfterBlock { anchor, text })
        }
        "replace" | "delete" | "replace_lines" | "delete_lines" => {
            if end_is_block || find(v, BLOCK).is_some() {
                return block_spec(v, action.starts_with("delete"));
            }
            let (start, end) = find(v, START)
                .and_then(line_range)
                .ok_or("`start` (a line number) is required")?;
            let end = find(v, END).and_then(line_no).or(end).unwrap_or(start);
            if start == 0 {
                return Err("line numbers start at 1".into());
            }
            if end < start {
                return Err(format!("`end` ({end}) is before `start` ({start})"));
            }
            let text = if action.starts_with("delete") {
                Vec::new()
            } else {
                content(v).ok_or(
                    "`content` (the new lines) is required; to remove lines, use \
                     \"action\": \"delete\"",
                )?
            };
            Ok(Spec::Replace { start, end, text })
        }
        "insert" | "insert_after" | "insert_before" | "append" | "add" => {
            if let Some(anchor) = find(v, AFTER_BLOCK).and_then(line_no) {
                if anchor == 0 {
                    return Err("line numbers start at 1".into());
                }
                let text = content(v)
                    .map(|t| if t.is_empty() { vec![String::new()] } else { t })
                    .ok_or("`content` (the lines to insert) is required")?;
                return Ok(Spec::AfterBlock { anchor, text });
            }
            let at = match find(v, &["before"]).and_then(line_no) {
                Some(n) => Gap::After(n.saturating_sub(1)),
                None => find(v, AFTER).and_then(gap).ok_or(
                    "`after` (the line number to insert after, or 0 for the top) is required",
                )?,
            };
            // An empty insert can only mean a blank line.
            let text = content(v)
                .map(|t| if t.is_empty() { vec![String::new()] } else { t })
                .ok_or("`content` (the lines to insert) is required")?;
            Ok(Spec::Insert { at, text })
        }
        other => Err(format!(
            "unknown action `{other}`; use replace, insert or delete"
        )),
    }
}

/// The `block` forms: `anchor` names a line in the construct, and `text` replaces it
/// (`None`, or the delete actions, remove the whole construct).
fn block_spec(v: &Value, delete: bool) -> Result<Spec, String> {
    let anchor = find(v, BLOCK)
        .or_else(|| find(v, START))
        .and_then(line_no)
        .ok_or("`block` (a line number in the construct to replace) is required")?;
    if anchor == 0 {
        return Err("line numbers start at 1".into());
    }
    let text = if delete {
        None
    } else {
        Some(content(v).ok_or(
            "`content` (the construct's new lines) is required; to remove it, use \
             \"action\": \"delete\"",
        )?)
    };
    Ok(Spec::Block { anchor, text })
}

/// `edits` sent as a JSON string, which some models do with nested arrays. They also
/// write `"action": "insert_after": 12`, folding the line into the action; that is
/// repaired before giving up.
fn edits_from_string(s: &str) -> Result<Vec<Value>, String> {
    let parsed = serde_json::from_str::<Value>(s)
        .or_else(|e| serde_json::from_str::<Value>(&unfold_actions(s)).map_err(|_| e));
    match parsed {
        Ok(Value::Array(a)) => Ok(a),
        Ok(one @ Value::Object(_)) => Ok(vec![one]),
        Ok(_) => Err("`edits` must be an array of edit objects".into()),
        Err(e) => Err(format!(
            "`edits` was a string that is not valid JSON ({e}). Send `edits` as an array of \
             objects, not a string, e.g. [{{\"start\": 12, \"end\": 14, \"content\": \"...\"}}]"
        )),
    }
}

/// `"action": "insert_after": 12` → `"action": "insert_after", "after": 12`.
fn unfold_actions(s: &str) -> String {
    let mut out = s.to_owned();
    for (action, field) in KEYED {
        for gap in ["", " "] {
            for colon in [":", ": "] {
                let folded = format!("\"action\"{colon}\"{action}\"{gap}:");
                let fixed = format!("\"action\"{colon}\"{action}\", \"{field}\":");
                out = out.replace(&folded, &fixed);
            }
        }
    }
    out
}

/// A splice over a file's lines (0-based): `from..to` becomes `text`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Op {
    from: usize,
    to: usize,
    text: Vec<String>,
}

fn to_op(spec: &Spec, count: usize, shown: &str) -> Result<Op, String> {
    let past = |n: usize| format!("line {n} is past the end of {shown} (it has {count} lines)");
    match spec {
        Spec::Replace { start, end, text } => {
            if *start > count {
                return Err(past(*start));
            }
            Ok(Op {
                from: start - 1,
                to: (*end).min(count),
                text: text.clone(),
            })
        }
        Spec::Insert { at, text } => {
            let at = match at {
                Gap::After(n) if *n > count => return Err(past(*n)),
                Gap::After(n) => *n,
                Gap::End => count,
            };
            Ok(Op {
                from: at,
                to: at,
                text: text.clone(),
            })
        }
        // `plan` resolves these against the outline before any `to_op` call.
        Spec::Block { .. } | Spec::AfterBlock { .. } => Err(
            "`block` could not be resolved (no outline for this file); give `start` and \
                 `end` lines"
                .into(),
        ),
    }
}

/// Where each line of `old` is in `new` (0-based), for the lines a diff keeps.
fn line_map(old: &[&str], new: &[&str]) -> Vec<Option<usize>> {
    let mut map = vec![None; old.len()];
    for op in similar::capture_diff_slices(similar::Algorithm::Patience, old, new) {
        if let similar::DiffOp::Equal {
            old_index,
            new_index,
            len,
        } = op
        {
            for i in 0..len {
                map[old_index + i] = Some(new_index + i);
            }
        }
    }
    map
}

/// `op`, numbered against an older version, moved onto the current one. The lines it
/// replaces must all be unchanged and still together, and so must a line beside them
/// (which tells a moved `}` from another `}`); an insert needs its neighbours to agree.
fn remap(op: &Op, map: &[Option<usize>], new_len: usize) -> Option<Op> {
    let at = |i: usize| map.get(i).copied().flatten();
    let moved = |from: usize| Op {
        from,
        to: from + (op.to - op.from),
        text: op.text.clone(),
    };
    if op.from == op.to {
        let above = op.from.checked_sub(1).map(|i| at(i).map(|m| m + 1));
        let below = (op.from < map.len()).then(|| at(op.from));
        return match (above, below) {
            (None, None) => Some(moved(0)),
            (Some(Some(a)), Some(Some(b))) if a == b => Some(moved(a)),
            (Some(Some(a)), None) | (None, Some(Some(a))) => Some(moved(a)),
            _ => None,
        };
    }
    let shift = |i: usize| at(i).map(|m| m as isize - i as isize);
    let start = at(op.from)?;
    let d = shift(op.from)?;
    if (op.from..op.to).any(|i| shift(i) != Some(d)) {
        return None;
    }
    // The start or end of the file, in both versions, anchors as well as a line does.
    let top = op.from == 0 && start == 0;
    let bottom = op.to == map.len() && start + (op.to - op.from) == new_len;
    let anchored = top
        || bottom
        || [op.from.checked_sub(1), (op.to < map.len()).then_some(op.to)]
            .into_iter()
            .flatten()
            .any(|i| shift(i) == Some(d));
    anchored.then(|| moved(start))
}

/// A line worth matching as a repeat: it has letters or digits, and is more than a
/// closing brace or `end`, so a body that really ends in `}` keeps it.
fn distinctive(line: &str) -> bool {
    line.trim().len() >= 4 && line.chars().any(char::is_alphanumeric)
}

/// Models often repeat the lines just outside the range they replace (to show where the
/// change goes), which would duplicate them. Such repeats at the start or end of `text`
/// are dropped when what is left still fills the range, the shape of a copied boundary;
/// an insert loses a first line that repeats the line it goes after. Returns a note.
fn drop_echoes(lines: &[&str], op: &mut Op) -> Option<String> {
    let same = |a: &str, b: &str| a.trim_end() == b.trim_end();
    let echo = |text: &[String], src: &[&str]| {
        text.len() == src.len()
            && text.iter().zip(src).all(|(a, b)| same(a, b))
            && text.iter().any(|l| distinctive(l))
    };
    let len = op.text.len();
    if op.from == op.to {
        let first = op.from.checked_sub(1).map(|i| lines[i]);
        if len > 1 && first.is_some_and(|f| distinctive(f) && same(&op.text[0], f)) {
            op.text.remove(0);
            return Some(format!(
                "The first line of `content` repeated line {} (the one it goes after), so it \
                 was left out.",
                op.from
            ));
        }
        return None;
    }
    let range = op.to - op.from;
    let lead = (1..=len.min(op.from))
        .rev()
        .find(|k| echo(&op.text[..*k], &lines[op.from - k..op.from]))
        .unwrap_or(0);
    let trail = (1..=len.min(lines.len() - op.to))
        .rev()
        .find(|k| echo(&op.text[len - k..], &lines[op.to..op.to + k]))
        .unwrap_or(0);
    let (lead, trail) = match (lead, trail) {
        (0, 0) => return None,
        (l, t) if l > 0 && t > 0 => {
            if l + t <= len && len - l - t == range {
                (l, t)
            } else {
                return None;
            }
        }
        (l, t) if range > 1 && len - l - t >= range => (l, t),
        _ => return None,
    };
    op.text.drain(len - trail..);
    op.text.drain(..lead);
    let mut parts = Vec::new();
    if lead > 0 {
        parts.push(format!(
            "the first {lead} line(s) of `content` repeated line(s) {}-{} just above the range",
            op.from - lead + 1,
            op.from
        ));
    }
    if trail > 0 {
        parts.push(format!(
            "the last {trail} line(s) repeated line(s) {}-{} just below it",
            op.to + 1,
            op.to + trail
        ));
    }
    Some(format!(
        "{} (kept, not replaced), so they were left out.",
        capitalize(&parts.join(", and "))
    ))
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

struct Plan {
    path: std::path::PathBuf,
    shown: String,
    file: TextFile,
    after: String,
    /// What each edit did, for the result.
    done: Vec<String>,
    /// The new lines (1-based, inclusive), whose numbers the result gives.
    spans: Vec<(usize, usize)>,
    notes: Vec<String>,
}

/// A refused edit: the message, and the current lines it shows (which become a view
/// the next try can number against, so a straight retry works).
#[derive(Debug)]
struct Refused {
    message: String,
    shown: Option<Reveal>,
}

/// Lines shown with a refusal: the version's tag and text, and the span (0-based,
/// exclusive end) the message numbered.
#[derive(Debug)]
struct Reveal {
    tag: String,
    text: String,
    from: usize,
    to: usize,
}

impl From<String> for Refused {
    fn from(message: String) -> Self {
        Self {
            message,
            shown: None,
        }
    }
}

impl From<&str> for Refused {
    fn from(message: &str) -> Self {
        message.to_owned().into()
    }
}

/// A refusal that shows lines `from..to` (0-based) of `text` under its tag.
fn refuse_in(why: String, shown: &str, tag: &str, text: &str, from: usize, to: usize) -> Refused {
    let lines = split(text);
    let (from, to) = (from.min(lines.len()), to.min(lines.len()));
    Refused {
        message: format!(
            "{why} Current lines there:\n{}\n{}",
            header(shown, tag),
            numbered(&lines, from, to)
        ),
        shown: Some(Reveal {
            tag: tag.to_owned(),
            text: text.to_owned(),
            from,
            to,
        }),
    }
}

/// A refusal that shows the current lines `from..to` under the file's tag.
fn refuse(why: String, shown: &str, text: &str, from: usize, to: usize) -> Refused {
    refuse_in(why, shown, &file_tag(text), text, from, to)
}

/// `path#ab12` (the header copied into `path`) → the path and the tag.
fn split_tag(raw: &str) -> (&str, Option<&str>) {
    let raw = raw.trim().trim_start_matches('[').trim_end_matches(']');
    match raw.rsplit_once('#') {
        Some((p, t)) if t.len() == 4 && t.bytes().all(|b| b.is_ascii_hexdigit()) => (p, Some(t)),
        _ => (raw, None),
    }
}

fn clean_tag(raw: &str) -> String {
    let t = raw.trim().trim_start_matches('[').trim_end_matches(']');
    t.rsplit_once('#')
        .map_or(t, |(_, t)| t)
        .trim()
        .to_ascii_lowercase()
}

/// A top-level argument, or the same key inside the first edit, where models also put
/// `path` and `tag`.
fn top<'a>(args: &'a Value, keys: &[&str]) -> Option<&'a str> {
    let first = match args.get("edits") {
        Some(Value::Array(a)) => a.first(),
        Some(one @ Value::Object(_)) => Some(one),
        _ => None,
    };
    opt_str(args, keys).or_else(|| opt_str(first?, keys))
}

fn target(args: &Value, ctx: &ToolCtx) -> Result<std::path::PathBuf, String> {
    let raw = top(args, PATH).ok_or("missing required string argument `path`")?;
    Ok(paths::resolve(&ctx.root, split_tag(raw).0))
}

fn specs(args: &Value) -> Result<Vec<Spec>, String> {
    let raw: Vec<Value> = match args.get("edits") {
        Some(Value::Array(a)) => a.clone(),
        Some(Value::String(s)) => edits_from_string(s)?,
        Some(one @ Value::Object(_)) => vec![one.clone()],
        _ => vec![args.clone()],
    };
    if raw.is_empty() {
        return Err("no edits given".into());
    }
    raw.iter()
        .enumerate()
        .map(|(i, v)| {
            parse_spec(v).map_err(|e| {
                if raw.len() > 1 {
                    format!("edit {}: {e}", i + 1)
                } else {
                    e
                }
            })
        })
        .collect()
}

/// The 1-based lines the specs name, for picking the version they were read from.
fn anchors(specs: &[Spec]) -> Vec<usize> {
    specs
        .iter()
        .flat_map(|s| match s {
            Spec::Replace { start, end, .. } => vec![*start, *end],
            Spec::Insert {
                at: Gap::After(n), ..
            } if *n > 0 => vec![*n],
            Spec::Insert { .. } => Vec::new(),
            Spec::Block { anchor, .. } | Spec::AfterBlock { anchor, .. } => vec![*anchor],
        })
        .collect()
}

/// The construct around `anchor` (1-based): the innermost definition that spans more
/// than one line, from the file's outline, with decorator lines above its first line
/// pulled in. Falls back to a one-line definition, so `"block": N` on a one-liner is
/// just that line. `None` when the language has no outline here or nothing encloses
/// the line; `Err` carries the next definition below, for the refusal.
fn around(path: &Path, text: &str, anchor: usize) -> Result<Option<(usize, usize)>, usize> {
    let Some(outline) = crate::outline::outline(path, text) else {
        return Ok(None);
    };
    let holding = outline
        .symbols
        .iter()
        .filter(|s| s.start <= anchor && anchor <= s.end);
    let span = holding
        .clone()
        .min_by_key(|s| s.end - s.start)
        .map(|s| (s.start, s.end));
    let span = match span {
        Some((a, b)) if b > a => Some((a, b)),
        // Only a one-line definition holds the line: that line is the construct.
        Some((a, _)) => Some((a, a)),
        None => {
            let next = outline
                .symbols
                .iter()
                .filter(|s| s.start > anchor)
                .map(|s| s.start)
                .min()
                .unwrap_or(anchor);
            return Err(next);
        }
    };
    let Some((start, end)) = span else {
        return Ok(None);
    };
    // A decorator belongs to the definition below it, even where the outline's span
    // starts at the `def`/`fn` line.
    let lines = split(text);
    let mut start = start;
    while start > 1 {
        let above = lines[start - 2].trim();
        let decorated = above.starts_with('@') || (above.starts_with("#[") && above.ends_with(']'));
        if !decorated {
            break;
        }
        start -= 1;
    }
    Ok(Some((start, end)))
}

fn plan(args: &Value, ctx: &ToolCtx) -> Result<Plan, Refused> {
    let path = target(args, ctx)?;
    let shown = paths::display(&ctx.root, &path);
    if !path.exists() {
        return Err(format!("{shown} does not exist; use `write` to create it").into());
    }
    let file = TextFile::load(&path)?;
    let specs = specs(args)?;
    let current = split(&file.text);
    let now = file_tag(&file.text);

    // The version the numbers come from: the tag given, else the newest view that showed
    // all the lines named, else the newest that showed any. Edit results show no lines,
    // so after an edit the numbers still mean the read they came from.
    let views = ctx.reads.views(&path);
    let given = top(args, TAG).map(clean_tag).or_else(|| {
        split_tag(top(args, PATH).unwrap_or_default())
            .1
            .map(clean_tag)
    });
    let mut notes = Vec::new();
    let base: Option<crate::registry::View> = match &given {
        Some(t) if *t == now => None,
        Some(t) if let Some(v) = views.iter().find(|v| v.tag == *t) => Some(v.clone()),
        _ if views.is_empty() => None,
        _ if !views.iter().any(|v| v.tag == now) => {
            let n = anchors(&specs).into_iter().min().unwrap_or(1);
            return Err(refuse(
                format!(
                    "{shown} changed since you last read it (not through edit_hashline), so \
                     these line numbers may be off; nothing was edited."
                ),
                &shown,
                &file.text,
                n.saturating_sub(1 + CONTEXT),
                n + 2 * CONTEXT,
            ));
        }
        _ => {
            let want = anchors(&specs);
            let v = views
                .iter()
                .rev()
                .find(|v| want.iter().all(|n| v.covers(*n)))
                .or_else(|| views.iter().rev().find(|v| v.covers_any()))
                .filter(|v| v.tag != now)
                .cloned();
            if let Some(t) = &given {
                let used = v.as_ref().map_or(now.as_str(), |v| v.tag.as_str());
                notes.push(format!(
                    "`{t}` is not a tag {shown} was shown with; the numbers were read \
                     against {}.",
                    header(&shown, used)
                ));
            }
            v
        }
    };

    // The version the numbers are numbered against: the base view, else the view of
    // the file as it is now. `block` anchors are resolved against it, in its numbering.
    let numbering = base
        .clone()
        .or_else(|| views.iter().rev().find(|v| v.tag == now).cloned());
    let numbering_text = numbering
        .as_ref()
        .map_or(file.text.clone(), |v| v.text.to_string());
    // The ranges the model wrote out itself. (`block` edits resolve their own, which
    // need no check: the model was shown the anchor line, and the construct is the
    // code's, not the model's counting.)
    let written: Vec<(usize, usize)> = specs
        .iter()
        .filter_map(|s| match s {
            Spec::Replace { start, end, .. } => Some((*start, *end)),
            _ => None,
        })
        .collect();

    // `block` edits become the lines of the construct around their anchor, numbered
    // against the same version as the numbers the model sent.
    let mut specs = specs;
    for spec in &mut specs {
        let (anchor, text) = match spec {
            Spec::Block { anchor, text } => (*anchor, text.take()),
            Spec::AfterBlock { anchor, text } => (*anchor, Some(std::mem::take(text))),
            _ => continue,
        };
        let after_construct = matches!(spec, Spec::AfterBlock { .. });
        // A line the numbered version lacks is one the model's own edits added: resolve
        // it against the file as it is, the way past-the-end numbers are re-read.
        let resolved = around(&path, &numbering_text, anchor).ok().flatten();
        let (span, from_now) = match resolved {
            Some(span) => (span, false),
            None => match numbering
                .as_ref()
                .and_then(|_| around(&path, &file.text, anchor).ok().flatten())
            {
                Some(span) => (span, true),
                None => {
                    let next = around(&path, &numbering_text, anchor)
                        .err()
                        .unwrap_or(anchor);
                    let why = if next == anchor {
                        format!(
                            "no function, class or other definition around line {anchor} of \
                             {shown} can be found here, so `block` cannot resolve; give `start` \
                             and `end` lines instead. Nothing was edited."
                        )
                    } else {
                        format!(
                            "line {anchor} of {shown} is not inside a definition; the next one \
                             starts at line {next}. Give `block` a line inside a function, class \
                             or other definition, or `start` and `end` lines. Nothing was \
                             edited."
                        )
                    };
                    return Err(refuse_in(
                        why,
                        &shown,
                        numbering.as_ref().map_or(now.as_str(), |v| v.tag.as_str()),
                        &numbering_text,
                        anchor.saturating_sub(1 + CONTEXT),
                        next.max(anchor) + CONTEXT,
                    ));
                }
            },
        };
        let (start, end) = span;
        notes.push(if from_now {
            format!(
                "the construct around line {anchor} is lines {start}-{end} as the file is \
                 now (`block`)"
            )
        } else {
            format!("lines {start}-{end} are the construct around line {anchor} (`block`)")
        });
        *spec = if after_construct {
            Spec::Insert {
                at: Gap::After(end),
                text: text.unwrap_or_default(),
            }
        } else {
            Spec::Replace {
                start,
                end,
                text: text.unwrap_or_default(),
            }
        };
    }

    let ops_for = |len: usize| -> Result<Vec<Op>, String> {
        specs.iter().map(|s| to_op(s, len, &shown)).collect()
    };
    // Set when past-the-end numbers were re-read against the current file: those the
    // model worked out itself, so no output could have shown them.
    let mut trusts_current = false;
    let (base, mut ops) = match base {
        Some(v) => match ops_for(split(&v.text).len()) {
            Ok(ops) => (Some(v), ops),
            // Past the end of that version but not of the file: numbers worked out
            // after the model's own edits, sent with the older tag.
            Err(e) => match ops_for(current.len()) {
                Ok(ops) => {
                    notes.push(format!(
                        "{} with {}, so the numbers were read against the file as it is \
                         now, {}.",
                        capitalize(&e),
                        header(&shown, &v.tag),
                        header(&shown, &now)
                    ));
                    trusts_current = true;
                    (None, ops)
                }
                Err(_) => return Err(e.into()),
            },
        },
        None => (None, ops_for(current.len())?),
    };
    let base_lines = base.as_ref().map(|v| split(&v.text));
    if let (Some(view), Some(old)) = (&base, &base_lines) {
        let map = line_map(old, &current);
        for op in &mut ops {
            let Some(new) = remap(op, &map, current.len()) else {
                // Show the current lines nearest to where the range was.
                let near = (0..=op.from.min(map.len().saturating_sub(1)))
                    .rev()
                    .find_map(|i| map.get(i).copied().flatten())
                    .unwrap_or(op.from.min(current.len()));
                let what = if op.from == op.to {
                    format!("The lines around line {}", op.from)
                } else if op.to == op.from + 1 {
                    format!("Line {}", op.to)
                } else {
                    format!("Lines {}-{}", op.from + 1, op.to)
                };
                return Err(refuse(
                    format!(
                        "{what} of {} changed since; nothing was edited.",
                        header(&shown, &view.tag)
                    ),
                    &shown,
                    &file.text,
                    near.saturating_sub(CONTEXT),
                    near + (op.to - op.from) + 2 * CONTEXT,
                ));
            };
            *op = new;
        }
    }

    // Lines an edit replaces must have been shown to the model: a range over lines no
    // output displayed (an elided middle, a folded outline, a number from memory)
    // mangles the file. The refusal shows them, and counts as reading them, so the
    // same edit again goes through.
    let cover = base.as_ref().or_else(|| {
        (!trusts_current)
            .then(|| views.iter().rev().find(|v| v.tag == now))
            .flatten()
    });
    if !written.is_empty() && !trusts_current && !opt_bool(args, &["force"]) {
        let unseen = written
            .iter()
            .find(|(start, end)| cover.is_none_or(|v| (*start..=*end).any(|n| !v.covers(n))));
        if let Some((start, end)) = unseen {
            let named = if start == end {
                format!("line {start}")
            } else {
                format!("lines {start}-{end}")
            };
            let (tag, text) = match cover {
                Some(v) => (v.tag.clone(), v.text.to_string()),
                // No view at all: the file was never read. `trusts_current` is false
                // here, or the numbers would have been re-read against the file.
                None => (now.clone(), file.text.clone()),
            };
            let why = if cover.is_some() {
                format!(
                    "the output these numbers come from did not show {named} of {shown} (it \
                     showed a range, a search hit or an outline), so they may be wrong. Nothing \
                     was edited; these lines now count as read, so re-issue the edit to apply \
                     it, or fix the numbers."
                )
            } else {
                format!(
                    "{shown} has not been read with its line numbers yet, so {named} cannot be \
                     checked. Nothing was edited; read it first (or send the edit again with \
                     `force`), and these lines now count as read."
                )
            };
            return Err(refuse_in(
                why,
                &shown,
                &tag,
                &text,
                start.saturating_sub(1 + CONTEXT),
                end + CONTEXT,
            ));
        }
    }

    for op in &mut ops {
        notes.extend(drop_echoes(&current, op));
    }
    // Where each edit's new lines land: the edits above it shift it. Called before the
    // ops are sorted, so each `done` lines up with its spec; the mend path calls it
    // again on its (single, still sorted-alone) op.
    let landing = |ops: &[Op]| -> (Vec<String>, Vec<(usize, usize)>) {
        let mut done = Vec::new();
        let mut spans = Vec::new();
        for (op, spec) in ops.iter().zip(&specs) {
            let shift: isize = ops
                .iter()
                .filter(|o| o.to <= op.from && !std::ptr::eq(*o, op))
                .map(|o| o.text.len() as isize - (o.to - o.from) as isize)
                .sum();
            let from = op.from.saturating_add_signed(shift);
            done.push(describe(spec, op.text.len(), from + 1));
            if !op.text.is_empty() {
                spans.push((from + 1, from + op.text.len()));
            }
        }
        (done, spans)
    };
    let (mut done, mut spans) = landing(&ops);
    ops.sort_by_key(|o| std::cmp::Reverse((o.from, o.to)));
    for w in ops.windows(2) {
        // Sorted from the bottom: the upper one must end before the lower one starts.
        let (lo, hi) = (&w[0], &w[1]);
        if hi.to > lo.from || (hi.from == lo.from && hi.to == lo.to) {
            return Err("two edits touch the same lines; combine them into one edit".into());
        }
    }
    let assemble = |ops: &[Op]| -> String {
        let mut out: Vec<String> = current.iter().map(|l| (*l).to_owned()).collect();
        for op in ops {
            out.splice(op.from..op.to, op.text.iter().cloned());
        }
        let mut text = out.join("\n");
        if !text.is_empty() && (file.text.ends_with('\n') || file.text.is_empty()) {
            text.push('\n');
        }
        text
    };
    let mut after = assemble(&ops);
    if after == file.text {
        return Err("the edit changes nothing".into());
    }
    // A range one line off still applies, but breaks the code; catch that here, before
    // the model spends steps on test failures. `force` takes the edit as it is.
    if !opt_bool(args, &["force"])
        && let Some(line) = crate::outline::new_syntax_error(&path, &file.text, &after)
    {
        // A lone replace whose range stops short of the closers it orphaned is mended
        // by taking them in, when that is what makes the file parse again.
        let mended = (ops.len() == 1 && ops[0].from < ops[0].to)
            .then(|| mend(&ops[0], &current, &path, &file.text))
            .flatten();
        if let Some((op, note)) = mended {
            notes.push(note);
            ops[0] = op;
            after = assemble(&ops);
            (done, spans) = landing(&ops);
        } else {
            let lines = split(&after);
            let around: Vec<String> = merge(
                spans
                    .iter()
                    .map(|(a, b)| (a.saturating_sub(1 + CONTEXT), b + CONTEXT))
                    .chain([(line.saturating_sub(1 + CONTEXT), line + CONTEXT)])
                    .collect(),
                lines.len(),
            )
            .into_iter()
            .map(|(a, b)| numbered(&lines, a, b))
            .collect();
            return Err(format!(
                "Nothing was edited: this edit would leave a syntax error at line {line}, and \
                 {shown} parsed cleanly before. Check the range: `end` may stop a line short, or \
                 `content` may repeat or leave out a line. The lines as they would read:\n{}\
                 If a later edit fixes it, send both in one call; to make this edit anyway, send \
                 it again with \"force\": true.",
                around.join("…\n")
            )
            .into());
        }
    }
    Ok(Plan {
        path,
        shown,
        file,
        after,
        done,
        spans,
        notes,
    })
}

/// Overlapping or touching windows (0-based, exclusive end) joined, clamped to `len`.
fn merge(mut w: Vec<(usize, usize)>, len: usize) -> Vec<(usize, usize)> {
    w.sort_unstable();
    let mut out: Vec<(usize, usize)> = Vec::new();
    for (a, b) in w {
        let (a, b) = (a.min(len), b.min(len));
        match out.last_mut() {
            Some(last) if a <= last.1 => last.1 = last.1.max(b),
            _ => out.push((a, b)),
        }
    }
    out.retain(|(a, b)| a < b);
    out
}

/// A line that only closes what came before it (a dangling `)` from a range that
/// stopped a line short), so taking it with the change cannot drop anything the model
/// meant to keep.
fn closer_only(line: &str) -> bool {
    let t = line.trim().trim_end_matches([',', ';']);
    matches!(t, ")" | "}" | "]" | "end" | "*/")
}

/// Mends a lone replace whose range left the parse broken: the range is extended over
/// up to three closer-only lines below it (or opener lines above it) when exactly that
/// makes the file parse again. Returns the fixed op and a note; `None` when no
/// extension repairs it. oh-my-pi's hashline mode calls this idea the syntax probe.
fn mend(op: &Op, current: &[&str], path: &Path, before: &str) -> Option<(Op, String)> {
    let parses = |op: &Op| -> bool {
        let mut out: Vec<String> = current.iter().map(|l| (*l).to_owned()).collect();
        out.splice(op.from..op.to, op.text.iter().cloned());
        let mut text = out.join("\n");
        if !text.is_empty() && before.ends_with('\n') {
            text.push('\n');
        }
        crate::outline::new_syntax_error(path, before, &text).is_none()
    };
    let mut try_end = op.clone();
    for k in 1..=3 {
        let i = op.to + k - 1;
        if i >= current.len() || !closer_only(current[i]) {
            break;
        }
        try_end.to = i + 1;
        if parses(&try_end) {
            let note = format!(
                "the range stopped short of line {}, which only closes what the new lines \
                 opened, so that line was taken with the change",
                i + 1
            );
            return Some((try_end, note));
        }
    }
    let mut try_start = op.clone();
    for k in 1..=3 {
        let i = op.from.checked_sub(k)?;
        let opener = current[i].trim_end().ends_with(['(', '{', '[', ':', '=']);
        if !opener {
            return None;
        }
        try_start.from = i;
        if parses(&try_start) {
            let note = format!(
                "line {} only opens what the new lines finish, so it was taken with the \
                 change",
                i + 1
            );
            return Some((try_start, note));
        }
    }
    None
}

/// What one edit did, in the numbers the model used.
fn describe(spec: &Spec, new: usize, now: usize) -> String {
    let lines = |n: usize| {
        if n == 1 {
            "1 line".to_owned()
        } else {
            format!("{n} lines")
        }
    };
    let span = |a: usize, b: usize| {
        if a == b {
            format!("line {a}")
        } else {
            format!("lines {a}-{b}")
        }
    };
    let landed = span(now, now + new - 1);
    match spec {
        Spec::Replace { start, end, .. } if new == 0 => format!("deleted {}", span(*start, *end)),
        Spec::Replace { start, end, .. } => {
            format!(
                "replaced {} with {} (now {landed})",
                span(*start, *end),
                lines(new)
            )
        }
        Spec::Insert {
            at: Gap::After(0), ..
        } => {
            format!("inserted {} at the top (now {landed})", lines(new))
        }
        Spec::Insert {
            at: Gap::After(n), ..
        } => {
            format!("inserted {} after line {n} (now {landed})", lines(new))
        }
        Spec::Insert { at: Gap::End, .. } => {
            format!("inserted {} at the end (now {landed})", lines(new))
        }
        // `plan` has turned these into the shapes above by the time `describe` runs.
        Spec::Block { .. } | Spec::AfterBlock { .. } => format!("edited {}", lines(new)),
    }
}

#[derive(Debug)]
pub struct Hashline;

impl Tool for Hashline {
    fn name(&self) -> &str {
        "edit_hashline"
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Mutating
    }

    fn available(&self, ctx: &ToolCtx) -> bool {
        ctx.hashline()
    }

    fn description(&self, _ctx: &ToolCtx) -> String {
        "Edit a file by line numbers from a numbered output (a `read`, a `grep` result \
         or this tool's own result). Pass `path` and the `tag` of the `[path#tag]` \
         header that output carried, plus `edits`, each one of:\n\
         - `start` and `end` (inclusive; `end` defaults to `start`) with `content`: \
         replace those lines; `\"action\": \"delete\"` drops `content` and deletes them.\n\
         - `block`: a line inside a function, class or other definition, with `content`: \
         replace that whole definition (no `end` to count); `\"action\": \"delete\"` \
         removes it. `after_block`: a line in it, with `content`: insert after it.\n\
         - `after`: a line number (0 for the top, \"$\" for the end) with `content`: \
         insert there. `before` also works.\n\
         `content` is exactly the new lines, final and indented, without line numbers. \
         Keep ranges tight: replace only the lines that change, never unchanged lines \
         around them, and use `after` for pure additions. Put every edit to one file in \
         one `edits` array, all numbered from the same output; they apply together, so \
         don't shift numbers for the others. The result names the new lines' numbers and \
         the file's new tag; later edits can use those, or an older output's numbers \
         with its tag: they are moved to where those lines are now. Lines that output \
         never showed, or that changed since, are refused with the current lines and a \
         new tag: read what it shows, then re-issue."
            .into()
    }

    fn schema(&self) -> Value {
        let edit = json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["replace", "insert", "delete"],
                    "description": "replace (default), insert (when `after` is given) or delete"
                },
                "start": { "type": "integer", "description": "First line to replace or delete" },
                "end": { "type": "integer", "description": "Last line to replace or delete (inclusive)" },
                "block": { "type": "integer", "description": "A line in the definition to replace (or delete) entirely; no `end` needed" },
                "after_block": { "type": "integer", "description": "Insert after the definition around this line" },
                "after": { "type": "integer", "description": "Insert after this line; 0 for the top" },
                "content": { "type": "string", "description": "The new lines (\"\" with action delete)" }
            },
            "required": ["content"]
        });
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path, relative to the workspace root or absolute" },
                "tag": { "type": "string", "description": "The tag from `[path#tag]` in the output the line numbers come from" },
                "edits": { "type": "array", "items": edit }
            },
            "required": ["path", "tag", "edits"]
        })
    }

    fn subject(&self, args: &Value, ctx: &ToolCtx) -> Result<Subject, String> {
        Ok(Subject::Edit(target(args, ctx)?))
    }

    fn summary(&self, args: &Value, ctx: &ToolCtx) -> String {
        let path = target(args, ctx)
            .map(|p| paths::display(&ctx.root, &p))
            .unwrap_or_default();
        format!("edit {path}")
    }

    fn preview(&self, args: &Value, ctx: &ToolCtx) -> Result<ApprovalRequest, String> {
        let p = plan(args, ctx).map_err(|r| r.message)?;
        Ok(ApprovalRequest::Edit {
            diff: unified_diff(&p.shown, &p.file.text, &p.after),
            path: p.shown,
        })
    }

    fn run<'a>(&'a self, args: Value, ctx: &'a ToolCtx) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            let p = match plan(&args, ctx) {
                Ok(p) => p,
                Err(r) => {
                    if let (Some(reveal), Ok(path)) = (&r.shown, target(&args, ctx)) {
                        ctx.reads.saw(
                            &path,
                            &reveal.tag,
                            &reveal.text,
                            (reveal.from + 1, reveal.to),
                        );
                    }
                    return ToolOutput::error(r.message);
                }
            };
            if let Err(e) = std::fs::write(&p.path, p.file.encode(&p.after)) {
                return ToolOutput::error(io_msg(&p.path, &e));
            }
            ctx.reads.record(&p.path);
            output(&p, ctx)
        })
    }
}

fn output(p: &Plan, ctx: &ToolCtx) -> ToolOutput {
    // The new version counts as showing only the new lines: other numbers without a tag
    // still mean the output they came from.
    let tag = file_tag(&p.after);
    ctx.reads.edited(&p.path, &tag, &p.after, &p.spans);
    let mut model = format!(
        "Edited {}, now {}: {}.",
        p.shown,
        header(&p.shown, &tag),
        p.done.join("; ")
    );
    for note in &p.notes {
        model.push_str(&format!(" {note}"));
    }
    let (add, del) = diff_stats(&p.file.text, &p.after);
    ToolOutput::ok(model, format!("+{add} -{del}")).with_detail(unified_diff(
        &p.shown,
        &p.file.text,
        &p.after,
    ))
}

/// Note a whole file as seen: `write` content, which the model wrote itself.
pub(crate) fn saw_whole(ctx: &ToolCtx, path: &Path, text: &str) {
    if ctx.hashline() {
        let text = text.replace("\r\n", "\n");
        let n = split(&text).len();
        ctx.reads.saw(path, &file_tag(&text), &text, (1, n.max(1)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(root: &Path) -> ToolCtx {
        ToolCtx {
            root: root.to_path_buf(),
            edit_format: quack_config::settings::EditFormat::Hashline,
            ..crate::registry::test_ctx()
        }
    }

    /// A workspace with `a.py` holding `text`, read once in full.
    fn setup(text: &str) -> (tempfile::TempDir, ToolCtx, String) {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.py"), text).expect("write");
        let c = ctx(dir.path());
        let tag = file_tag(text);
        c.reads
            .saw(&dir.path().join("a.py"), &tag, text, (1, split(text).len()));
        (dir, c, tag)
    }

    fn apply(text: &str, edits: Value) -> Result<String, String> {
        let (_dir, c, tag) = setup(text);
        plan(&json!({"path": "a.py", "tag": tag, "edits": edits}), &c)
            .map(|p| p.after)
            .map_err(|r| r.message)
    }

    async fn edit(c: &ToolCtx, args: Value) -> ToolOutput {
        Hashline.run(args, c).await
    }

    fn on_disk(c: &ToolCtx) -> String {
        std::fs::read_to_string(c.root.join("a.py")).expect("read")
    }

    #[test]
    fn replace_insert_delete_in_one_call() {
        let text = "fn a() {}\nfn b() {}\nfn c() {}\nfn d() {}\n";
        let edits = json!([
            {"start": 2, "content": "fn b2() {}\nfn b3() {}"},
            {"after": 0, "content": "// top"},
            {"action": "delete", "start": 4},
        ]);
        assert_eq!(
            apply(text, edits),
            Ok("// top\nfn a() {}\nfn b2() {}\nfn b3() {}\nfn c() {}\n".into())
        );
        // The end of the file, and an insert before a line.
        assert_eq!(
            apply("a\nb\n", json!([{"after": "end", "content": "c"}])),
            Ok("a\nb\nc\n".into())
        );
        assert_eq!(
            apply("a\nb\n", json!([{"before": 2, "content": "x"}])),
            Ok("a\nx\nb\n".into())
        );
        // Empty content deletes.
        assert_eq!(
            apply("a\nb\nc\n", json!([{"start": 2, "content": ""}])),
            Ok("a\nc\n".into())
        );
    }

    #[test]
    fn numbers_and_ranges_in_the_shapes_models_send() {
        let text = "a\nb\nc\nd\n";
        for start in [
            json!(2),
            json!("2"),
            json!("2:k3x"),
            json!("a.py:2"),
            json!("2|b"),
        ] {
            assert_eq!(
                apply(text, json!([{"start": start, "end": 3, "content": "X"}])),
                Ok("a\nX\nd\n".into()),
                "{start}"
            );
        }
        for range in [json!("2-3"), json!("2..3"), json!([2, 3])] {
            assert_eq!(
                apply(text, json!([{"start": range, "content": "X"}])),
                Ok("a\nX\nd\n".into()),
                "{range}"
            );
        }
        // The action as the key, and a single edit without `edits`.
        assert_eq!(
            apply(
                text,
                json!([{"insert_after": 1, "content": "a2"}, {"delete": 4}])
            ),
            Ok("a\na2\nb\nc\n".into())
        );
        let (_dir, c, tag) = setup(text);
        let one = json!({"path": "a.py", "tag": tag, "start": 1, "content": "A"});
        assert_eq!(
            plan(&one, &c).map(|p| p.after).ok(),
            Some("A\nb\nc\nd\n".into())
        );
        // The tag copied onto the path.
        let tagged =
            json!({"path": format!("a.py#{tag}"), "edits": [{"start": 1, "content": "A"}]});
        assert!(plan(&tagged, &c).is_ok());
    }

    #[test]
    fn bad_numbers_say_what_is_wrong() {
        let err = |edits| apply("a\nb\n", edits).expect_err("refused");
        assert!(err(json!([{"start": 5, "content": "x"}])).contains("past the end"));
        assert!(err(json!([{"start": 2, "end": 1, "content": "x"}])).contains("before"));
        assert!(err(json!([{"start": 1}])).contains("`content`"));
        assert!(
            err(json!([{"start": 1, "content": "x"}, {"start": 1, "content": "y"}]))
                .contains("same lines")
        );
        assert!(err(json!([{"start": 1, "content": "a"}])).contains("changes nothing"));
    }

    // deepseek-v4.1-flash sent `edits` as a string with the line folded into the action.
    #[test]
    fn stringified_edits_are_repaired_or_explained() {
        let text = "a\nb\nc\n";
        let s = "[{\"action\": \"insert_after\": 2, \"content\": \"b2\"}]";
        assert_eq!(apply(text, json!(s)), Ok("a\nb\nb2\nc\n".into()));
        let err = apply(
            text,
            json!("[{\"action\": \"replace\", \"start\": 1, \"end\": \"to"),
        )
        .expect_err("truncated");
        assert!(
            err.contains("not valid JSON") && err.contains("not a string"),
            "{err}"
        );
    }

    #[test]
    fn numbers_copied_into_content_are_dropped() {
        let text = "a\nb\nc\n";
        assert_eq!(
            apply(text, json!([{"start": 2, "content": "   2\tB\n   3\tC"}])),
            Ok("a\nB\nC\nc\n".into())
        );
        assert_eq!(
            apply(text, json!([{"start": 2, "content": "2:k3x|B"}])),
            Ok("a\nB\nc\n".into())
        );
        // Not counting up by one: content, kept as written.
        assert_eq!(
            apply(text, json!([{"start": 2, "content": "1\tx\n5\ty"}])),
            Ok("a\n1\tx\n5\ty\nc\n".into())
        );
    }

    #[test]
    fn repeated_boundary_lines_are_dropped() {
        let text = "def f():\n    one = 1\n    two = 2\n    return one\n";
        // Replacing 2-3 but repeating line 1 above and line 4 below.
        let content = "def f():\n    one = 10\n    two = 20\n    return one";
        assert_eq!(
            apply(text, json!([{"start": 2, "end": 3, "content": content}])),
            Ok("def f():\n    one = 10\n    two = 20\n    return one\n".into())
        );
        // An insert that starts with the line it goes after.
        assert_eq!(
            apply(
                text,
                json!([{"after": 2, "content": "    one = 1\n    half = 0.5"}])
            ),
            Ok("def f():\n    one = 1\n    half = 0.5\n    two = 2\n    return one\n".into())
        );
        // A closing brace is not distinctive: a body that ends in `}` keeps it.
        let text = "fn f() {\n    a();\n    b();\n}\n";
        assert_eq!(
            apply(
                text,
                json!([{"start": 2, "end": 3, "content": "    if x {\n        a();\n    }"}])
            ),
            Ok("fn f() {\n    if x {\n        a();\n    }\n}\n".into())
        );
    }

    #[tokio::test]
    async fn numbers_from_an_earlier_read_follow_the_lines() {
        let text = "a\nb\nc\nd\ne\nf\n";
        let (_dir, c, first) = setup(text);
        // An edit at the top shifts everything below by one.
        let out = edit(
            &c,
            json!({"path": "a.py", "tag": first, "edits": [{"after": 0, "content": "top"}]}),
        )
        .await;
        assert!(out.ok, "{}", out.model);
        // Numbers from the first read, sent with its tag: `e` is line 5 there.
        let out = edit(
            &c,
            json!({"path": "a.py", "tag": first, "edits": [{"start": 5, "content": "E"}]}),
        )
        .await;
        assert!(out.ok, "{}", out.model);
        assert_eq!(on_disk(&c), "top\na\nb\nc\nd\nE\nf\n");
        // Without a tag, a number the last result gave ("now line 6") means that version,
        // and one it didn't means the read's line.
        let out = edit(
            &c,
            json!({"path": "a.py", "edits": [{"start": 6, "content": "EE"}]}),
        )
        .await;
        assert!(out.model.contains("(now line 6)"), "{}", out.model);
        assert_eq!(on_disk(&c), "top\na\nb\nc\nd\nEE\nf\n");
        let out = edit(
            &c,
            json!({"path": "a.py", "edits": [{"start": 2, "content": "B"}]}),
        )
        .await;
        assert!(out.ok, "{}", out.model);
        assert_eq!(on_disk(&c), "top\na\nB\nc\nd\nEE\nf\n");
    }

    #[tokio::test]
    async fn many_edits_keep_the_read_they_are_numbered_from() {
        let text = (1..=40).map(|i| format!("line {i}\n")).collect::<String>();
        let (_dir, c, tag) = setup(&text);
        // More edits than versions kept, each numbered from the one read.
        for n in (2..=30).step_by(3) {
            let args = json!({"path": "a.py", "tag": tag, "edits": [{"after": n, "content": format!("after {n}")}]});
            let out = edit(&c, args).await;
            assert!(out.ok, "{n}: {}", out.model);
        }
        let now = on_disk(&c);
        assert!(now.contains("line 29\nafter 29\nline 30\n"), "{now}");
    }

    #[tokio::test]
    async fn without_a_tag_numbers_outside_the_last_result_mean_the_read() {
        let text = (1..=40).map(|i| format!("line {i}\n")).collect::<String>();
        let (_dir, c, _) = setup(&text);
        let out = edit(
            &c,
            json!({"path": "a.py", "edits": [{"after": 2, "content": "new"}]}),
        )
        .await;
        assert!(out.ok, "{}", out.model);
        // Line 30 was not shown after the edit, so it is the read's line 30.
        let out = edit(
            &c,
            json!({"path": "a.py", "edits": [{"start": 30, "content": "thirty"}]}),
        )
        .await;
        assert!(out.ok, "{}", out.model);
        let now = on_disk(&c);
        assert!(now.contains("line 29\nthirty\nline 31\n"), "{now}");
    }

    #[tokio::test]
    async fn lines_changed_since_are_refused_with_the_current_view() {
        let (_dir, c, first) = setup("a\nb\nc\n");
        let out = edit(
            &c,
            json!({"path": "a.py", "tag": first, "edits": [{"start": 2, "content": "B"}]}),
        )
        .await;
        assert!(out.ok, "{}", out.model);
        // Line 2 changed since the first read: refused, showing it as it is now.
        let out = edit(
            &c,
            json!({"path": "a.py", "tag": first, "edits": [{"start": 2, "content": "X"}]}),
        )
        .await;
        assert!(!out.ok, "{}", out.model);
        assert!(
            out.model.contains("changed since") && out.model.contains("   2\tB"),
            "{}",
            out.model
        );
        assert_eq!(on_disk(&c), "a\nB\nc\n");
    }

    #[tokio::test]
    async fn a_file_changed_outside_is_refused_without_a_tag() {
        let (_dir, c, _) = setup("a\nb\nc\n");
        std::fs::write(c.root.join("a.py"), "z\na\nb\nc\n").expect("write");
        let out = edit(
            &c,
            json!({"path": "a.py", "edits": [{"start": 2, "content": "B"}]}),
        )
        .await;
        assert!(
            !out.ok && out.model.contains("changed since you last read"),
            "{}",
            out.model
        );
        // The refusal showed the current lines, so the retry goes through.
        let out = edit(
            &c,
            json!({"path": "a.py", "edits": [{"start": 3, "content": "B"}]}),
        )
        .await;
        assert!(out.ok, "{}", out.model);
        assert_eq!(on_disk(&c), "z\na\nB\nc\n");
    }

    #[tokio::test]
    async fn the_result_is_one_line_with_the_new_numbers_and_tag() {
        let text = (1..=30).map(|i| format!("l{i}\n")).collect::<String>();
        let (_dir, c, tag) = setup(&text);
        let out = edit(
            &c,
            json!({"path": "a.py", "tag": tag, "edits": [
                {"start": 5, "content": "five\nfive_b"},
                {"start": 25, "end": 26, "content": ""},
                {"after": 0, "content": "top"},
            ]}),
        )
        .await;
        assert!(out.ok, "{}", out.model);
        let tag = file_tag(&on_disk(&c));
        assert_eq!(
            out.model,
            format!(
                "Edited a.py, now [a.py#{tag}]: replaced line 5 with 2 lines (now lines 6-7); \
                 deleted lines 25-26; inserted 1 line at the top (now line 1)."
            )
        );
    }

    // glm-5.3-flash on partial-shipment (2026-10-21, hashline4).
    #[tokio::test]
    async fn shapes_from_the_fourth_pass() {
        let (_dir, c, tag) = setup("a\nb\nc\n");
        // `path` and `tag` inside the edit.
        let out = edit(
            &c,
            json!({"edits": [{"after": 1, "content": "a2", "path": "a.py", "tag": tag}]}),
        )
        .await;
        assert!(out.ok, "{}", out.model);
        // An empty insert is a blank line.
        let out = edit(
            &c,
            json!({"path": "a.py", "tag": tag, "edits": [{"action": "insert", "after": 2, "content": ""}]}),
        )
        .await;
        assert!(out.ok, "{}", out.model);
        assert_eq!(on_disk(&c), "a\na2\nb\n\nc\n");
        // Numbers past the end of the tagged read, worked out after the model's own
        // inserts: read against the file as it is now.
        let out = edit(
            &c,
            json!({"path": "a.py", "tag": tag, "edits": [{"start": 5, "content": "C"}]}),
        )
        .await;
        assert!(
            out.ok && out.model.contains("as it is now"),
            "{}",
            out.model
        );
        assert_eq!(on_disk(&c), "a\na2\nb\n\nC\n");
    }

    // glm-5.3-flash's off-by-one ranges left SyntaxErrors behind (2026-10-21, hashline5).
    // glm-5.3-flash's off-by-one ranges left SyntaxErrors behind (2026-10-21, hashline5):
    // a range that stops short of a closer it orphaned is mended by taking it in.
    #[cfg(feature = "lang-common")]
    #[test]
    fn a_range_short_of_a_closer_is_mended() {
        let text = "def f(x):\n    total = add(\n        x,\n    )\n    return total\n";
        // Replacing lines 2-3 orphans the `)` on line 4: the mend takes it too.
        let content = "    total = add(x)";
        let (_dir, c, tag) = setup(text);
        let mended = json!({"path": "a.py", "tag": tag, "edits": [
            {"start": 2, "end": 3, "content": content}
        ]});
        let p = plan(&mended, &c).expect("mended");
        assert_eq!(p.after, "def f(x):\n    total = add(x)\n    return total\n");
        assert!(
            p.notes.iter().any(|n| n.contains("line 4")),
            "{:?}",
            p.notes
        );
        // The right range goes through unchanged, and a file broken before doesn't
        // block edits.
        assert_eq!(
            apply(text, json!([{"start": 2, "end": 4, "content": content}])),
            Ok("def f(x):\n    total = add(x)\n    return total\n".into())
        );
        assert!(
            apply(
                "def f(:\n    pass\nx = 1\n",
                json!([{"start": 3, "content": "x = (2"}])
            )
            .is_ok()
        );
    }

    #[cfg(feature = "lang-common")]
    #[test]
    fn an_edit_that_breaks_the_syntax_is_refused_unless_forced() {
        let text = "def f(x):\n    total = add(\n        x,\n    )\n    return total\n";
        // An unmatched `(` nothing below can close: refused, `force` takes it.
        let err = apply(text, json!([{"start": 5, "content": "    return total("}]))
            .expect_err("refused");
        assert!(
            err.contains("syntax error at line") && err.contains("force"),
            "{err}"
        );
        let (_dir, c, tag) = setup(text);
        let forced = json!({"path": "a.py", "tag": tag, "force": true, "edits": [
            {"start": 5, "content": "    return total("}
        ]});
        assert!(plan(&forced, &c).is_ok());
    }

    // oh-my-pi's `PUT N*:`: name a line, get its whole definition, no `end` to count.
    #[cfg(feature = "lang-common")]
    #[test]
    fn block_edits_take_the_whole_construct() {
        let text = "import os\n\n\ndef one():\n    a = 1\n\n\n@wrapper\ndef two(x):\n    b = 2\n    return b\n\n\nclass Three:\n    def m(self):\n        return 3\n";
        // Replace the function around line 5.
        assert_eq!(
            apply(text, json!([{"block": 5, "content": "def one():\n    return 0"}])),
            Ok("import os\n\n\ndef one():\n    return 0\n\n\n@wrapper\ndef two(x):\n    b = 2\n    return b\n\n\nclass Three:\n    def m(self):\n        return 3\n".into())
        );
        // The decorator above `two` comes with it.
        assert_eq!(
            apply(
                text,
                json!([{"block": 9, "content": "@new\ndef two(x):\n    return 20"}])
            ),
            Ok("import os\n\n\ndef one():\n    a = 1\n\n\n@new\ndef two(x):\n    return 20\n\n\nclass Three:\n    def m(self):\n        return 3\n".into())
        );
        // The innermost construct: the method, not the class.
        assert_eq!(
            apply(text, json!([{"action": "delete", "block": 15}])),
            Ok("import os\n\n\ndef one():\n    a = 1\n\n\n@wrapper\ndef two(x):\n    b = 2\n    return b\n\n\nclass Three:\n".into())
        );
        // Insert after the construct, at its depth: sibling position.
        assert_eq!(
            apply(text, json!([{"after_block": 10, "content": "\ndef extra():\n    pass"}])),
            Ok("import os\n\n\ndef one():\n    a = 1\n\n\n@wrapper\ndef two(x):\n    b = 2\n    return b\n\ndef extra():\n    pass\n\n\nclass Three:\n    def m(self):\n        return 3\n".into())
        );
        // `end: "block"` and `"replace_block"` spell the same thing.
        assert_eq!(
            apply(
                text,
                json!([{"start": 5, "end": "block", "content": "def one():\n    return 0"}])
            ),
            Ok("import os\n\n\ndef one():\n    return 0\n\n\n@wrapper\ndef two(x):\n    b = 2\n    return b\n\n\nclass Three:\n    def m(self):\n        return 3\n".into())
        );
        assert_eq!(
            apply(
                text,
                json!([{"action": "replace_block", "block": 5, "content": "def one():\n    return 0"}])
            ),
            Ok("import os\n\n\ndef one():\n    return 0\n\n\n@wrapper\ndef two(x):\n    b = 2\n    return b\n\n\nclass Three:\n    def m(self):\n        return 3\n".into())
        );
        // A line between definitions has no construct around it.
        let err = apply(text, json!([{"block": 2, "content": "x"}])).expect_err("refused");
        assert!(
            err.contains("not inside a definition") && err.contains("line 4"),
            "{err}"
        );
    }

    // Without a grammar there is no outline, so `block` cannot resolve and says so.
    #[test]
    fn block_edits_without_an_outline_say_what_to_do() {
        let text = "a\nb\nc\n";
        let err = apply(text, json!([{"block": 2, "content": "x"}])).expect_err("refused");
        assert!(
            err.contains("cannot resolve") && err.contains("`start` and `end`"),
            "{err}"
        );
    }

    // Editing lines no output showed mangles files; the refusal shows them and counts
    // as reading them, so a straight retry works (oh-my-pi's unseen-lines rule).
    #[tokio::test]
    async fn edits_to_lines_never_shown_are_refused_then_retried() {
        let text = (1..=8).map(|i| format!("line {i}\n")).collect::<String>();
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.py"), &text).expect("write");
        let c = ctx(dir.path());
        // No read at all yet.
        let out = edit(
            &c,
            json!({"path": "a.py", "edits": [{"start": 3, "content": "three"}]}),
        )
        .await;
        assert!(
            !out.ok && out.model.contains("has not been read"),
            "{}",
            out.model
        );
        assert_eq!(on_disk(&c), text);
        // The refusal showed the lines, so the same edit now goes through.
        let out = edit(
            &c,
            json!({"path": "a.py", "edits": [{"start": 3, "content": "three"}]}),
        )
        .await;
        assert!(out.ok, "{}", out.model);

        // A read that showed only part of the file: the rest is unseen.
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.py"), &text).expect("write");
        let c = ctx(dir.path());
        let tag = file_tag(&text);
        c.reads.saw(&dir.path().join("a.py"), &tag, &text, (1, 2));
        let out = edit(
            &c,
            json!({"path": "a.py", "tag": tag, "edits": [{"start": 6, "content": "six"}]}),
        )
        .await;
        assert!(
            !out.ok
                && out.model.contains("did not show line 6")
                && out.model.contains("   6\tline 6"),
            "{}",
            out.model
        );
        let out = edit(
            &c,
            json!({"path": "a.py", "tag": tag, "edits": [{"start": 6, "content": "six"}]}),
        )
        .await;
        assert!(out.ok, "{}", out.model);
        let now = on_disk(&c);
        assert!(now.contains("line 5\nsix\nline 7"), "{now}");
        // `force` skips the check.
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.py"), &text).expect("write");
        let c = ctx(dir.path());
        let tag = file_tag(&text);
        c.reads.saw(&dir.path().join("a.py"), &tag, &text, (1, 2));
        let out = edit(
            &c,
            json!({"path": "a.py", "tag": tag, "force": true, "edits": [
                {"start": 6, "content": "six"}
            ]}),
        )
        .await;
        assert!(out.ok, "{}", out.model);
    }

    #[tokio::test]
    async fn read_and_grep_show_the_tag_an_edit_accepts() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.py"), "x = 1\ntotal = 2\n").expect("write");
        let c = ctx(dir.path());
        let tag = file_tag("x = 1\ntotal = 2\n");
        let out = crate::search::Grep
            .run(json!({"pattern": "total"}), &c)
            .await;
        assert!(
            out.model
                .contains(&format!("[a.py#{tag}]\na.py:2: total = 2")),
            "{}",
            out.model
        );
        let out = crate::read::Read.run(json!({"path": "a.py"}), &c).await;
        assert!(
            out.model
                .starts_with(&format!("[a.py#{tag}]\n   1\tx = 1\n")),
            "{}",
            out.model
        );
        let out = edit(
            &c,
            json!({"path": "a.py", "tag": tag, "edits": [{"start": 2, "content": "total = 3"}]}),
        )
        .await;
        assert!(out.ok, "{}", out.model);
        assert_eq!(on_disk(&c), "x = 1\ntotal = 3\n");
    }

    #[test]
    fn crlf_and_a_missing_final_newline_are_kept() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.py"), "a\r\nb\r\n").expect("write");
        let c = ctx(dir.path());
        // A read shows the file LF-normalized; the edit still writes CRLF back.
        c.reads.saw(
            &dir.path().join("a.py"),
            &file_tag("a\nb\n"),
            "a\nb\n",
            (1, 2),
        );
        let p = plan(
            &json!({"path": "a.py", "edits": [{"start": 1, "content": "A"}]}),
            &c,
        )
        .map_err(|r| r.message)
        .expect("plan");
        assert_eq!(p.file.encode(&p.after), "A\r\nb\r\n");
        assert_eq!(
            apply("a\nb", json!([{"start": 2, "content": "B"}])),
            Ok("a\nB".into())
        );
    }
}
