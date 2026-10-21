//! Syntax highlighting for fenced code in the transcript (docs/plan/06-tui.md#rendering-performance),
//! with the engine's tree-sitter grammars and their `grammars/` overrides.
//!
//! Drawing only reads the cache. The app fills it as assistant text arrives: a closed code
//! block is highlighted at once when it is small and its language's queries are compiled,
//! and otherwise on a blocking thread (the first block in a language compiles them, which
//! takes tens of milliseconds), showing plain until the result comes back.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use quack_core::grammar::{Grammars, Highlighted, language_for_tag};
use tokio::sync::mpsc;

/// Blocks up to this many bytes are highlighted on the spot.
const INLINE_MAX: usize = 8 * 1024;
/// Larger blocks are left plain.
const MAX: usize = 256 * 1024;
/// Highlighted blocks kept; the cache starts over past this.
const KEEP: usize = 512;

pub(crate) type Runs = Arc<Vec<Highlighted>>;
type Done = (u64, Option<Runs>);

#[derive(Debug)]
pub(crate) struct Highlights {
    grammars: Arc<Grammars>,
    ready: HashMap<u64, Runs>,
    pending: HashSet<u64>,
    tx: mpsc::UnboundedSender<Done>,
    rx: Option<mpsc::UnboundedReceiver<Done>>,
}

fn key(lang: &str, code: &str) -> u64 {
    let mut h = std::hash::DefaultHasher::new();
    (lang, code).hash(&mut h);
    h.finish()
}

impl Highlights {
    pub fn new(grammars: Arc<Grammars>) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self {
            grammars,
            ready: HashMap::new(),
            pending: HashSet::new(),
            tx,
            rx: Some(rx),
        }
    }

    /// Where results from the blocking thread arrive; the event loop takes it once.
    pub fn take_results(&mut self) -> Option<mpsc::UnboundedReceiver<Done>> {
        self.rx.take()
    }

    /// New grammars (after `/reload`): everything is highlighted again.
    pub fn reset(&mut self, grammars: Arc<Grammars>) {
        self.grammars = grammars;
        self.ready.clear();
        self.pending.clear();
    }

    /// The runs for a block, if they are ready.
    pub fn get(&self, tag: &str, code: &str) -> Option<&Runs> {
        let lang = language_for_tag(tag)?;
        self.ready.get(&key(lang, code))
    }

    /// Highlight the closed code blocks of `markdown` that are not done yet. Returns
    /// whether any became ready now.
    pub fn scan(&mut self, markdown: &str) -> bool {
        let mut changed = false;
        for (tag, code) in crate::markdown::code_blocks(markdown) {
            let Some(lang) = language_for_tag(&tag) else {
                continue;
            };
            let k = key(lang, &code);
            if code.len() > MAX || self.ready.contains_key(&k) || self.pending.contains(&k) {
                continue;
            }
            if self.ready.len() >= KEEP {
                self.ready.clear();
            }
            let handle = tokio::runtime::Handle::try_current();
            match handle {
                Ok(handle) if code.len() > INLINE_MAX || !self.grammars.highlight_ready(lang) => {
                    self.pending.insert(k);
                    let (grammars, tx) = (self.grammars.clone(), self.tx.clone());
                    handle.spawn_blocking(move || {
                        let runs = grammars.highlight(lang, &code).map(Arc::new);
                        let _ = tx.send((k, runs));
                    });
                }
                _ => {
                    if let Some(runs) = self.grammars.highlight(lang, &code) {
                        self.ready.insert(k, Arc::new(runs));
                        changed = true;
                    }
                }
            }
        }
        changed
    }

    /// A result from the blocking thread.
    pub fn done(&mut self, (k, runs): Done) {
        if self.pending.remove(&k)
            && let Some(runs) = runs
        {
            self.ready.insert(k, runs);
        }
    }
}
