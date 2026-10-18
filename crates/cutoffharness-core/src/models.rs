//! The model catalogue for `/models`: every configured provider, its declared models, and
//! what its models endpoint reports. Discovery calls run in parallel with a short timeout.

use std::sync::Arc;
use std::time::Duration;

use cutoffharness_config::{ItemState, Loaded, ProviderFile, glob_match, load_providers};
use cutoffharness_protocol::{ModelInfo, ProviderModels};
use cutoffharness_providers::Provider;
use tokio::task::JoinSet;

const CATALOG_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) async fn catalog(config: Arc<Loaded>) -> Vec<ProviderModels> {
    let cfg = config.clone();
    let items = tokio::task::spawn_blocking(move || load_providers(&cfg))
        .await
        .unwrap_or_default();
    let mut tasks = JoinSet::new();
    for (order, item) in items.into_iter().enumerate() {
        let network = config.settings.network.clone();
        tasks.spawn(async move {
            let entry = match item.state {
                ItemState::Loaded(file) => probe(&item.id, file, network).await,
                ItemState::Disabled(why) => unavailable(&item.id, why),
                ItemState::Failed(d) => unavailable(&item.id, d.one_line()),
            };
            (order, entry)
        });
    }
    let mut out: Vec<(usize, ProviderModels)> = Vec::new();
    while let Some(Ok(r)) = tasks.join_next().await {
        out.push(r);
    }
    // Usable providers first, each group in file order.
    out.sort_by_key(|(order, p)| (!p.available, *order));
    out.into_iter().map(|(_, p)| p).collect()
}

fn unavailable(id: &str, note: String) -> ProviderModels {
    ProviderModels {
        id: id.to_owned(),
        available: false,
        note,
        models: Vec::new(),
    }
}

async fn probe(
    id: &str,
    file: ProviderFile,
    network: cutoffharness_config::settings::NetworkSettings,
) -> ProviderModels {
    if let Some(cutoffharness_config::Secret::Env { env, .. }) = &file.api_key
        && std::env::var_os(env).is_none()
    {
        return unavailable(id, format!("set {env} to use this provider"));
    }
    let declared: Vec<ModelInfo> = file
        .models
        .iter()
        .map(|m| ModelInfo {
            id: m.id.clone(),
            alias: m.alias.clone(),
            context_window: m.context_window,
        })
        .collect();
    let include = file.discovery.include.clone();
    let pid = id.to_owned();
    let built = tokio::task::spawn_blocking(move || {
        Provider::new(&pid, file, &network, &|k| std::env::var_os(k))
    })
    .await;
    let provider = match built {
        Ok(Ok(p)) => p,
        Ok(Err(e)) => return unavailable(id, e.message),
        Err(e) => return unavailable(id, e.to_string()),
    };
    let (discovered, note) =
        match tokio::time::timeout(CATALOG_TIMEOUT, provider.list_models()).await {
            Ok(Ok(list)) => (list, String::new()),
            Ok(Err(e)) => (Vec::new(), e.message),
            Err(_) => (Vec::new(), "listing models timed out".to_owned()),
        };
    let mut models = declared;
    for d in discovered {
        if !include.is_empty() && !include.iter().any(|g| glob_match(g, &d.id)) {
            continue;
        }
        match models.iter_mut().find(|m| m.id == d.id) {
            Some(m) => m.context_window = m.context_window.or(d.context_window),
            None => models.push(ModelInfo {
                id: d.id,
                alias: None,
                context_window: d.context_window,
            }),
        }
    }
    let available = !models.is_empty();
    let note = match (available, note.is_empty()) {
        (true, true) => format!("{} models", models.len()),
        (true, false) => format!("{} declared models; discovery failed: {note}", models.len()),
        (false, true) => "no models listed or discovered".to_owned(),
        (false, false) => note,
    };
    ProviderModels {
        id: id.to_owned(),
        available,
        note,
        models,
    }
}
