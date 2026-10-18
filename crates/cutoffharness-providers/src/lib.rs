//! Wire-protocol adapters, streaming, tool-call normalization and tokenizers.
//! See docs/plan/02-providers.md.
//!
//! `openai-chat` is the baseline adapter and is always built: every build needs at least
//! one, and it covers nearly every open-model server. `ollama` speaks Ollama's native API.
//! The other `proto-*` adapters land later.

mod error;
#[cfg(feature = "proto-ollama")]
mod ollama;
mod openai_chat;
mod provider;
mod reasoning;
mod sse;
pub mod tokens;
pub mod toolparse;
mod types;

pub use error::{ErrorKind, ProviderError};
pub use provider::{Provider, http_client};
pub use reqwest;
pub use types::{
    ChatRequest, DiscoveredModel, FinishReason, Message, ResponseBuilder, StreamEvent, ToolCall,
    ToolSpec,
};
