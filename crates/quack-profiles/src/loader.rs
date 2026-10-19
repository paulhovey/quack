//! Reads `profiles.d/<id>/profile.toml` from every layer, and selects one per model.

use std::path::PathBuf;

use quack_config::profile::ProfileFile;
use quack_config::{ItemState, Loaded, glob_match, load_dir_items};

/// A loaded profile with its prompt template read.
#[derive(Debug, Clone)]
pub struct Profile {
    pub id: String,
    pub file: ProfileFile,
    /// Profile directory; `None` for the compiled-in empty profile.
    pub dir: Option<PathBuf>,
    pub system_template: String,
    /// `compact.md` from the profile directory: the compaction prompt, if overridden.
    pub compact_prompt: Option<String>,
    /// `plan.md`: appended to the user's message in plan mode, if overridden.
    pub plan_prompt: Option<String>,
}

impl Profile {
    /// The compiled-in fallback: no prompt beyond the instructions, internal tool names,
    /// no sampling overrides (docs/plan/03-model-profiles.md#selection).
    pub fn empty() -> Self {
        Self {
            id: "empty".into(),
            file: ProfileFile {
                id: Some("empty".into()),
                description: "Compiled-in fallback with no prompt".into(),
                source: "built in".into(),
                enabled: true,
                matching: Default::default(),
                prompt: Default::default(),
                defaults: Default::default(),
                tools: Vec::new(),
                exclusive: false,
            },
            dir: None,
            system_template: "{{ instructions }}".into(),
            compact_prompt: None,
            plan_prompt: None,
        }
    }
}

/// Load all enabled profiles. Failures come back as one-line warnings.
pub fn load_profiles(loaded: &Loaded) -> (Vec<Profile>, Vec<String>) {
    let mut profiles = Vec::new();
    let mut warnings = Vec::new();
    for item in load_dir_items::<ProfileFile>(&loaded.item_layers(), "profiles.d", "profile.toml") {
        match item.state {
            ItemState::Loaded(mut file) => {
                let dir = item.path.parent().map(PathBuf::from).unwrap_or_default();
                // A description naming a `.md` file is read from the profile directory.
                for t in &mut file.tools {
                    let Some(d) = t.description.as_deref().filter(|d| d.ends_with(".md")) else {
                        continue;
                    };
                    match std::fs::read_to_string(dir.join(d)) {
                        Ok(text) => t.description = Some(text.trim().to_owned()),
                        Err(e) => warnings.push(format!(
                            "profile `{}`: cannot read {d} for tool `{}`: {e}",
                            item.id, t.name
                        )),
                    }
                }
                let prompt = dir.join(&file.prompt.system);
                let system_template = match std::fs::read_to_string(&prompt) {
                    Ok(t) => t.trim_start_matches('\u{feff}').to_owned(),
                    Err(e) => {
                        warnings.push(format!(
                            "profile `{}`: cannot read {}: {e}",
                            item.id,
                            prompt.display()
                        ));
                        continue;
                    }
                };
                let optional = |name: &str| {
                    std::fs::read_to_string(dir.join(name))
                        .ok()
                        .map(|t| t.trim_start_matches('\u{feff}').trim().to_owned())
                        .filter(|t| !t.is_empty())
                };
                let compact_prompt = optional("compact.md");
                let plan_prompt = optional("plan.md");
                profiles.push(Profile {
                    id: item.id,
                    file,
                    dir: Some(dir),
                    system_template,
                    compact_prompt,
                    plan_prompt,
                });
            }
            ItemState::Disabled(_) => {}
            ItemState::Failed(d) => warnings.push(d.one_line()),
        }
    }
    (profiles, warnings)
}

/// Pick the profile for a model: explicit id → best `[match]` → `generic` → empty.
pub fn select(
    profiles: &[Profile],
    requested: &str,
    model_names: &[&str],
) -> Result<Profile, String> {
    if !requested.is_empty() && requested != "auto" {
        if requested == "empty" {
            return Ok(Profile::empty());
        }
        return profiles
            .iter()
            .find(|p| p.id == requested)
            .cloned()
            .ok_or_else(|| format!("profile `{requested}` is not installed"));
    }
    let best = profiles
        .iter()
        .filter(|p| {
            p.file.matching.model.iter().any(|g| {
                model_names
                    .iter()
                    .any(|m| glob_match(&g.to_lowercase(), &m.to_lowercase()))
            })
        })
        .max_by_key(|p| p.file.matching.priority);
    Ok(best
        .or_else(|| profiles.iter().find(|p| p.id == "generic"))
        .cloned()
        .unwrap_or_else(Profile::empty))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(id: &str, globs: &[&str], priority: i32) -> Profile {
        let mut p = Profile::empty();
        p.id = id.into();
        p.file.matching.model = globs.iter().map(|g| (*g).to_owned()).collect();
        p.file.matching.priority = priority;
        p
    }

    #[test]
    fn selection_order() {
        let ps = vec![
            profile("generic", &[], 0),
            profile("qwen-code", &["*qwen3-coder*"], 50),
            profile("any-qwen", &["*qwen*"], 10),
        ];
        let pick = |req: &str, m: &str| select(&ps, req, &[m]).map(|p| p.id);
        assert_eq!(
            pick("auto", "Qwen/Qwen3-Coder-480B"),
            Ok("qwen-code".into())
        );
        assert_eq!(pick("auto", "qwen3.8-flash"), Ok("any-qwen".into()));
        assert_eq!(pick("auto", "glm-5"), Ok("generic".into()));
        assert_eq!(pick("any-qwen", "glm-5"), Ok("any-qwen".into()));
        assert!(pick("missing", "glm-5").is_err());
        assert_eq!(
            select(&[], "auto", &["x"]).map(|p| p.id),
            Ok("empty".into())
        );
    }
}
