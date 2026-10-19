//! `index`: the outline of a file (definitions with line ranges), or of every source file
//! in a directory, one line each.

use futures::future::BoxFuture;
use quack_protocol::ApprovalRequest;
use serde_json::{Value, json};

use crate::outline::{language_of, outline};
use crate::paths;
use crate::registry::{Subject, Tool, ToolCtx, ToolKind, ToolOutput, opt_str};
use crate::search::{blocking, walker};
use crate::text::{MAX_EDIT_BYTES, TextFile};

const PATH: &[&str] = &["path", "file_path", "file", "dir", "directory"];
/// A directory's outline stops at about this many characters (some 3,000 tokens).
const MAX_DIR_CHARS: usize = 12_000;
const LINE_CHARS: usize = 160;

#[derive(Debug)]
pub struct Index;

fn load(path: &std::path::Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() > MAX_EDIT_BYTES {
        return None;
    }
    TextFile::from_bytes(&std::fs::read(path).ok()?).map(|f| f.text)
}

impl Tool for Index {
    fn name(&self) -> &str {
        "index"
    }

    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }

    fn available(&self, _ctx: &ToolCtx) -> bool {
        cfg!(feature = "lang-common")
    }

    fn description(&self, _ctx: &ToolCtx) -> String {
        "Outline a source file: its functions, types, methods and modules with their line \
         ranges, nested. On a directory, lists each source file with its top-level \
         definitions. Use it to find your way before reading, then `read` the lines you need \
         (or `read` with `symbol`). Supports Rust, Python, JavaScript, TypeScript and Go."
            .into()
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File or directory; default is the workspace root" }
            }
        })
    }

    fn subject(&self, args: &Value, ctx: &ToolCtx) -> Result<Subject, String> {
        Ok(Subject::Read(opt_str(args, PATH).map_or_else(
            || ctx.root.clone(),
            |p| paths::resolve(&ctx.root, p),
        )))
    }

    fn summary(&self, args: &Value, ctx: &ToolCtx) -> String {
        let path =
            opt_str(args, PATH).map_or_else(|| ctx.root.clone(), |p| paths::resolve(&ctx.root, p));
        format!("index {}", paths::display(&ctx.root, &path))
    }

    fn preview(&self, args: &Value, ctx: &ToolCtx) -> Result<ApprovalRequest, String> {
        Ok(ApprovalRequest::Tool {
            name: self.summary(args, ctx),
            arguments: args.to_string(),
        })
    }

    fn run<'a>(&'a self, args: Value, ctx: &'a ToolCtx) -> BoxFuture<'a, ToolOutput> {
        let root = ctx.root.clone();
        let path = opt_str(&args, PATH).map_or_else(|| root.clone(), |p| paths::resolve(&root, p));
        Box::pin(blocking(move || {
            let shown = paths::display(&root, &path);
            if path.is_file() {
                let text = load(&path).ok_or_else(|| format!("{shown} is not a text file"))?;
                let o = outline(&path, &text).ok_or_else(|| {
                    format!("no outline for {shown}: its language is not supported; use `read`")
                })?;
                let n = o.symbols.len();
                return Ok(ToolOutput::ok(o.render(&shown), format!("{n} definitions")));
            }
            if !path.is_dir() {
                return Err(format!("{shown} does not exist"));
            }
            let mut out = String::new();
            let (mut files, mut left) = (0, 0);
            for entry in walker(&path, None).flatten() {
                let p = entry.path();
                if !entry.file_type().is_some_and(|t| t.is_file()) || language_of(p).is_none() {
                    continue;
                }
                if out.len() > MAX_DIR_CHARS {
                    left += 1;
                    continue;
                }
                let Some(o) = load(p).and_then(|t| outline(p, &t)) else {
                    continue;
                };
                files += 1;
                let name = paths::display(&root, p);
                let room = LINE_CHARS.saturating_sub(name.len() + 16);
                out.push_str(&format!(
                    "{name} ({} lines): {}\n",
                    o.lines,
                    o.summary(room)
                ));
            }
            if files == 0 {
                return Ok(ToolOutput::ok(
                    format!("No supported source files under {shown}."),
                    "no files",
                ));
            }
            if left > 0 {
                out.push_str(&format!("… {left} more files; index a subdirectory\n"));
            }
            Ok(ToolOutput::ok(out, format!("{files} files")))
        }))
    }
}
