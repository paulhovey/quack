//! Internal tools, the tool registry and the permission engine.
//! See docs/plan/04-agent-loop-and-tools.md and 11-security.md.

mod ask;
mod blocks;
mod edit;
pub mod grammar;
mod hashline;
mod image;
mod index;
mod lsp;
mod memory;
pub mod outline;
mod patch;
pub mod paths;
pub mod permissions;
#[cfg(feature = "lang-common")]
mod ps_split;
mod read;
mod registry;
mod search;
mod shell;
mod shell_split;
mod skill;
mod task;
mod text;
mod todo;
mod write;

pub use ask::{Answers, AskRequest, Asker};
pub use permissions::{Decision, Permissions};
pub use registry::{
    ReadTracker, Registry, SandboxState, Subject, Tool, ToolCtx, ToolKind, ToolOutput,
};
pub use skill::expand as expand_skill;
pub use task::{AgentInfo, SpawnRequest, Spawner, TaskReport, TaskRequest};
pub use todo::Plan;
