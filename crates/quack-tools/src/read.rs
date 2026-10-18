//! `read`: a file or a line range, line-numbered.

use futures::future::BoxFuture;
use quack_protocol::ApprovalRequest;
use serde_json::{Value, json};

use crate::paths;
use crate::registry::{Subject, Tool, ToolCtx, ToolKind, ToolOutput, arg_str, opt_u64};
use crate::text::{TextFile, io_msg, looks_binary};

const DEFAULT_LIMIT: usize = 2000;
const MAX_LINE_CHARS: usize = 2000;
const PATH: &[&str] = &["path", "file_path", "filename", "file"];

#[derive(Debug)]
pub struct Read;

impl Tool for Read {
    fn name(&self) -> &str {
        "read"
    }

    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }

    fn description(&self, ctx: &ToolCtx) -> String {
        let prefix = if ctx.hashline {
            "`<line>:<tag>|`, the tags edit_hashline uses"
        } else {
            "their line numbers"
        };
        format!(
            "Read a text file. Returns lines prefixed with {prefix}. \
             Reads up to {DEFAULT_LIMIT} lines by default; use `offset` (1-based line) and `limit` \
             for large files. Read a file before editing it."
        )
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path, relative to the workspace root or absolute" },
                "offset": { "type": "integer", "description": "First line to read, 1-based" },
                "limit": { "type": "integer", "description": "Maximum number of lines" }
            },
            "required": ["path"]
        })
    }

    fn subject(&self, args: &Value, ctx: &ToolCtx) -> Result<Subject, String> {
        Ok(Subject::Read(paths::resolve(
            &ctx.root,
            arg_str(args, PATH)?,
        )))
    }

    fn summary(&self, args: &Value, ctx: &ToolCtx) -> String {
        let path = arg_str(args, PATH)
            .map(|p| paths::display(&ctx.root, &paths::resolve(&ctx.root, p)))
            .unwrap_or_default();
        match opt_u64(args, &["offset"]) {
            Some(o) => format!("read {path}:{o}"),
            None => format!("read {path}"),
        }
    }

    fn preview(&self, args: &Value, ctx: &ToolCtx) -> Result<ApprovalRequest, String> {
        Ok(ApprovalRequest::Tool {
            name: self.summary(args, ctx),
            arguments: args.to_string(),
        })
    }

    fn run<'a>(&'a self, args: Value, ctx: &'a ToolCtx) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move { read(&args, ctx).unwrap_or_else(ToolOutput::error) })
    }
}

fn read(args: &Value, ctx: &ToolCtx) -> Result<ToolOutput, String> {
    let path = paths::resolve(&ctx.root, arg_str(args, PATH)?);
    let shown = paths::display(&ctx.root, &path);
    if path.is_dir() {
        return Err(format!("{shown} is a directory; use list_dir"));
    }
    let bytes = std::fs::read(&path).map_err(|e| io_msg(&path, &e))?;
    if looks_binary(&bytes) {
        return Err(format!("{shown} is a binary file ({} bytes)", bytes.len()));
    }
    let file = TextFile::from_bytes(&bytes).ok_or_else(|| format!("{shown} is not UTF-8 text"))?;
    ctx.reads.record(&path);

    let lines: Vec<&str> = file.text.lines().collect();
    let total = lines.len();
    let offset =
        usize::try_from(opt_u64(args, &["offset", "start_line"]).unwrap_or(1).max(1)).unwrap_or(1);
    let limit = usize::try_from(
        opt_u64(args, &["limit"])
            .unwrap_or(DEFAULT_LIMIT as u64)
            .max(1),
    )
    .unwrap_or(DEFAULT_LIMIT);
    if total == 0 {
        return Ok(ToolOutput::ok(format!("({shown} is empty)"), "empty file"));
    }
    if offset > total {
        return Err(format!(
            "offset {offset} is past the end of {shown} ({total} lines)"
        ));
    }
    let end = (offset - 1 + limit).min(total);
    let width = end.to_string().len().max(4);
    let mut out = String::new();
    for (i, line) in lines[offset - 1..end].iter().enumerate() {
        let n = offset + i;
        if ctx.hashline {
            // Long lines stay whole: an edit may need to name them.
            out.push_str(&format!("{n}:{}|{line}\n", crate::hashline::tag(line)));
            continue;
        }
        if line.chars().count() > MAX_LINE_CHARS {
            let cut: String = line.chars().take(MAX_LINE_CHARS).collect();
            out.push_str(&format!("{n:>width$}\t{cut}… [line truncated]\n"));
        } else {
            out.push_str(&format!("{n:>width$}\t{line}\n"));
        }
    }
    if offset > 1 || end < total {
        out.push_str(&format!(
            "\n(lines {offset}-{end} of {total}; use offset/limit to read more)\n"
        ));
    }
    Ok(ToolOutput::ok(
        out,
        format!("lines {offset}-{end} of {total}"),
    ))
}
