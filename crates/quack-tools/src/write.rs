//! `write`: create or overwrite a file.

use futures::future::BoxFuture;
use quack_protocol::ApprovalRequest;
use serde_json::{Value, json};

use crate::paths;
use crate::registry::{Subject, Tool, ToolCtx, ToolKind, ToolOutput, arg_str};
use crate::text::{TextFile, diff_stats, io_msg, unified_diff};

const PATH: &[&str] = &["path", "file_path", "filename", "file"];
const CONTENT: &[&str] = &["content", "contents", "text"];

#[derive(Debug)]
pub struct Write;

/// The planned change: path, old text (if any), and the bytes to write.
struct Plan {
    path: std::path::PathBuf,
    shown: String,
    before: Option<String>,
    after: String,
    encoded: String,
}

fn plan(args: &Value, ctx: &ToolCtx) -> Result<Plan, String> {
    let path = paths::resolve(&ctx.root, arg_str(args, PATH)?);
    let shown = paths::display(&ctx.root, &path);
    let content = arg_str(args, CONTENT)?.to_owned();
    if path.is_dir() {
        return Err(format!("{shown} is a directory"));
    }
    if path.exists() {
        let file = TextFile::load(&path)?;
        if ctx.reads.is_stale(&path) {
            return Err(format!(
                "{shown} changed on disk since you last read it; read it again first"
            ));
        }
        let after = content.replace("\r\n", "\n");
        Ok(Plan {
            encoded: file.encode(&after),
            path,
            shown,
            before: Some(file.text),
            after,
        })
    } else {
        Ok(Plan {
            encoded: content.clone(),
            path,
            shown,
            before: None,
            after: content.replace("\r\n", "\n"),
        })
    }
}

impl Tool for Write {
    fn name(&self) -> &str {
        "write"
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Mutating
    }

    fn description(&self, _ctx: &ToolCtx) -> String {
        "Create a new file or replace a file's entire content. Parent directories are created. \
         Prefer `edit` for changing part of an existing file."
            .into()
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path, relative to the workspace root or absolute" },
                "content": { "type": "string", "description": "The complete file content" }
            },
            "required": ["path", "content"]
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
        format!("write {path}")
    }

    fn preview(&self, args: &Value, ctx: &ToolCtx) -> Result<ApprovalRequest, String> {
        let p = plan(args, ctx)?;
        Ok(ApprovalRequest::Edit {
            diff: unified_diff(&p.shown, p.before.as_deref().unwrap_or(""), &p.after),
            path: p.shown,
        })
    }

    fn run<'a>(&'a self, args: Value, ctx: &'a ToolCtx) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            let run = || -> Result<ToolOutput, String> {
                let p = plan(&args, ctx)?;
                if let Some(parent) = p.path.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| io_msg(parent, &e))?;
                }
                std::fs::write(&p.path, &p.encoded).map_err(|e| io_msg(&p.path, &e))?;
                ctx.reads.record(&p.path);
                let before = p.before.as_deref().unwrap_or("");
                let (add, del) = diff_stats(before, &p.after);
                let verb = if p.before.is_some() {
                    "Updated"
                } else {
                    "Created"
                };
                Ok(ToolOutput::ok(
                    format!("{verb} {} ({} lines).", p.shown, p.after.lines().count()),
                    format!("+{add} -{del}"),
                )
                .with_detail(unified_diff(&p.shown, before, &p.after)))
            };
            run().unwrap_or_else(ToolOutput::error)
        })
    }
}
