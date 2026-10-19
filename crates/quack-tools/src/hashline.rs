//! `edit_hashline` (internal `edit.hashline`): hash-anchored edits. With
//! `[tools] edit_format = "hashline"`, `read` tags each line as `<line>:<hash>|<text>`, and
//! edits name lines by those tags instead of retyping the old text. A tag whose hash no
//! longer matches is looked for nearby (lines move after earlier edits); if it is gone,
//! the edit is refused with a fresh tagged view of that area, so retries converge.

use futures::future::BoxFuture;
use quack_protocol::ApprovalRequest;
use serde_json::{Value, json};

use crate::paths;
use crate::registry::{Subject, Tool, ToolCtx, ToolKind, ToolOutput, arg_str, opt_str};
use crate::text::{TextFile, diff_stats, io_msg, unified_diff};

const PATH: &[&str] = &["path", "file_path", "filename", "file"];
/// How far a tag's line may have moved and still be found.
const DRIFT: usize = 50;
/// Lines of fresh context shown around an edit or a stale tag.
const CONTEXT: usize = 3;

/// A short hash of the line's content (trailing whitespace ignored): 3 base-32 characters.
pub(crate) fn tag(line: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in line.trim_end().bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    const ALPHABET: &[u8; 32] = b"abcdefghijkmnpqrstuvwxyz23456789";
    (0..3)
        .map(|i| char::from(ALPHABET[((h >> (i * 5)) & 31) as usize]))
        .collect()
}

/// `n:hash|text` for lines `from..to` (0-based, exclusive end).
pub(crate) fn tagged(lines: &[&str], from: usize, to: usize) -> String {
    (from..to.min(lines.len()))
        .map(|i| format!("{}:{}|{}", i + 1, tag(lines[i]), lines[i]))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The line a tag like `12:k3x` (or `12:k3x|text`, or just `k3x`) names, as a 0-based index.
fn resolve(lines: &[&str], anchor: &str) -> Result<usize, String> {
    let anchor = anchor.split('|').next().unwrap_or_default().trim();
    let (hint, hash) = match anchor.split_once(':') {
        Some((n, h)) => (n.trim().parse::<usize>().ok(), h.trim()),
        None => (None, anchor),
    };
    if hash.is_empty() {
        return Err(format!("`{anchor}` is not a line tag like `12:k3x`"));
    }
    let matches = |i: usize| lines.get(i).is_some_and(|l| tag(l) == hash);
    if let Some(n) = hint.filter(|n| *n >= 1) {
        let at = n - 1;
        if matches(at) {
            return Ok(at);
        }
        // Nearest line with this hash, within the drift window.
        for d in 1..=DRIFT {
            for i in [at.checked_sub(d), at.checked_add(d)].into_iter().flatten() {
                if matches(i) {
                    return Ok(i);
                }
            }
        }
        let from = at.saturating_sub(CONTEXT);
        return Err(format!(
            "line tag `{anchor}` no longer matches the file. Current lines there:\n{}",
            tagged(lines, from, at + CONTEXT + 1)
        ));
    }
    let all: Vec<usize> = (0..lines.len()).filter(|i| matches(*i)).collect();
    match all.as_slice() {
        [one] => Ok(*one),
        [] => Err(format!("no line has the tag `{hash}`; read the file again")),
        _ => Err(format!(
            "the tag `{hash}` matches {} lines; include the line number, e.g. `{}:{hash}`",
            all.len(),
            all[0] + 1
        )),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Op {
    /// Replace lines `start..=end` with `text` (empty text deletes them).
    Replace {
        start: usize,
        end: usize,
        text: String,
    },
    /// Insert `text` after line `after` (`None`: at the top).
    Insert { after: Option<usize>, text: String },
}

fn content(v: &Value) -> String {
    let text = ["content", "text", "new", "lines"]
        .iter()
        .find_map(|k| match v.get(*k) {
            Some(Value::String(s)) => Some(s.clone()),
            Some(Value::Array(a)) => Some(
                a.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            _ => None,
        })
        .unwrap_or_default();
    text.replace("\r\n", "\n")
}

fn parse_op(v: &Value, lines: &[&str]) -> Result<Op, String> {
    let action = opt_str(v, &["action", "op", "operation", "type"]).unwrap_or("replace");
    let get = |keys: &[&str]| opt_str(v, keys).map(|a| resolve(lines, a)).transpose();
    match action {
        "replace" | "delete" => {
            let start = get(&["start", "from", "line", "anchor"])?
                .ok_or("`start` (a line tag) is required")?;
            let end = get(&["end", "to"])?.unwrap_or(start);
            if end < start {
                return Err("`end` is before `start`".into());
            }
            let text = if action == "delete" {
                String::new()
            } else {
                content(v)
            };
            Ok(Op::Replace { start, end, text })
        }
        "insert_after" | "insert" | "append" => {
            let after = match opt_str(v, &["after", "anchor", "start", "line"]) {
                Some(a) if a.trim() == "0" || a.trim().eq_ignore_ascii_case("top") => None,
                Some(a) => Some(resolve(lines, a)?),
                None => return Err("`after` (a line tag, or 0 for the top) is required".into()),
            };
            Ok(Op::Insert {
                after,
                text: content(v),
            })
        }
        other => Err(format!(
            "unknown action `{other}`; use replace, insert_after or delete"
        )),
    }
}

/// The span an op touches, for overlap checks: `(first line, last line + 1)`.
fn span(op: &Op) -> (usize, usize) {
    match op {
        Op::Replace { start, end, .. } => (*start, end + 1),
        Op::Insert { after, .. } => {
            let at = after.map_or(0, |a| a + 1);
            (at, at)
        }
    }
}

struct Plan {
    path: std::path::PathBuf,
    shown: String,
    file: TextFile,
    after: String,
    /// 0-based range of changed lines in the new text, for the fresh view.
    touched: (usize, usize),
}

fn plan(args: &Value, ctx: &ToolCtx) -> Result<Plan, String> {
    let path = paths::resolve(&ctx.root, arg_str(args, PATH)?);
    let shown = paths::display(&ctx.root, &path);
    if !path.exists() {
        return Err(format!("{shown} does not exist; use `write` to create it"));
    }
    let file = TextFile::load(&path)?;
    let lines: Vec<&str> = file.text.split('\n').collect();
    let raw: Vec<Value> = match args.get("edits") {
        Some(Value::Array(a)) => a.clone(),
        Some(Value::String(s)) => serde_json::from_str::<Vec<Value>>(s)
            .map_err(|_| "`edits` must be an array".to_owned())?,
        _ => vec![args.clone()],
    };
    if raw.is_empty() {
        return Err("no edits given".into());
    }
    let mut ops: Vec<Op> = raw
        .iter()
        .map(|v| parse_op(v, &lines))
        .collect::<Result<_, _>>()?;
    ops.sort_by_key(|o| std::cmp::Reverse(span(o)));
    for w in ops.windows(2) {
        // Sorted from the bottom: the later one must end before the earlier one starts.
        let (hi, lo) = (span(&w[0]), span(&w[1]));
        if lo.1 > hi.0 || (lo == hi && lo.0 == lo.1) {
            return Err("two edits touch the same lines; combine them into one".into());
        }
    }
    let mut out: Vec<String> = lines.iter().map(|l| (*l).to_owned()).collect();
    for op in &ops {
        let (from, to, text) = match op {
            Op::Replace { start, end, text } => (*start, end + 1, text),
            Op::Insert { after, text } => {
                let at = after.map_or(0, |a| a + 1);
                (at, at, text)
            }
        };
        let new = if text.is_empty() {
            Vec::new()
        } else {
            text.trim_end_matches('\n')
                .split('\n')
                .map(str::to_owned)
                .collect()
        };
        out.splice(from..to, new);
    }
    // The changed lines: what differs once the common start and end are set aside.
    let same_start = lines
        .iter()
        .zip(&out)
        .take_while(|(a, b)| **a == b.as_str())
        .count();
    let same_end = lines
        .iter()
        .rev()
        .zip(out.iter().rev())
        .take(lines.len().min(out.len()) - same_start)
        .take_while(|(a, b)| **a == b.as_str())
        .count();
    let touched = (same_start, out.len() - same_end);
    let after = out.join("\n");
    if after == file.text {
        return Err("the edit changes nothing".into());
    }
    Ok(Plan {
        path,
        shown,
        file,
        after,
        touched,
    })
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
        "Edit a file by line tags. `read` shows each line as `<line>:<tag>|<text>`; name \
         lines by their `<line>:<tag>` (e.g. `12:k3x`) instead of copying old text. Actions: \
         `replace` lines `start` to `end` (inclusive; `end` defaults to `start`) with \
         `content`; `insert_after` the line `after` (use 0 for the top of the file); \
         `delete` lines `start` to `end`. Several non-overlapping edits can go in `edits`, \
         all using tags from the same read. The result shows the changed area with fresh \
         tags; use those for further edits."
            .into()
    }

    fn schema(&self) -> Value {
        let edit = json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["replace", "insert_after", "delete"] },
                "start": { "type": "string", "description": "First line's tag, e.g. 12:k3x" },
                "end": { "type": "string", "description": "Last line's tag (replace, delete)" },
                "after": { "type": "string", "description": "Tag of the line to insert after (insert_after), or 0" },
                "content": { "type": "string", "description": "New lines (replace, insert_after)" }
            },
            "required": ["action"]
        });
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path, relative to the workspace root or absolute" },
                "edits": { "type": "array", "items": edit }
            },
            "required": ["path", "edits"]
        })
    }

    fn subject(&self, args: &Value, ctx: &ToolCtx) -> Result<Subject, String> {
        Ok(Subject::Edit(paths::resolve(
            &ctx.root,
            arg_str(args, PATH)?,
        )))
    }

    fn summary(&self, args: &Value, ctx: &ToolCtx) -> String {
        let path = arg_str(args, PATH)
            .map(|p| paths::display(&ctx.root, &paths::resolve(&ctx.root, p)))
            .unwrap_or_default();
        format!("edit {path}")
    }

    fn preview(&self, args: &Value, ctx: &ToolCtx) -> Result<ApprovalRequest, String> {
        let p = plan(args, ctx)?;
        Ok(ApprovalRequest::Edit {
            diff: unified_diff(&p.shown, &p.file.text, &p.after),
            path: p.shown,
        })
    }

    fn run<'a>(&'a self, args: Value, ctx: &'a ToolCtx) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            let run = || -> Result<ToolOutput, String> {
                let p = plan(&args, ctx)?;
                std::fs::write(&p.path, p.file.encode(&p.after))
                    .map_err(|e| io_msg(&p.path, &e))?;
                ctx.reads.record(&p.path);
                let (add, del) = diff_stats(&p.file.text, &p.after);
                let lines: Vec<&str> = p.after.split('\n').collect();
                let from = p.touched.0.saturating_sub(CONTEXT);
                let to = p.touched.1 + CONTEXT;
                Ok(ToolOutput::ok(
                    format!(
                        "Edited {}. Lines around the change, with fresh tags:\n{}",
                        p.shown,
                        tagged(&lines, from, to)
                    ),
                    format!("+{add} -{del}"),
                )
                .with_detail(unified_diff(&p.shown, &p.file.text, &p.after)))
            };
            run().unwrap_or_else(ToolOutput::error)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(root: &std::path::Path) -> ToolCtx {
        ToolCtx {
            root: root.to_path_buf(),
            shell: quack_shell::ShellKind::detect("sh", None),
            default_timeout: std::time::Duration::from_secs(5),
            max_timeout: std::time::Duration::from_secs(5),
            cancel: Default::default(),
            on_output: None,
            reads: Default::default(),
            memory: None,
            skills: Default::default(),
            lsp: None,
            sandbox: Default::default(),
            plan: Default::default(),
            edit_format: quack_config::settings::EditFormat::Hashline,
            outline_above: 0,
        }
    }

    fn t(lines: &[&str], n: usize) -> String {
        format!("{n}:{}", tag(lines[n - 1]))
    }

    #[tokio::test]
    async fn replace_insert_delete_in_one_call() {
        let dir = tempfile::tempdir().expect("tempdir");
        let f = dir.path().join("a.rs");
        let text = "fn a() {}\nfn b() {}\nfn c() {}\nfn d() {}\n";
        std::fs::write(&f, text).expect("write");
        let lines: Vec<&str> = text.split('\n').collect();
        let args = json!({"path": "a.rs", "edits": [
            {"action": "replace", "start": t(&lines, 2), "content": "fn b2() {}\nfn b3() {}"},
            {"action": "insert_after", "after": "0", "content": "// top"},
            {"action": "delete", "start": t(&lines, 4)},
        ]});
        let out = Hashline.run(args, &ctx(dir.path())).await;
        assert!(out.ok, "{}", out.model);
        assert_eq!(
            std::fs::read_to_string(&f).expect("read"),
            "// top\nfn a() {}\nfn b2() {}\nfn b3() {}\nfn c() {}\n"
        );
        assert!(
            out.model
                .contains(&format!("3:{}|fn b2() {{}}", tag("fn b2() {}"))),
            "{}",
            out.model
        );
    }

    #[test]
    fn tags_survive_drift_and_stale_tags_are_refused() {
        let lines = ["x", "y", "fn target() {}", "z"];
        // The model's tag says line 1, but the line is now at 3.
        let anchor = format!("1:{}", tag("fn target() {}"));
        assert_eq!(resolve(&lines, &anchor), Ok(2));
        let err = resolve(&lines, "2:zzz").expect_err("stale");
        assert!(
            err.contains("no longer matches") && err.contains(&format!("2:{}|y", tag("y"))),
            "{err}"
        );
        // A bare hash works when it is unique.
        assert_eq!(resolve(&lines, &tag("z")), Ok(3));
        assert_eq!(tag("a  "), tag("a"));
    }

    #[test]
    fn overlapping_edits_are_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.txt"), "1\n2\n3\n").expect("write");
        let lines = ["1", "2", "3"];
        let args = json!({"path": "a.txt", "edits": [
            {"action": "replace", "start": t(&lines, 1), "end": t(&lines, 2), "content": "x"},
            {"action": "delete", "start": t(&lines, 2)},
        ]});
        assert!(plan(&args, &ctx(dir.path())).is_err());
    }
}
