//! The ACP messages quack reads (protocol version 1, <https://agentclientprotocol.com>).
//!
//! Only the fields quack uses; anything else is ignored. Replies and notifications are
//! written with `json!` (`convert.rs`, `session.rs`). The schema crate is left out on
//! purpose: it turns on serde_json's `preserve_order` for the whole binary, which would
//! change the key order of every request body sent to models.

use std::path::PathBuf;

use serde::Deserialize;
use serde_json::Value;

/// ACP protocol version 1, the one quack speaks.
pub(crate) const PROTOCOL_VERSION: u16 = 1;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Initialize {
    #[serde(default)]
    pub protocol_version: u16,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NewSession {
    pub cwd: PathBuf,
    #[serde(default)]
    pub mcp_servers: Vec<McpServer>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LoadSession {
    pub session_id: String,
    pub cwd: PathBuf,
    #[serde(default)]
    pub mcp_servers: Vec<McpServer>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Prompt {
    pub session_id: String,
    pub prompt: Vec<ContentBlock>,
}

/// `session/cancel` and `session/close`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionRef {
    pub session_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SetMode {
    pub session_id: String,
    pub mode_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SetConfigOption {
    pub session_id: String,
    pub config_id: String,
    /// A select's value id (a string); booleans are not used by quack.
    pub value: Value,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ContentBlock {
    Text {
        text: String,
    },
    ResourceLink {
        uri: String,
    },
    Resource {
        resource: EmbeddedResource,
    },
    /// Images and audio, which quack does not take yet.
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
pub(crate) struct EmbeddedResource {
    pub uri: String,
    /// Absent for binary contents (`blob`).
    #[serde(default)]
    pub text: Option<String>,
}

/// An editor's MCP server. stdio servers carry no `type`; `http` and `sse` do.
#[derive(Debug, Deserialize)]
pub(crate) struct McpServer {
    pub name: String,
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    #[serde(default)]
    pub command: Option<PathBuf>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: Vec<NameValue>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub headers: Vec<NameValue>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct NameValue {
    pub name: String,
    pub value: String,
}

/// The editor's answer to `session/request_permission`.
#[derive(Debug, Deserialize)]
pub(crate) struct PermissionResponse {
    pub outcome: PermissionOutcome,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub(crate) enum PermissionOutcome {
    Selected {
        #[serde(rename = "optionId")]
        option_id: String,
    },
    #[serde(other)]
    Cancelled,
}
