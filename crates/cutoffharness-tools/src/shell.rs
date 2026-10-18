//! `shell`: run one command in the configured shell.

use std::time::Duration;

use cutoffharness_protocol::ApprovalRequest;
use cutoffharness_shell::{RunOptions, run};
use futures::future::BoxFuture;
use serde_json::{Value, json};

use crate::paths;
use crate::registry::{Subject, Tool, ToolCtx, ToolKind, ToolOutput, arg_str, opt_u64};
use crate::text::trim_middle;

/// Model-facing output budget, in bytes: head and tail are kept.
const HEAD: usize = 8 * 1024;
const TAIL: usize = 16 * 1024;
const CMD: &[&str] = &["command", "cmd", "script"];

#[derive(Debug)]
pub struct Shell;

impl Tool for Shell {
    fn name(&self) -> &'static str {
        "shell"
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Mutating
    }

    fn description(&self, ctx: &ToolCtx) -> String {
        let shell = ctx.shell.label();
        let os = std::env::consts::OS;
        format!(
            "Run a command with {shell} on {os} in the workspace root and return its exit code and \
             combined output. Write commands for {shell}. There is no stdin, so interactive \
             commands fail; pass non-interactive flags. Default timeout {}s, maximum {}s.",
            ctx.default_timeout.as_secs(),
            ctx.max_timeout.as_secs()
        )
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "The command to run" },
                "timeout_secs": { "type": "integer", "description": "Timeout in seconds" }
            },
            "required": ["command"]
        })
    }

    fn subject(&self, args: &Value, _ctx: &ToolCtx) -> Result<Subject, String> {
        Ok(Subject::Shell(arg_str(args, CMD)?.trim().to_owned()))
    }

    fn summary(&self, args: &Value, _ctx: &ToolCtx) -> String {
        format!("$ {}", arg_str(args, CMD).unwrap_or_default().trim())
    }

    fn preview(&self, args: &Value, ctx: &ToolCtx) -> Result<ApprovalRequest, String> {
        Ok(ApprovalRequest::Shell {
            command: arg_str(args, CMD)?.trim().to_owned(),
            cwd: paths::display(&ctx.root, &ctx.root),
        })
    }

    fn run<'a>(&'a self, args: Value, ctx: &'a ToolCtx) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            let command = match arg_str(&args, CMD) {
                Ok(c) => c.trim().to_owned(),
                Err(e) => return ToolOutput::error(e),
            };
            let timeout = opt_u64(&args, &["timeout_secs", "timeout"])
                .map(Duration::from_secs)
                .unwrap_or(ctx.default_timeout)
                .min(ctx.max_timeout);
            let opts = RunOptions {
                cwd: ctx.root.clone(),
                timeout,
                cancel: ctx.cancel.clone(),
                on_output: ctx.on_output.clone(),
            };
            let r = match run(&ctx.shell, &command, opts).await {
                Ok(r) => r,
                Err(e) => {
                    return ToolOutput::error(format!(
                        "could not start {}: {e}",
                        ctx.shell.program().display()
                    ));
                }
            };
            let status = if r.timed_out {
                format!(
                    "timed out after {}s (process tree killed)",
                    timeout.as_secs()
                )
            } else if r.cancelled {
                "interrupted by the user".to_owned()
            } else {
                match r.exit_code {
                    Some(c) => format!("exit code {c}"),
                    None => "killed by a signal".to_owned(),
                }
            };
            let mut body = r.output.clone();
            if r.dropped_bytes > 0 {
                body.push_str(&format!(
                    "\n… [{} more bytes not captured]",
                    r.dropped_bytes
                ));
            }
            let trimmed = trim_middle(&body, HEAD, TAIL);
            let model = if trimmed.trim().is_empty() {
                format!("{status}\n(no output)")
            } else {
                format!("{status}\n{trimmed}")
            };
            let ok = r.exit_code == Some(0);
            let lines = r.output.lines().count();
            ToolOutput {
                ok,
                model,
                summary: format!("{status} · {lines} lines"),
                detail: Some(body),
            }
        })
    }
}
