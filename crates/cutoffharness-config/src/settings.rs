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
    pub defaults: DefaultsSettings,
    /// Keybinding overrides: action name → key chord.
    pub keys: BTreeMap<String, String>,
    /// Only honoured in the system layer.
    pub policy: Policy,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ModelSettings {
    /// `<provider-id>/<model-id>` or `<provider-id>/<alias>`. Empty: pick with `/models`.
    pub default: String,
    /// Model for titles, summaries and cheap subtasks. Empty: use `default`.
    pub small: String,
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
    /// Capture the mouse. Off by default because it breaks native text selection.
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
            mouse: false,
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
    /// Collapse to the status bar once everything is ready.
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
}

impl Default for ToolSettings {
    fn default() -> Self {
        Self {
            edit_format: EditFormat::Auto,
            shell: "auto".into(),
            default_timeout_secs: 120,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EditFormat {
    Auto,
    StrReplace,
    ApplyPatch,
    WholeFile,
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
}

impl PermissionMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::Ask => "ask",
            Self::AutoEdit => "auto-edit",
            Self::FullAuto => "full-auto",
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
    /// Store project memory under `.cutoffharness/memory` so it can be committed.
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
    /// Instruction file names. Other harnesses' files are detected and offered, never loaded silently.
    #[serde(deserialize_with = "list::de")]
    #[schemars(with = "ListOrAppend")]
    pub files: Vec<String>,
    pub max_bytes: u64,
}

impl Default for InstructionSettings {
    fn default() -> Self {
        Self {
            files: vec!["AGENTS.md".into()],
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

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct AllowList {
    /// Id globs. Empty allows everything.
    #[serde(deserialize_with = "list::de")]
    #[schemars(with = "ListOrAppend")]
    pub allowed: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ProjectConfigPolicy {
    #[default]
    TrustedOnly,
    Never,
    Always,
}
