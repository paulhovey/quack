//! Headless client: `cutoffharness exec` (docs/plan/12-headless-and-acp.md).
//!
//! Scaffold: runs one turn and prints it. Approval handling, `--max-steps`, `--resume`
//! and the stable stream-json schema land in M1.

#![allow(clippy::print_stdout)]

pub mod output;

use std::io::Write;

use cutoffharness_core::EngineHandle;
use cutoffharness_protocol::{Event, InputItem, Op, TurnOverrides, Usage};
use tokio::sync::broadcast::error::RecvError;

pub use output::OutputFormat;

/// Exit codes (docs/plan/12-headless-and-acp.md#behaviour).
pub mod exit {
    pub const OK: i32 = 0;
    pub const AGENT_ERROR: i32 = 1;
    pub const CONFIG_ERROR: i32 = 2;
    pub const INTERRUPTED: i32 = 130;
}

/// Run a single prompt to completion. Returns the process exit code.
pub async fn run(mut engine: EngineHandle, prompt: String, format: OutputFormat) -> i32 {
    let op = Op::UserTurn {
        items: vec![InputItem::text(prompt)],
        overrides: TurnOverrides::default(),
    };
    if !engine.submit(op).await {
        return exit::AGENT_ERROR;
    }
    let mut stdout = std::io::stdout().lock();
    let mut text = String::new();
    let mut session_id = None;
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
        match event {
            Event::SessionConfigured { session } => session_id = Some(session.id.0),
            Event::AssistantDelta { text: delta } => {
                if format == OutputFormat::Text {
                    let _ = write!(stdout, "{delta}");
                    let _ = stdout.flush();
                }
                text.push_str(&delta);
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
                finish(
                    &mut stdout,
                    format,
                    &text,
                    session_id.as_deref(),
                    Usage::default(),
                    "interrupted",
                );
                break exit::INTERRUPTED;
            }
            Event::Error { error } => {
                let _ = writeln!(std::io::stderr(), "error: {}", error.message);
                break exit::AGENT_ERROR;
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
            let _ = writeln!(out);
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
