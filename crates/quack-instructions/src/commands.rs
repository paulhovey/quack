//! Markdown custom slash commands (docs/plan/08-instructions-skills-commands.md#custom-slash-commands).
//!
//! `commands/review.md` is `/review`; `commands/test/flaky.md` is `/test:flaky`. The file is
//! a prompt template with optional front matter (`description`, `args`). `{{ args }}` (or
//! `{{ args | default("…") }}`, or `$ARGUMENTS` as other harnesses write it) becomes what
//! was typed after the name; a template without a placeholder gets it appended.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomCommand {
    /// Without the slash: `review`, `test:flaky`.
    pub name: String,
    pub description: String,
    /// What to type after the name, e.g. `[focus]`.
    pub args: String,
    pub path: PathBuf,
    /// The layer it came from, e.g. `user` or `project`.
    pub source: String,
}

impl CustomCommand {
    /// The prompt for this command with `args`.
    pub fn render(&self, args: &str) -> Result<String, String> {
        let text = std::fs::read_to_string(&self.path)
            .map_err(|e| format!("cannot read {}: {e}", self.path.display()))?;
        Ok(fill(split(&text).1.trim(), args.trim()))
    }
}

/// Front matter fields (when the file has any) and the body.
fn split(text: &str) -> (Vec<(String, String)>, String) {
    let text = text.trim_start_matches('\u{feff}');
    if text.starts_with("---")
        && let Ok(parsed) = crate::skills::parse(text)
    {
        return parsed;
    }
    (Vec::new(), text.to_owned())
}

/// Put `args` into the template's placeholders, or after it when it has none.
fn fill(template: &str, args: &str) -> String {
    let mut out = String::with_capacity(template.len() + args.len());
    let mut rest = template;
    let mut placed = false;
    while let Some(open) = rest.find("{{") {
        let Some(close) = rest[open..].find("}}") else {
            break;
        };
        let inner = rest[open + 2..open + close].trim();
        let value = if inner == "args" {
            Some(args.to_owned())
        } else if let Some(filter) = inner.strip_prefix("args")
            && let Some(default) = filter
                .trim()
                .strip_prefix('|')
                .map(str::trim)
                .and_then(|f| f.strip_prefix("default("))
                .and_then(|f| f.trim_end().strip_suffix(')'))
        {
            let default = default.trim().trim_matches(['"', '\'']);
            Some(if args.is_empty() { default } else { args }.to_owned())
        } else {
            None
        };
        match value {
            Some(v) => {
                out.push_str(&rest[..open]);
                out.push_str(&v);
                placed = true;
            }
            None => out.push_str(&rest[..open + close + 2]),
        }
        rest = &rest[open + close + 2..];
    }
    out.push_str(rest);
    if out.contains("$ARGUMENTS") {
        out = out.replace("$ARGUMENTS", args);
        placed = true;
    }
    if !placed && !args.is_empty() {
        out.push_str("\n\n");
        out.push_str(args);
    }
    out
}

/// Commands under each `commands/` root, lowest precedence first: a later file with the
/// same name replaces an earlier one. Returns them sorted by name, and one line per file
/// that could not be read.
pub fn discover(roots: &[(String, PathBuf)]) -> (Vec<CustomCommand>, Vec<String>) {
    let mut found: Vec<CustomCommand> = Vec::new();
    let mut failed = Vec::new();
    for (source, root) in roots {
        let mut files = Vec::new();
        walk(root, &mut files);
        files.sort();
        for path in files {
            let Ok(rel) = path.strip_prefix(root) else {
                continue;
            };
            let name: Vec<String> = rel
                .with_extension("")
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            let name = name.join(":");
            let valid = !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':'));
            if !valid {
                failed.push(format!(
                    "{}: command names use letters, digits, `-` and `_`",
                    path.display()
                ));
                continue;
            }
            let text = match std::fs::read_to_string(&path) {
                Ok(t) => t,
                Err(e) => {
                    failed.push(format!("{}: {e}", path.display()));
                    continue;
                }
            };
            let (fields, body) = split(&text);
            let get = |k: &str| {
                fields
                    .iter()
                    .find(|(key, _)| key == k)
                    .map(|(_, v)| v.clone())
            };
            let description = get("description").unwrap_or_else(|| {
                // No front matter: the first line of the prompt says what it does.
                let first = body.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
                let first = first.trim().trim_start_matches('#').trim();
                first.chars().take(80).collect()
            });
            found.retain(|c| c.name != name);
            found.push(CustomCommand {
                name,
                description,
                args: get("args")
                    .or_else(|| get("argument-hint"))
                    .unwrap_or_default(),
                path,
                source: source.clone(),
            });
        }
    }
    found.sort_by(|a, b| a.name.cmp(&b.name));
    (found, failed)
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, out);
        } else if path.extension().is_some_and(|e| e == "md") {
            out.push(path);
        }
    }
}

/// `/<name> args` for one of `commands`: the rendered prompt and the command's name.
pub fn expand(commands: &[CustomCommand], prompt: &str) -> Option<Result<String, String>> {
    let rest = prompt.strip_prefix('/')?;
    let (name, args) = rest
        .split_once(char::is_whitespace)
        .map_or((rest, ""), |(n, a)| (n, a));
    let cmd = commands.iter().find(|c| c.name == name)?;
    Some(cmd.render(args))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders() {
        assert_eq!(
            fill("Review {{ args }} now.", "the parser"),
            "Review the parser now."
        );
        assert_eq!(
            fill("Focus: {{args | default(\"correctness\")}}.", ""),
            "Focus: correctness."
        );
        assert_eq!(fill("Fix $ARGUMENTS.", "#12"), "Fix #12.");
        assert_eq!(
            fill("Explain this repo.", "briefly"),
            "Explain this repo.\n\nbriefly"
        );
        assert_eq!(
            fill("Keep {{ other }} as is.", ""),
            "Keep {{ other }} as is."
        );
    }

    #[test]
    fn discovered_by_path_with_layers_overriding() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (user, project) = (tmp.path().join("user"), tmp.path().join("project"));
        std::fs::create_dir_all(user.join("test")).expect("mkdir");
        std::fs::create_dir_all(&project).expect("mkdir");
        std::fs::write(
            user.join("review.md"),
            "---\ndescription: Review the diff\nargs: \"[focus]\"\n---\n\nReview, focusing on {{ args }}.\n",
        )
        .expect("write");
        std::fs::write(
            user.join("test").join("flaky.md"),
            "# Find flaky tests\n\nLook.\n",
        )
        .expect("write");
        std::fs::write(project.join("review.md"), "Project review: $ARGUMENTS").expect("write");
        std::fs::write(project.join("bad name.md"), "x").expect("write");
        let (cmds, failed) = discover(&[("user".into(), user), ("project".into(), project)]);
        let names: Vec<(&str, &str)> = cmds
            .iter()
            .map(|c| (c.name.as_str(), c.source.as_str()))
            .collect();
        assert_eq!(names, [("review", "project"), ("test:flaky", "user")]);
        assert_eq!(cmds[1].description, "Find flaky tests");
        assert_eq!(failed.len(), 1, "{failed:?}");
        let r = expand(&cmds, "/review the cache")
            .expect("found")
            .expect("renders");
        assert_eq!(r, "Project review: the cache");
        assert!(expand(&cmds, "/nope").is_none());
        assert!(expand(&cmds, "plain text").is_none());
    }
}
