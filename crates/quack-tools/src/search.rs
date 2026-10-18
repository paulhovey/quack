//! `list_dir`, `glob` and `grep`. All respect .gitignore and skip `.git`.

use std::path::{Path, PathBuf};

use quack_protocol::ApprovalRequest;
use futures::future::BoxFuture;
use serde_json::{Value, json};

use crate::paths;
use crate::registry::{
    Subject, Tool, ToolCtx, ToolKind, ToolOutput, arg_str, opt_bool, opt_str, opt_u64,
};
use crate::text::looks_binary;

const MAX_LIST: usize = 400;
const MAX_GLOB: usize = 200;
const MAX_GREP: usize = 200;
const MAX_GREP_LINE: usize = 300;
const MAX_GREP_FILE: u64 = 5 * 1024 * 1024;
const PATH: &[&str] = &["path", "dir", "directory"];

fn walker(root: &Path, max_depth: Option<usize>) -> ignore::Walk {
    ignore::WalkBuilder::new(root)
        .hidden(false)
        .max_depth(max_depth)
        .sort_by_file_path(Path::cmp)
        .filter_entry(|e| e.file_name() != ".git")
        .build()
}

fn base(args: &Value, ctx: &ToolCtx) -> PathBuf {
    opt_str(args, PATH).map_or_else(|| ctx.root.clone(), |p| paths::resolve(&ctx.root, p))
}

/// Run blocking filesystem work off the async runtime.
async fn blocking(f: impl FnOnce() -> Result<ToolOutput, String> + Send + 'static) -> ToolOutput {
    tokio::task::spawn_blocking(f)
        .await
        .unwrap_or_else(|e| Err(format!("tool panicked: {e}")))
        .unwrap_or_else(ToolOutput::error)
}

fn read_preview(name: String, args: &Value) -> Result<ApprovalRequest, String> {
    Ok(ApprovalRequest::Tool {
        name,
        arguments: args.to_string(),
    })
}

// ---------------------------------------------------------------- list_dir

#[derive(Debug)]
pub struct ListDir;

impl Tool for ListDir {
    fn name(&self) -> &str {
        "list_dir"
    }
    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }
    fn description(&self, _ctx: &ToolCtx) -> String {
        "List a directory as an indented tree (default depth 2). Respects .gitignore.".into()
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Directory; default is the workspace root" },
                "depth": { "type": "integer", "description": "How many levels to show (1-5, default 2)" }
            }
        })
    }
    fn subject(&self, args: &Value, ctx: &ToolCtx) -> Result<Subject, String> {
        Ok(Subject::Read(base(args, ctx)))
    }
    fn summary(&self, args: &Value, ctx: &ToolCtx) -> String {
        format!("list {}", paths::display(&ctx.root, &base(args, ctx)))
    }
    fn preview(&self, args: &Value, ctx: &ToolCtx) -> Result<ApprovalRequest, String> {
        read_preview(self.summary(args, ctx), args)
    }
    fn run<'a>(&'a self, args: Value, ctx: &'a ToolCtx) -> BoxFuture<'a, ToolOutput> {
        let dir = base(&args, ctx);
        let root = ctx.root.clone();
        let depth = usize::try_from(
            opt_u64(&args, &["depth", "max_depth"])
                .unwrap_or(2)
                .clamp(1, 5),
        )
        .unwrap_or(2);
        Box::pin(blocking(move || {
            let shown = paths::display(&root, &dir);
            if !dir.is_dir() {
                return Err(format!("{shown} is not a directory"));
            }
            let mut out = format!("{shown}/\n");
            let mut n = 0;
            let mut more = 0;
            for entry in walker(&dir, Some(depth)).flatten() {
                if entry.depth() == 0 {
                    continue;
                }
                if n >= MAX_LIST {
                    more += 1;
                    continue;
                }
                n += 1;
                let name = entry.file_name().to_string_lossy();
                let slash = if entry.file_type().is_some_and(|t| t.is_dir()) {
                    "/"
                } else {
                    ""
                };
                out.push_str(&format!("{}{name}{slash}\n", "  ".repeat(entry.depth())));
            }
            if more > 0 {
                out.push_str(&format!("… {more} more entries; list a subdirectory\n"));
            }
            Ok(ToolOutput::ok(out, format!("{n} entries")))
        }))
    }
}

// ---------------------------------------------------------------- glob

#[derive(Debug)]
pub struct Glob;

impl Tool for Glob {
    fn name(&self) -> &str {
        "glob"
    }
    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }
    fn description(&self, _ctx: &ToolCtx) -> String {
        "Find files by glob pattern, e.g. `**/*.rs` or `src/**/test_*.py`. Respects .gitignore."
            .into()
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Glob pattern, relative to `path`" },
                "path": { "type": "string", "description": "Directory to search; default is the workspace root" }
            },
            "required": ["pattern"]
        })
    }
    fn subject(&self, args: &Value, ctx: &ToolCtx) -> Result<Subject, String> {
        Ok(Subject::Read(base(args, ctx)))
    }
    fn summary(&self, args: &Value, _ctx: &ToolCtx) -> String {
        format!(
            "glob {}",
            arg_str(args, &["pattern", "glob"]).unwrap_or_default()
        )
    }
    fn preview(&self, args: &Value, ctx: &ToolCtx) -> Result<ApprovalRequest, String> {
        read_preview(self.summary(args, ctx), args)
    }
    fn run<'a>(&'a self, args: Value, ctx: &'a ToolCtx) -> BoxFuture<'a, ToolOutput> {
        let dir = base(&args, ctx);
        let root = ctx.root.clone();
        Box::pin(blocking(move || {
            let pattern = arg_str(&args, &["pattern", "glob"])?;
            // A bare `*.rs` means "anywhere", as models usually intend.
            let pattern = if pattern.contains('/') {
                pattern.to_owned()
            } else {
                format!("**/{pattern}")
            };
            let matcher = globset::GlobBuilder::new(&pattern)
                .literal_separator(true)
                .build()
                .map_err(|e| format!("invalid glob `{pattern}`: {e}"))?
                .compile_matcher();
            let mut hits = Vec::new();
            let mut total = 0;
            for entry in walker(&dir, None).flatten() {
                if !entry.file_type().is_some_and(|t| t.is_file()) {
                    continue;
                }
                let rel = entry.path().strip_prefix(&dir).unwrap_or(entry.path());
                if matcher.is_match(rel) {
                    total += 1;
                    if hits.len() < MAX_GLOB {
                        hits.push(paths::display(&root, entry.path()));
                    }
                }
            }
            if hits.is_empty() {
                return Ok(ToolOutput::ok("No files matched.", "no matches"));
            }
            let mut out = hits.join("\n");
            if total > hits.len() {
                out.push_str(&format!(
                    "\n… {} more; narrow the pattern",
                    total - hits.len()
                ));
            }
            Ok(ToolOutput::ok(out, format!("{total} files")))
        }))
    }
}

// ---------------------------------------------------------------- grep

#[derive(Debug)]
pub struct Grep;

impl Tool for Grep {
    fn name(&self) -> &str {
        "grep"
    }
    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }
    fn description(&self, _ctx: &ToolCtx) -> String {
        "Search file contents with a regular expression (Rust regex syntax). Returns \
         `path:line: text` for each match. Use `glob` to limit the files searched, e.g. `*.rs`."
            .into()
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Regular expression" },
                "path": { "type": "string", "description": "File or directory; default is the workspace root" },
                "glob": { "type": "string", "description": "Only search files matching this glob" },
                "case_insensitive": { "type": "boolean" }
            },
            "required": ["pattern"]
        })
    }
    fn subject(&self, args: &Value, ctx: &ToolCtx) -> Result<Subject, String> {
        Ok(Subject::Read(base(args, ctx)))
    }
    fn summary(&self, args: &Value, _ctx: &ToolCtx) -> String {
        format!(
            "grep {}",
            arg_str(args, &["pattern", "regex", "query"]).unwrap_or_default()
        )
    }
    fn preview(&self, args: &Value, ctx: &ToolCtx) -> Result<ApprovalRequest, String> {
        read_preview(self.summary(args, ctx), args)
    }
    fn run<'a>(&'a self, args: Value, ctx: &'a ToolCtx) -> BoxFuture<'a, ToolOutput> {
        let dir = base(&args, ctx);
        let root = ctx.root.clone();
        Box::pin(blocking(move || {
            let pattern = arg_str(&args, &["pattern", "regex", "query"])?;
            let re = regex::RegexBuilder::new(pattern)
                .case_insensitive(opt_bool(&args, &["case_insensitive", "ignore_case", "-i"]))
                .build()
                .map_err(|e| format!("invalid regex: {e}"))?;
            let filter = opt_str(&args, &["glob", "include"])
                .map(|g| {
                    let g = if g.contains('/') {
                        g.to_owned()
                    } else {
                        format!("**/{g}")
                    };
                    globset::Glob::new(&g)
                        .map(|g| g.compile_matcher())
                        .map_err(|e| format!("invalid glob: {e}"))
                })
                .transpose()?;

            let mut out = String::new();
            let mut matches = 0;
            let mut files = 0;
            let entries: Box<dyn Iterator<Item = PathBuf>> = if dir.is_file() {
                Box::new(std::iter::once(dir.clone()))
            } else {
                Box::new(
                    walker(&dir, None)
                        .flatten()
                        .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
                        .map(ignore::DirEntry::into_path),
                )
            };
            for path in entries {
                if filter
                    .as_ref()
                    .is_some_and(|f| !f.is_match(path.strip_prefix(&dir).unwrap_or(&path)))
                {
                    continue;
                }
                if std::fs::metadata(&path).map_or(true, |m| m.len() > MAX_GREP_FILE) {
                    continue;
                }
                let Ok(bytes) = std::fs::read(&path) else {
                    continue;
                };
                if looks_binary(&bytes) {
                    continue;
                }
                let text = String::from_utf8_lossy(&bytes);
                let mut in_file = false;
                for (i, line) in text.lines().enumerate() {
                    if !re.is_match(line) {
                        continue;
                    }
                    matches += 1;
                    if !in_file {
                        in_file = true;
                        files += 1;
                    }
                    if matches <= MAX_GREP {
                        let line = line.trim_end();
                        let shown = if line.chars().count() > MAX_GREP_LINE {
                            format!("{}…", line.chars().take(MAX_GREP_LINE).collect::<String>())
                        } else {
                            line.to_owned()
                        };
                        out.push_str(&format!(
                            "{}:{}: {shown}\n",
                            paths::display(&root, &path),
                            i + 1
                        ));
                    }
                }
            }
            if matches == 0 {
                return Ok(ToolOutput::ok("No matches.", "no matches"));
            }
            if matches > MAX_GREP {
                out.push_str(&format!(
                    "… {} more matches; narrow the pattern or path\n",
                    matches - MAX_GREP
                ));
            }
            Ok(ToolOutput::ok(
                out,
                format!("{matches} matches in {files} files"),
            ))
        }))
    }
}
