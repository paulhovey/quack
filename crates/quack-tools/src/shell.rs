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
            let distilled = distill_output(&body);
            let trimmed = trim_middle(&distilled, HEAD, TAIL);
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
                images: Vec::new(),
                summary: format!("{status} · {lines} lines"),
                full: (trimmed != body).then(|| body.clone()),
                detail: Some(body),
            }
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
enum LineKind {
    PassTest,
    Compiling,
    Other,
}

fn classify_line(line: &str) -> LineKind {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return LineKind::Other;
    }

    if trimmed.contains("FAILED")
        || trimmed.contains("FAIL")
        || trimmed.contains("Error")
        || trimmed.contains("error")
        || trimmed.contains("Traceback")
        || trimmed.contains("AssertionError")
        || trimmed.contains("failures:")
    {
        return LineKind::Other;
    }

    if (trimmed.starts_with("test ")
        && (trimmed.ends_with("... ok") || trimmed.ends_with("... ignored")))
        || trimmed.ends_with(" ... ok")
        || trimmed.ends_with(" PASSED")
        || trimmed.starts_with("PASSED ")
        || trimmed.starts_with("--- PASS:")
        || trimmed.starts_with("✓ ")
        || (trimmed.starts_with("PASS ") && !trimmed.contains("FAIL"))
        || (trimmed.starts_with("ok ") && !trimmed.contains("not ok"))
    {
        return LineKind::PassTest;
    }

    if trimmed.starts_with("Compiling ")
        || (trimmed.starts_with('[')
            && trimmed.contains(']')
            && (trimmed.contains("Building ") || trimmed.contains("Compiling ")))
    {
        return LineKind::Compiling;
    }

    LineKind::Other
}

pub(crate) fn distill_output(raw: &str) -> String {
    let lines: Vec<&str> = raw.lines().collect();
    if lines.len() <= 10 {
        return raw.to_string();
    }

    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut i = 0;
    while i < lines.len() {
        match classify_line(lines[i]) {
            LineKind::PassTest => {
                let start = i;
                while i < lines.len() && matches!(classify_line(lines[i]), LineKind::PassTest) {
                    i += 1;
                }
                let count = i - start;
                if count >= 3 {
                    out.push(format!("[... {count} passed tests omitted ...]"));
                } else {
                    for line in &lines[start..i] {
                        out.push((*line).to_string());
                    }
                }
            }
            LineKind::Compiling => {
                let start = i;
                while i < lines.len() && matches!(classify_line(lines[i]), LineKind::Compiling) {
                    i += 1;
                }
                let count = i - start;
                if count >= 4 {
                    out.push(format!("[... compiled {count} packages ...]"));
                } else {
                    for line in &lines[start..i] {
                        out.push((*line).to_string());
                    }
                }
            }
            LineKind::Other => {
                out.push(lines[i].to_string());
                i += 1;
            }
        }
    }
    let mut result = out.join("\n");
    if raw.ends_with('\n') {
        result.push('\n');
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distills_passing_tests() {
        let mut raw = String::from("running 20 tests\n");
        for i in 1..=15 {
            raw.push_str(&format!("test test_{i} ... ok\n"));
        }
        raw.push_str("test test_fail ... FAILED\n");
        raw.push_str("failures:\n    test_fail\n");
        raw.push_str("test result: FAILED. 15 passed; 1 failed\n");

        let distilled = distill_output(&raw);
        assert!(
            distilled.contains("[... 15 passed tests omitted ...]"),
            "{distilled}"
        );
        assert!(
            distilled.contains("test test_fail ... FAILED"),
            "{distilled}"
        );
        assert!(
            distilled.contains("failures:\n    test_fail"),
            "{distilled}"
        );
    }

    #[test]
    fn preserves_short_output() {
        let raw = "test a ... ok\ntest b ... ok\n";
        assert_eq!(distill_output(raw), raw);
    }
}
