//! Model profiles: loading, selection, prompt rendering and tool-surface mapping.
//! See docs/plan/03-model-profiles.md.

mod loader;
mod render;
mod surface;

pub use loader::{Profile, load_profiles, select};
pub use render::PromptVars;
pub use surface::{InternalTool, SurfaceTool, ToolSurface};
