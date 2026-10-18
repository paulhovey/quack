//! First-run setup (docs/plan/14-packaging-and-distribution.md#first-run-experience):
//! finding model servers on this machine and writing minimal provider files to the user
//! layer. Shared by `cutoffharness setup` and the TUI's `/setup`.
//!
//! Every provider is just a server (a `base_url`, and a key when it needs one). The engine
//! names and hosted services below only suggest where to look and what to call it; nothing
//! ships as a provider until the user picks it.
//!
//! Detection only runs when the user asks for it, and only probes loopback addresses.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use cutoffharness_config::{ItemState, LayerKind, Loaded, ProviderFile, load_providers};
use cutoffharness_protocol::{DetectedServer, ModelInfo, ProviderModels};
use cutoffharness_providers::Provider;
use tokio::task::JoinSet;

const PROBE_TIMEOUT: Duration = Duration::from_millis(1500);

/// A server kind and the address it listens on by default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub api: Option<String>,
    /// What this engine needs for tool calls, written into the provider file.
    pub note: String,
}

impl Candidate {
    fn new(id: &str, name: &str, base_url: &str, api: Option<&str>, note: &str) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            base_url: base_url.into(),
            api: api.map(Into::into),
            note: note.into(),
        }
    }
}

/// The servers `setup` looks for, on their default ports.
pub fn local_servers() -> Vec<Candidate> {
    vec![
        Candidate::new(
            "ollama",
            "Ollama",
            "http://127.0.0.1:11434",
            Some("ollama"),
            "Uses Ollama's native API, so each request sets the context window (its OpenAI \
             endpoint silently cuts long prompts).",
        ),
        Candidate::new(
            "llama-cpp",
            "llama.cpp",
            "http://127.0.0.1:8080/v1",
            None,
            "Start llama-server with `--jinja` so tool calls work.",
        ),
        Candidate::new(
            "vllm",
            "vLLM",
            "http://127.0.0.1:8000/v1",
            None,
            "Start vLLM with `--enable-auto-tool-choice --tool-call-parser <parser>` for your \
             model so tool calls work.",
        ),
        Candidate::new(
            "lmstudio",
            "LM Studio",
            "http://127.0.0.1:1234/v1",
            None,
            "",
        ),
        Candidate::new(
            "sglang",
            "SGLang",
            "http://127.0.0.1:30000/v1",
            None,
            "Start SGLang with `--tool-call-parser <parser>` for your model so tool calls work.",
        ),
    ]
}

/// The comment for a server found on this machine: where it came from and, for a known
/// engine, what it needs for tool calls.
pub fn local_comment(id: &str) -> String {
    match local_servers().into_iter().find(|c| c.id == id) {
        Some(c) if c.note.is_empty() => format!("{} on this machine, found by setup.", c.name),
        Some(c) => format!("{} on this machine, found by setup.\n{}", c.name, c.note),
        None => "Found on this machine by setup.".into(),
    }
}

/// A hosted service with an OpenAI-compatible API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hosted {
    pub id: &'static str,
    pub name: &'static str,
    /// Heading the wizard lists it under.
    pub group: &'static str,
    pub base_url: &'static str,
    /// The variable its key is conventionally read from.
    pub env: &'static str,
}

/// Services that host many open models.
pub const OPEN_MODEL_HOSTS: &str = "Open-model services";
/// The model makers' own APIs and coding plans.
pub const MODEL_MAKERS: &str = "From the model makers";

/// The hosted services `setup` offers, in the order it lists them.
pub fn hosted_services() -> Vec<Hosted> {
    let h = |id, name, group, base_url, env| Hosted {
        id,
        name,
        group,
        base_url,
        env,
    };
    let (open, maker) = (OPEN_MODEL_HOSTS, MODEL_MAKERS);
    vec![
        h(
            "openrouter",
            "OpenRouter",
            open,
            "https://openrouter.ai/api/v1",
            "OPENROUTER_API_KEY",
        ),
        h(
            "synthetic",
            "Synthetic",
            open,
            "https://api.synthetic.new/openai/v1",
            "SYNTHETIC_API_KEY",
        ),
        h(
            "charm-hyper",
            "Charm Hyper",
            open,
            "https://hyper.charm.land/v1",
            "CHARM_API_KEY",
        ),
        h(
            "together",
            "Together AI",
            open,
            "https://api.together.xyz/v1",
            "TOGETHER_API_KEY",
        ),
        h(
            "fireworks",
            "Fireworks AI",
            open,
            "https://api.fireworks.ai/inference/v1",
            "FIREWORKS_API_KEY",
        ),
        h(
            "deepinfra",
            "DeepInfra",
            open,
            "https://api.deepinfra.com/v1/openai",
            "DEEPINFRA_API_KEY",
        ),
        h(
            "novita",
            "Novita AI",
            open,
            "https://api.novita.ai/openai/v1",
            "NOVITA_API_KEY",
        ),
        // Plan keys (Vibe) and pay-as-you-go keys use the same API.
        h(
            "mistral",
            "Mistral (API or Vibe plan)",
            maker,
            "https://api.mistral.ai/v1",
            "MISTRAL_API_KEY",
        ),
        // Kimi Code plan keys only work on the coding endpoint, and API keys only on Moonshot's.
        h(
            "kimi-code",
            "Kimi Code plan",
            maker,
            "https://api.kimi.com/coding/v1",
            "KIMI_API_KEY",
        ),
        h(
            "moonshot",
            "Moonshot API (Kimi)",
            maker,
            "https://api.moonshot.ai/v1",
            "MOONSHOT_API_KEY",
        ),
        // Likewise for Z.ai: Coding Plan keys need the coding endpoint.
        h(
            "zai-coding",
            "Z.ai GLM Coding Plan",
            maker,
            "https://api.z.ai/api/coding/paas/v4",
            "ZAI_API_KEY",
        ),
        h(
            "zai",
            "Z.ai API",
            maker,
            "https://api.z.ai/api/paas/v4",
            "ZAI_API_KEY",
        ),
        h(
            "deepseek",
            "DeepSeek API",
            maker,
            "https://api.deepseek.com",
            "DEEPSEEK_API_KEY",
        ),
    ]
}

/// Where a hosted provider's key comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyFrom {
    /// The OS credential store, under the provider id.
    Keyring,
    /// The service's environment variable.
    Env,
}

/// Probe the default local ports. Servers that answer with a model list are returned, in
/// the order of [`local_servers`].
pub async fn detect(config: Arc<Loaded>) -> Vec<DetectedServer> {
    detect_in(config, local_servers()).await
}

pub async fn detect_in(config: Arc<Loaded>, candidates: Vec<Candidate>) -> Vec<DetectedServer> {
    let mut tasks = JoinSet::new();
    for (order, c) in candidates.into_iter().enumerate() {
        let network = config.settings.network.clone();
        tasks.spawn(async move { (order, probe(c, network).await) });
    }
    let mut found: Vec<(usize, DetectedServer)> = Vec::new();
    while let Some(Ok((order, server))) = tasks.join_next().await {
        if let Some(s) = server {
            found.push((order, s));
        }
    }
    found.sort_by_key(|(order, _)| *order);
    let cfg = config.clone();
    let existing = tokio::task::spawn_blocking(move || configured_urls(&cfg))
        .await
        .unwrap_or_default();
    found
        .into_iter()
        .map(|(_, mut s)| {
            s.configured = existing
                .iter()
                .find(|(_, url)| same_server(url, &s.base_url))
                .map(|(id, _)| id.clone());
            s
        })
        .collect()
}

async fn probe(
    c: Candidate,
    network: cutoffharness_config::settings::NetworkSettings,
) -> Option<DetectedServer> {
    let mut c = c;
    let build = |c: &Candidate| {
        let file = provider_file(&c.base_url, c.api.as_deref());
        let (id, network) = (c.id.clone(), network.clone());
        tokio::task::spawn_blocking(move || {
            Provider::new(&id, file?, &network, &|k| std::env::var_os(k)).ok()
        })
    };
    let provider = match build(&c).await.ok()? {
        Some(p) => p,
        // A build without the native Ollama adapter talks to Ollama's OpenAI endpoint.
        None if c.api.as_deref() == Some("ollama") => {
            c.base_url = format!("{}/v1", c.base_url);
            c.api = None;
            build(&c).await.ok()??
        }
        None => return None,
    };
    let list = tokio::time::timeout(PROBE_TIMEOUT, provider.list_models())
        .await
        .ok()?
        .ok()?;
    Some(DetectedServer {
        id: c.id,
        name: c.name,
        base_url: c.base_url,
        api: c.api,
        note: c.note,
        models: list
            .into_iter()
            .map(|m| ModelInfo {
                id: m.id,
                alias: None,
                context_window: m.context_window,
            })
            .collect(),
        configured: None,
    })
}

fn provider_file(base_url: &str, api: Option<&str>) -> Option<ProviderFile> {
    let mut v = serde_json::json!({ "base_url": base_url });
    if let Some(api) = api {
        v["api"] = api.into();
    }
    serde_json::from_value(v).ok()
}

/// The configured provider that already points at `base_url`, if any.
pub fn configured_at(config: &Loaded, base_url: &str) -> Option<String> {
    configured_urls(config)
        .into_iter()
        .find(|(_, url)| same_server(url, base_url))
        .map(|(id, _)| id)
}

/// Loaded providers and their `base_url`s.
fn configured_urls(config: &Loaded) -> Vec<(String, String)> {
    load_providers(config)
        .into_iter()
        .filter_map(|i| match i.state {
            ItemState::Loaded(f) => Some((i.id, f.base_url)),
            _ => None,
        })
        .collect()
}

/// Two base URLs reach the same server: `localhost` and `127.0.0.1` are the same host,
/// and Ollama's native and `/v1` addresses are the same server.
fn same_server(a: &str, b: &str) -> bool {
    let norm = |u: &str| {
        u.trim_end_matches('/')
            .trim_end_matches("/v1")
            .replace("://localhost", "://127.0.0.1")
            .to_ascii_lowercase()
    };
    norm(a) == norm(b)
}

/// The text of a minimal provider file.
pub fn provider_text(comment: &str, base_url: &str, api: Option<&str>) -> String {
    let quote = |s: &str| serde_json::Value::from(s).to_string();
    let mut text = String::new();
    for line in comment.lines() {
        text.push_str(&format!("# {line}\n"));
    }
    text.push_str(&format!("base_url = {}\n", quote(base_url)));
    if let Some(api) = api {
        text.push_str(&format!("api = {}\n", quote(api)));
    }
    text
}

/// The text of a hosted service's provider file, with its key from `key`.
pub fn hosted_text(service: &Hosted, key: KeyFrom) -> String {
    let mut text = provider_text(
        &format!("{}, added by setup.", service.name),
        service.base_url,
        None,
    );
    text.push_str(&match key {
        KeyFrom::Keyring => format!("api_key = {{ keyring = \"{}\" }}\n", service.id),
        KeyFrom::Env => format!("api_key = {{ env = \"{}\" }}\n", service.env),
    });
    text
}

/// Provider ids become file names, so keep them plain.
pub fn check_id(id: &str) -> Result<(), String> {
    if id.is_empty()
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!(
            "`{id}` is not a valid provider id: use letters, digits, `-` and `_`"
        ));
    }
    Ok(())
}

/// What [`add_provider`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Added {
    /// The provider id to use, which may differ from the one asked for.
    pub id: String,
    /// The file written; `None` when a configured provider already points at the server.
    pub path: Option<PathBuf>,
}

/// Add a server as a provider in the user layer. A provider that already points at the
/// same server is reused; an id that is taken gets a numbered suffix.
pub fn add_provider(
    config: &Loaded,
    id: &str,
    base_url: &str,
    api: Option<&str>,
    comment: &str,
) -> Result<Added, String> {
    add_file(config, id, base_url, &provider_text(comment, base_url, api))
}

/// Add a hosted service as a provider in the user layer, as [`add_provider`] does.
pub fn add_hosted(config: &Loaded, service: &Hosted, key: KeyFrom) -> Result<Added, String> {
    add_file(
        config,
        service.id,
        service.base_url,
        &hosted_text(service, key),
    )
}

fn add_file(config: &Loaded, id: &str, base_url: &str, text: &str) -> Result<Added, String> {
    check_id(id)?;
    let items = load_providers(config);
    if let Some(item) = items.iter().find(|i| {
        i.loaded()
            .is_some_and(|f| same_server(&f.base_url, base_url))
    }) {
        return Ok(Added {
            id: item.id.clone(),
            path: None,
        });
    }
    let dir = providers_dir(config)?;
    let taken =
        |id: &str| items.iter().any(|i| i.id == id) || dir.join(format!("{id}.toml")).exists();
    let id = (1..100)
        .map(|n| {
            if n == 1 {
                id.to_owned()
            } else {
                format!("{id}-{n}")
            }
        })
        .find(|id| !taken(id))
        .ok_or_else(|| format!("too many providers named {id}"))?;
    let path = dir.join(format!("{id}.toml"));
    write_new(&path, text)?;
    Ok(Added {
        id,
        path: Some(path),
    })
}

/// `<user config>/providers.d`.
pub fn providers_dir(config: &Loaded) -> Result<PathBuf, String> {
    config
        .dirs
        .layer_dir(LayerKind::User)
        .map(|d| d.join("providers.d"))
        .ok_or_else(|| "the user layer has no directory".to_owned())
}

/// Create a file that must not exist yet.
pub fn write_new(path: &Path, text: &str) -> Result<(), String> {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .and_then(|mut f| f.write_all(text.as_bytes()))
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// Set `model.default` in the user layer when no default is configured. Returns the file
/// written, if any.
pub fn default_model_if_unset(config: &Loaded, reference: &str) -> Result<Option<PathBuf>, String> {
    if !config.settings.model.default.is_empty() {
        return Ok(None);
    }
    let quoted = serde_json::Value::from(reference).to_string();
    config
        .save_setting(LayerKind::User, "model.default", &quoted)
        .map(|(path, _)| Some(path))
}

/// Every configured provider and its models, as `/models` lists them.
pub async fn providers(config: Arc<Loaded>) -> Vec<ProviderModels> {
    crate::models::catalog(config).await
}

/// Probe one configured provider for its models, as `/models` does.
pub async fn provider_models(config: Arc<Loaded>, id: &str) -> Option<ProviderModels> {
    crate::models::catalog_of(config, Some(id))
        .await
        .into_iter()
        .next()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_server_ignores_localhost_and_v1() {
        assert!(same_server(
            "http://localhost:11434",
            "http://127.0.0.1:11434/v1/"
        ));
        assert!(!same_server(
            "http://127.0.0.1:8080/v1",
            "http://127.0.0.1:8000/v1"
        ));
    }

    #[test]
    fn provider_text_is_minimal_and_parses() {
        let text = provider_text(
            "Ollama on this machine.",
            "http://127.0.0.1:11434",
            Some("ollama"),
        );
        assert_eq!(
            text,
            "# Ollama on this machine.\nbase_url = \"http://127.0.0.1:11434\"\napi = \"ollama\"\n"
        );
        assert!(check_id("llama-cpp_2").is_ok());
        assert!(check_id("../x").is_err());
        assert!(check_id("").is_err());
    }

    #[test]
    fn hosted_files_parse_with_either_key() {
        for service in hosted_services() {
            for key in [KeyFrom::Keyring, KeyFrom::Env] {
                let text = hosted_text(&service, key);
                let f: ProviderFile = toml::from_str(&text).expect("parse");
                assert_eq!(f.base_url, service.base_url);
                assert!(f.api_key.is_some(), "{text}");
            }
        }
        assert_eq!(
            hosted_text(&hosted_services()[0], KeyFrom::Env),
            "# OpenRouter, added by setup.\nbase_url = \"https://openrouter.ai/api/v1\"\napi_key = { env = \"OPENROUTER_API_KEY\" }\n"
        );
        assert_eq!(
            local_comment("llama-cpp"),
            "llama.cpp on this machine, found by setup.\nStart llama-server with `--jinja` so tool calls work."
        );
    }
}
