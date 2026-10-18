//! MCP client manager on top of rmcp (docs/plan/09-mcp.md): servers from `mcp.d/`, over
//! stdio or streamable HTTP, connected in the background.

mod manager;
mod output;
mod spawn;

pub use manager::{
    CallOutput, MAX_RESTARTS, McpManager, ServerSpec, ServerState, ServerStatus, ToolInfo,
    model_tool_name,
};
