//! The engine: a tokio task that owns a session, receives [`Op`]s and emits [`Event`]s.
//!
//! Clients (TUI, headless exec, ACP) hold an [`EngineHandle`] and never call subsystems
//! directly (docs/plan/00-architecture.md#engine-protocol).

mod engine;
mod env;
mod models;
mod session;
mod startup;
mod turn;

pub use engine::{Engine, EngineHandle};
pub use session::{Resume, list_sessions};

pub use cutoffharness_protocol as protocol;
