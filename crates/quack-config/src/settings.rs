//! The `config.toml` settings tree. Every field has a compiled default, so any partial
//! file deserializes; unknown keys are errors.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::list::{self, ListOrAppend};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub model: ModelSettings,
    pub ui: UiSettings,
    pub context: ContextSettings,
    pub tools: ToolSettings,
    pub permissions: PermissionSettings,
    pub sandbox: SandboxSettings,
    pub memory: MemorySettings,
    pub sessions: SessionSettings,
    pub network: NetworkSettings,
    pub instructions: InstructionSettings,
    pub skills: SkillSettings,
    pub telemetry: TelemetrySettings,
    /// Disable provider files from lower layers without deleting them.
    pub providers: ItemToggles,
    pub mcp: ItemToggles,
    pub lsp: ItemToggles,
    /// Turn off hooks from lower layers; the system layer's hooks cannot be turned off.
    pub hooks: ItemToggles,
    pub defaults: DefaultsSettings,
    /// Keybinding overrides: action name → key chord.
    pub keys: BTreeMap<String, String>,
    /// Only honoured in the system layer.
    pub policy: Policy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ModelSettings {
    /// `<provider-id>/<model-id>` or `<provider-id>/<alias>`. Empty: pick with `/models`.
    pub default: String,
    /// Model for titles, summaries and cheap subtasks. Empty: use `default`.
    pub small: String,
    /// Model that writes plans in plan mode, e.g. a larger one. Accepting the plan hands it
    /// to the session's model as one message, instead of the planner's transcript. Empty:
    /// plan with the session's model.
    pub planner: String,
    /// Start with the model last switched to, when it is still available. A project's
    /// `model.default`, `QUACK_MODEL` and `-m` still win; `default` is the fallback.
    pub remember_last: bool,
    /// A profile id for every model this session, e.g. `qwen-code`. Empty: each model's
    /// own `profile` (normally `auto`).
    pub profile: String,
}

impl Default for ModelSettings {
    fn default() -> Self {
        Self {
            default: String::new(),
            small: String::new(),
            planner: String::new(),
            remember_last: true,
            profile: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct UiSettings {
    /// `auto`, `dark`, `light`, or a theme name from `themes/`.
    pub theme: String,
    pub show_reasoning: ShowReasoning,
    pub startup_panel: StartupPanel,
    /// Use ASCII instead of box-drawing and spinner glyphs.
    pub ascii: bool,
    /// Capture the mouse for wheel scrolling, clicks in the input and selecting text to copy.
    /// The terminal's own selection then needs Shift held; turn this off to leave it to the
    /// terminal.
    pub mouse: bool,
    /// Replace spinners with static indicators.
    pub reduced_motion: bool,
}

impl Default for UiSettings {
    fn default() -> Self {
        Self {
            theme: "auto".into(),
            show_reasoning: ShowReasoning::Collapsed,
            startup_panel: StartupPanel::Auto,
            ascii: false,
            mouse: true,
            reduced_motion: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ShowReasoning {
    Hidden,
    Collapsed,
    Expanded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum StartupPanel {
    /// Show only when something needs attention or startup is slow (over half a second).
    Auto,
    Always,
    Never,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ContextSettings {
    /// Fraction of the model's context window we plan to use.
    pub budget_ratio: f64,
    pub cache_strategy: CacheStrategy,
    pub compaction_threshold: f64,
}

impl Default for ContextSettings {
    fn default() -> Self {
        Self {
            budget_ratio: 0.85,
            cache_strategy: CacheStrategy::Auto,
            compaction_threshold: 0.80,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum CacheStrategy {
    Preserve,
    Aggressive,
    Auto,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ToolSettings {
    pub edit_format: EditFormat,
    /// `auto`, `bash`, `zsh`, `pwsh`, `powershell`, `cmd`, or a path.
    pub shell: String,
    pub default_timeout_secs: u64,
    /// `read` without a line range shows the outline of source files larger than this
    /// (in tokens), so the model reads only the part it needs. 0: always the whole file.
    pub read_outline_tokens: u32,
}

impl Default for ToolSettings {
    fn default() -> Self {
        Self {
            edit_format: EditFormat::Auto,
            shell: "auto".into(),
            default_timeout_secs: 120,
            read_outline_tokens: 2000,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EditFormat {
    /// The profile's choice, else `str_replace`.
    Auto,
    /// `edit`: replace a unique snippet.
    StrReplace,
    /// Hash-anchored edits: `read` tags each line, and edits name lines by tag.
    Hashline,
    /// `apply_patch`: the Codex patch format, several files per call.
    ApplyPatch,
    /// No edit tool: files are rewritten whole with `write`.
    WholeFile,
    /// `edit_blocks`: SEARCH/REPLACE blocks under each file's path.
    SearchReplaceBlocks,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct PermissionSettings {
    pub mode: PermissionMode,
    #[serde(deserialize_with = "list::de")]
    #[schemars(with = "ListOrAppend")]
    pub allow: Vec<String>,
    #[serde(deserialize_with = "list::de")]
    #[schemars(with = "ListOrAppend")]
    pub ask: Vec<String>,
    #[serde(deserialize_with = "list::de")]
    #[schemars(with = "ListOrAppend")]
    pub deny: Vec<String>,
}

/// Ordered from least to most permissive, so policy can cap with `min`.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum PermissionMode {
    ReadOnly,
    #[default]
    Ask,
    AutoEdit,
    FullAuto,
    /// No approvals and no sandbox. Only the built-in deny list and `deny` rules apply.
    Yolo,
}

impl PermissionMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::Ask => "ask",
            Self::AutoEdit => "auto-edit",
            Self::FullAuto => "full-auto",
            Self::Yolo => "yolo",
        }
    }
}

/// The OS sandbox for shell commands (docs/plan/11-security.md#os-sandboxing).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct SandboxSettings {
    pub mode: SandboxMode,
    /// More folders commands may write to, besides the workspace and temp folders
    /// (e.g. `["~/.cargo", "~/.cache"]`).
    #[serde(deserialize_with = "list::de")]
    #[schemars(with = "ListOrAppend")]
    pub writable: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxMode {
    /// `workspace-write` where this OS supports it (Linux with Landlock, macOS);
    /// elsewhere commands run without a sandbox, with a notice.
    #[default]
    Auto,
    /// Commands run without a sandbox.
    Off,
    /// Commands may write only in the workspace and temp folders, with no network.
    WorkspaceWrite,
    /// Like `workspace-write`, with network access.
    WorkspaceWriteNet,
    /// Commands may not write anywhere, and have no network.
    ReadOnly,
}

impl SandboxMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Off => "off",
            Self::WorkspaceWrite => "workspace-write",
            Self::WorkspaceWriteNet => "workspace-write-net",
            Self::ReadOnly => "read-only",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct MemorySettings {
    /// Where memory notes live. Empty: `<user data>/memory`.
    pub path: String,
    pub project_scope: MemoryScope,
    /// Whether the model's memory writes need approval.
    pub writes: MemoryWrites,
    /// Cap on the memory index in the system prompt.
    pub index_max_tokens: u32,
}

impl Default for MemorySettings {
    fn default() -> Self {
        Self {
            path: String::new(),
            project_scope: MemoryScope::default(),
            writes: MemoryWrites::default(),
            index_max_tokens: 1_500,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum MemoryWrites {
    /// Writes show in the transcript and are easy to undo.
    #[default]
    Auto,
    Ask,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum MemoryScope {
    #[default]
    DataDir,
    /// Store project memory under `.quack/memory` so it can be committed.
    Repo,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct SessionSettings {
    /// Empty: `<user data>/sessions`.
    pub path: String,
    /// 0 keeps sessions forever.
    pub retention_days: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct NetworkSettings {
    /// Block all egress except provider base URLs and `egress_allow`.
    pub offline: bool,
    #[serde(deserialize_with = "list::de")]
    #[schemars(with = "ListOrAppend")]
    pub egress_allow: Vec<String>,
    /// Empty: `HTTPS_PROXY` / system.
    pub proxy: String,
    /// Extra PEM roots, in addition to the OS store.
    pub ca_bundle: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct InstructionSettings {
    /// Instruction file names, in order of preference; the first one in a folder is read.
    #[serde(deserialize_with = "list::de")]
    #[schemars(with = "ListOrAppend")]
    pub files: Vec<String>,
    /// Also read other agents' files listed in `compat.toml` (CLAUDE.md, GEMINI.md,
    /// .cursorrules, …) where there is no file from `files`.
    pub other_agents: bool,
    pub max_bytes: u64,
}

impl Default for InstructionSettings {
    fn default() -> Self {
        Self {
            files: vec!["AGENTS.md".into()],
            other_agents: true,
            max_bytes: 32 * 1024,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct SkillSettings {
    /// More skill folders, e.g. another harness's `.claude/skills` once you accept it.
    /// Relative paths are from the workspace root.
    #[serde(deserialize_with = "list::de")]
    #[schemars(with = "ListOrAppend")]
    pub compat_paths: Vec<String>,
    /// Skill names to skip.
    #[serde(deserialize_with = "list::de")]
    #[schemars(with = "ListOrAppend")]
    pub disabled: Vec<String>,
    /// Cap on the skill list in the system prompt.
    pub index_max_tokens: u32,
}

impl Default for SkillSettings {
    fn default() -> Self {
        Self {
            compat_paths: Vec::new(),
            disabled: Vec::new(),
            index_max_tokens: 1_500,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct TelemetrySettings {
    /// Empty disables telemetry. Never enabled implicitly.
    pub otel_endpoint: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ItemToggles {
    #[serde(deserialize_with = "list::de")]
    #[schemars(with = "ListOrAppend")]
    pub disabled: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct DefaultsSettings {
    /// Override the defaults bundle location; `none` disables it.
    pub dir: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Policy {
    /// Dotted settings keys that lower-precedence layers cannot change.
    #[serde(deserialize_with = "list::de")]
    #[schemars(with = "ListOrAppend")]
    pub locked: Vec<String>,
    pub providers: AllowList,
    pub mcp: AllowList,
    pub project_config: ProjectConfigPolicy,
    pub max_permission_mode: Option<PermissionMode>,
}

impl Policy {
    /// Whether changing `key` would change a locked setting: the key itself is locked, it
    /// sits inside a locked table, or it is a table holding a locked key.
    pub fn locks(&self, key: &str) -> bool {
        let under = |a: &str, b: &str| a.strip_prefix(b).is_some_and(|r| r.starts_with('.'));
        self.locked
            .iter()
            .any(|l| l == key || under(key, l) || under(l, key))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct AllowList {
    /// Id globs. Empty allows everything.
    #[serde(deserialize_with = "list::de")]
    #[schemars(with = "ListOrAppend")]
    pub allowed: Vec<String>,
    /// Host globs a provider's `base_url` or a remote MCP server's `url` must match, e.g.
    /// `["llm.corp.example", "127.0.0.1"]`. Empty allows every host.
    #[serde(deserialize_with = "list::de")]
    #[schemars(with = "ListOrAppend")]
    pub hosts: Vec<String>,
}

impl AllowList {
    /// Whether policy allows an item with this id and, for a network item, this URL.
    pub fn allows(&self, id: &str, url: Option<&str>) -> bool {
        use crate::items::glob_match;
        let id_ok = self.allowed.is_empty() || self.allowed.iter().any(|g| glob_match(g, id));
        let host_ok = match url {
            Some(url) if !self.hosts.is_empty() => {
                let host = crate::url_host(url);
                self.hosts.iter().any(|g| glob_match(g, host))
            }
            _ => true,
        };
        id_ok && host_ok
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ProjectConfigPolicy {
    #[default]
    TrustedOnly,
    Never,
    Always,
}
