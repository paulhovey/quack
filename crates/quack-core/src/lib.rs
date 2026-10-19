//! The engine: a tokio task that owns a session, receives [`Op`]s and emits [`Event`]s.
//!
//! Clients (TUI, headless exec, ACP) hold an [`EngineHandle`] and never call subsystems
//! directly (docs/plan/00-architecture.md#engine-protocol).

mod context;
pub mod doctor;
mod engine;
mod env;
mod hooks;
mod lsp;
mod mcp;
mod models;
mod sandbox;
mod session;
pub mod setup;
mod startup;
mod stats;
mod turn;

pub use engine::{Engine, EngineHandle};
pub use session::{Resume, list_sessions};

pub use quack_protocol as protocol;
