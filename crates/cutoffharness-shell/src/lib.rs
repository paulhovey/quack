//! Process spawning and process-tree kill, per OS. See docs/plan/04-agent-loop-and-tools.md#shell-tool.
//!
//! Unix children get their own process group, killed as a whole on timeout or interrupt.
//! Windows children go into a Job Object that kills everything in it when closed, so
//! nothing outlives the command, or the harness; `taskkill /T` is the fallback.

mod detect;
#[cfg(windows)]
mod job;
mod run;

pub use detect::ShellKind;
pub use run::{RunOptions, RunResult, run};
