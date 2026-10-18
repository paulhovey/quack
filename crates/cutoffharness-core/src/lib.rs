//! The engine: a tokio task that owns a session, receives [`Op`]s and emits [`Event`]s.
//!
//! Clients (TUI, headless exec, ACP) hold an [`EngineHandle`] and never call subsystems
//! directly (docs/plan/00-architecture.md#engine-protocol).
//!
//! Scaffold status: configuration and provider files are loaded for real; the other
//! startup subsystems report placeholder progress, and turns are answered by a stub
//! until the provider adapters land in M1.

mod engine;
mod startup;

pub use engine::{Engine, EngineHandle};

pub use cutoffharness_protocol as protocol;
