//! Internal tools, the tool registry and the permission engine.
//! See docs/plan/04-agent-loop-and-tools.md and 11-security.md.

mod edit;
mod memory;
pub mod paths;
pub mod permissions;
mod read;
mod registry;
mod search;
mod shell;
mod skill;
mod text;
mod write;

pub use permissions::{Decision, Permissions};
pub use registry::{ReadTracker, Registry, Subject, Tool, ToolCtx, ToolKind, ToolOutput};
pub use skill::expand as expand_skill;
