//! First-run setup (docs/plan/14-packaging-and-distribution.md#first-run-experience):
//! finding model servers on this machine and writing minimal provider files to the user
//! layer. Shared by `cutoffharness setup` and the TUI's `/setup`.
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
}

impl Candidate {
    fn new(id: &str, name: &str, base_url: &str, api: Option<&str>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            base_url: base_url.into(),
            api: api.map(Into::into),
        }
    }
}

/// The servers `setup` looks for, on their default ports.
pub fn local_servers() -> Vec<Candidate> {
    vec![
        Candidate::new("ollama", "Ollama", "http://127.0.0.1:11434", Some("ollama")),
        Candidate::new("llama-cpp", "llama.cpp", "http://127.0.0.1:8080/v1", None),
        Candidate::new("vllm", "vLLM", "http://127.0.0.1:8000/v1", None),
        Candidate::new("lmstudio", "LM Studio", "http://127.0.0.1:1234/v1", None),
        Candidate::new("sglang", "SGLang", "http://127.0.0.1:30000/v1", None),
    ]
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
    write_new(&path, &provider_text(comment, base_url, api))?;
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
}
