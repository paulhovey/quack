//! Wire-protocol adapters, streaming, tool-call normalization and tokenizers.
//! See docs/plan/02-providers.md.
//!
//! `openai-chat` is the baseline adapter and is always built: every build needs at least
//! one, and it covers nearly every open-model server. `ollama` speaks Ollama's native API,
//! `anthropic-messages` the Anthropic Messages API and `openai-responses` the OpenAI
//! Responses API, each behind its `proto-*` feature. `auth-chatgpt` adds the ChatGPT
//! sign-in ([`login`]).

#[cfg(feature = "proto-anthropic")]
mod anthropic;
mod error;
#[cfg(feature = "auth-chatgpt")]
pub mod login;
#[cfg(feature = "proto-ollama")]
mod ollama;
mod openai_chat;
mod provider;
mod reasoning;
#[cfg(feature = "proto-openai-responses")]
mod responses;
mod sse;
pub mod tokens;
pub mod toolparse;
mod types;

pub use error::{ErrorKind, ProviderError};
pub use provider::{Provider, http_client, supports};
pub use reqwest;
pub use types::{
    ChatRequest, DiscoveredModel, FinishReason, Message, ResponseBuilder, Sealed, StreamEvent,
    ToolCall, ToolSpec, fingerprint, unseal,
};
