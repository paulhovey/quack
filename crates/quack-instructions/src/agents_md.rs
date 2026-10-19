//! AGENTS.md chain discovery, nested on-demand loading and `@` imports
//! (docs/plan/08-instructions-skills-commands.md#agentsmd).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Instructions loaded at session start.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Loaded {
    /// `<instructions source="…">` blocks, in precedence order (later wins).
    pub text: String,
    /// Each file as shown to the user, with `(truncated)` where the cap applied.
    pub sources: Vec<String>,
    /// Directories whose instruction file (or lack of one) is already accounted for.
    pub dirs: Vec<PathBuf>,
}

/// What to look for and how much of it to keep.
#[derive(Debug, Clone)]
pub struct Rules {
    /// File names in order of preference; the first one present in a directory wins.
    pub names: Vec<String>,
    /// Cap on the total size of the loaded text.
    pub max_bytes: usize,
}

/// Global files first (`globals`, each with the directory its imports may read from),
/// then the chain from the workspace root down to `cwd`.
pub fn load(globals: &[(PathBuf, PathBuf)], root: &Path, cwd: &Path, rules: &Rules) -> Loaded {
    let mut out = Loaded::default();
    for (file, scope) in globals {
        add(&mut out, file, scope, root, rules);
    }
    for dir in chain(root, cwd) {
        if let Some(file) = find(&dir, &rules.names) {
            add(&mut out, &file, root, root, rules);
        }
        out.dirs.push(dir);
    }
    out
}

fn add(out: &mut Loaded, file: &Path, scope: &Path, root: &Path, rules: &Rules) {
    let Some(body) = read(file, scope) else {
        return;
    };
    let room = rules.max_bytes.saturating_sub(out.text.len());
    if room == 0 {
        out.sources
            .push(format!("{} (skipped: max_bytes)", shown(file, root)));
        return;
    }
    let (block, truncated) = block(file, root, &body, room);
    if !out.text.is_empty() {
        out.text.push_str("\n\n");
    }
    out.text.push_str(&block);
    out.sources.push(if truncated {
        format!("{} (truncated)", shown(file, root))
    } else {
        shown(file, root)
    });
}

/// `root`, then each directory down to `cwd`. Just `root` when `cwd` is outside it.
fn chain(root: &Path, cwd: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![root.to_path_buf()];
    if let Ok(rel) = cwd.strip_prefix(root) {
        let mut d = root.to_path_buf();
        for c in rel.components() {
            d.push(c);
            dirs.push(d.clone());
        }
    }
    dirs
}

fn find(dir: &Path, names: &[String]) -> Option<PathBuf> {
    names.iter().map(|n| dir.join(n)).find(|p| p.is_file())
}

fn shown(path: &Path, root: &Path) -> String {
    path.strip_prefix(root).map_or_else(
        |_| path.display().to_string(),
        |p| p.display().to_string().replace('\\', "/"),
    )
}

/// The file's text with imports expanded; `None` when it is missing or empty.
fn read(file: &Path, scope: &Path) -> Option<String> {
    let text = std::fs::read_to_string(file).ok()?;
    let text = text.trim_start_matches('\u{feff}').trim();
    (!text.is_empty()).then(|| expand_imports(text, file, scope))
}

/// Replace lines of the form `@path/to/file.md` with that file's contents. Paths are
/// relative to the importing file and must stay inside `scope` (the workspace, or the
/// config directory for global files), so a repository cannot pull in `~/.ssh`. One level
/// deep: imports inside imported files stay as written. Each file is imported once.
fn expand_imports(text: &str, file: &Path, scope: &Path) -> String {
    let base = file.parent().unwrap_or(scope);
    let scope = scope.canonicalize().unwrap_or_else(|_| scope.to_path_buf());
    let mut seen: HashSet<PathBuf> = file.canonicalize().into_iter().collect();
    let mut out = Vec::new();
    for line in text.lines() {
        let target = line
            .trim()
            .strip_prefix('@')
            .filter(|t| !t.is_empty() && !t.contains(char::is_whitespace));
        let Some(target) = target else {
            out.push(line.to_owned());
            continue;
        };
        let resolved = base.join(target).canonicalize();
        match resolved {
            Ok(p) if !p.starts_with(&scope) => {
                out.push(format!("[import {target} skipped: outside the workspace]"));
            }
            Ok(p) if !seen.insert(p.clone()) => {}
            Ok(p) => match std::fs::read_to_string(&p) {
                Ok(t) => out.push(t.trim_start_matches('\u{feff}').trim_end().to_owned()),
                Err(_) => out.push(line.to_owned()),
            },
            // Not a file: probably an @mention in prose, so leave it alone.
            Err(_) => out.push(line.to_owned()),
        }
    }
    out.join("\n")
}

fn block(file: &Path, root: &Path, body: &str, room: usize) -> (String, bool) {
    let mut body = body.to_owned();
    let truncated = body.len() > room;
    if truncated {
        let mut cut = room;
        while !body.is_char_boundary(cut) {
            cut -= 1;
        }
        body.truncate(cut);
        body.push_str("\n… [truncated: instructions.max_bytes]");
    }
    (
        format!(
            "<instructions source=\"{}\">\n{body}\n</instructions>",
            shown(file, root)
        ),
        truncated,
    )
}

/// The `<instructions source="…">` blocks in `text`: `(source, body)`, in order.
pub fn blocks(text: &str) -> Vec<(String, String)> {
    text.split("<instructions source=\"")
        .skip(1)
        .filter_map(|part| {
            let (src, rest) = part.split_once("\">\n")?;
            let body = rest.split("\n</instructions>").next().unwrap_or(rest);
            Some((src.to_owned(), body.to_owned()))
        })
        .collect()
}

/// Instruction files below the workspace root, attached to the first tool result that
/// touches their directory, so monorepo instructions only cost context when relevant.
#[derive(Debug)]
pub struct Nested {
    root: PathBuf,
    rules: Rules,
    seen: Mutex<HashSet<PathBuf>>,
}

impl Nested {
    /// `seen`: directories already accounted for (the startup chain).
    pub fn new(root: &Path, rules: Rules, seen: impl IntoIterator<Item = PathBuf>) -> Self {
        Self {
            root: root.to_path_buf(),
            rules,
            seen: Mutex::new(seen.into_iter().collect()),
        }
    }

    /// Mark the directories of instruction files already in the conversation (from their
    /// `<instructions source="…">` tags), e.g. after a resume.
    pub fn mark_shown(&self, text: &str) {
        let Ok(mut seen) = self.seen.lock() else {
            return;
        };
        for part in text.split("<instructions source=\"").skip(1) {
            if let Some((src, _)) = part.split_once('"')
                && let Some(dir) = self.root.join(src).parent()
            {
                seen.insert(dir.to_path_buf());
            }
        }
    }

    /// Instructions for directories between the root and `path` not seen yet, as text to
    /// append to a tool result. `path` may be a file or a directory.
    pub fn attach(&self, path: &Path) -> Option<String> {
        let dir = if path.is_dir() { path } else { path.parent()? };
        let rel = dir.strip_prefix(&self.root).ok()?;
        let mut seen = self.seen.lock().ok()?;
        let mut blocks = Vec::new();
        let mut d = self.root.clone();
        for c in rel.components() {
            d.push(c);
            if !seen.insert(d.clone()) {
                continue;
            }
            if let Some(file) = find(&d, &self.rules.names)
                && let Some(body) = read(&file, &self.root)
            {
                blocks.push(block(&file, &self.root, &body, self.rules.max_bytes).0);
            }
        }
        (!blocks.is_empty()).then(|| {
            format!(
                "[instructions for {}/ — follow them for files there]\n{}",
                shown(dir, &self.root),
                blocks.join("\n\n")
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules() -> Rules {
        Rules {
            names: vec!["AGENTS.md".into(), "CLAUDE.md".into()],
            max_bytes: 10_000,
        }
    }

    fn write(p: &Path, text: &str) {
        std::fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
        std::fs::write(p, text).expect("write");
    }

    #[test]
    fn chain_from_root_to_cwd_with_first_name_winning() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        write(&root.join("AGENTS.md"), "root rules");
        write(&root.join("CLAUDE.md"), "duplicate, ignored");
        write(&root.join("app/CLAUDE.md"), "app rules");
        write(&root.join("app/web/AGENTS.md"), "web rules");
        write(&root.join("other/AGENTS.md"), "not on the chain");
        let global = tmp.path().join("cfg/AGENTS.md");
        write(&global, "global rules");

        let l = load(
            &[(global.clone(), global.parent().expect("dir").to_path_buf())],
            root,
            &root.join("app/web"),
            &rules(),
        );
        let order: Vec<usize> = ["global rules", "root rules", "app rules", "web rules"]
            .iter()
            .map(|t| l.text.find(t).expect(t))
            .collect();
        assert!(order.windows(2).all(|w| w[0] < w[1]), "{}", l.text);
        assert!(!l.text.contains("duplicate") && !l.text.contains("not on the chain"));
        assert_eq!(
            &l.sources[1..],
            ["AGENTS.md", "app/CLAUDE.md", "app/web/AGENTS.md"]
        );
        assert_eq!(l.dirs.len(), 3);
    }

    #[test]
    fn imports_are_one_level_and_stay_in_scope() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("repo");
        write(
            &root.join("AGENTS.md"),
            "intro\n@docs/style.md\n@docs/style.md\n@../secret.txt\nmail @someone about it",
        );
        write(&root.join("docs/style.md"), "use tabs\n@nested.md");
        write(&tmp.path().join("secret.txt"), "SECRET");
        let l = load(&[], &root, &root, &rules());
        assert!(
            l.text.contains("intro\nuse tabs\n@nested.md\n"),
            "{}",
            l.text
        );
        assert_eq!(l.text.matches("use tabs").count(), 1);
        assert!(!l.text.contains("SECRET"));
        assert!(
            l.text
                .contains("[import ../secret.txt skipped: outside the workspace]")
        );
        assert!(l.text.contains("mail @someone about it"));
    }

    #[test]
    fn the_cap_truncates_then_skips() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        write(&root.join("AGENTS.md"), &"x".repeat(100));
        write(&root.join("a/AGENTS.md"), "more");
        let r = Rules {
            max_bytes: 60,
            ..rules()
        };
        let l = load(&[], root, &root.join("a"), &r);
        assert_eq!(
            l.sources,
            ["AGENTS.md (truncated)", "a/AGENTS.md (skipped: max_bytes)"]
        );
    }

    #[test]
    fn nested_files_attach_once() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        write(&root.join("AGENTS.md"), "root");
        write(&root.join("pkg/AGENTS.md"), "pkg rules");
        write(&root.join("pkg/deep/AGENTS.md"), "deep rules");
        write(&root.join("pkg/deep/x.rs"), "");
        let n = Nested::new(root, rules(), [root.to_path_buf()]);
        let first = n.attach(&root.join("pkg/deep/x.rs")).expect("attached");
        assert!(first.contains("pkg rules") && first.contains("deep rules"));
        assert!(first.starts_with("[instructions for pkg/deep/"));
        assert_eq!(n.attach(&root.join("pkg/deep/x.rs")), None);
        assert_eq!(n.attach(&root.join("x.rs")), None);

        // After a resume, files already in the conversation are not attached again.
        let again = Nested::new(root, rules(), [root.to_path_buf()]);
        again.mark_shown(&first);
        assert_eq!(again.attach(&root.join("pkg/deep/x.rs")), None);
    }

    #[test]
    fn blocks_split_back_out() {
        let text = "intro\n<instructions source=\"AGENTS.md\">\nbe brief\n</instructions>\n\n<instructions source=\"web/AGENTS.md\">\nuse pnpm\nalways\n</instructions>";
        assert_eq!(
            blocks(text),
            [
                ("AGENTS.md".to_owned(), "be brief".to_owned()),
                ("web/AGENTS.md".to_owned(), "use pnpm\nalways".to_owned())
            ]
        );
    }
}
