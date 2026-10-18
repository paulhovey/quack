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
pub use run::{RunOptions, RunResult, Tree, run};

/// Find a program on `PATH` (`search`), the way a shell would: on Windows `npx` finds
/// `npx.cmd` through `PATHEXT`. Paths with a directory part are resolved against `cwd`.
pub fn resolve_program(
    program: &str,
    search: Option<&std::ffi::OsStr>,
    cwd: &std::path::Path,
) -> Result<std::path::PathBuf, String> {
    which::which_in(program, search, cwd).map_err(|e| format!("`{program}`: {e}"))
}
