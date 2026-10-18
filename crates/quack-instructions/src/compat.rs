//! Other harnesses' instruction files, listed in `compat.toml` (defaults bundle, system or
//! user folder; the highest one present wins). Delete the file to stop reading them.

use std::path::{Path, PathBuf};

/// Instruction file names from `[instructions]` in `compat.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Compat {
    /// Relative to each folder, e.g. `CLAUDE.md`, `.github/copilot-instructions.md`.
    pub project: Vec<String>,
    /// Global files, e.g. `~/.claude/CLAUDE.md`.
    pub user: Vec<String>,
}

impl Compat {
    /// Read `compat.toml` from the highest of `dirs` (lowest precedence first) that has one.
    pub fn load(dirs: &[PathBuf]) -> Self {
        dirs.iter()
            .rev()
            .find_map(|d| std::fs::read_to_string(d.join("compat.toml")).ok())
            .map(|t| Self::parse(&t))
            .unwrap_or_default()
    }

    pub fn parse(text: &str) -> Self {
        let Ok(table) = text.parse::<toml::Table>() else {
            return Self::default();
        };
        let list = |key: &str| -> Vec<String> {
            table
                .get("instructions")
                .and_then(|i| i.get(key))
                .and_then(toml::Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(toml::Value::as_str)
                        // Folders of rules (e.g. `.cursor/rules/`) are not single files.
                        .filter(|s| !s.ends_with('/'))
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default()
        };
        Self {
            project: list("project"),
            user: list("user"),
        }
    }

    /// Files in `dir` from other harnesses whose content is not already in `agents`
    /// (the AGENTS.md there): not a link to it, not a copy, not just `@AGENTS.md`.
    pub fn found(&self, dir: &Path, agents: &Path) -> Vec<String> {
        let agents_text = std::fs::read_to_string(agents).ok();
        let agents_real = agents.canonicalize().ok();
        self.project
            .iter()
            .filter(|name| {
                let p = dir.join(name);
                if !p.is_file() {
                    return false;
                }
                if agents_real.is_some() && p.canonicalize().ok() == agents_real {
                    return false;
                }
                let text = std::fs::read_to_string(&p).unwrap_or_default();
                let t = text.trim();
                !(t.is_empty()
                    || t.eq_ignore_ascii_case("@AGENTS.md")
                    || agents_text.as_deref().is_some_and(|a| a.trim() == t))
            })
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_shipped_file() {
        let text = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../defaults/compat.toml"),
        )
        .expect("defaults/compat.toml");
        let c = Compat::parse(&text);
        assert!(c.project.contains(&"CLAUDE.md".to_owned()));
        assert!(!c.project.iter().any(|p| p.ends_with('/')));
        assert!(c.user.contains(&"~/.claude/CLAUDE.md".to_owned()));
    }

    #[test]
    fn skips_files_that_only_point_at_agents_md() {
        let dir = tempfile::tempdir().expect("tempdir");
        let d = dir.path();
        std::fs::write(d.join("AGENTS.md"), "Use tabs.\n").expect("write");
        std::fs::write(d.join("CLAUDE.md"), "@AGENTS.md\n").expect("write");
        std::fs::write(d.join("GEMINI.md"), "Use tabs.").expect("write");
        std::fs::write(d.join(".cursorrules"), "Prefer small functions.").expect("write");
        let c = Compat {
            project: vec![
                "CLAUDE.md".into(),
                "GEMINI.md".into(),
                ".cursorrules".into(),
            ],
            user: Vec::new(),
        };
        assert_eq!(c.found(d, &d.join("AGENTS.md")), [".cursorrules"]);
    }
}
