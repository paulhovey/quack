//! Agent Skills (`SKILL.md`) discovery and progressive disclosure
//! (docs/plan/08-instructions-skills-commands.md#skills).
//!
//! Only names and descriptions go into the prompt; the model loads a skill's body with the
//! `skill` tool, or the user invokes it with `/<name>`.

use std::path::{Path, PathBuf};

const MAX_DESCRIPTION: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    /// The skill's directory; files the body mentions are relative to it.
    pub dir: PathBuf,
    /// Where it was found, e.g. `user` or `project (.agents/skills)`.
    pub source: String,
}

impl Skill {
    /// The body of `SKILL.md`, without front matter.
    pub fn body(&self) -> Result<String, String> {
        let path = self.dir.join("SKILL.md");
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        Ok(parse(&text)?.1.trim().to_owned())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Found {
    pub skills: Vec<Skill>,
    /// One line per skill that failed validation.
    pub failed: Vec<String>,
}

/// Skills from `roots` (lowest precedence first; a later skill with the same name replaces
/// an earlier one). `disabled` names are skipped.
pub fn discover(roots: &[(String, PathBuf)], disabled: &[String]) -> Found {
    let mut found = Found::default();
    for (source, root) in roots {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        let mut dirs: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.join("SKILL.md").is_file())
            .collect();
        dirs.sort();
        for dir in dirs {
            match load(&dir, source) {
                Ok(skill) if disabled.contains(&skill.name) => {}
                Ok(skill) => {
                    found.skills.retain(|s| s.name != skill.name);
                    found.skills.push(skill);
                }
                Err(e) => found
                    .failed
                    .push(format!("{}: {e}", dir.join("SKILL.md").display())),
            }
        }
    }
    found.skills.sort_by(|a, b| a.name.cmp(&b.name));
    found
}

fn load(dir: &Path, source: &str) -> Result<Skill, String> {
    let text = std::fs::read_to_string(dir.join("SKILL.md")).map_err(|e| e.to_string())?;
    let (front, _) = parse(&text)?;
    let get = |k: &str| {
        front
            .iter()
            .find(|(key, _)| key == k)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    };
    let name = get("name");
    let dir_name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if name.is_empty() {
        return Err("front matter has no `name`".into());
    }
    if name != dir_name {
        return Err(format!(
            "`name: {name}` does not match the directory `{dir_name}`"
        ));
    }
    let description = get("description");
    if description.is_empty() {
        return Err("front matter has no `description`".into());
    }
    if description.chars().count() > MAX_DESCRIPTION {
        return Err(format!("description is over {MAX_DESCRIPTION} characters"));
    }
    Ok(Skill {
        name,
        description,
        dir: dir.to_path_buf(),
        source: source.to_owned(),
    })
}

/// Split `---` front matter from the body. Handles the YAML the format uses in practice:
/// `key: value`, quoted values, and `>` / `|` blocks of indented lines. Nested maps (such as
/// `metadata:`) are skipped.
pub fn parse(text: &str) -> Result<(Vec<(String, String)>, String), String> {
    let text = text.trim_start_matches('\u{feff}');
    let Some(rest) = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))
    else {
        return Err("SKILL.md must start with `---` front matter".into());
    };
    let (front, body) = match rest.find("\n---") {
        Some(end) => {
            let after = &rest[end + 4..];
            (&rest[..end], after.split_once('\n').map_or("", |(_, b)| b))
        }
        None => return Err("front matter is not closed with `---`".into()),
    };
    let mut fields: Vec<(String, String)> = Vec::new();
    let mut block: Option<(String, bool, Vec<String>)> = None;
    let flush = |fields: &mut Vec<(String, String)>, block: Option<(String, bool, Vec<String>)>| {
        if let Some((key, folded, lines)) = block {
            let value = if folded {
                lines.join(" ")
            } else {
                lines.join("\n")
            };
            fields.push((key, value.trim().to_owned()));
        }
    };
    for line in front.lines() {
        let line = line.trim_end_matches('\r');
        if line.starts_with([' ', '\t']) {
            if let Some((_, _, lines)) = &mut block {
                lines.push(line.trim().to_owned());
            }
            continue;
        }
        flush(&mut fields, block.take());
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let (key, value) = (key.trim().to_owned(), value.trim());
        match value {
            ">" | ">-" | ">+" => block = Some((key, true, Vec::new())),
            "|" | "|-" | "|+" => block = Some((key, false, Vec::new())),
            // A nested map: its indented lines are skipped.
            "" => block = None,
            v => fields.push((key, unquote(v))),
        }
    }
    flush(&mut fields, block);
    Ok((fields, body.to_owned()))
}

fn unquote(v: &str) -> String {
    for q in ['"', '\''] {
        if let Some(inner) = v.strip_prefix(q).and_then(|r| r.strip_suffix(q)) {
            return inner.replace("\\\"", "\"").replace("''", "'");
        }
    }
    v.to_owned()
}

/// The prompt index: one line per skill, cut to `max_bytes`. Descriptions are shortened
/// before skills are dropped.
pub fn index(skills: &[Skill], max_bytes: usize) -> String {
    let line = |s: &Skill, max_desc: usize| {
        let d: String = s.description.chars().take(max_desc).collect();
        let cut = if d.len() < s.description.len() {
            "…"
        } else {
            ""
        };
        format!("- {}: {d}{cut}", s.name)
    };
    let mut max_desc = MAX_DESCRIPTION;
    loop {
        let lines: Vec<String> = skills.iter().map(|s| line(s, max_desc)).collect();
        let text = lines.join("\n");
        if text.len() <= max_bytes || max_desc <= 80 {
            if text.len() <= max_bytes {
                return text;
            }
            // Still too long with short descriptions: keep the skills that fit.
            let mut out = String::new();
            for l in lines {
                if out.len() + l.len() + 1 > max_bytes {
                    break;
                }
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(&l);
            }
            return out;
        }
        max_desc /= 2;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill(root: &Path, dir: &str, front: &str, body: &str) {
        let d = root.join(dir);
        std::fs::create_dir_all(&d).expect("mkdir");
        std::fs::write(d.join("SKILL.md"), format!("---\n{front}\n---\n{body}")).expect("write");
    }

    #[test]
    fn front_matter_forms() {
        let (f, body) = parse(
            "---\nname: x\ndescription: >\n  folded\n  text\nnotes: |\n  a\n  b\nmetadata:\n  k: v\nquoted: \"q: v\"\n---\n# Body\n",
        )
        .expect("parse");
        let get = |k: &str| f.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str());
        assert_eq!(get("description"), Some("folded text"));
        assert_eq!(get("notes"), Some("a\nb"));
        assert_eq!(get("quoted"), Some("q: v"));
        assert_eq!(get("k"), None);
        assert_eq!(body, "# Body\n");
        assert!(parse("no front matter").is_err());
    }

    #[test]
    fn layers_override_and_invalid_skills_are_reported() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (user, project) = (tmp.path().join("u"), tmp.path().join("p"));
        skill(
            &user,
            "deploy",
            "name: deploy\ndescription: user version",
            "u",
        );
        skill(&user, "lint", "name: lint\ndescription: lints", "l");
        skill(
            &project,
            "deploy",
            "name: deploy\ndescription: project version",
            "p",
        );
        skill(&project, "bad", "name: other\ndescription: d", "");
        skill(&project, "nodesc", "name: nodesc", "");
        skill(&project, "off", "name: off\ndescription: d", "");
        let found = discover(
            &[("user".into(), user), ("project".into(), project)],
            &["off".into()],
        );
        let names: Vec<(&str, &str)> = found
            .skills
            .iter()
            .map(|s| (s.name.as_str(), s.description.as_str()))
            .collect();
        assert_eq!(names, [("deploy", "project version"), ("lint", "lints")]);
        assert_eq!(found.failed.len(), 2);
        assert_eq!(found.skills[0].body().expect("body"), "p");
    }

    #[test]
    fn index_shortens_descriptions_first() {
        let s = |n: &str| Skill {
            name: n.into(),
            description: "d".repeat(300),
            dir: PathBuf::new(),
            source: String::new(),
        };
        let skills = [s("a"), s("b")];
        assert_eq!(index(&skills, 10_000).len(), 2 * 305 + 1);
        let short = index(&skills, 400);
        assert_eq!(short.lines().count(), 2);
        assert!(short.len() <= 400 && short.contains('…'));
        assert_eq!(index(&skills, 100).lines().count(), 1);
    }
}
