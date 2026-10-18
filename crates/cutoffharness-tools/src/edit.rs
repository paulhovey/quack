//! `edit` (internal `edit.str_replace`): replace a unique snippet, with fuzzy fallbacks.
//!
//! Tried in order: exact → trailing whitespace ignored → indentation ignored (replacement
//! re-indented) → blank lines ignored. A non-exact match is reported to the model.

use cutoffharness_protocol::ApprovalRequest;
use futures::future::BoxFuture;
use serde_json::{Value, json};

use crate::paths;
use crate::registry::{Subject, Tool, ToolCtx, ToolKind, ToolOutput, arg_str, opt_bool};
use crate::text::{TextFile, diff_stats, io_msg, unified_diff};

const PATH: &[&str] = &["path", "file_path", "filename", "file"];
const OLD: &[&str] = &["old_string", "old_str", "old", "search"];
const NEW: &[&str] = &["new_string", "new_str", "new", "replace"];

#[derive(Debug)]
pub struct StrReplace;

struct Plan {
    path: std::path::PathBuf,
    shown: String,
    file: TextFile,
    after: String,
    note: Option<&'static str>,
    count: usize,
}

fn plan(args: &Value, ctx: &ToolCtx) -> Result<Plan, String> {
    let path = paths::resolve(&ctx.root, arg_str(args, PATH)?);
    let shown = paths::display(&ctx.root, &path);
    let old = arg_str(args, OLD)?.replace("\r\n", "\n");
    let new = arg_str(args, NEW)?.replace("\r\n", "\n");
    let all = opt_bool(args, &["replace_all", "all"]);

    if !path.exists() {
        if old.is_empty() {
            return Err(format!("{shown} does not exist; use `write` to create it"));
        }
        return Err(format!("{shown} does not exist"));
    }
    let file = TextFile::load(&path)?;
    if ctx.reads.is_stale(&path) {
        return Err(format!(
            "{shown} changed on disk since you last read it; read it again before editing"
        ));
    }
    if old == new {
        return Err("old_string and new_string are identical; nothing to change".into());
    }
    if old.is_empty() {
        if file.text.is_empty() {
            return Ok(Plan {
                path,
                shown,
                file,
                after: new,
                note: None,
                count: 1,
            });
        }
        return Err("old_string is empty; give the exact text to replace".into());
    }
    let (after, note, count) =
        replace(&file.text, &old, &new, all).map_err(|e| format!("{e} (in {shown})"))?;
    Ok(Plan {
        path,
        shown,
        file,
        after,
        note,
        count,
    })
}

/// Apply the replacement; returns the new text, a note for fuzzy matches, and the count.
pub(crate) fn replace(
    text: &str,
    old: &str,
    new: &str,
    all: bool,
) -> Result<(String, Option<&'static str>, usize), String> {
    let hits: Vec<usize> = text.match_indices(old).map(|(i, _)| i).collect();
    match hits.len() {
        0 => {}
        1 => return Ok((text.replacen(old, new, 1), None, 1)),
        n if all => return Ok((text.replace(old, new), None, n)),
        _ => {
            let lines: Vec<String> = hits.iter().map(|&i| line_of(text, i).to_string()).collect();
            return Err(format!(
                "old_string matches {} places (lines {}); include more surrounding lines to make it unique, or set replace_all",
                hits.len(),
                lines.join(", ")
            ));
        }
    }

    let file_lines: Vec<&str> = text.split('\n').collect();
    let old_lines: Vec<&str> = old.trim_end_matches('\n').split('\n').collect();
    let strategies: [(Strategy, &'static str); 3] = [
        (trailing_ws, "matched after ignoring trailing whitespace"),
        (
            indentation,
            "matched after ignoring indentation; replacement re-indented",
        ),
        (blank_lines, "matched after ignoring blank lines"),
    ];
    for (find, note) in strategies {
        let windows = find(&file_lines, &old_lines);
        match windows.len() {
            0 => continue,
            1 => {
                let (s, e) = windows[0];
                let replacement = if note.starts_with("matched after ignoring indentation") {
                    reindent(new, &old_lines, file_lines[s..e].iter().copied())
                } else {
                    new.trim_end_matches('\n').to_owned()
                };
                return Ok((splice(&file_lines, s, e, &replacement), Some(note), 1));
            }
            _ => {
                let lines: Vec<String> = windows.iter().map(|(s, _)| (s + 1).to_string()).collect();
                return Err(format!(
                    "old_string matches {} places when whitespace is ignored (lines {}); include more context",
                    windows.len(),
                    lines.join(", ")
                ));
            }
        }
    }

    let first = old_lines
        .iter()
        .find(|l| !l.trim().is_empty())
        .map(|l| l.trim());
    let hint = first
        .map(|f| {
            file_lines
                .iter()
                .enumerate()
                .filter(|(_, l)| l.trim() == f)
                .map(|(i, _)| (i + 1).to_string())
                .take(5)
                .collect::<Vec<_>>()
        })
        .filter(|v| !v.is_empty())
        .map(|v| {
            format!(
                "; its first line appears at line {} but the lines after it differ",
                v.join(", ")
            )
        })
        .unwrap_or_default();
    Err(format!(
        "old_string was not found{hint}. Read the file again and copy the exact text"
    ))
}

type Strategy = fn(&[&str], &[&str]) -> Vec<(usize, usize)>;

fn windows_where(
    file: &[&str],
    old: &[&str],
    eq: impl Fn(&str, &str) -> bool,
) -> Vec<(usize, usize)> {
    if old.is_empty() || old.len() > file.len() {
        return Vec::new();
    }
    (0..=file.len() - old.len())
        .filter(|&i| old.iter().enumerate().all(|(k, o)| eq(file[i + k], o)))
        .map(|i| (i, i + old.len()))
        .collect()
}

fn trailing_ws(file: &[&str], old: &[&str]) -> Vec<(usize, usize)> {
    windows_where(file, old, |a, b| a.trim_end() == b.trim_end())
}

fn indentation(file: &[&str], old: &[&str]) -> Vec<(usize, usize)> {
    windows_where(file, old, |a, b| a.trim() == b.trim())
}

fn blank_lines(file: &[&str], old: &[&str]) -> Vec<(usize, usize)> {
    let wanted: Vec<&str> = old
        .iter()
        .map(|l| l.trim_end())
        .filter(|l| !l.trim().is_empty())
        .collect();
    if wanted.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for start in 0..file.len() {
        if file[start].trim_end() != wanted[0] {
            continue;
        }
        let mut k = 1;
        let mut i = start + 1;
        while k < wanted.len() && i < file.len() {
            if file[i].trim().is_empty() {
                i += 1;
                continue;
            }
            if file[i].trim_end() != wanted[k] {
                break;
            }
            k += 1;
            i += 1;
        }
        if k == wanted.len() {
            out.push((start, i));
        }
    }
    out
}

fn indent_of(s: &str) -> &str {
    &s[..s.len() - s.trim_start().len()]
}

/// Shift `new` from the indentation the model used to the file's indentation.
fn reindent<'a>(new: &str, old: &[&str], window: impl Iterator<Item = &'a str>) -> String {
    let old_indent = old
        .iter()
        .find(|l| !l.trim().is_empty())
        .map_or("", |l| indent_of(l));
    let file_indent = window
        .into_iter()
        .find(|l| !l.trim().is_empty())
        .map_or("", indent_of)
        .to_owned();
    new.trim_end_matches('\n')
        .split('\n')
        .map(|l| {
            if l.trim().is_empty() {
                String::new()
            } else {
                match l.strip_prefix(old_indent) {
                    Some(rest) => format!("{file_indent}{rest}"),
                    None => l.to_owned(),
                }
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Replace lines `[s, e)` with `replacement`.
fn splice(lines: &[&str], s: usize, e: usize, replacement: &str) -> String {
    let mut out: Vec<&str> = Vec::with_capacity(lines.len());
    out.extend_from_slice(&lines[..s]);
    out.push(replacement);
    out.extend_from_slice(&lines[e..]);
    out.join("\n")
}

fn line_of(text: &str, byte: usize) -> usize {
    text[..byte].matches('\n').count() + 1
}

impl Tool for StrReplace {
    fn name(&self) -> &str {
        "edit"
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Mutating
    }

    fn description(&self, _ctx: &ToolCtx) -> String {
        "Replace text in a file. `old_string` must match the file exactly (including indentation) \
         and be unique unless `replace_all` is true; include a few surrounding lines to make it \
         unique. Read the file first."
            .into()
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path, relative to the workspace root or absolute" },
                "old_string": { "type": "string", "description": "Exact text to replace" },
                "new_string": { "type": "string", "description": "Replacement text" },
                "replace_all": { "type": "boolean", "description": "Replace every occurrence (default false)" }
            },
            "required": ["path", "old_string", "new_string"]
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
                let mut msg = format!("Edited {}", p.shown);
                if p.count > 1 {
                    msg.push_str(&format!(" ({} replacements)", p.count));
                }
                if let Some(note) = p.note {
                    msg.push_str(&format!(" ({note})"));
                }
                msg.push('.');
                Ok(
                    ToolOutput::ok(msg, format!("+{add} -{del}")).with_detail(unified_diff(
                        &p.shown,
                        &p.file.text,
                        &p.after,
                    )),
                )
            };
            run().unwrap_or_else(ToolOutput::error)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_and_ambiguous() {
        let (t, note, _) = replace("a\nb\nc\n", "b", "B", false).expect("unique");
        assert_eq!((t.as_str(), note), ("a\nB\nc\n", None));
        let err = replace("x\nx\n", "x", "y", false).expect_err("ambiguous");
        assert!(err.contains("lines 1, 2"), "{err}");
        assert_eq!(replace("x\nx\n", "x", "y", true).expect("all").0, "y\ny\n");
    }

    #[test]
    fn indentation_is_normalized_and_reapplied() {
        let file = "fn main() {\n        let a = 1;\n        let b = 2;\n}\n";
        let (t, note, _) = replace(
            file,
            "let a = 1;\nlet b = 2;",
            "let a = 10;\nif a > 1 {\n    go();\n}",
            false,
        )
        .expect("fuzzy");
        assert!(note.is_some());
        assert_eq!(
            t,
            "fn main() {\n        let a = 10;\n        if a > 1 {\n            go();\n        }\n}\n"
        );
    }

    #[test]
    fn trailing_whitespace_and_blank_lines() {
        let file = "a  \nb\n\nc\n";
        let (t, note, _) = replace(file, "a\nb", "z", false).expect("trailing");
        assert_eq!(t, "z\n\nc\n");
        assert_eq!(note, Some("matched after ignoring trailing whitespace"));
        let (t, _, _) = replace(file, "b\nc", "y", false).expect("blank");
        assert_eq!(t, "a  \ny\n");
    }

    #[test]
    fn not_found_points_at_first_line() {
        let err = replace("one\ntwo\nthree\n", "two\nTHREE", "x", false).expect_err("missing");
        assert!(err.contains("line 2"), "{err}");
    }
}
