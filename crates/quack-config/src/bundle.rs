//! Bundles: folders in `<layer>/bundles/` that group skills, commands and item files under
//! one name (docs/plan/08-instructions-skills-commands.md#bundles).
//!
//! An enabled bundle's folder is an extra item folder just below its layer's own, so its
//! `mcp.d`, `hooks.d`, `profiles.d`, `skills`, … load as if they were in that layer: a
//! layer's own file wins over a bundle's with the same name, and trust applies unchanged.
//! Two bundles with the same item is a warning; the one later by name wins.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::Deserialize;

use crate::paths::LayerKind;

/// `bundle.toml`: marks a folder as a bundle. Every field is optional.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct BundleFile {
    /// Shown in `/bundles`; the folder name when empty.
    pub name: String,
    pub version: String,
    pub description: String,
}

/// An installed bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bundle {
    /// The folder name, which `[bundles] disabled` uses.
    pub id: String,
    pub file: BundleFile,
    pub layer: LayerKind,
    pub dir: PathBuf,
    pub enabled: bool,
    /// What it holds, e.g. `2 skills, 1 MCP server`.
    pub contents: String,
}

/// Folders a bundle may hold, with what one entry in each is.
const KINDS: &[(&str, &str)] = &[
    ("skills", "skill"),
    ("commands", "command"),
    ("mcp.d", "MCP server"),
    ("hooks.d", "hook"),
    ("lsp.d", "language server"),
    ("profiles.d", "profile"),
    ("agents.d", "agent"),
    ("providers.d", "provider"),
];

/// The bundles in each layer, by id, and warnings for clashes and unreadable files.
pub(crate) fn discover(
    layers: &[(LayerKind, PathBuf)],
    disabled: &[String],
) -> (Vec<Bundle>, Vec<String>) {
    let mut bundles = Vec::new();
    let mut warnings = Vec::new();
    for (layer, dir) in layers {
        let Ok(entries) = std::fs::read_dir(dir.join("bundles")) else {
            continue;
        };
        let mut dirs: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        dirs.sort();
        // Item path within the layer → the bundle that last provided it.
        let mut owners: BTreeMap<String, String> = BTreeMap::new();
        for d in dirs {
            let id = d
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let manifest = d.join("bundle.toml");
            if !manifest.is_file() {
                warnings.push(format!(
                    "{} has no bundle.toml, so it is not loaded as a bundle",
                    d.display()
                ));
                continue;
            }
            let file = match std::fs::read_to_string(&manifest)
                .map_err(|e| e.to_string())
                .and_then(|t| toml::from_str::<BundleFile>(&t).map_err(|e| e.to_string()))
            {
                Ok(f) => f,
                Err(e) => {
                    warnings.push(format!("{}: {e}", manifest.display()));
                    continue;
                }
            };
            let enabled = !disabled.contains(&id);
            let mut counts = Vec::new();
            for (sub, noun) in KINDS {
                let names = entries_in(&d.join(sub));
                if names.is_empty() {
                    continue;
                }
                counts.push(count(names.len(), noun));
                if !enabled {
                    continue;
                }
                for name in names {
                    let key = format!("{sub}/{name}");
                    if let Some(earlier) = owners.insert(key.clone(), id.clone()) {
                        warnings.push(format!(
                            "bundles `{earlier}` and `{id}` in the {layer} layer both have {key}; `{id}` wins"
                        ));
                    }
                }
            }
            bundles.push(Bundle {
                id,
                file,
                layer: *layer,
                dir: d,
                enabled,
                contents: if counts.is_empty() {
                    "empty".into()
                } else {
                    counts.join(", ")
                },
            });
        }
    }
    (bundles, warnings)
}

/// The item names in a bundle folder: files and folders, without hidden ones.
fn entries_in(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !n.starts_with('.'))
        .collect();
    names.sort();
    names
}

fn count(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, text: &str) {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d).expect("mkdir");
        }
        std::fs::write(path, text).expect("write");
    }

    #[test]
    fn finds_bundles_counts_contents_and_warns_on_clashes() {
        let tmp = tempfile::tempdir().expect("tmp");
        let user = tmp.path().join("user");
        let b = user.join("bundles");
        write(
            &b.join("a-tools").join("bundle.toml"),
            "name = \"A tools\"\n",
        );
        write(&b.join("a-tools").join("mcp.d").join("gh.toml"), "");
        write(
            &b.join("a-tools").join("skills").join("x").join("SKILL.md"),
            "",
        );
        write(&b.join("b-tools").join("bundle.toml"), "");
        write(&b.join("b-tools").join("mcp.d").join("gh.toml"), "");
        write(&b.join("c-off").join("bundle.toml"), "");
        write(&b.join("c-off").join("mcp.d").join("gh.toml"), "");
        std::fs::create_dir_all(b.join("stray")).expect("mkdir");
        write(&b.join("bad").join("bundle.toml"), "nme = 1");

        let (found, warnings) = discover(&[(LayerKind::User, user.clone())], &["c-off".to_owned()]);
        let ids: Vec<_> = found.iter().map(|b| (b.id.as_str(), b.enabled)).collect();
        assert_eq!(
            ids,
            [("a-tools", true), ("b-tools", true), ("c-off", false)]
        );
        assert_eq!(found[0].file.name, "A tools");
        assert_eq!(found[0].contents, "1 skill, 1 MCP server");
        assert_eq!(found[1].contents, "1 MCP server");
        assert_eq!(warnings.len(), 3, "{warnings:?}");
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("`a-tools` and `b-tools`") && w.contains("mcp.d/gh.toml")),
            "{warnings:?}"
        );
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("stray") && w.contains("no bundle.toml")),
            "{warnings:?}"
        );
        assert!(warnings.iter().any(|w| w.contains("nme")), "{warnings:?}");
    }

    #[test]
    fn bundle_items_load_below_their_layer() {
        let tmp = tempfile::tempdir().expect("tmp");
        let user = tmp.path().join("user");
        let kit = user.join("bundles").join("kit");
        write(&kit.join("bundle.toml"), "description = \"team kit\"\n");
        let provider = |url: &str| format!("base_url = \"{url}\"\nmodels = [\"m\"]\n");
        write(
            &kit.join("providers.d").join("kit.toml"),
            &provider("http://kit"),
        );
        write(
            &kit.join("providers.d").join("own.toml"),
            &provider("http://bundle"),
        );
        write(
            &user.join("providers.d").join("own.toml"),
            &provider("http://user"),
        );
        let ws = tmp.path().join("ws");
        std::fs::create_dir_all(&ws).expect("mkdir");
        let vars: std::collections::HashMap<&str, std::ffi::OsString> = [
            ("QUACK_CONFIG_DIR", user.clone().into_os_string()),
            (
                "QUACK_SYSTEM_DIR",
                tmp.path().join("system").into_os_string(),
            ),
            ("QUACK_DATA_DIR", tmp.path().join("data").into_os_string()),
            ("QUACK_CACHE_DIR", tmp.path().join("cache").into_os_string()),
            ("QUACK_DEFAULTS_DIR", "none".into()),
        ]
        .into_iter()
        .collect();
        let load = || {
            crate::load_with(
                &|k| vars.get(k).cloned(),
                &crate::LoadOptions::new(ws.clone()),
            )
            .expect("load")
        };
        let urls = |loaded: &crate::Loaded| {
            crate::load_providers(loaded)
                .iter()
                .filter_map(|i| i.loaded().map(|p| (i.id.clone(), p.base_url.clone())))
                .collect::<Vec<_>>()
        };
        let loaded = load();
        assert_eq!(loaded.bundles.len(), 1);
        assert_eq!(
            urls(&loaded),
            [
                ("kit".to_owned(), "http://kit".to_owned()),
                ("own".to_owned(), "http://user".to_owned()),
            ]
        );

        write(
            &user.join("config.toml"),
            "[bundles]\ndisabled = [\"kit\"]\n",
        );
        let loaded = load();
        assert!(!loaded.bundles[0].enabled);
        assert_eq!(
            urls(&loaded),
            [("own".to_owned(), "http://user".to_owned())]
        );
    }
}
