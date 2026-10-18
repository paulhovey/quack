//! Wire-protocol adapters, streaming, tool-call normalization and tokenizers. See docs/plan/02-providers.md.
//!
//! Placeholder crate: real implementation lands in M1.

pub mod adapter;
pub mod http;
pub mod reasoning;
pub mod tokenizer;
pub mod toolparse;
