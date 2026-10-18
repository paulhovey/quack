//! `providers.d/<id>.toml` schema (docs/plan/02-providers.md#provider-file-schema).

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::secret::Secret;

type JsonMap = serde_json::Map<String, serde_json::Value>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProviderFile {
    /// Defaults to the file stem.
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: String,
    /// Wire protocol: `openai-chat`, `openai-responses`, `anthropic-messages`, `ollama`.
    pub api: String,
    pub base_url: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default)]
    pub api_key: Option<Secret>,
    /// Default `Authorization`.
    #[serde(default)]
    pub auth_header: Option<String>,
    /// Default `Bearer`; empty sends the raw key.
    #[serde(default)]
    pub auth_scheme: Option<String>,
    #[serde(default)]
    pub headers: BTreeMap<String, Secret>,
    #[serde(default)]
    pub tls: Tls,
    #[serde(default)]
    pub http: Http,
    #[serde(default)]
    pub discovery: Discovery,
    #[serde(default)]
    pub models: Vec<ModelEntry>,
}

fn yes() -> bool {
    true
}

impl ProviderFile {
    /// Find a model by id or alias.
    pub fn model(&self, name: &str) -> Option<&ModelEntry> {
        self.models
            .iter()
            .find(|m| m.id == name || m.alias.as_deref() == Some(name))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Tls {
    /// Extra CA, in addition to the OS store.
    pub ca_file: Option<String>,
    pub client_cert: Option<String>,
    pub client_key: Option<String>,
    /// Warns loudly; can be blocked by policy.
    pub insecure_skip_verify: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Http {
    pub timeout_connect_secs: u64,
    /// Long prefill on big contexts can be slow.
    pub timeout_idle_secs: u64,
    pub max_retries: u32,
    /// Overrides `[network].proxy`.
    pub proxy: String,
}

impl Default for Http {
    fn default() -> Self {
        Self {
            timeout_connect_secs: 10,
            timeout_idle_secs: 300,
            max_retries: 3,
            proxy: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Discovery {
    /// Query the models endpoint at startup (async) and merge with `[[models]]`.
    pub enabled: bool,
    /// Model id globs to keep.
    pub include: Vec<String>,
    pub default_profile: String,
}

impl Default for Discovery {
    fn default() -> Self {
        Self {
            enabled: false,
            include: Vec::new(),
            default_profile: "auto".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ModelEntry {
    pub id: String,
    /// Short name shown in the UI and accepted in `<provider>/<alias>`.
    #[serde(default)]
    pub alias: Option<String>,
    #[serde(default)]
    pub context_window: Option<u32>,
    #[serde(default)]
    pub max_output: Option<u32>,
    /// `auto` or a profile id.
    #[serde(default = "auto")]
    pub profile: String,
    #[serde(default)]
    pub vision: bool,
    #[serde(default)]
    pub tool_calling: ToolCalling,
    #[serde(default)]
    pub reasoning: Reasoning,
    #[serde(default)]
    pub tokenizer: Tokenizer,
    #[serde(default)]
    pub sampling: Sampling,
    /// Merged verbatim into the request JSON.
    #[serde(default)]
    #[schemars(with = "JsonMap")]
    pub extra_body: toml::Table,
    #[serde(default)]
    pub cost: Option<Cost>,
    #[serde(default)]
    pub serving: Serving,
}

fn auto() -> String {
    "auto".into()
}

impl ModelEntry {
    pub fn display_name(&self) -> &str {
        self.alias.as_deref().unwrap_or(&self.id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ToolCalling {
    pub mode: ToolCallMode,
    /// `auto`, `hermes`, `qwen3-xml`, `mistral`, `glm`, `json-block`, `deepseek`, `kimi`.
    pub text_format: String,
    pub parallel: bool,
}

impl Default for ToolCalling {
    fn default() -> Self {
        Self {
            mode: ToolCallMode::Native,
            text_format: "auto".into(),
            parallel: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ToolCallMode {
    Native,
    Text,
    Grammar,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Reasoning {
    pub format: ReasoningFormat,
    /// Default comes from the profile.
    pub preserve_in_history: Option<bool>,
    pub effort: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ReasoningFormat {
    #[default]
    None,
    #[serde(rename = "reasoning_content")]
    ReasoningContent,
    Reasoning,
    ThinkTags,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Tokenizer {
    pub source: TokenizerSource,
    /// For `hf-file`: relative to the provider file's directory, or absolute.
    pub path: String,
    /// For `tiktoken`, e.g. `o200k_base`.
    pub encoding: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum TokenizerSource {
    HfFile,
    Tiktoken,
    Server,
    #[default]
    Estimate,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Sampling {
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub top_k: Option<u32>,
    pub min_p: Option<f64>,
    pub repetition_penalty: Option<f64>,
}

/// Per million tokens.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Cost {
    pub input: f64,
    pub output: f64,
    pub cached_input: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Serving {
    /// Prefill tokens/second hint for cache-aware pruning. Unset: measure.
    pub prefill_tps: Option<u32>,
    /// Does the server reuse the KV prefix?
    pub prefix_cache: bool,
}

impl Default for Serving {
    fn default() -> Self {
        Self {
            prefill_tps: None,
            prefix_cache: true,
        }
    }
}
