//! `providers.d/<id>.toml` schema (docs/plan/02-providers.md#provider-file-schema).
//!
//! Only `base_url` is required. A typical file is three lines:
//!
//! ```toml
//! base_url = "https://api.example.com/v1"
//! api_key = { env = "EXAMPLE_API_KEY" }
//! models = ["qwen3-coder"]
//! ```
//!
//! Everything else is optional and defaults to what works for OpenAI-compatible servers.

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
    /// Shown by setup and `/providers`; defaults to the id.
    #[serde(default)]
    pub name: String,
    /// Heading setup lists a hosted service under.
    #[serde(default)]
    pub group: String,
    /// A hint shown by setup, e.g. a server flag tool calls need.
    #[serde(default)]
    pub note: String,
    /// Wire protocol: `openai-chat` (default), `openai-responses`, `anthropic-messages`, `ollama`.
    #[serde(default = "openai_chat")]
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
    /// Model ids as plain strings, or tables for per-model settings.
    #[serde(default, deserialize_with = "models_de")]
    #[schemars(with = "Vec<ModelSpec>")]
    pub models: Vec<ModelEntry>,
}

fn yes() -> bool {
    true
}

fn openai_chat() -> String {
    "openai-chat".into()
}

/// Schema-only view of a `models` element.
#[derive(JsonSchema)]
#[serde(untagged)]
#[allow(dead_code)]
enum ModelSpec {
    Id(String),
    Full(Box<ModelEntry>),
}

/// Accepts `"id"` or a table per element, keeping `deny_unknown_fields` errors for tables
/// (an untagged enum would turn every mistake into "did not match any variant").
fn models_de<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<ModelEntry>, D::Error> {
    struct One(ModelEntry);
    impl<'de> Deserialize<'de> for One {
        fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            struct V;
            impl<'de> serde::de::Visitor<'de> for V {
                type Value = ModelEntry;
                fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    f.write_str("a model id string or a table with `id`")
                }
                fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<ModelEntry, E> {
                    Ok(ModelEntry::new(v))
                }
                fn visit_map<A: serde::de::MapAccess<'de>>(
                    self,
                    m: A,
                ) -> Result<ModelEntry, A::Error> {
                    ModelEntry::deserialize(serde::de::value::MapAccessDeserializer::new(m))
                }
            }
            d.deserialize_any(V).map(One)
        }
    }
    Ok(Vec::<One>::deserialize(d)?
        .into_iter()
        .map(|o| o.0)
        .collect())
}

/// A provider file's text with `api_key` read from the OS credential store entry `name`,
/// keeping its comments and other settings.
pub fn with_keyring_key(text: &str, name: &str) -> Result<String, String> {
    let mut doc: toml_edit::DocumentMut = text.parse().map_err(|e| format!("{e}"))?;
    let mut key = toml_edit::InlineTable::new();
    key.insert("keyring", name.into());
    doc["api_key"] = toml_edit::value(key);
    Ok(doc.to_string())
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
    /// An entry with every setting at its default.
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            alias: None,
            context_window: None,
            max_output: None,
            profile: auto(),
            vision: false,
            tool_calling: ToolCalling::default(),
            reasoning: Reasoning::default(),
            tokenizer: Tokenizer::default(),
            sampling: Sampling::default(),
            extra_body: toml::Table::new(),
            cost: None,
            serving: Serving::default(),
        }
    }

    pub fn display_name(&self) -> &str {
        self.alias.as_deref().unwrap_or(&self.id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ToolCalling {
    pub mode: ToolCallMode,
    /// `auto`, `hermes`, `qwen3-xml`, `glm`, `json-block`, `deepseek`, `kimi`.
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
    /// Read `reasoning_content` / `reasoning` fields, and split `<think>` tags when the reply opens with one.
    #[default]
    Auto,
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

/// US dollars per million tokens, for `/stats`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Cost {
    pub input: f64,
    pub output: f64,
    /// Prompt tokens served from the server's cache. Unset: the `input` price.
    pub cached_input: Option<f64>,
}

impl Cost {
    /// Dollars for one request.
    pub fn of(&self, prompt: u32, cached: u32, completion: u32) -> f64 {
        let cached = cached.min(prompt);
        (f64::from(prompt - cached) * self.input
            + f64::from(cached) * self.cached_input.unwrap_or(self.input)
            + f64::from(completion) * self.output)
            / 1_000_000.0
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyring_key_replaces_the_variable_and_keeps_the_rest() {
        let text = "#:schema ../schemas/provider.json\n# A service.\nname = \"X\"\n\
                    base_url = \"https://x/v1\"\napi_key = { env = \"X_KEY\" }\n";
        let out = with_keyring_key(text, "x").expect("edit");
        assert_eq!(
            out,
            "#:schema ../schemas/provider.json\n# A service.\nname = \"X\"\n\
             base_url = \"https://x/v1\"\napi_key = { keyring = \"x\" }\n"
        );
    }

    #[test]
    fn minimal_file() {
        let p: ProviderFile = toml::from_str(
            r#"
            base_url = "https://api.example.com/v1"
            api_key = { env = "EXAMPLE_API_KEY" }
            models = ["qwen3-coder", { id = "glm-4.6", context_window = 131072 }]
            "#,
        )
        .expect("parses");
        assert_eq!(p.api, "openai-chat");
        assert_eq!(p.models[0], ModelEntry::new("qwen3-coder"));
        assert_eq!(p.models[1].context_window, Some(131072));
    }

    #[test]
    fn model_table_typos_are_reported_precisely() {
        let err = toml::from_str::<ProviderFile>(
            "base_url = \"x\"\n[[models]]\nid = \"m\"\ncontext_windw = 1\n",
        )
        .expect_err("typo");
        assert!(err.to_string().contains("context_windw"), "{err}");
    }
}
