//! Token budget, tree-sitter index, pruning and compaction. See docs/plan/05-context-management.md.
//!
//! Everything here is a pure function over the message history, so the engine can apply
//! the same operation live and again when it replays a session log.

pub mod compact;
pub mod index;
pub mod ledger;
pub mod prune;

use quack_providers::Message;

/// Indices where history can be cut without separating a tool call from its result: every
/// user message and every assistant message.
pub fn boundaries(history: &[Message]) -> impl Iterator<Item = usize> + '_ {
    history
        .iter()
        .enumerate()
        .filter(|(_, m)| matches!(m, Message::User { .. } | Message::Assistant { .. }))
        .map(|(i, _)| i)
}
