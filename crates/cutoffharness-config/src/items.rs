//! One-file-per-item directories (`providers.d`, `mcp.d`, `lsp.d`, `hooks.d`).
//!
//! Items merge by id (default: file stem). A higher layer's item replaces a lower one
//! entirely. A file containing only `enabled = false` disables the lower item. A file
//! that fails to parse fails only that item.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;

use crate::diagnostic::{FileDiagnostic, read_text};
use crate::paths::LayerKind;

#[derive(Debug)]
pub struct Item<T> {
    pub id: String,
    pub path: PathBuf,
    pub layer: LayerKind,
    pub state: ItemState<T>,
}

#[derive(Debug)]
pub enum ItemState<T> {
    Loaded(T),
    Disabled(String),
    Failed(FileDiagnostic),
}

impl<T> Item<T> {
    pub fn loaded(&self) -> Option<&T> {
        match &self.state {
            ItemState::Loaded(t) => Some(t),
            _ => None,
        }
    }
}

/// Load `<layer>/<subdir>/*.toml` for each layer, lowest precedence first.
pub fn load_items<T: DeserializeOwned>(
    layers: &[(LayerKind, PathBuf)],
    subdir: &str,
) -> Vec<Item<T>> {
    let mut by_id: BTreeMap<String, Item<T>> = BTreeMap::new();
    for (layer, dir) in layers {
        for path in toml_files(&dir.join(subdir)) {
            let stem = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let (id, state) = load_one::<T>(&path, stem);
            let state = match state {
                Some(state) => state,
                None => ItemState::Disabled(format!("disabled by {}", path.display())),
            };
            by_id.insert(
                id.clone(),
                Item {
                    id,
                    path,
                    layer: *layer,
                    state,
                },
            );
        }
    }
    by_id.into_values().collect()
}

/// `None` state means the file is a bare `enabled = false` override.
fn load_one<T: DeserializeOwned>(path: &Path, stem: String) -> (String, Option<ItemState<T>>) {
    let text = match read_text(path) {
        Ok(t) => t,
        Err(e) => return (stem, Some(ItemState::Failed(FileDiagnostic::io(path, &e)))),
    };
    let table = match toml::from_str::<toml::Table>(&text) {
        Ok(t) => t,
        Err(e) => {
            return (
                stem,
                Some(ItemState::Failed(FileDiagnostic::from_toml(
                    path, &text, &e,
                ))),
            );
        }
    };
    let id = table
        .get("id")
        .and_then(toml::Value::as_str)
        .map_or(stem, str::to_owned);
    let enabled = table.get("enabled").and_then(toml::Value::as_bool);
    if enabled == Some(false) && table.keys().all(|k| k == "enabled" || k == "id") {
        return (id, None);
    }
    let state = match toml::from_str::<T>(&text) {
        Ok(_) if enabled == Some(false) => ItemState::Disabled("enabled = false".into()),
        Ok(item) => ItemState::Loaded(item),
        Err(e) => ItemState::Failed(FileDiagnostic::from_toml(path, &text, &e)),
    };
    (id, Some(state))
}

fn toml_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == "toml"))
        .collect();
    files.sort();
    files
}

/// Apply `[<kind>] disabled = [...]` and the policy allow-list.
pub(crate) fn apply_toggles<T>(items: &mut [Item<T>], disabled: &[String], allowed: &[String]) {
    for item in items {
        if matches!(item.state, ItemState::Failed(_)) {
            continue;
        }
        if disabled.iter().any(|d| d == &item.id) {
            item.state = ItemState::Disabled("listed in `disabled`".into());
        } else if !allowed.is_empty() && !allowed.iter().any(|g| glob_match(g, &item.id)) {
            item.state = ItemState::Disabled("not allowed by policy".into());
        }
    }
}

/// Minimal `*` glob for id matching.
pub(crate) fn glob_match(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == text;
    }
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if !text.starts_with(first) || !text[first.len()..].ends_with(last) {
        return false;
    }
    let mut rest = &text[first.len()..text.len() - last.len()];
    for mid in &parts[1..parts.len() - 1] {
        match rest.find(mid) {
            Some(i) => rest = &rest[i + mid.len()..],
            None => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::ProviderFile;

    fn write(dir: &Path, name: &str, text: &str) {
        std::fs::create_dir_all(dir).expect("mkdir");
        std::fs::write(dir.join(name), text).expect("write");
    }

    const LLAMA: &str = "api = \"openai-chat\"\nbase_url = \"http://127.0.0.1:8080/v1\"\n";

    #[test]
    fn items_merge_by_id_and_disable_cleanly() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (defaults, user) = (tmp.path().join("d"), tmp.path().join("u"));
        write(&defaults.join("providers.d"), "llama.toml", LLAMA);
        write(&defaults.join("providers.d"), "other.toml", LLAMA);
        write(&defaults.join("providers.d"), "broken.toml", "api = 1");
        write(&user.join("providers.d"), "other.toml", "enabled = false");

        let layers = vec![(LayerKind::Defaults, defaults), (LayerKind::User, user)];
        let mut items = load_items::<ProviderFile>(&layers, "providers.d");
        apply_toggles(&mut items, &[], &[]);
        let state = |id: &str| &items.iter().find(|i| i.id == id).expect("item").state;
        assert!(matches!(state("llama"), ItemState::Loaded(_)));
        assert!(matches!(state("other"), ItemState::Disabled(_)));
        assert!(matches!(state("broken"), ItemState::Failed(_)));
    }

    #[test]
    fn missing_directory_is_empty() {
        let items = load_items::<ProviderFile>(
            &[(LayerKind::User, PathBuf::from("/nonexistent"))],
            "providers.d",
        );
        assert!(items.is_empty());
    }

    #[test]
    fn globs() {
        assert!(glob_match("corp-*", "corp-vllm"));
        assert!(glob_match("*", "x"));
        assert!(glob_match("a*b*c", "aXbYc"));
        assert!(!glob_match("corp-*", "novita"));
        assert!(!glob_match("a*a", "a"));
    }
}
