//! `apply_patch` (edit format `apply_patch`): the patch format Codex-trained models write.
//!
//! ```text
//! *** Begin Patch
//! *** Add File: src/new.rs
//! +fn main() {}
//! *** Update File: src/lib.rs
//! *** Move to: src/core.rs        (optional)
//! @@ fn parse                     (optional: a line to find before the hunk)
//!  context
//! -old
//! +new
//! *** Delete File: src/old.rs
//! *** End Patch
//! ```
//!
//! Hunks are found in order, exactly first, then ignoring trailing whitespace, then
//! ignoring indentation. Every file is checked before any is written. [`Change`] and its
//! helpers are shared with `edit_blocks`.

use std::path::PathBuf;

use futures::future::BoxFuture;
use quack_config::settings::EditFormat;
use quack_protocol::ApprovalRequest;
use serde_json::{Value, json};

use crate::paths;
use crate::registry::{Subject, Tool, ToolCtx, ToolKind, ToolOutput, arg_str};
use crate::text::{TextFile, diff_stats, io_msg, unified_diff};

const PATCH: &[&str] = &["patch", "input", "diff", "content"];

#[derive(Debug)]
pub struct ApplyPatch;

/// One file's planned change.
pub(crate) struct Change {
    pub path: PathBuf,
    pub shown: String,
    /// The file as it is; `None` when it is created.
    pub before: Option<TextFile>,
    /// The new text; `None` deletes the file.
    pub after: Option<String>,
}

impl Change {
    fn before_text(&self) -> &str {
        self.before.as_ref().map_or("", |f| f.text.as_str())
    }
}

/// Load an existing file for changing, refusing one that changed since it was read.
pub(crate) fn load(ctx: &ToolCtx, path: &std::path::Path, shown: &str) -> Result<TextFile, String> {
    if !path.exists() {
        return Err(format!("{shown} does not exist"));
    }
    let file = TextFile::load(path)?;
    if ctx.reads.is_stale(path) {
        return Err(format!(
            "{shown} changed on disk since you last read it; read it again before editing"
        ));
    }
    Ok(file)
}

pub(crate) fn diff(changes: &[Change]) -> String {
    changes
        .iter()
        .map(|c| unified_diff(&c.shown, c.before_text(), c.after.as_deref().unwrap_or("")))
        .collect()
}

pub(crate) fn preview(changes: &[Change]) -> ApprovalRequest {
    ApprovalRequest::Edit {
        path: match changes {
            [one] => one.shown.clone(),
            many => many
                .iter()
                .map(|c| c.shown.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        },
        diff: diff(changes),
    }
}

/// Write every change; reports what was done.
pub(crate) fn write(changes: &[Change], ctx: &ToolCtx) -> Result<ToolOutput, String> {
    let mut lines = Vec::new();
    let (mut add, mut del) = (0, 0);
    for c in changes {
        match &c.after {
            Some(text) => {
                if let Some(dir) = c.path.parent() {
                    std::fs::create_dir_all(dir).map_err(|e| io_msg(dir, &e))?;
                }
                let bytes = match &c.before {
                    Some(f) => f.encode(text),
                    None => text.clone(),
                };
                std::fs::write(&c.path, bytes).map_err(|e| io_msg(&c.path, &e))?;
                ctx.reads.record(&c.path);
                lines.push(match c.before {
                    Some(_) => format!("Edited {}", c.shown),
                    None => format!("Created {}", c.shown),
                });
                if let Some(before) = &c.before
                    && let Some(line) =
                        crate::outline::new_syntax_error(&c.path, &before.text, text)
                {
                    lines.push(format!(
                        "\nWarning: Tree-sitter detected a syntax error in {} at line {line} after this change (the file previously parsed cleanly). Check brackets, indentation or delimiters.",
                        c.shown
                    ));
                }
            }
            None => {
                std::fs::remove_file(&c.path).map_err(|e| io_msg(&c.path, &e))?;
                lines.push(format!("Deleted {}", c.shown));
            }
        }
        let (a, d) = diff_stats(c.before_text(), c.after.as_deref().unwrap_or(""));
        add += a;
        del += d;
    }
    Ok(ToolOutput::ok(lines.join("\n") + ".", format!("+{add} -{del}")).with_detail(diff(changes)))
}

#[derive(Debug, PartialEq)]
enum Op {
    Add {
        path: String,
        text: String,
    },
    Delete {
        path: String,
    },
    Update {
        path: String,
        to: Option<String>,
        hunks: Vec<Hunk>,
    },
}

#[derive(Debug, Default, PartialEq)]
struct Hunk {
    /// `@@ <line>`: a line to find first.
    anchor: Option<String>,
    old: Vec<String>,
    new: Vec<String>,
    /// `*** End of File`: the hunk ends the file.
    eof: bool,
}

fn parse(patch: &str) -> Result<Vec<Op>, String> {
    let text = patch.replace("\r\n", "\n");
    let mut lines = text.lines().peekable();
    // Models sometimes wrap the patch in a fence or a heredoc; skip up to the start.
    for l in lines.by_ref() {
        if l.trim() == "*** Begin Patch" {
            break;
        }
    }
    let mut ops = Vec::new();
    let mut ended = false;
    while let Some(line) = lines.next() {
        let line = line.trim_end();
        if line == "*** End Patch" {
            ended = true;
            break;
        }
        if line.is_empty() {
            continue;
        }
        if let Some(path) = line.strip_prefix("*** Add File: ") {
            let mut body = Vec::new();
            while let Some(l) = lines.next_if(|l| !l.starts_with("*** ")) {
                body.push(l.strip_prefix('+').unwrap_or(l));
            }
            let mut text = body.join("\n");
            if !body.is_empty() {
                text.push('\n');
            }
            ops.push(Op::Add {
                path: path.trim().to_owned(),
                text,
            });
        } else if let Some(path) = line.strip_prefix("*** Delete File: ") {
            ops.push(Op::Delete {
                path: path.trim().to_owned(),
            });
        } else if let Some(path) = line.strip_prefix("*** Update File: ") {
            let to = lines
                .next_if(|l| l.starts_with("*** Move to: "))
                .map(|l| l["*** Move to: ".len()..].trim().to_owned());
            let mut hunks: Vec<Hunk> = Vec::new();
            let mut cur = Hunk::default();
            let flush = |cur: &mut Hunk, hunks: &mut Vec<Hunk>| {
                if !cur.old.is_empty() || !cur.new.is_empty() {
                    hunks.push(std::mem::take(cur));
                }
            };
            while let Some(l) =
                lines.next_if(|l| !l.starts_with("*** ") || l.trim_end() == "*** End of File")
            {
                if l.trim_end() == "*** End of File" {
                    cur.eof = true;
                    flush(&mut cur, &mut hunks);
                } else if let Some(anchor) = l.strip_prefix("@@") {
                    flush(&mut cur, &mut hunks);
                    let a = anchor.trim();
                    cur.anchor = (!a.is_empty()).then(|| a.to_owned());
                } else if let Some(t) = l.strip_prefix('-') {
                    cur.old.push(t.to_owned());
                } else if let Some(t) = l.strip_prefix('+') {
                    cur.new.push(t.to_owned());
                } else {
                    // Context; a bare empty line is an empty context line.
                    let t = l.strip_prefix(' ').unwrap_or(l);
                    cur.old.push(t.to_owned());
                    cur.new.push(t.to_owned());
                }
            }
            flush(&mut cur, &mut hunks);
            if hunks.is_empty() && to.is_none() {
                return Err(format!("`*** Update File: {path}` has no changes"));
            }
            ops.push(Op::Update {
                path: path.trim().to_owned(),
                to,
                hunks,
            });
        } else {
            return Err(format!(
                "unexpected line `{line}`; each file starts with `*** Add File: `, \
                 `*** Update File: ` or `*** Delete File: `"
            ));
        }
    }
    if ops.is_empty() {
        return Err(
            "no file operations found; the patch starts with `*** Begin Patch` and \
                    lists files with `*** Update File: <path>` and similar"
                .into(),
        );
    }
    if !ended {
        tracing::debug!("patch without `*** End Patch`; applied anyway");
    }
    Ok(ops)
}

/// Find `old` in `lines` at or after `from`: exactly, then ignoring trailing whitespace,
/// then ignoring indentation. Returns the start index.
fn seek(lines: &[String], old: &[String], from: usize, eof: bool) -> Option<usize> {
    if old.is_empty() {
        return Some(if eof { lines.len() } else { from });
    }
    if old.len() > lines.len() {
        return None;
    }
    let eqs: [fn(&str, &str) -> bool; 3] = [
        |a, b| a == b,
        |a, b| a.trim_end() == b.trim_end(),
        |a, b| a.trim() == b.trim(),
    ];
    for eq in eqs {
        let fits = |i: usize| old.iter().enumerate().all(|(k, o)| eq(&lines[i + k], o));
        if eof {
            let i = lines.len() - old.len();
            if i >= from && fits(i) {
                return Some(i);
            }
        }
        if let Some(i) = (from..=lines.len() - old.len()).find(|&i| fits(i)) {
            return Some(i);
        }
    }
    None
}

fn apply_hunks(text: &str, hunks: &[Hunk], shown: &str) -> Result<String, String> {
    let trailing = text.ends_with('\n');
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let mut at = 0;
    for (n, h) in hunks.iter().enumerate() {
        if let Some(anchor) = &h.anchor {
            match lines[at.min(lines.len())..]
                .iter()
                .position(|l| l.trim() == anchor.trim())
            {
                Some(i) => at += i + 1,
                // The anchor is only a hint (`@@ -1,3 +1,4 @@` headers end up here too).
                None => tracing::debug!(anchor, "patch anchor not found"),
            }
        }
        let start = seek(&lines, &h.old, at, h.eof)
            .or_else(|| seek(&lines, &h.old, 0, h.eof))
            .ok_or_else(|| {
                format!(
                    "hunk {} of {shown} does not match the file: these lines were not found:\n{}\n\
                     Read the file again and copy the context lines exactly",
                    n + 1,
                    h.old.join("\n")
                )
            })?;
        let end = start + h.old.len();
        lines.splice(start..end, h.new.iter().cloned());
        at = start + h.new.len();
    }
    let mut out = lines.join("\n");
    if trailing || !out.is_empty() {
        out.push('\n');
    }
    Ok(out)
}

fn plan(args: &Value, ctx: &ToolCtx) -> Result<Vec<Change>, String> {
    let ops = parse(arg_str(args, PATCH)?)?;
    let resolve = |p: &str| {
        let path = paths::resolve(&ctx.root, p);
        let shown = paths::display(&ctx.root, &path);
        (path, shown)
    };
    let mut changes: Vec<Change> = Vec::new();
    for op in ops {
        match op {
            Op::Add { path, text } => {
                let (path, shown) = resolve(&path);
                if path.exists() {
                    return Err(format!(
                        "{shown} already exists; use `*** Update File:` to change it"
                    ));
                }
                changes.push(Change {
                    path,
                    shown,
                    before: None,
                    after: Some(text),
                });
            }
            Op::Delete { path } => {
                let (path, shown) = resolve(&path);
                let before = load(ctx, &path, &shown)?;
                changes.push(Change {
                    path,
                    shown,
                    before: Some(before),
                    after: None,
                });
            }
            Op::Update { path, to, hunks } => {
                let (path, shown) = resolve(&path);
                // A second section for the same file continues from the first.
                let (before, current) = match changes.iter().position(|c| c.path == path) {
                    Some(i) => {
                        let c = changes.remove(i);
                        let cur = c
                            .after
                            .clone()
                            .ok_or_else(|| format!("{shown} is deleted earlier in the patch"))?;
                        (c.before, cur)
                    }
                    None => {
                        let f = load(ctx, &path, &shown)?;
                        let text = f.text.clone();
                        (Some(f), text)
                    }
                };
                let after = apply_hunks(&current, &hunks, &shown)?;
                match to {
                    Some(to) => {
                        let (dest, dest_shown) = resolve(&to);
                        if dest.exists() && dest != path {
                            return Err(format!("cannot move {shown}: {dest_shown} exists"));
                        }
                        changes.push(Change {
                            path: path.clone(),
                            shown,
                            before: before.clone(),
                            after: None,
                        });
                        changes.push(Change {
                            path: dest,
                            shown: dest_shown,
                            before: None,
                            after: Some(match &before {
                                Some(f) => f.encode(&after),
                                None => after,
                            }),
                        });
                    }
                    None => changes.push(Change {
                        path,
                        shown,
                        before,
                        after: Some(after),
                    }),
                }
            }
        }
    }
    Ok(changes)
}

/// The paths a patch names, for permissions, without reading any file.
fn named(args: &Value, ctx: &ToolCtx) -> Result<Vec<PathBuf>, String> {
    let mut out = Vec::new();
    for op in parse(arg_str(args, PATCH)?)? {
        match op {
            Op::Add { path, .. } | Op::Delete { path } => out.push(path),
            Op::Update { path, to, .. } => {
                out.push(path);
                out.extend(to);
            }
        }
    }
    let mut paths: Vec<PathBuf> = out.iter().map(|p| paths::resolve(&ctx.root, p)).collect();
    paths.dedup();
    Ok(paths)
}

impl Tool for ApplyPatch {
    fn name(&self) -> &str {
        "apply_patch"
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Mutating
    }

    fn available(&self, ctx: &ToolCtx) -> bool {
        ctx.edit_format == EditFormat::ApplyPatch
    }

    fn description(&self, _ctx: &ToolCtx) -> String {
        "Change files with a patch. The patch starts with `*** Begin Patch` and ends with \
         `*** End Patch`. Between them, each file is one section:\n\
         `*** Add File: <path>` then the new lines, each starting with `+`;\n\
         `*** Delete File: <path>`;\n\
         `*** Update File: <path>` (optionally followed by `*** Move to: <path>`), then hunks. \
         A hunk may start with `@@ <a line just above it>`, then lists lines starting with ` ` \
         (context, unchanged), `-` (removed) or `+` (added). Give about 3 lines of context \
         above and below each change, copied exactly. Paths are relative to the workspace root."
            .into()
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "patch": { "type": "string", "description": "The whole patch, from `*** Begin Patch` to `*** End Patch`" }
            },
            "required": ["patch"]
        })
    }

    fn subject(&self, args: &Value, ctx: &ToolCtx) -> Result<Subject, String> {
        Ok(match named(args, ctx)?.as_slice() {
            [one] => Subject::Edit(one.clone()),
            many => Subject::Edits(many.to_vec()),
        })
    }

    fn summary(&self, args: &Value, ctx: &ToolCtx) -> String {
        match named(args, ctx).as_deref() {
            Ok([one]) => format!("patch {}", paths::display(&ctx.root, one)),
            Ok(many) => format!("patch {} files", many.len()),
            Err(_) => "patch".into(),
        }
    }

    fn preview(&self, args: &Value, ctx: &ToolCtx) -> Result<ApprovalRequest, String> {
        Ok(preview(&plan(args, ctx)?))
    }

    fn run<'a>(&'a self, args: Value, ctx: &'a ToolCtx) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            plan(&args, ctx)
                .and_then(|changes| write(&changes, ctx))
                .unwrap_or_else(ToolOutput::error)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hunks(patch: &str) -> Vec<Hunk> {
        match parse(patch).expect("parses").remove(0) {
            Op::Update { hunks, .. } => hunks,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parses_every_section() {
        let ops = parse(
            "```\n*** Begin Patch\n*** Add File: a.txt\n+one\n+two\n*** Delete File: b.txt\n\
             *** Update File: c.txt\n*** Move to: d.txt\n@@ fn x\n ctx\n-old\n+new\n*** End Patch\n```",
        )
        .expect("parses");
        assert_eq!(
            ops[0],
            Op::Add {
                path: "a.txt".into(),
                text: "one\ntwo\n".into()
            }
        );
        assert_eq!(
            ops[1],
            Op::Delete {
                path: "b.txt".into()
            }
        );
        let Op::Update { path, to, hunks } = &ops[2] else {
            panic!("{ops:?}");
        };
        assert_eq!((path.as_str(), to.as_deref()), ("c.txt", Some("d.txt")));
        assert_eq!(hunks[0].anchor.as_deref(), Some("fn x"));
        assert_eq!(hunks[0].old, ["ctx", "old"]);
        assert_eq!(hunks[0].new, ["ctx", "new"]);
    }

    #[test]
    fn applies_hunks_in_order_with_fuzz() {
        let file = "fn a() {\n    x();\n}\n\nfn b() {\n    x();\n}\n";
        // The second `x();` is meant: the anchor puts the search after `fn b`.
        let h = hunks(
            "*** Begin Patch\n*** Update File: f\n@@ fn b() {\n-    x();\n+    y();\n*** End Patch",
        );
        assert_eq!(
            apply_hunks(file, &h, "f").expect("applies"),
            "fn a() {\n    x();\n}\n\nfn b() {\n    y();\n}\n"
        );
        // Indentation differs: still found.
        let h =
            hunks("*** Begin Patch\n*** Update File: f\n fn a() {\n-x();\n+z();\n*** End Patch");
        assert!(apply_hunks(file, &h, "f").expect("fuzzy").contains("z();"));
        let h = hunks("*** Begin Patch\n*** Update File: f\n-missing\n+x\n*** End Patch");
        let err = apply_hunks(file, &h, "f").expect_err("no match");
        assert!(err.contains("missing"), "{err}");
    }

    #[test]
    fn end_of_file_hunk_appends() {
        let h = hunks("*** Begin Patch\n*** Update File: f\n+last\n*** End of File\n*** End Patch");
        assert_eq!(apply_hunks("a\nb\n", &h, "f").expect("ok"), "a\nb\nlast\n");
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse("just some text").is_err());
        assert!(parse("*** Begin Patch\nhello\n*** End Patch").is_err());
    }
}
