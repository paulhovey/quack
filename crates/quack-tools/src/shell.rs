//! `shell`: run one command in the configured shell.

use std::time::Duration;

use futures::future::BoxFuture;
use quack_protocol::ApprovalRequest;
use quack_shell::{RunOptions, run};
use serde_json::{Value, json};

use crate::paths;
use crate::registry::{
    SandboxState, Subject, Tool, ToolCtx, ToolKind, ToolOutput, arg_str, opt_u64,
};
use crate::text::trim_middle;

/// Model-facing output budget, in bytes: head and tail are kept.
const HEAD: usize = 8 * 1024;
const TAIL: usize = 16 * 1024;
const CMD: &[&str] = &["command", "cmd", "script"];

#[derive(Debug)]
pub struct Shell;

/// The policy to run this call under; `None` runs it outside the sandbox.
fn policy(args: &Value, ctx: &ToolCtx) -> Option<std::sync::Arc<quack_sandbox::Policy>> {
    match &ctx.sandbox {
        SandboxState::On(p) if !escapes(args) => Some(p.clone()),
        _ => None,
    }
}

/// The model asked to run outside the sandbox (`"sandbox": false`).
fn escapes(args: &Value) -> bool {
    match args.get("sandbox") {
        Some(Value::Bool(b)) => !b,
        Some(Value::String(s)) => s.eq_ignore_ascii_case("false"),
        _ => false,
    }
}

/// Whether this call runs outside a sandbox that is configured.
fn outside(args: &Value, ctx: &ToolCtx) -> bool {
    match &ctx.sandbox {
        SandboxState::Off => false,
        SandboxState::On(_) => escapes(args),
        SandboxState::Unavailable(_) => true,
    }
}

impl Tool for Shell {
    fn name(&self) -> &str {
        "shell"
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Mutating
    }

    fn description(&self, ctx: &ToolCtx) -> String {
        let shell = ctx.shell.label();
        let os = std::env::consts::OS;
        let sandbox = match &ctx.sandbox {
            SandboxState::On(p) => format!(
                " Commands run in a sandbox: they can read anything but write only in the \
                 workspace and temp folders{}. If the sandbox blocks a command you need, run \
                 it again with `sandbox: false`; the user is asked to approve that.",
                if p.network { "" } else { ", with no network" }
            ),
            _ => String::new(),
        };
        format!(
            "Run a command with {shell} on {os} in the workspace root and return its exit code and \
             combined output. Write commands for {shell}. There is no stdin, so interactive \
             commands fail; pass non-interactive flags. Default timeout {}s, maximum {}s.{sandbox}",
            ctx.default_timeout.as_secs(),
            ctx.max_timeout.as_secs()
        )
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "The command to run" },
                "timeout_secs": { "type": "integer", "description": "Timeout in seconds" },
                "sandbox": { "type": "boolean", "description": "Set false only to rerun a command the sandbox blocked" }
            },
            "required": ["command"]
        })
    }

    fn subject(&self, args: &Value, ctx: &ToolCtx) -> Result<Subject, String> {
        let command = arg_str(args, CMD)?.trim().to_owned();
        Ok(if outside(args, ctx) {
            Subject::ShellOutsideSandbox(command)
        } else {
            Subject::Shell(command)
        })
    }

    fn summary(&self, args: &Value, _ctx: &ToolCtx) -> String {
        format!("$ {}", arg_str(args, CMD).unwrap_or_default().trim())
    }

    fn preview(&self, args: &Value, ctx: &ToolCtx) -> Result<ApprovalRequest, String> {
        Ok(ApprovalRequest::Shell {
            command: arg_str(args, CMD)?.trim().to_owned(),
            cwd: paths::display(&ctx.root, &ctx.root),
            outside_sandbox: outside(args, ctx),
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
                sandbox: policy(&args, ctx),
            };
            let sandboxed = opts.sandbox.is_some();
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
            let mut model = if trimmed.trim().is_empty() {
                format!("{status}\n(no output)")
            } else {
                format!("{status}\n{trimmed}")
            };
            let ok = r.exit_code == Some(0);
            if sandboxed && !ok && quack_sandbox::looks_blocked(&r.output) {
                model.push_str(
                    "\n\nThis command ran in the sandbox (writes only in the workspace and temp \
                     folders; network as configured). If the sandbox caused the failure and the \
                     command is needed, run it again with `sandbox: false` to ask the user.",
                );
            }
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
