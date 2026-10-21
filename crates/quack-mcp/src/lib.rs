//! MCP client manager on top of rmcp (docs/plan/09-mcp.md): servers from `mcp.d/`, over
//! stdio or streamable HTTP, connected in the background.

mod ask;
mod manager;
mod output;
mod spawn;

pub use ask::{Elicited, Field, FieldKind, Form, SampleMessage, SampleRequest, Sampled, ServerAsk};
pub use manager::{
    CallOutput, MAX_RESTARTS, McpManager, PromptArg, PromptInfo, ResourceInfo, ServerSpec,
    ServerState, ServerStatus, ToolInfo, model_tool_name,
};
