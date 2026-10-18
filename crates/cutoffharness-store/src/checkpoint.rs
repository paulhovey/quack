//! File pre-images for undo (docs/plan/07-sessions-memory-checkpoints.md#checkpoints-file-undo).
//!
//! Before an edit tool changes a file, its bytes (or the fact that it did not exist) are
//! saved under `<session>/checkpoints/`. `manifest.jsonl` records each saved pre-image, the
//! hash of the file after the call, and undos. Restoring writes the saved bytes back, unless
//! the file changed again since the call (someone else edited it), which is reported as a
//! conflict.

use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
enum Line {
    /// A file is about to change. `seq` names the blob, if the file existed.
    Pre {
        seq: u64,
        turn: u64,
        call_id: String,
        path: PathBuf,
        existed: bool,
    },
    /// The call finished; `hash` is the file afterwards (`None`: it does not exist).
    Post {
        seq: u64,
        ok: bool,
        hash: Option<u64>,
    },
    /// The changes of a call were undone.
    Undo { call_id: String },
}

/// One file changed by one call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub seq: u64,
    pub turn: u64,
    pub call_id: String,
    pub path: PathBuf,
    pub existed: bool,
    pub ok: bool,
    pub after: Option<u64>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Restored {
    pub restored: Vec<PathBuf>,
    /// Files changed since the call, left alone.
    pub conflicts: Vec<PathBuf>,
}

#[derive(Debug)]
pub struct Checkpoints {
    dir: PathBuf,
    next: Mutex<u64>,
}

impl Checkpoints {
    /// `dir` is `<session>/checkpoints`; created on first use.
    pub fn open(dir: impl Into<PathBuf>) -> Self {
        let dir = dir.into();
        let next = read_lines(&dir)
            .iter()
            .filter_map(|l| match l {
                Line::Pre { seq, .. } => Some(seq + 1),
                _ => None,
            })
            .max()
            .unwrap_or(0);
        Self {
            dir,
            next: Mutex::new(next),
        }
    }

    fn append(&self, line: &Line) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join("manifest.jsonl"))?;
        let mut text = serde_json::to_string(line).map_err(std::io::Error::other)?;
        text.push('\n');
        f.write_all(text.as_bytes())
    }

    fn blob(&self, seq: u64) -> PathBuf {
        self.dir.join(format!("{seq:06}.pre"))
    }

    /// Save `path` as it is now, before `call_id` changes it. Returns the entry's number.
    pub fn before(&self, turn: u64, call_id: &str, path: &Path) -> std::io::Result<u64> {
        let seq = {
            let mut n = self
                .next
                .lock()
                .map_err(|_| std::io::Error::other("poisoned"))?;
            let seq = *n;
            *n += 1;
            seq
        };
        let existed = match std::fs::read(path) {
            Ok(bytes) => {
                std::fs::create_dir_all(&self.dir)?;
                std::fs::write(self.blob(seq), bytes)?;
                true
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => return Err(e),
        };
        self.append(&Line::Pre {
            seq,
            turn,
            call_id: call_id.to_owned(),
            path: path.to_path_buf(),
            existed,
        })?;
        Ok(seq)
    }

    /// Record the file after the call.
    pub fn after(&self, seq: u64, path: &Path, ok: bool) -> std::io::Result<()> {
        self.append(&Line::Post {
            seq,
            ok,
            hash: hash_file(path),
        })
    }

    /// Changes not undone yet, oldest first. Calls that failed are left out.
    pub fn changes(&self) -> Vec<Change> {
        let lines = read_lines(&self.dir);
        let mut out: Vec<Change> = Vec::new();
        for l in &lines {
            match l {
                Line::Pre {
                    seq,
                    turn,
                    call_id,
                    path,
                    existed,
                } => out.push(Change {
                    seq: *seq,
                    turn: *turn,
                    call_id: call_id.clone(),
                    path: path.clone(),
                    existed: *existed,
                    ok: false,
                    after: None,
                }),
                Line::Post { seq, ok, hash } => {
                    if let Some(c) = out.iter_mut().find(|c| c.seq == *seq) {
                        c.ok = *ok;
                        c.after = *hash;
                    }
                }
                Line::Undo { call_id } => out.retain(|c| &c.call_id != call_id),
            }
        }
        out.retain(|c| c.ok);
        out
    }

    /// The changes of the most recent call that changed files.
    pub fn last_call(&self) -> Vec<Change> {
        let changes = self.changes();
        let Some(last) = changes.last().map(|c| c.call_id.clone()) else {
            return Vec::new();
        };
        changes.into_iter().filter(|c| c.call_id == last).collect()
    }

    /// Put the files of `changes` back as they were before; newest change first, so a
    /// call that touched a file twice ends at the oldest state. With `force`, files changed
    /// since are overwritten too.
    pub fn restore(&self, changes: &[Change], force: bool) -> std::io::Result<Restored> {
        let mut result = Restored::default();
        for c in changes.iter().rev() {
            if !force && hash_file(&c.path) != c.after {
                if !result.conflicts.contains(&c.path) {
                    result.conflicts.push(c.path.clone());
                }
                continue;
            }
            if c.existed {
                let bytes = std::fs::read(self.blob(c.seq))?;
                if let Some(parent) = c.path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&c.path, bytes)?;
            } else if c.path.exists() {
                std::fs::remove_file(&c.path)?;
            }
            if !result.restored.contains(&c.path) {
                result.restored.push(c.path.clone());
            }
        }
        if result.conflicts.is_empty() {
            let mut calls: Vec<&str> = changes.iter().map(|c| c.call_id.as_str()).collect();
            calls.dedup();
            for call_id in calls {
                self.append(&Line::Undo {
                    call_id: call_id.to_owned(),
                })?;
            }
        }
        Ok(result)
    }
}

fn read_lines(dir: &Path) -> Vec<Line> {
    let Ok(f) = std::fs::File::open(dir.join("manifest.jsonl")) else {
        return Vec::new();
    };
    BufReader::new(f)
        .lines()
        .map_while(Result::ok)
        .filter_map(|l| serde_json::from_str(&l).ok())
        .collect()
}

/// FNV-1a over the file's bytes; `None` when it does not exist. Only used to notice that a
/// file changed, not for security.
fn hash_file(path: &Path) -> Option<u64> {
    let bytes = std::fs::read(path).ok()?;
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    Some(h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn undo_restores_bytes_and_removes_new_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cp = Checkpoints::open(tmp.path().join("cp"));
        let a = tmp.path().join("a.txt");
        let b = tmp.path().join("new/b.txt");
        std::fs::write(&a, b"one\r\n").expect("write");

        let s = cp.before(1, "c1", &a).expect("before");
        std::fs::write(&a, b"two\r\n").expect("edit");
        cp.after(s, &a, true).expect("after");
        let s = cp.before(1, "c2", &b).expect("before");
        std::fs::create_dir_all(b.parent().expect("parent")).expect("mkdir");
        std::fs::write(&b, b"new").expect("create");
        cp.after(s, &b, true).expect("after");
        // A failed call is not undoable.
        let s = cp.before(1, "c3", &a).expect("before");
        cp.after(s, &a, false).expect("after");

        let last = cp.last_call();
        assert_eq!(last.len(), 1);
        assert_eq!(last[0].call_id, "c2");
        let r = cp.restore(&last, false).expect("restore");
        assert_eq!(r.restored, std::slice::from_ref(&b));
        assert!(!b.exists());

        // Reopening continues from the manifest.
        let cp = Checkpoints::open(tmp.path().join("cp"));
        let last = cp.last_call();
        assert_eq!(last[0].call_id, "c1");
        cp.restore(&last, false).expect("restore");
        assert_eq!(std::fs::read(&a).expect("read"), b"one\r\n");
        assert!(cp.last_call().is_empty());
    }

    #[test]
    fn files_changed_since_are_conflicts() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cp = Checkpoints::open(tmp.path().join("cp"));
        let a = tmp.path().join("a.txt");
        std::fs::write(&a, "one").expect("write");
        let s = cp.before(1, "c1", &a).expect("before");
        std::fs::write(&a, "two").expect("edit");
        cp.after(s, &a, true).expect("after");
        std::fs::write(&a, "user edit").expect("user");

        let last = cp.last_call();
        let r = cp.restore(&last, false).expect("restore");
        assert_eq!(r.conflicts, std::slice::from_ref(&a));
        assert_eq!(std::fs::read_to_string(&a).expect("read"), "user edit");
        assert_eq!(cp.last_call().len(), 1, "still undoable");
        let r = cp.restore(&last, true).expect("force");
        assert_eq!(r.restored, std::slice::from_ref(&a));
        assert_eq!(std::fs::read_to_string(&a).expect("read"), "one");
    }
}
