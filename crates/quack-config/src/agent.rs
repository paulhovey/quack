//! `agents.d/<id>.toml` schema: sub-agents the model can start with the `task` tool
//! (docs/plan/04-agent-loop-and-tools.md#sub-agents).
//!
//! An agent needs only a description, which the model reads to choose one, and usually a
//! prompt:
//!
//! ```toml
//! description = "Finds where something is implemented and reports back"
//! prompt = "Search the code with read-only tools and report what you found."
//! ```

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentFile {
    /// Defaults to the file stem. The model names the agent by it.
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// What the agent is for. The model reads it in the `task` tool's description.
    pub description: String,
    /// Added after the model's system prompt for the agent's whole run.
    #[serde(default)]
    pub prompt: String,
    /// Internal tool names the agent may use (`read`, `grep`, `shell`, ...); `mcp` adds the
    /// MCP servers' tools. Empty: every tool the session has. Sub-agents never get `task`
    /// or `ask_user`.
    #[serde(default)]
    pub tools: Vec<String>,
    /// The agent changes nothing: edits are refused and only commands that read run, as in
    /// plan mode. Read-only agents may run in plan mode, and several started in one reply
    /// run at the same time.
    #[serde(default)]
    pub read_only: bool,
    /// `small` (the `[model] small` model), `<provider>/<model>`, or empty for the model
    /// running the turn. Falls back to that model when the one named cannot load.
    #[serde(default)]
    pub model: String,
    /// Model calls before the agent stops and reports what it has.
    #[serde(default)]
    pub max_steps: Option<u32>,
}

fn yes() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_lines_are_enough() {
        let a: AgentFile = toml::from_str("description = \"d\"\nprompt = \"p\"").expect("parses");
        assert!(a.enabled && !a.read_only && a.tools.is_empty() && a.model.is_empty());
    }

    #[test]
    fn default_agents_parse() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../defaults/agents.d");
        for e in std::fs::read_dir(&dir).expect("defaults/agents.d") {
            let p = e.expect("entry").path();
            if p.extension().is_some_and(|x| x == "toml") {
                let text = std::fs::read_to_string(&p).expect("read");
                toml::from_str::<AgentFile>(&text)
                    .unwrap_or_else(|e| panic!("{}: {e}", p.display()));
            }
        }
    }
}
