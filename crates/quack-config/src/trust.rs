//! Project trust records in `<user data>/trust.toml` (docs/plan/01-configuration.md#project-trust).
//!
//! A decision is stored with the canonical repo path and a fingerprint of every file in
//! `.quack/` that can make the harness run something or contact somewhere: all of
//! it except `skills/`, `commands/` and `memory/`, which are text. When the fingerprint
//! changes, a trusted project is skipped again until the user looks at it. A record
//! without a `hash` (written by hand or by an administrator) trusts any content.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::sha256::Sha256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TrustDecision {
    Trusted,
    /// Load `config.toml` only: no item directories that can run executables.
    SettingsOnly,
    Ignored,
}

impl TrustDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Trusted => "trusted",
            Self::SettingsOnly => "settings-only",
            Self::Ignored => "ignored",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "trusted" | "trust" => Some(Self::Trusted),
            "settings-only" | "settings" => Some(Self::SettingsOnly),
            "ignored" | "ignore" => Some(Self::Ignored),
            _ => None,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct TrustFile {
    #[serde(default)]
    project: Vec<TrustRecord>,
}

#[derive(Debug, Deserialize)]
struct TrustRecord {
    path: String,
    decision: TrustDecision,
    #[serde(default)]
    hash: String,
}

/// A stored decision, and whether the project's files still match it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Found {
    pub decision: TrustDecision,
    pub current: bool,
}

/// Folders in `.quack/` that hold only text and need no trust.
const TEXT_DIRS: &[&str] = &["skills", "commands", "memory"];

/// Item folders named in the trust prompt, with what one file in each is.
const KINDS: &[(&str, &str)] = &[
    ("mcp.d", "MCP server"),
    ("hooks.d", "hook"),
    ("lsp.d", "language server"),
    ("providers.d", "provider"),
    ("profiles.d", "profile"),
    ("agents.d", "agent"),
];

/// Larger files are fingerprinted by size only, so a stray file cannot stall startup.
const MAX_HASHED: u64 = 4 * 1024 * 1024;

pub(crate) fn lookup(user_data: &Path, workspace_root: &Path, project: &Path) -> Option<Found> {
    let text = crate::diagnostic::read_text(&user_data.join("trust.toml")).ok()?;
    let file: TrustFile = toml::from_str(&text).ok()?;
    let want = workspace_root.canonicalize().ok()?;
    let record = file
        .project
        .into_iter()
        .rfind(|r| Path::new(&r.path).canonicalize().ok().as_deref() == Some(want.as_path()))?;
    let current = record.decision == TrustDecision::Ignored
        || record.hash.is_empty()
        || record.hash == fingerprint(project);
    Some(Found {
        decision: record.decision,
        current,
    })
}

/// Store a decision for the workspace, replacing any earlier one. Comments and other
/// records in `trust.toml` are kept.
pub(crate) fn record(
    user_data: &Path,
    workspace_root: &Path,
    project: &Path,
    decision: TrustDecision,
) -> Result<PathBuf, String> {
    let file = user_data.join("trust.toml");
    let text = match std::fs::read_to_string(&file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("cannot read {}: {e}", file.display())),
    };
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e| format!("{}: {e}", file.display()))?;
    let root = workspace_root
        .canonicalize()
        .map_err(|e| format!("cannot resolve {}: {e}", workspace_root.display()))?;
    let path = root.display().to_string();
    let item = doc
        .entry("project")
        .or_insert(toml_edit::Item::ArrayOfTables(Default::default()));
    let records = item
        .as_array_of_tables_mut()
        .ok_or_else(|| format!("{}: `project` is not a list of tables", file.display()))?;
    records.retain(|t| {
        t.get("path")
            .and_then(|p| p.as_str())
            .and_then(|p| Path::new(p).canonicalize().ok())
            .is_none_or(|p| p != root)
    });
    let mut t = toml_edit::Table::new();
    t.insert("path", toml_edit::value(path));
    t.insert("decision", toml_edit::value(decision.as_str()));
    t.insert("hash", toml_edit::value(fingerprint(project)));
    records.push(t);
    std::fs::create_dir_all(user_data)
        .map_err(|e| format!("cannot create {}: {e}", user_data.display()))?;
    let tmp = file.with_extension("toml.tmp");
    std::fs::write(&tmp, doc.to_string())
        .and_then(|()| std::fs::rename(&tmp, &file))
        .map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("cannot write {}: {e}", file.display())
        })?;
    Ok(file)
}

/// SHA-256 over the relative path and contents of every file that needs trust, in
/// sorted order. Symbolic links are hashed by their target path, never followed.
pub(crate) fn fingerprint(project: &Path) -> String {
    let mut h = Sha256::default();
    for (rel, path) in trusted_files(project) {
        h.update(rel.as_bytes());
        h.update(&[0]);
        let meta = std::fs::symlink_metadata(&path);
        match meta {
            Ok(m) if m.file_type().is_symlink() => {
                let target = std::fs::read_link(&path).unwrap_or_default();
                h.update(b"link:");
                h.update(target.to_string_lossy().as_bytes());
            }
            Ok(m) if m.len() > MAX_HASHED => h.update(format!("size:{}", m.len()).as_bytes()),
            Ok(_) => h.update(&std::fs::read(&path).unwrap_or_default()),
            Err(_) => {}
        }
        h.update(&[0]);
    }
    h.hex()
}

/// Files under `.quack/` outside the text folders, as `(relative path, path)`.
fn trusted_files(project: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    let mut stack = vec![(String::new(), project.to_path_buf(), 0usize)];
    while let Some((rel, dir, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let child = if rel.is_empty() {
                name.clone()
            } else {
                format!("{rel}/{name}")
            };
            let Ok(kind) = e.file_type() else { continue };
            if kind.is_dir() {
                if !(depth == 0 && TEXT_DIRS.contains(&name.as_str())) && depth < 8 {
                    stack.push((child, e.path(), depth + 1));
                }
            } else {
                out.push((child, e.path()));
            }
        }
    }
    out.sort();
    out
}

/// What a project's `.quack/` would add, for the trust prompt.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectSummary {
    /// `config.toml` exists.
    pub settings: bool,
    /// `(count, what)` for each item folder with files, e.g. `(2, "MCP server")`.
    pub items: Vec<(usize, &'static str)>,
    /// Other files that need trust (scripts, unknown folders).
    pub other: usize,
}

impl ProjectSummary {
    pub fn read(project: &Path) -> Self {
        let mut s = Self::default();
        let mut counts = vec![0usize; KINDS.len()];
        for (rel, _) in trusted_files(project) {
            if rel == "config.toml" {
                s.settings = true;
            } else if let Some(i) = KINDS
                .iter()
                .position(|(dir, _)| rel.strip_prefix(dir).is_some_and(|r| r.starts_with('/')))
            {
                counts[i] += 1;
            } else {
                s.other += 1;
            }
        }
        s.items = KINDS
            .iter()
            .zip(counts)
            .filter(|(_, n)| *n > 0)
            .map(|((_, what), n)| (n, *what))
            .collect();
        s
    }

    /// Nothing in the folder can run anything or change settings.
    pub fn is_empty(&self) -> bool {
        !self.settings && self.items.is_empty() && self.other == 0
    }

    /// "settings, 2 MCP servers, 1 hook".
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if self.settings {
            parts.push("settings".to_owned());
        }
        for (n, what) in &self.items {
            parts.push(format!("{n} {what}{}", if *n == 1 { "" } else { "s" }));
        }
        if self.other > 0 {
            parts.push(format!(
                "{} other file{}",
                self.other,
                if self.other == 1 { "" } else { "s" }
            ));
        }
        parts.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, text: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
        std::fs::write(p, text).expect("write");
    }

    #[test]
    fn fingerprint_ignores_text_folders_and_sees_changes() {
        let tmp = tempfile::tempdir().expect("tmp");
        let p = tmp.path();
        write(p, "config.toml", "[ui]\nascii = true");
        write(p, "mcp.d/gh.toml", "command = [\"gh\"]");
        let first = fingerprint(p);
        write(p, "skills/x/SKILL.md", "text");
        write(p, "memory/notes/a.md", "text");
        assert_eq!(fingerprint(p), first, "text folders need no trust");
        write(p, "mcp.d/gh.toml", "command = [\"evil\"]");
        assert_ne!(fingerprint(p), first);
        write(p, "mcp.d/gh.toml", "command = [\"gh\"]");
        write(p, "mcp.d/sub/new.toml", "");
        assert_ne!(fingerprint(p), first, "new files count");
    }

    #[test]
    fn summary_counts_what_would_load() {
        let tmp = tempfile::tempdir().expect("tmp");
        let p = tmp.path();
        assert!(ProjectSummary::read(p).is_empty());
        write(p, "skills/x/SKILL.md", "text");
        assert!(ProjectSummary::read(p).is_empty());
        write(p, "config.toml", "");
        write(p, "mcp.d/a.toml", "");
        write(p, "mcp.d/b.toml", "");
        write(p, "hooks.d/c.toml", "");
        write(p, "run.sh", "");
        assert_eq!(
            ProjectSummary::read(p).describe(),
            "settings, 2 MCP servers, 1 hook, 1 other file"
        );
    }

    #[test]
    fn record_replaces_the_decision_and_keeps_comments() {
        let tmp = tempfile::tempdir().expect("tmp");
        let data = tmp.path().join("data");
        let repo = tmp.path().join("repo");
        let project = repo.join(".quack");
        write(&project, "config.toml", "");
        write(&data, "trust.toml", "# mine\n");
        record(&data, &repo, &project, TrustDecision::Ignored).expect("record");
        record(&data, &repo, &project, TrustDecision::Trusted).expect("record");
        let text = std::fs::read_to_string(data.join("trust.toml")).expect("read");
        assert!(text.contains("# mine\n"), "{text}");
        assert_eq!(text.matches("[[project]]").count(), 1, "{text}");
        let found = lookup(&data, &repo, &project).expect("found");
        assert_eq!(found.decision, TrustDecision::Trusted);
        assert!(found.current);
        write(&project, "mcp.d/x.toml", "command = [\"x\"]");
        assert!(!lookup(&data, &repo, &project).expect("found").current);
    }
}
