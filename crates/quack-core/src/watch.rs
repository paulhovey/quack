//! Noticing edited config files (docs/plan/01-configuration.md#hot-reload).
//!
//! Every few seconds, off the async threads, the files in the system, user and project
//! config folders are stamped (path, size, modification time). A change that holds still
//! for one more check is reported once as a notice that `/reload` applies it. Nothing is
//! reloaded on its own yet: a reload restarts every MCP server and drops the session's
//! approvals, which an edit to a skill should not do. Polling needs no new crates.

use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use quack_config::Loaded;
use quack_protocol::Event;
use tokio::sync::{broadcast, watch};

const EVERY: Duration = Duration::from_secs(2);

/// Folders inside a config folder that hold no configuration.
const SKIP: &[&str] = &["memory", "worktrees", "schemas"];

/// Watch the config folders until the engine (the sender of `config`) is gone.
pub(crate) fn spawn(mut config: watch::Receiver<Arc<Loaded>>, tx: broadcast::Sender<Event>) {
    tokio::spawn(async move {
        let mut roots = folders(&config.borrow_and_update());
        let mut baseline = stamp(roots.clone()).await;
        let mut pending = None;
        let mut tick = tokio::time::interval(EVERY);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                changed = config.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    // A reload read the files: what they hold now is in effect.
                    roots = folders(&config.borrow_and_update());
                    baseline = stamp(roots.clone()).await;
                    pending = None;
                }
                _ = tick.tick() => {
                    let now = stamp(roots.clone()).await;
                    if now == baseline {
                        pending = None;
                    } else if pending == Some(now) {
                        baseline = now;
                        pending = None;
                        let _ = tx.send(Event::Notice {
                            message: "config files changed · /reload to apply them".into(),
                        });
                    } else {
                        pending = Some(now);
                    }
                }
            }
        }
    });
}

/// The config folders a user edits: system, user, and the project's `.quack/`.
fn folders(config: &Loaded) -> Vec<PathBuf> {
    let d = &config.dirs;
    vec![d.system.clone(), d.user_config.clone(), d.project.clone()]
}

async fn stamp(roots: Vec<PathBuf>) -> u64 {
    tokio::task::spawn_blocking(move || {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for root in &roots {
            walk(root, 0, &mut h);
        }
        h.finish()
    })
    .await
    .unwrap_or_default()
}

fn walk(dir: &Path, depth: usize, h: &mut impl Hasher) {
    if depth > 6 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.filter_map(Result::ok).collect();
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for e in entries {
        let name = e.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || name.ends_with(".tmp") || SKIP.contains(&name.as_ref()) {
            continue;
        }
        let Ok(meta) = e.metadata() else {
            continue;
        };
        if meta.is_dir() {
            walk(&e.path(), depth + 1, h);
        } else {
            e.path().hash(h);
            meta.len().hash(h);
            meta.modified().ok().hash(h);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stamps_change_with_config_files_only() {
        let tmp = tempfile::tempdir().expect("tmp");
        let root = tmp.path().to_path_buf();
        std::fs::create_dir_all(root.join("mcp.d")).expect("mkdir");
        std::fs::write(root.join("config.toml"), "").expect("write");
        let first = stamp(vec![root.clone()]).await;
        assert_eq!(stamp(vec![root.clone()]).await, first);

        std::fs::create_dir_all(root.join("memory")).expect("mkdir");
        std::fs::write(root.join("memory").join("note.md"), "x").expect("write");
        std::fs::write(root.join("config.toml.tmp"), "x").expect("write");
        assert_eq!(stamp(vec![root.clone()]).await, first, "not configuration");

        std::fs::write(root.join("mcp.d").join("gh.toml"), "command = \"gh\"").expect("write");
        assert_ne!(stamp(vec![root]).await, first);
    }
}
