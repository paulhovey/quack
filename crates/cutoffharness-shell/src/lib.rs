//! Process spawning and process-tree kill, per OS. See docs/plan/04-agent-loop-and-tools.md#shell-tool.
//!
//! Unix children get their own process group, killed as a whole on timeout or interrupt.
//! Windows uses `taskkill /T` for now; Job Objects (kill-on-close) are planned.

mod detect;
mod run;

pub use detect::ShellKind;
pub use run::{RunOptions, RunResult, run};
