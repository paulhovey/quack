//! `profiles.d/<id>/profile.toml` schema (docs/plan/03-model-profiles.md).
//!
//! A minimal profile is a prompt and a provenance line:
//!
//! ```toml
//! description = "Short prompt for unknown models"
//! source = "Original"
//! ```
//!
//! with the prompt in `system.md` next to it. With no `[[tools]]`, every internal tool is
//! exposed under its internal name.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::provider::{ReasoningFormat, Sampling};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProfileFile {
    /// Defaults to the directory name.
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub description: String,
    /// Where the prompt and tool descriptions come from. Required for shipped defaults.
    pub source: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default, rename = "match")]
    pub matching: Match,
    #[serde(default)]
    pub prompt: Prompt,
    #[serde(default)]
    pub defaults: Defaults,
    /// Model-facing tool surface. Empty: all internal tools under their internal names.
    #[serde(default)]
    pub tools: Vec<ToolMapping>,
}

fn yes() -> bool {
    true
}

/// Used when a model's `profile = "auto"`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Match {
    /// Globs over the model id or alias, e.g. `*qwen3-coder*`.
    pub model: Vec<String>,
    /// Highest matching priority wins.
    pub priority: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Prompt {
    /// minijinja template, relative to the profile directory.
    pub system: String,
}

impl Default for Prompt {
    fn default() -> Self {
        Self {
            system: "system.md".into(),
        }
    }
}

/// Defaults that a provider's model entry can override.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Defaults {
    pub sampling: Sampling,
    pub reasoning_format: Option<ReasoningFormat>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ToolMapping {
    /// Name the model sees.
    pub name: String,
    /// Internal tool, e.g. `read`, `edit`, `shell`. Defaults to `name`.
    #[serde(default)]
    pub maps_to: Option<String>,
    /// Description shown to the model; default: the internal tool's own.
    #[serde(default)]
    pub description: Option<String>,
    /// Model-facing parameter name → mapping onto the internal parameter.
    #[serde(default)]
    pub params: BTreeMap<String, ParamMapping>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ParamMapping {
    /// Internal parameter name.
    pub to: String,
    #[serde(default)]
    pub transform: Option<Transform>,
}

/// The closed set of parameter transforms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Transform {
    /// Paths are accepted absolute or relative already; kept for profile compatibility.
    AbsToWorkspace,
    ZeroToOneBased,
}
