//! Markdown memory notes at a configurable path
//! (docs/plan/07-sessions-memory-checkpoints.md#memory).
//!
//! Each scope is a directory holding `MEMORY.md` (one index line per note) and
//! `notes/<name>.md` (front matter plus body). Everything is plain text the user can read,
//! edit and commit; lines of the index the user wrote by hand are kept.

use std::path::{Path, PathBuf};

use crate::time;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Global,
    Project,
}

impl Scope {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "global" | "user" => Some(Self::Global),
            "project" | "" => Some(Self::Project),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Project => "project",
        }
    }
}

/// Note types, from the plan.
pub const KINDS: &[&str] = &["user", "project", "feedback", "reference"];

#[derive(Debug, Clone)]
pub struct MemoryStore {
    pub global: PathBuf,
    pub project: PathBuf,
}

impl MemoryStore {
    pub fn new(global: impl Into<PathBuf>, project: impl Into<PathBuf>) -> Self {
        Self {
            global: global.into(),
            project: project.into(),
        }
    }

    pub fn dir(&self, scope: Scope) -> &Path {
        match scope {
            Scope::Global => &self.global,
            Scope::Project => &self.project,
        }
    }

    fn note_path(&self, scope: Scope, name: &str) -> PathBuf {
        self.dir(scope).join("notes").join(format!("{name}.md"))
    }

    /// Both indexes for the system prompt, capped at `max_bytes`; empty when there are none.
    pub fn index_text(&self, max_bytes: usize) -> String {
        let mut out = String::new();
        for scope in [Scope::Global, Scope::Project] {
            let Ok(text) = std::fs::read_to_string(self.dir(scope).join("MEMORY.md")) else {
                continue;
            };
            let text = text.trim();
            if text.is_empty() {
                continue;
            }
            if !out.is_empty() {
                out.push_str("\n\n");
            }
            out.push_str(&format!("{} memory:\n{text}", scope.label()));
        }
        if out.len() > max_bytes {
            let mut cut = max_bytes;
            while !out.is_char_boundary(cut) {
                cut -= 1;
            }
            // Whole lines only.
            cut = out[..cut].rfind('\n').unwrap_or(0);
            out.truncate(cut);
            out.push_str("\n… [more notes; use the memory tool to list them]");
        }
        out
    }

    /// Note names with descriptions, by scope.
    pub fn list(&self, scope: Scope) -> Vec<(String, String)> {
        let Ok(dir) = std::fs::read_dir(self.dir(scope).join("notes")) else {
            return Vec::new();
        };
        let mut notes: Vec<(String, String)> = dir
            .filter_map(Result::ok)
            .filter_map(|e| {
                let name = e
                    .file_name()
                    .to_string_lossy()
                    .strip_suffix(".md")?
                    .to_owned();
                let text = std::fs::read_to_string(e.path()).unwrap_or_default();
                Some((name, front(&text, "description").unwrap_or_default()))
            })
            .collect();
        notes.sort();
        notes
    }

    pub fn read(&self, scope: Scope, name: &str) -> Result<String, String> {
        check_name(name)?;
        std::fs::read_to_string(self.note_path(scope, name))
            .map_err(|_| format!("no {} note named `{name}`", scope.label()))
    }

    pub fn exists(&self, scope: Scope, name: &str) -> bool {
        check_name(name).is_ok() && self.note_path(scope, name).is_file()
    }

    /// Create or replace a note and its index line. Returns the note's path.
    pub fn write(
        &self,
        scope: Scope,
        name: &str,
        description: &str,
        kind: &str,
        body: &str,
    ) -> Result<PathBuf, String> {
        check_name(name)?;
        let description = description.lines().next().unwrap_or_default().trim();
        if description.is_empty() {
            return Err("a note needs a one-line description".into());
        }
        if !KINDS.contains(&kind) {
            return Err(format!("type must be one of {}", KINDS.join(", ")));
        }
        let path = self.note_path(scope, name);
        let today = &time::rfc3339(time::unix_now())[..10];
        let text = format!(
            "---\nname: {name}\ndescription: {description}\ntype: {kind}\nupdated: {today}\n---\n\n{}\n",
            body.trim()
        );
        let io = |e: std::io::Error| format!("cannot write {}: {e}", path.display());
        std::fs::create_dir_all(path.parent().unwrap_or(self.dir(scope))).map_err(io)?;
        std::fs::write(&path, text).map_err(io)?;
        self.set_index_line(scope, name, Some(description))?;
        Ok(path)
    }

    pub fn delete(&self, scope: Scope, name: &str) -> Result<(), String> {
        check_name(name)?;
        let path = self.note_path(scope, name);
        std::fs::remove_file(&path)
            .map_err(|_| format!("no {} note named `{name}`", scope.label()))?;
        self.set_index_line(scope, name, None)
    }

    /// Replace (or remove, with `None`) the index line linking to this note.
    fn set_index_line(
        &self,
        scope: Scope,
        name: &str,
        description: Option<&str>,
    ) -> Result<(), String> {
        let path = self.dir(scope).join("MEMORY.md");
        let old = std::fs::read_to_string(&path).unwrap_or_default();
        let link = format!("](notes/{name}.md)");
        let line = description.map(|d| format!("- [{name}](notes/{name}.md) — {d}"));
        let mut lines: Vec<String> = Vec::new();
        let mut placed = false;
        for l in old.lines() {
            if l.contains(&link) {
                if let (Some(new), false) = (&line, placed) {
                    lines.push(new.clone());
                    placed = true;
                }
            } else {
                lines.push(l.to_owned());
            }
        }
        if let (Some(new), false) = (line, placed) {
            lines.push(new);
        }
        let mut text = lines.join("\n");
        if !text.is_empty() {
            text.push('\n');
        }
        std::fs::write(&path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))
    }
}

/// Names are file names on every OS: lowercase letters, digits and dashes.
fn check_name(name: &str) -> Result<(), String> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !name.starts_with('-');
    if ok {
        Ok(())
    } else {
        Err(format!(
            "invalid note name `{name}`: use lowercase letters, digits and dashes (at most 64)"
        ))
    }
}

/// A front-matter field.
fn front(text: &str, key: &str) -> Option<String> {
    let rest = text.strip_prefix("---")?;
    let end = rest.find("\n---")?;
    rest[..end].lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        (k.trim() == key).then(|| v.trim().to_owned())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_read_index_delete() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let m = MemoryStore::new(tmp.path().join("g"), tmp.path().join("p"));
        std::fs::create_dir_all(&m.project).expect("mkdir");
        std::fs::write(m.project.join("MEMORY.md"), "# Hand-written heading\n").expect("write");

        m.write(
            Scope::Project,
            "needs-protoc",
            "Build needs protoc",
            "project",
            "Install it.",
        )
        .expect("write");
        m.write(
            Scope::Project,
            "needs-protoc",
            "Build needs protoc 28",
            "project",
            "v28.",
        )
        .expect("update");
        m.write(
            Scope::Global,
            "prefers-tabs",
            "User prefers tabs",
            "user",
            "Tabs.",
        )
        .expect("write");

        let index = std::fs::read_to_string(m.project.join("MEMORY.md")).expect("index");
        assert_eq!(
            index,
            "# Hand-written heading\n- [needs-protoc](notes/needs-protoc.md) — Build needs protoc 28\n"
        );
        let note = m.read(Scope::Project, "needs-protoc").expect("read");
        assert!(note.contains("type: project") && note.ends_with("v28.\n"));
        assert_eq!(
            m.list(Scope::Project),
            [(
                "needs-protoc".to_owned(),
                "Build needs protoc 28".to_owned()
            )]
        );
        let prompt = m.index_text(10_000);
        assert!(prompt.starts_with("global memory:\n- [prefers-tabs]"));
        assert!(prompt.contains("project memory:\n# Hand-written heading"));

        m.delete(Scope::Project, "needs-protoc").expect("delete");
        assert!(!m.exists(Scope::Project, "needs-protoc"));
        assert_eq!(
            std::fs::read_to_string(m.project.join("MEMORY.md")).expect("index"),
            "# Hand-written heading\n"
        );
    }

    #[test]
    fn rejects_bad_input() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let m = MemoryStore::new(tmp.path().join("g"), tmp.path().join("p"));
        for bad in ["", "../x", "A", "a b", "-x"] {
            assert!(
                m.write(Scope::Global, bad, "d", "user", "b").is_err(),
                "{bad}"
            );
        }
        assert!(m.write(Scope::Global, "x", "", "user", "b").is_err());
        assert!(m.write(Scope::Global, "x", "d", "secret", "b").is_err());
    }

    #[test]
    fn index_is_capped_on_a_line_boundary() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let m = MemoryStore::new(tmp.path().join("g"), tmp.path().join("p"));
        for n in 0..20 {
            m.write(
                Scope::Global,
                &format!("n{n}"),
                "some description",
                "user",
                "b",
            )
            .expect("write");
        }
        let t = m.index_text(200);
        assert!(t.len() < 260 && t.ends_with("use the memory tool to list them]"));
        assert!(
            t.lines()
                .rev()
                .nth(1)
                .is_some_and(|l| l.ends_with("some description"))
        );
    }
}
