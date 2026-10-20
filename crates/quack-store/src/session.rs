//! Append-only JSONL session logs, a per-project index, and loading for resume.
//!
//! Layout: `<sessions>/<project-key>/index.jsonl` plus one directory per session holding
//! `events.jsonl`. Every line is `{"v":1,"t":"<type>","ts":"…", …payload}`. The store
//! does not interpret payloads; the engine owns their meaning.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::time;

pub const FORMAT_VERSION: u64 = 1;

/// One line of `index.jsonl`. Later lines for the same id update earlier ones.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexEntry {
    pub id: String,
    pub dir: String,
    pub started: u64,
    pub updated: u64,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub turns: u32,
}

/// Sessions of one project.
#[derive(Debug, Clone)]
pub struct ProjectSessions {
    pub dir: PathBuf,
}

impl ProjectSessions {
    pub fn new(sessions_root: &Path, workspace_root: &Path) -> Self {
        Self {
            dir: sessions_root.join(project_key(workspace_root)),
        }
    }

    fn index_path(&self) -> PathBuf {
        self.dir.join("index.jsonl")
    }

    /// Sessions, most recently updated first.
    pub fn list(&self) -> Vec<IndexEntry> {
        let Ok(f) = File::open(self.index_path()) else {
            return Vec::new();
        };
        let mut by_id: Vec<IndexEntry> = Vec::new();
        for line in BufReader::new(f).lines().map_while(Result::ok) {
            let Ok(e) = serde_json::from_str::<IndexEntry>(&line) else {
                continue;
            };
            // Keep entries in order of their last write, to break ties within a second.
            by_id.retain(|x| x.id != e.id);
            by_id.push(e);
        }
        by_id.reverse();
        by_id.sort_by_key(|e| std::cmp::Reverse(e.updated));
        by_id
    }

    /// Find by exact id or unique prefix.
    pub fn find(&self, id: &str) -> Result<IndexEntry, String> {
        let all = self.list();
        if let Some(e) = all.iter().find(|e| e.id == id) {
            return Ok(e.clone());
        }
        let hits: Vec<&IndexEntry> = all.iter().filter(|e| e.id.starts_with(id)).collect();
        match hits.as_slice() {
            [one] => Ok((*one).clone()),
            [] => Err(format!("no session `{id}` in this project")),
            _ => Err(format!(
                "session id `{id}` is ambiguous; give more characters"
            )),
        }
    }

    pub fn latest(&self) -> Option<IndexEntry> {
        self.list().into_iter().next()
    }

    fn append_index(&self, e: &IndexEntry) -> std::io::Result<()> {
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.index_path())?;
        writeln!(f, "{}", serde_json::to_string(e).unwrap_or_default())
    }

    /// Start a new session log.
    pub fn create(&self, id: &str, model: &str) -> std::io::Result<SessionLog> {
        let now = time::unix_now();
        let stamp = time::rfc3339(now).replace(':', "-");
        let dir_name = format!("{}_{id}", stamp.trim_end_matches('Z'));
        let dir = self.dir.join(&dir_name);
        std::fs::create_dir_all(&dir)?;
        let entry = IndexEntry {
            id: id.to_owned(),
            dir: dir_name,
            started: now,
            updated: now,
            title: String::new(),
            model: model.to_owned(),
            turns: 0,
        };
        self.append_index(&entry)?;
        SessionLog::open(self.clone(), entry, &dir)
    }

    /// Reopen an existing session to continue appending to it.
    pub fn reopen(&self, entry: IndexEntry) -> std::io::Result<SessionLog> {
        let dir = self.dir.join(&entry.dir);
        SessionLog::open(self.clone(), entry, &dir)
    }

    /// Read a session's records. A truncated last line (from a crash) is skipped.
    pub fn read(&self, entry: &IndexEntry) -> std::io::Result<Vec<Record>> {
        read_events(&self.dir.join(&entry.dir).join("events.jsonl"))
    }
}

/// A parsed log line.
#[derive(Debug, Clone, PartialEq)]
pub struct Record {
    pub kind: String,
    pub ts: String,
    /// The payload fields (everything except `v`, `t`, `ts`).
    pub data: Map<String, Value>,
}

pub fn read_events(path: &Path) -> std::io::Result<Vec<Record>> {
    let f = File::open(path)?;
    let mut out = Vec::new();
    for (n, line) in BufReader::new(f).lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Map<String, Value>>(&line) {
            Ok(mut m) => {
                let kind = m
                    .remove("t")
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_default();
                let ts = m
                    .remove("ts")
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_default();
                m.remove("v");
                out.push(Record { kind, ts, data: m });
            }
            Err(e) => {
                tracing::warn!(line = n + 1, error = %e, path = %path.display(), "skipping unreadable session line")
            }
        }
    }
    Ok(out)
}

/// An open session log. Each append is flushed; `sync` fsyncs (end of turn).
#[derive(Debug)]
pub struct SessionLog {
    project: ProjectSessions,
    pub entry: IndexEntry,
    file: File,
}

impl SessionLog {
    fn open(project: ProjectSessions, entry: IndexEntry, dir: &Path) -> std::io::Result<Self> {
        let path = dir.join("events.jsonl");
        // A crash can leave a partial last line; end it so the next record starts clean.
        let torn = std::fs::read(&path).is_ok_and(|b| b.last().is_some_and(|&c| c != b'\n'));
        let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
        if torn {
            file.write_all(b"\n")?;
        }
        Ok(Self {
            project,
            entry,
            file,
        })
    }

    pub fn dir(&self) -> PathBuf {
        self.project.dir.join(&self.entry.dir)
    }

    /// A log of its own for a sub-agent run: `agents/<name>.jsonl` in this session's folder.
    /// Resuming the session never reads it.
    pub fn child(&self, name: &str) -> std::io::Result<SessionLog> {
        let dir = self.dir().join("agents");
        std::fs::create_dir_all(&dir)?;
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join(format!("{name}.jsonl")))?;
        Ok(Self {
            project: self.project.clone(),
            entry: self.entry.clone(),
            file,
        })
    }

    /// Append one record. `payload` must serialize to a JSON object.
    pub fn append(&mut self, kind: &str, payload: impl Serialize) -> std::io::Result<()> {
        let mut m = Map::new();
        m.insert("v".into(), Value::from(FORMAT_VERSION));
        m.insert("t".into(), Value::from(kind));
        m.insert("ts".into(), Value::from(time::rfc3339(time::unix_now())));
        if let Ok(Value::Object(fields)) = serde_json::to_value(payload) {
            m.extend(fields);
        }
        let mut line = serde_json::to_string(&Value::Object(m)).unwrap_or_default();
        line.push('\n');
        self.file.write_all(line.as_bytes())?;
        self.file.flush()
    }

    /// Durably store everything so far and update the index (end of each turn).
    pub fn sync(&mut self, title: Option<&str>, turns: u32) -> std::io::Result<()> {
        self.file.sync_data()?;
        self.entry.updated = time::unix_now();
        self.entry.turns = turns;
        if let Some(t) = title {
            self.entry.title = t.to_owned();
        }
        self.project.append_index(&self.entry)
    }
}

/// `<last two path components>-<hash>`, safe on every filesystem.
pub fn project_key(root: &Path) -> String {
    let full = root.to_string_lossy();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in full.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    let tail: Vec<String> = root
        .components()
        .rev()
        .take(2)
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    let name: String = tail
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("-")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{name}-{:08x}", hash as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_append_list_reopen() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ps = ProjectSessions::new(tmp.path(), Path::new("/home/u/src/proj"));
        let mut log = ps.create("abc123", "p/m").expect("create");
        log.append(
            "message",
            serde_json::json!({"message": {"role": "user", "content": "hi"}}),
        )
        .expect("append");
        log.sync(Some("hi"), 1).expect("sync");

        let list = ps.list();
        assert_eq!(list.len(), 1);
        assert_eq!((list[0].title.as_str(), list[0].turns), ("hi", 1));
        let entry = ps.find("abc").expect("prefix");
        let recs = ps.read(&entry).expect("read");
        assert_eq!(recs[0].kind, "message");
        assert_eq!(recs[0].data["message"]["content"], "hi");

        // A crash leaves a partial line; it is skipped.
        let events = log.dir().join("events.jsonl");
        OpenOptions::new()
            .append(true)
            .open(&events)
            .and_then(|mut f| f.write_all(b"{\"v\":1,\"t\":\"mess"))
            .expect("corrupt");
        let mut again = ps.reopen(entry.clone()).expect("reopen");
        assert_eq!(ps.read(&entry).expect("read").len(), 1);
        again
            .append("usage", serde_json::json!({"prompt_tokens": 3}))
            .expect("append");
        let recs = ps.read(&entry).expect("read");
        assert_eq!(recs.len(), 2);
        assert_eq!(recs[1].kind, "usage");
        assert!(ps.project_key_is_stable());
    }

    impl ProjectSessions {
        fn project_key_is_stable(&self) -> bool {
            self.dir
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("src-proj-"))
        }
    }
}
