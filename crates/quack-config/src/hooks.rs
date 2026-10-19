//! `hooks.d/<id>.toml` schema (docs/plan/11-security.md#hooks).
//!
//! A hook needs only its event and its command:
//!
//! ```toml
//! event = "user_prompt"
//! match = { prompt = '/browse/[A-Z][A-Z0-9_]*-\d+' }
//! command = ["{python}", "{dir}/jira/jira.py"]
//! env = { JIRA_TOKEN = { keyring = "jira" } }
//! ```
//!
//! The command gets the event as JSON on stdin and may answer with JSON on stdout.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::secret::Secret;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HookFile {
    /// Defaults to the file stem.
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default)]
    pub description: String,
    pub event: HookEvent,
    /// Which calls or prompts start the hook. Empty: every one.
    #[serde(default, rename = "match")]
    pub matcher: HookMatch,
    /// Program and arguments. `{dir}` is this file's folder, `{python}` the Python found on
    /// `PATH` (`python3`, `python` or `py`), and a leading `~/` the home folder.
    pub command: Vec<String>,
    /// Extra environment variables for the command; values may be secrets.
    #[serde(default)]
    pub env: BTreeMap<String, Secret>,
    /// Variables passed through from the harness's own environment. Default: a minimal set
    /// (`PATH`, `HOME`, temp and locale variables, proxies, and what Windows needs).
    #[serde(default)]
    pub inherit_env: Option<Vec<String>>,
    /// Working directory; default: the workspace root.
    #[serde(default)]
    pub cwd: String,
    #[serde(default = "timeout")]
    pub timeout_secs: u64,
    /// What a failure (a crash, a timeout, unreadable output) means.
    #[serde(default)]
    pub on_error: OnError,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HookEvent {
    /// A session opens: at startup, `/new` or `/resume`. Context it returns is added to
    /// the session's next prompt.
    SessionStart,
    /// The user sent a prompt. The hook can add context to it, or block it.
    UserPrompt,
    /// A tool call is about to run. The hook can allow, deny or ask, and change the
    /// arguments.
    PreTool,
    /// A tool call finished. Context it returns is added to the result the model sees.
    PostTool,
    /// A turn finished or was stopped.
    TurnEnd,
    /// The session closes: on quit, `/new` or `/resume`.
    SessionEnd,
    /// The conversation is about to be summarized. Context it returns guides the summary.
    PreCompact,
}

impl HookEvent {
    pub fn label(self) -> &'static str {
        match self {
            Self::SessionStart => "session_start",
            Self::UserPrompt => "user_prompt",
            Self::PreTool => "pre_tool",
            Self::PostTool => "post_tool",
            Self::TurnEnd => "turn_end",
            Self::SessionEnd => "session_end",
            Self::PreCompact => "pre_compact",
        }
    }

    /// Whether `additional_context` from this event reaches the model.
    pub fn takes_context(self) -> bool {
        matches!(
            self,
            Self::SessionStart | Self::UserPrompt | Self::PostTool | Self::PreCompact
        )
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HookMatch {
    /// `pre_tool`, `post_tool`: tool name glob, e.g. `shell` or `mcp__github__*`.
    #[serde(default)]
    pub tool: String,
    /// `pre_tool`, `post_tool`: glob on the call's target: a shell command, a file path
    /// relative to the workspace, or otherwise the arguments as JSON.
    #[serde(default)]
    pub pattern: String,
    /// `user_prompt`: regular expression searched for in the prompt. Its matches are sent
    /// to the command as `matches`.
    #[serde(default)]
    pub prompt: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OnError {
    /// Block what the hook guards (the prompt or the tool call).
    Deny,
    /// Carry on quietly (the failure is still logged).
    Allow,
    /// Carry on with a warning.
    #[default]
    Warn,
}

impl HookFile {
    /// Problems the file's shape alone shows.
    pub fn check(&self) -> Result<(), String> {
        if self.command.first().is_none_or(|c| c.trim().is_empty()) {
            return Err("`command` is empty; give the program and its arguments".into());
        }
        let tool_event = matches!(self.event, HookEvent::PreTool | HookEvent::PostTool);
        if !tool_event && !(self.matcher.tool.is_empty() && self.matcher.pattern.is_empty()) {
            return Err(format!(
                "`match.tool` and `match.pattern` apply only to pre_tool and post_tool, not {}",
                self.event.label()
            ));
        }
        if self.event != HookEvent::UserPrompt && !self.matcher.prompt.is_empty() {
            return Err(format!(
                "`match.prompt` applies only to user_prompt, not {}",
                self.event.label()
            ));
        }
        if self.timeout_secs == 0 {
            return Err("`timeout_secs` must be at least 1".into());
        }
        Ok(())
    }
}

fn yes() -> bool {
    true
}

fn timeout() -> u64 {
    10
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_files() {
        let f: HookFile =
            toml::from_str("event = \"turn_end\"\ncommand = [\"notify-send\", \"done\"]\n")
                .expect("parse");
        assert_eq!(f.event, HookEvent::TurnEnd);
        assert_eq!(f.timeout_secs, 10);
        assert_eq!(f.on_error, OnError::Warn);
        assert_eq!(f.check(), Ok(()));

        let f: HookFile = toml::from_str(
            "event = \"pre_tool\"\nmatch = { tool = \"shell\", pattern = \"*psql*prod*\" }\n\
             command = [\"python3\", \"check.py\"]\non_error = \"deny\"\n",
        )
        .expect("parse");
        assert_eq!(f.matcher.tool, "shell");
        assert_eq!(f.on_error, OnError::Deny);
        assert_eq!(f.check(), Ok(()));
    }

    #[test]
    fn misplaced_matchers_and_empty_commands_are_reported() {
        let f: HookFile = toml::from_str(
            "event = \"user_prompt\"\nmatch = { tool = \"shell\" }\ncommand = [\"x\"]\n",
        )
        .expect("parse");
        assert!(f.check().is_err_and(|e| e.contains("match.tool")));
        let f: HookFile =
            toml::from_str("event = \"pre_tool\"\nmatch = { prompt = \"x\" }\ncommand = [\"x\"]\n")
                .expect("parse");
        assert!(f.check().is_err_and(|e| e.contains("match.prompt")));
        let f: HookFile = toml::from_str("event = \"pre_tool\"\ncommand = []\n").expect("parse");
        assert!(f.check().is_err());
        assert!(toml::from_str::<HookFile>("event = \"before_tool\"\ncommand = [\"x\"]").is_err());
    }

    #[test]
    fn shipped_examples_parse() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../defaults/hooks.d");
        let mut n = 0;
        for e in std::fs::read_dir(&dir).expect("defaults/hooks.d") {
            let path = e.expect("entry").path();
            if path.extension().is_some_and(|x| x == "toml") {
                let text = std::fs::read_to_string(&path).expect("read");
                let f: HookFile =
                    toml::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
                assert_eq!(f.check(), Ok(()), "{}", path.display());
                assert!(!f.enabled, "{} must ship disabled", path.display());
                n += 1;
            }
        }
        assert!(n >= 1);
    }
}
