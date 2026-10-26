//! Markdown memory notes at a configurable path
//! (docs/plan/07-sessions-memory-checkpoints.md#memory).
//!
//! Each scope is a directory holding `MEMORY.md` (one index line per note) and
//! `notes/<name>.md` (front matter plus body). Everything is plain text the user can read,
//! edit, sync and commit. The notes are the source of truth: the index's note lines are
//! rebuilt from their front matter (`reconcile`), so a hand edit, a sync conflict or two
//! sessions writing at once can't leave it wrong. Lines the user wrote by hand are kept.

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
    /// Bytes of index the prompt takes; the rest is left out.
    pub index_cap: usize,
}

impl MemoryStore {
    pub fn new(global: impl Into<PathBuf>, project: impl Into<PathBuf>) -> Self {
        Self {
            global: global.into(),
            project: project.into(),
            // `memory.index_max_tokens`' default, at about 3.5 bytes a token.
            index_cap: 5_250,
        }
    }

    pub fn with_index_cap(mut self, bytes: usize) -> Self {
        self.index_cap = bytes;
        self
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

    /// Both indexes, uncut.
    fn index_full(&self) -> String {
        let mut out = String::new();
        for scope in [Scope::Global, Scope::Project] {
            let text = self.reconcile(scope);
            let text = text.trim();
            if text.is_empty() {
                continue;
            }
            if !out.is_empty() {
                out.push_str("\n\n");
            }
            out.push_str(&format!("{} memory:\n{text}", scope.label()));
        }
        out
    }

    /// Both indexes for the prompt, capped at `index_cap`; empty when there are none.
    pub fn index_text(&self) -> String {
        let mut out = self.index_full();
        if out.len() > self.index_cap {
            let mut cut = self.index_cap;
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

    /// How full the index is, as a percentage of `index_cap`. Past 100, notes are left out
    /// of the prompt.
    pub fn index_fill(&self) -> usize {
        self.index_full().len() * 100 / self.index_cap.max(1)
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
                // Skips sync tools' conflict copies (`x.sync-conflict-….md`) and stray files.
                check_name(&name).ok()?;
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
        self.reconcile(scope);
        Ok(path)
    }

    pub fn delete(&self, scope: Scope, name: &str) -> Result<(), String> {
        check_name(name)?;
        let path = self.note_path(scope, name);
        std::fs::remove_file(&path)
            .map_err(|_| format!("no {} note named `{name}`", scope.label()))?;
        self.reconcile(scope);
        Ok(())
    }

    /// The scope's index, brought in line with its notes and saved when that changed it.
    /// Each note gets one line, built from its front matter, where its line was or else at
    /// the end; lines for missing notes and merge-conflict markers go; other lines stay.
    fn reconcile(&self, scope: Scope) -> String {
        let path = self.dir(scope).join("MEMORY.md");
        let old = std::fs::read_to_string(&path).unwrap_or_default();
        let notes = self.list(scope);
        let line = |(name, d): &(String, String)| {
            let d = if d.is_empty() { name } else { d };
            format!("- [{name}](notes/{name}.md) — {d}")
        };
        let mut placed: Vec<&str> = Vec::new();
        let mut lines: Vec<String> = Vec::new();
        for l in old.lines() {
            if ["<<<<<<<", "|||||||", "=======", ">>>>>>>"]
                .iter()
                .any(|m| l.starts_with(m))
            {
                continue;
            }
            match linked(l) {
                None => lines.push(l.to_owned()),
                Some(name) => {
                    if let Some(n) = notes.iter().find(|n| n.0 == name)
                        && !placed.contains(&name)
                    {
                        lines.push(line(n));
                        placed.push(name);
                    }
                }
            }
        }
        for n in notes.iter().filter(|n| !placed.contains(&n.0.as_str())) {
            lines.push(line(n));
        }
        let mut text = lines.join("\n");
        if !text.is_empty() {
            text.push('\n');
        }
        if text != old {
            // Best effort: a read-only folder still gets a correct index in the prompt.
            let _ = std::fs::write(&path, &text);
        }
        text
    }
}

/// Said once the index is 80% full (`MemoryStore::index_fill`), so notes get merged before
/// some drop out of the prompt.
pub fn fill_warning(fill: usize) -> Option<String> {
    match fill {
        0..80 => None,
        80..=100 => Some(format!("index {fill}% full")),
        _ => Some(format!(
            "index {fill}% full: notes past the cap are left out of the prompt"
        )),
    }
}

/// The note an index line links to, as in `- [name](notes/name.md) — …`.
fn linked(line: &str) -> Option<&str> {
    let rest = &line[line.find("](notes/")? + "](notes/".len()..];
    let name = &rest[..rest.find(".md)")?];
    check_name(name).ok().map(|()| name)
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
        let prompt = m.index_text();
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
        let m = MemoryStore::new(tmp.path().join("g"), tmp.path().join("p")).with_index_cap(200);
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
        assert!(m.index_fill() > 100);
        let t = m.index_text();
        assert!(t.len() < 260 && t.ends_with("use the memory tool to list them]"));
        assert!(
            t.lines()
                .rev()
                .nth(1)
                .is_some_and(|l| l.ends_with("some description"))
        );
    }

    #[test]
    fn index_is_rebuilt_from_the_notes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let m = MemoryStore::new(tmp.path().join("g"), tmp.path().join("p"));
        m.write(Scope::Project, "a", "Note a", "project", "A.")
            .expect("write");
        m.write(Scope::Project, "b", "Note b", "project", "B.")
            .expect("write");
        // A merge conflict, a duplicate, a line for a deleted note, and a note written by
        // hand (or synced in) with no index line. A sync tool's conflict copy is ignored.
        std::fs::write(
            m.project.join("MEMORY.md"),
            "# Mine\n<<<<<<< HEAD\n- [a](notes/a.md) — old a\n=======\n\
             - [a](notes/a.md) — other a\n- [gone](notes/gone.md) — Gone\n>>>>>>> theirs\n",
        )
        .expect("index");
        std::fs::write(
            m.project.join("notes/c.md"),
            "---\nname: c\ndescription: Note c\ntype: project\n---\n\nC.\n",
        )
        .expect("note");
        std::fs::write(m.project.join("notes/a.sync-conflict-20261025.md"), "x").expect("copy");

        assert_eq!(
            m.index_text(),
            "project memory:\n# Mine\n- [a](notes/a.md) — Note a\n\
             - [b](notes/b.md) — Note b\n- [c](notes/c.md) — Note c"
        );
        // Saved, so the user sees the repaired file too.
        assert!(
            std::fs::read_to_string(m.project.join("MEMORY.md"))
                .expect("index")
                .starts_with("# Mine\n- [a](notes/a.md) — Note a\n- [b]")
        );
        assert_eq!(m.list(Scope::Project).len(), 3);
    }
}
