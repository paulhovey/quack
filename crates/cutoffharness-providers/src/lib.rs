//! Wire-protocol adapters, streaming, tool-call normalization and tokenizers.
//! See docs/plan/02-providers.md.
//!
//! `openai-chat` is the baseline adapter and is always built: every build needs at least
//! one, and it covers nearly every open-model server. The other `proto-*` adapters land later.

mod error;
mod openai_chat;
mod provider;
mod reasoning;
mod sse;
pub mod tokens;
pub mod toolparse;
mod types;

pub use error::{ErrorKind, ProviderError};
pub use provider::Provider;
pub use types::{
    ChatRequest, DiscoveredModel, FinishReason, Message, ResponseBuilder, StreamEvent, ToolCall,
    ToolSpec,
};
