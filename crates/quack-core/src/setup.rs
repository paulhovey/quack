//! First-run setup (docs/plan/14-packaging-and-distribution.md#first-run-experience):
//! finding model servers on this machine and writing minimal provider files to the user
//! layer. Shared by `quack setup` and the TUI's `/setup`.
//!
//! Every provider is just a server (a `base_url`, and a key when it needs one). Nothing
//! here names a server or a service: what setup looks for and offers are the providers
//! already loaded, which out of the box are the defaults bundle's `providers.d`. Delete
//! those files and setup only takes a URL.
//!
//! Detection only runs when the user asks for it, and only probes loopback addresses.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use quack_config::{
    ItemState, LayerKind, Loaded, ProviderFile, Secret, is_loopback, load_providers,
};
use quack_protocol::{DetectedServer, ModelInfo, ProviderModels};
use quack_providers::Provider;
use tokio::task::JoinSet;

const PROBE_TIMEOUT: Duration = Duration::from_millis(1500);

/// A server to look for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub api: Option<String>,
    /// What this engine needs for tool calls, e.g. a server flag. May be empty.
    pub note: String,
}

/// The configured providers on this machine, which setup looks for.
pub fn local_candidates(config: &Loaded) -> Vec<Candidate> {
    load_providers(config)
        .into_iter()
        .filter_map(|i| {
            let f = i.loaded()?;
            is_loopback(&f.base_url).then(|| Candidate {
                name: display_name(&i.id, f),
                id: i.id.clone(),
                base_url: f.base_url.clone(),
                api: (f.api != "openai-chat").then(|| f.api.clone()),
                note: f.note.clone(),
            })
        })
        .collect()
}

fn display_name(id: &str, f: &ProviderFile) -> String {
    if f.name.is_empty() {
        id.to_owned()
    } else {
        f.name.clone()
    }
}

/// A configured provider on another machine that takes a key: what setup's hosted step
/// lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Service {
    pub id: String,
    pub name: String,
    /// Heading it is listed under.
    pub group: String,
    pub base_url: String,
    /// The variable its key is read from, for `api_key = { env = ... }`.
    pub env: Option<String>,
    /// Its key can be read now: the variable is set, or the key comes from elsewhere.
    pub ready: bool,
    /// The file it is configured in.
    pub path: PathBuf,
}

/// Heading for a service whose file sets no `group`.
pub const OTHER_SERVICES: &str = "Other services";

/// The hosted services setup offers, grouped in the order each group first appears and
/// by id within a group.
pub fn services(config: &Loaded) -> Vec<Service> {
    let mut out: Vec<Service> = load_providers(config)
        .into_iter()
        .filter_map(|i| {
            let f = i.loaded()?;
            if is_loopback(&f.base_url) {
                return None;
            }
            let key = f.api_key.as_ref()?;
            let env = match key {
                Secret::Env { env, .. } => Some(env.clone()),
                _ => None,
            };
            Some(Service {
                name: display_name(&i.id, f),
                group: if f.group.is_empty() {
                    OTHER_SERVICES.to_owned()
                } else {
                    f.group.clone()
                },
                base_url: f.base_url.clone(),
                ready: env.as_ref().is_none_or(|e| std::env::var_os(e).is_some()),
                env,
                id: i.id.clone(),
                path: i.path.clone(),
            })
        })
        .collect();
    let mut groups: Vec<String> = Vec::new();
    for s in &out {
        if s.group != OTHER_SERVICES && !groups.contains(&s.group) {
            groups.push(s.group.clone());
        }
    }
    groups.push(OTHER_SERVICES.to_owned());
    out.sort_by_key(|s| groups.iter().position(|g| *g == s.group));
    out
}

/// Read a service's key from the OS credential store: its provider file is copied to the
/// user layer (or edited there) with `api_key = { keyring = "<id>" }`. Store the key with
/// `keyring_set(id, ..)` first. Returns the file written.
pub fn use_keyring_key(config: &Loaded, service: &Service) -> Result<PathBuf, String> {
    let text = std::fs::read_to_string(&service.path)
        .map_err(|e| format!("cannot read {}: {e}", service.path.display()))?;
    let text = quack_config::provider::with_keyring_key(&text, &service.id)?;
    let target = providers_dir(config)?.join(format!("{}.toml", service.id));
    if target == service.path {
        std::fs::write(&target, text)
            .map_err(|e| format!("cannot write {}: {e}", target.display()))?;
    } else {
        write_new(&target, &text)?;
    }
    Ok(target)
}

/// Probe the configured servers on this machine. Those that answer with a model list are
/// returned, in the order of [`local_candidates`].
pub async fn detect(config: Arc<Loaded>) -> Vec<DetectedServer> {
    let cfg = config.clone();
    let candidates = tokio::task::spawn_blocking(move || local_candidates(&cfg))
        .await
        .unwrap_or_default();
    detect_in(config, candidates).await
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
    network: quack_config::settings::NetworkSettings,
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

/// Refuse a provider the system policy would disable anyway.
pub fn check_policy(config: &Loaded, id: &str, base_url: &str) -> Result<(), String> {
    if config.settings.policy.providers.allows(id, Some(base_url)) {
        Ok(())
    } else {
        Err(format!(
            "the system policy does not allow `{id}` at {base_url} ([policy] providers)"
        ))
    }
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

fn add_file(config: &Loaded, id: &str, base_url: &str, text: &str) -> Result<Added, String> {
    check_id(id)?;
    check_policy(config, id, base_url)?;
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
}
