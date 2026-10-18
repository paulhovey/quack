//! `lsp`: definitions, references, hover, symbols and diagnostics from language servers.
//! Offered only when a server in `lsp.d` is installed.

use quack_protocol::ApprovalRequest;
use futures::future::BoxFuture;
use quack_lsp::ops::{self, Operation, Query};
use serde_json::{Value, json};

use crate::paths;
use crate::registry::{Subject, Tool, ToolCtx, ToolKind, ToolOutput, opt_str, opt_u64};

const OP: &[&str] = &["operation", "op", "action", "command"];
const PATH: &[&str] = &["path", "file_path", "filename", "file"];
const SYMBOL: &[&str] = &["symbol", "name", "query"];

#[derive(Debug)]
pub struct Lsp;

fn operation(args: &Value) -> Result<Operation, String> {
    let name = opt_str(args, OP).ok_or("missing required string argument `operation`")?;
    Operation::parse(name).ok_or_else(|| {
        format!(
            "unknown operation `{name}`; use one of: {}",
            Operation::NAMES.join(", ")
        )
    })
}

impl Tool for Lsp {
    fn name(&self) -> &str {
        "lsp"
    }

    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }

    fn available(&self, ctx: &ToolCtx) -> bool {
        ctx.lsp.as_ref().is_some_and(|m| !m.installed().is_empty())
    }

    fn description(&self, ctx: &ToolCtx) -> String {
        let langs: Vec<String> = ctx
            .lsp
            .as_ref()
            .map(|m| {
                m.installed()
                    .iter()
                    .map(|(_, ext)| format!(".{}", ext.join(" .")))
                    .collect()
            })
            .unwrap_or_default();
        format!(
            "Ask a language server about code ({} files). Operations: `definition` (where a \
             symbol is defined), `references` (where it is used), `hover` (its type and docs), \
             `symbols` (with `symbol`: search the workspace by name; with only `path`: the \
             file's outline), `diagnostics` (errors and warnings; for `path`, or everywhere). \
             For definition/references/hover give `path`, `line` and the `symbol` name on that \
             line. Edits already report new errors, so you rarely need `diagnostics`.",
            langs.join(" ")
        )
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "operation": { "type": "string", "enum": Operation::NAMES },
                "path": { "type": "string", "description": "File path, relative to the workspace root or absolute" },
                "line": { "type": "integer", "description": "1-based line number" },
                "symbol": { "type": "string", "description": "Identifier on that line, or a name to search for" },
                "column": { "type": "integer", "description": "1-based column, instead of `symbol`" }
            },
            "required": ["operation"]
        })
    }

    fn subject(&self, args: &Value, ctx: &ToolCtx) -> Result<Subject, String> {
        operation(args)?;
        Ok(Subject::Read(match opt_str(args, PATH) {
            Some(p) => paths::resolve(&ctx.root, p),
            None => ctx.root.clone(),
        }))
    }

    fn summary(&self, args: &Value, ctx: &ToolCtx) -> String {
        let op = opt_str(args, OP).unwrap_or("?");
        let mut s = format!("lsp {op}");
        if let Some(sym) = opt_str(args, SYMBOL) {
            s.push_str(&format!(" {sym}"));
        }
        if let Some(p) = opt_str(args, PATH) {
            s.push_str(&format!(" {}", paths::display(&ctx.root, &paths::resolve(&ctx.root, p))));
            if let Some(l) = opt_u64(args, &["line"]) {
                s.push_str(&format!(":{l}"));
            }
        }
        s
    }

    fn preview(&self, args: &Value, ctx: &ToolCtx) -> Result<ApprovalRequest, String> {
        Ok(ApprovalRequest::Tool {
            name: "lsp".into(),
            arguments: self.summary(args, ctx),
        })
    }

    fn run<'a>(&'a self, args: Value, ctx: &'a ToolCtx) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            let Some(mgr) = &ctx.lsp else {
                return ToolOutput::error("no language servers are configured");
            };
            let op = match operation(&args) {
                Ok(op) => op,
                Err(e) => return ToolOutput::error(e),
            };
            let to_u32 = |k: &str| opt_u64(&args, &[k]).and_then(|n| u32::try_from(n).ok());
            let query = Query {
                path: opt_str(&args, PATH).map(|p| paths::resolve(&ctx.root, p)),
                line: to_u32("line"),
                column: to_u32("column").or_else(|| to_u32("character")),
                symbol: opt_str(&args, SYMBOL).map(str::to_owned),
            };
            let root = ctx.root.clone();
            let display = move |p: &std::path::Path| paths::display(&root, p);
            let run = ops::run(mgr, op, query, &display);
            let result = tokio::select! {
                r = run => r,
                () = ctx.cancel.cancelled() => Err("interrupted by the user".into()),
            };
            match result {
                Ok(text) => {
                    let first = text.lines().next().unwrap_or_default().to_owned();
                    ToolOutput::ok(text, first)
                }
                Err(e) => ToolOutput::error(e),
            }
        })
    }
}
