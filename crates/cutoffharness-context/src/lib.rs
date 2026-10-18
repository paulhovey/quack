//! Token budget, tree-sitter index, pruning and compaction. See docs/plan/05-context-management.md.
//!
//! Placeholder crate: real implementation lands in M2.

pub mod compact;
pub mod index;
pub mod ledger;
pub mod prune;
