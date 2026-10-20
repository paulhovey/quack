//! minijinja prompt rendering. The output must stay byte-stable for a session, so volatile
//! values (date, git state) are captured once at session start by the caller.

use serde::Serialize;

use crate::Profile;

/// Template variables (docs/plan/03-model-profiles.md#templates).
#[derive(Debug, Clone, Default, Serialize)]
pub struct PromptVars {
    pub os: String,
    pub shell: String,
    pub cwd: String,
    pub workspace_root: String,
    pub date: String,
    pub model: String,
    pub context_window: Option<u32>,
    /// Merged AGENTS.md content.
    pub instructions: String,
    pub folder_listing: String,
    /// Branch at session start, if a git repository.
    pub git_branch: String,
    /// The memory indexes (global, then project), already capped.
    pub memory_index: String,
    /// One line per installed skill, already capped.
    pub skills: String,
    /// The names the model sees for the session's edit tool (the `[tools] edit_format`
    /// in use) and for `write`. The same name when files are only rewritten whole.
    pub edit_tool: String,
    pub write_tool: String,
}

impl Profile {
    pub fn render_system(&self, vars: &PromptVars) -> Result<String, String> {
        self.render("system.md", &self.system_template, vars)
    }

    /// The per-session context for the first user message; empty without `context.md`.
    pub fn render_context(&self, vars: &PromptVars) -> Result<String, String> {
        self.context_template
            .as_deref()
            .map_or(Ok(String::new()), |t| self.render("context.md", t, vars))
    }

    fn render(&self, name: &str, template: &str, vars: &PromptVars) -> Result<String, String> {
        let mut env = minijinja::Environment::new();
        env.add_template(name, template)
            .map_err(|e| format!("profile `{}`: {name}: template error: {e}", self.id))?;
        let tmpl = env
            .get_template(name)
            .map_err(|e| format!("profile `{}`: {e}", self.id))?;
        let out = tmpl
            .render(minijinja::value::Serde(vars))
            .map_err(|e| format!("profile `{}`: {name}: render error: {e}", self.id))?;
        Ok(out.trim().to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_and_conditionals_work() {
        let mut p = Profile::empty();
        p.system_template =
            "You run on {{ os }} with {{ shell }}.{% if instructions %}\n\n{{ instructions }}{% endif %}\n".into();
        let vars = PromptVars {
            os: "linux".into(),
            shell: "bash".into(),
            ..PromptVars::default()
        };
        assert_eq!(
            p.render_system(&vars),
            Ok("You run on linux with bash.".into())
        );
        assert_eq!(Profile::empty().render_system(&vars), Ok(String::new()));
        assert_eq!(p.render_context(&vars), Ok(String::new()), "no context.md");
        p.context_template = Some("<context>{{ os }}</context>\n".into());
        assert_eq!(
            p.render_context(&vars),
            Ok("<context>linux</context>".into())
        );
    }
}
