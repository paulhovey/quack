//! Headless client: `cutoffharness exec` (docs/plan/12-headless-and-acp.md).
//!
//! The answer goes to stdout; tool activity and errors go to stderr in text mode.
//! Nobody can answer an approval prompt here, so anything that would ask is denied and
//! the model is told why. `--permission-mode` or `[permissions] allow` rules widen this.

#![allow(clippy::print_stdout)]

pub mod output;

use std::io::Write;

use cutoffharness_core::EngineHandle;
use cutoffharness_protocol::{
    ApprovalDecision, Event, InputItem, LoadState, Op, Subsystem, TurnOverrides, Usage,
};
use tokio::sync::broadcast::error::RecvError;

pub use output::OutputFormat;

/// Exit codes (docs/plan/12-headless-and-acp.md#behaviour).
pub mod exit {
    pub const OK: i32 = 0;
    pub const AGENT_ERROR: i32 = 1;
    pub const CONFIG_ERROR: i32 = 2;
    pub const INTERRUPTED: i32 = 130;
}

const HEADLESS_DENIAL: &str = "this run is headless and nobody can approve actions. \
Continue without it if you can; otherwise explain what you would run. \
(The user can allow it with --permission-mode or a [permissions] allow rule.)";

/// Run a single prompt to completion. Returns the process exit code.
pub async fn run(mut engine: EngineHandle, prompt: String, format: OutputFormat) -> i32 {
    // A headless run is one turn, so wait for MCP servers to connect (each within its
    // own startup timeout) and their tools to be there before sending the prompt.
    let mut prompt = Some(prompt);
    let mut stdout = std::io::stdout().lock();
    let mut stderr = std::io::stderr();
    let mut text = String::new();
    let mut session_id = None;
    let mut failed = false;
    let code = loop {
        let event = match engine.events.recv().await {
            Ok(ev) => ev,
            Err(RecvError::Lagged(_)) => continue,
            Err(RecvError::Closed) => break exit::AGENT_ERROR,
        };
        if format == OutputFormat::StreamJson {
            let _ = writeln!(
                stdout,
                "{}",
                serde_json::to_string(&event).unwrap_or_default()
            );
        }
        let text_mode = format == OutputFormat::Text;
        if let Event::StartupProgress {
            subsystem: Subsystem::Mcp,
            state,
        } = &event
            && !matches!(state, LoadState::Pending | LoadState::Running { .. })
            && let Some(prompt) = prompt.take()
        {
            if let LoadState::NeedsAttention { detail } = state
                && text_mode
            {
                let _ = writeln!(stderr, "warning: MCP: {detail}");
            }
            let op = Op::UserTurn {
                items: vec![InputItem::text(prompt)],
                overrides: TurnOverrides::default(),
            };
            if !engine.submit(op).await {
                break exit::AGENT_ERROR;
            }
        }
        match event {
            Event::SessionConfigured { session } => session_id = Some(session.id.0),
            Event::AssistantDelta { text: delta } => {
                if text_mode {
                    let _ = write!(stdout, "{delta}");
                    let _ = stdout.flush();
                }
                text.push_str(&delta);
            }
            Event::ToolCallBegin { call } => {
                if !text.is_empty() && !text.ends_with('\n') {
                    text.push('\n');
                    if text_mode {
                        let _ = writeln!(stdout);
                    }
                }
                if text_mode {
                    let _ = writeln!(stderr, "› {}", call.summary);
                }
            }
            Event::ToolCallEnd { result, .. } => {
                if text_mode && !result.ok {
                    let _ = writeln!(stderr, "  ✗ {}", result.summary);
                }
            }
            Event::ApprovalRequest { call_id, .. } => {
                let _ = engine
                    .ops
                    .send(Op::ApprovalResponse {
                        call_id,
                        decision: ApprovalDecision::DenyWithFeedback {
                            feedback: HEADLESS_DENIAL.into(),
                        },
                    })
                    .await;
                if text_mode {
                    let _ = writeln!(stderr, "  ✗ denied: needs approval (see --permission-mode)");
                }
            }
            Event::Compacting { reason } => {
                if text_mode {
                    let _ = writeln!(stderr, "› summarizing earlier conversation ({reason})");
                }
            }
            Event::Warning { message } => {
                if text_mode {
                    let _ = writeln!(stderr, "warning: {message}");
                }
            }
            Event::TurnComplete { usage, .. } => {
                finish(
                    &mut stdout,
                    format,
                    &text,
                    session_id.as_deref(),
                    usage,
                    "complete",
                );
                break exit::OK;
            }
            Event::TurnAborted { .. } => {
                let (reason, code) = if failed {
                    ("error", exit::AGENT_ERROR)
                } else {
                    ("interrupted", exit::INTERRUPTED)
                };
                finish(
                    &mut stdout,
                    format,
                    &text,
                    session_id.as_deref(),
                    Usage::default(),
                    reason,
                );
                break code;
            }
            Event::Error { error } => {
                failed = true;
                if format != OutputFormat::StreamJson {
                    let _ = writeln!(stderr, "error: {}", error.message);
                }
            }
            _ => {}
        }
    };
    drop(stdout);
    engine.shutdown().await;
    code
}

fn finish(
    out: &mut impl Write,
    format: OutputFormat,
    text: &str,
    session: Option<&str>,
    usage: Usage,
    reason: &str,
) {
    match format {
        OutputFormat::Text => {
            if !text.is_empty() {
                let _ = writeln!(out);
            }
        }
        OutputFormat::Json => {
            let obj = serde_json::json!({
                "result": text,
                "session_id": session,
                "usage": usage,
                "exit_reason": reason,
            });
            let _ = writeln!(out, "{obj}");
        }
        OutputFormat::StreamJson => {}
    }
}
