//! `edit_blocks` (edit format `search_replace_blocks`): SEARCH/REPLACE blocks, each under
//! the path of its file.
//!
//! ```text
//! src/lib.rs
//! <<<<<<< SEARCH
//! old lines
//! =======
//! new lines
//! >>>>>>> REPLACE
//! ```
//!
//! Each SEARCH is matched like `edit`'s `old_string` (exact, then fuzzy). An empty SEARCH
//! creates the file. Every block is checked before any file is written.

use std::path::PathBuf;

use futures::future::BoxFuture;
use quack_config::settings::EditFormat;
use quack_protocol::ApprovalRequest;
use serde_json::{Value, json};

use crate::patch::{Change, load, preview, write};
use crate::paths;
use crate::registry::{Subject, Tool, ToolCtx, ToolKind, ToolOutput, arg_str, opt_str};

const BLOCKS: &[&str] = &["blocks", "edits", "content", "input"];
const PATH: &[&str] = &["path", "file_path", "file"];

#[derive(Debug)]
pub struct EditBlocks;

#[derive(Debug, PartialEq)]
struct Block {
    path: String,
    search: String,
    replace: String,
}

fn marker(l: &str, c: char) -> bool {
    let t = l.trim_end();
    t.len() >= 5 && t.chars().take(5).all(|x| x == c)
}

fn parse(text: &str, default_path: Option<&str>) -> Result<Vec<Block>, String> {
    let text = text.replace("\r\n", "\n");
    let mut blocks = Vec::new();
    let mut path: Option<String> = default_path.map(str::to_owned);
    let mut last_line: Option<&str> = None;
    let mut lines = text.lines();
    while let Some(l) = lines.next() {
        if !(marker(l, '<') && l.contains("SEARCH")) {
            let t = l.trim();
            // The path line sits just above the block; fences and blank lines are skipped.
            if !t.is_empty() && !t.starts_with("```") {
                last_line = Some(t);
            }
            continue;
        }
        if let Some(p) = last_line.take() {
            path = Some(p.trim_matches(['`', '*', ':']).trim().to_owned());
        }
        let Some(file) = path.clone() else {
            return Err(
                "a SEARCH block has no file path; put the path on the line above it".into(),
            );
        };
        let mut search = Vec::new();
        let mut replace = Vec::new();
        let mut in_replace = false;
        let mut closed = false;
        for l in lines.by_ref() {
            if !in_replace && marker(l, '=') {
                in_replace = true;
            } else if in_replace && marker(l, '>') {
                closed = true;
                break;
            } else if in_replace {
                replace.push(l);
            } else {
                search.push(l);
            }
        }
        if !closed {
            return Err(format!(
                "the block for {file} is not closed; it needs `=======` and `>>>>>>> REPLACE`"
            ));
        }
        let join = |v: &[&str]| {
            if v.is_empty() {
                String::new()
            } else {
                v.join("\n") + "\n"
            }
        };
        blocks.push(Block {
            path: file,
            search: join(&search),
            replace: join(&replace),
        });
    }
    if blocks.is_empty() {
        return Err("no SEARCH/REPLACE blocks found".into());
    }
    Ok(blocks)
}

fn plan(args: &Value, ctx: &ToolCtx) -> Result<Vec<Change>, String> {
    let blocks = parse(arg_str(args, BLOCKS)?, opt_str(args, PATH))?;
    let mut changes: Vec<Change> = Vec::new();
    for (n, b) in blocks.iter().enumerate() {
        let path = paths::resolve(&ctx.root, &b.path);
        let shown = paths::display(&ctx.root, &path);
        let i = match changes.iter().position(|c| c.path == path) {
            Some(i) => i,
            None => {
                let before = if b.search.is_empty() && !path.exists() {
                    None
                } else {
                    Some(load(ctx, &path, &shown)?)
                };
                let after = before.as_ref().map(|f| f.text.clone());
                changes.push(Change {
                    path,
                    shown,
                    before,
                    after,
                });
                changes.len() - 1
            }
        };
        let c = &mut changes[i];
        let current = c.after.clone().unwrap_or_default();
        let next = if b.search.is_empty() {
            if !current.is_empty() {
                return Err(format!(
                    "block {}: SEARCH is empty but {} is not; copy the lines to replace",
                    n + 1,
                    c.shown
                ));
            }
            b.replace.clone()
        } else {
            let (text, _, _) = crate::edit::replace(&current, &b.search, &b.replace, false)
                .map_err(|e| format!("block {} ({}): {}", n + 1, c.shown, e))?;
            text
        };
        c.after = Some(next);
    }
    Ok(changes)
}

fn named(args: &Value, ctx: &ToolCtx) -> Result<Vec<PathBuf>, String> {
    let mut out: Vec<PathBuf> = Vec::new();
    for b in parse(arg_str(args, BLOCKS)?, opt_str(args, PATH))? {
        let p = paths::resolve(&ctx.root, &b.path);
        if !out.contains(&p) {
            out.push(p);
        }
    }
    Ok(out)
}

impl Tool for EditBlocks {
    fn name(&self) -> &str {
        "edit_blocks"
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Mutating
    }

    fn available(&self, ctx: &ToolCtx) -> bool {
        ctx.edit_format == EditFormat::SearchReplaceBlocks
    }

    fn description(&self, _ctx: &ToolCtx) -> String {
        "Change files with SEARCH/REPLACE blocks. For each change, write the file path on its \
         own line, then:\n<<<<<<< SEARCH\n(the exact lines to find)\n=======\n(the lines to put \
         there)\n>>>>>>> REPLACE\nThe SEARCH lines must match the file exactly and only once; \
         include enough lines to be unique. An empty SEARCH creates a new file. Several blocks \
         and files can go in one call."
            .into()
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "blocks": { "type": "string", "description": "One or more blocks, each under its file path" }
            },
            "required": ["blocks"]
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
            Ok([one]) => format!("edit {}", paths::display(&ctx.root, one)),
            Ok(many) => format!("edit {} files", many.len()),
            Err(_) => "edit".into(),
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

    #[test]
    fn parses_paths_and_blocks() {
        let b = parse(
            "src/a.rs\n```rust\n<<<<<<< SEARCH\nold\n=======\nnew\n>>>>>>> REPLACE\n```\n\n\
             <<<<<<< SEARCH\ntwo\n=======\n>>>>>>> REPLACE\n\nb.txt\n<<<<<<< SEARCH\n=======\nhi\n>>>>>>> REPLACE",
            None,
        )
        .expect("parses");
        assert_eq!(b.len(), 3);
        assert_eq!(
            (
                b[0].path.as_str(),
                b[0].search.as_str(),
                b[0].replace.as_str()
            ),
            ("src/a.rs", "old\n", "new\n")
        );
        // No new path line: the same file.
        assert_eq!(
            (b[1].path.as_str(), b[1].replace.as_str()),
            ("src/a.rs", "")
        );
        assert_eq!((b[2].path.as_str(), b[2].search.as_str()), ("b.txt", ""));
    }

    #[test]
    fn needs_a_path_and_a_closed_block() {
        assert!(parse("<<<<<<< SEARCH\na\n=======\nb\n>>>>>>> REPLACE", None).is_err());
        assert!(parse("a.rs\n<<<<<<< SEARCH\na\n=======\nb\n", None).is_err());
        assert!(parse("<<<<<<< SEARCH\na\n=======\nb\n>>>>>>> REPLACE", Some("x")).is_ok());
    }
}
