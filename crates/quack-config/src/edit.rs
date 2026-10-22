//! Writing settings back to a layer's `config.toml` (`/config`, "save as default" in `/models`).
//!
//! Edits keep the file's comments and layout, are validated against the schema before
//! anything is written, and replace the file atomically.

use std::path::{Path, PathBuf};

use serde_json::Value as Json;
use toml::Value;

use crate::load::{Loaded, Origin, parse_override};
use crate::merge;
use crate::paths::LayerKind;
use crate::settings::Settings;

/// One effective setting, for the `/config` editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingRow {
    pub key: String,
    /// The effective value as TOML.
    pub value: String,
    pub origin: String,
    pub description: String,
    pub is_bool: bool,
    /// Locked by the system policy; cannot be changed here.
    pub locked: bool,
}

impl Loaded {
    /// Path of a layer's `config.toml`, if the layer has a directory.
    pub fn layer_file(&self, layer: LayerKind) -> Option<PathBuf> {
        self.dirs.layer_dir(layer).map(|d| d.join("config.toml"))
    }

    /// Every editable leaf setting with its value, origin and schema description.
    pub fn setting_rows(&self) -> Vec<SettingRow> {
        let table = toml::Table::try_from(&self.settings).unwrap_or_default();
        let docs = descriptions();
        merge::leaves(&table)
            .into_iter()
            .filter(|(k, _)| !k.starts_with("policy.") && !k.starts_with("keys."))
            .map(|(key, v)| SettingRow {
                value: v.to_string(),
                origin: self.origin_of(&key).to_string(),
                description: docs
                    .iter()
                    .find(|(k, _)| *k == key)
                    .map(|(_, d)| d.clone())
                    .unwrap_or_default(),
                is_bool: v.is_bool(),
                locked: self.settings.policy.locks(&key),
                key,
            })
            .collect()
    }

    /// Set `key` in the given layer's `config.toml`. `raw` parses as a TOML value and falls
    /// back to a string, like `-c key=value`. Returns the file written and, when a higher
    /// layer, the environment or the command line still wins, a note saying so.
    pub fn save_setting(
        &self,
        layer: LayerKind,
        key: &str,
        raw: &str,
    ) -> Result<(PathBuf, Option<String>), String> {
        if key.starts_with("policy") {
            return Err("policy can only be set in the system layer by an administrator".into());
        }
        if self.settings.policy.locks(key) {
            return Err(format!("`{key}` is locked by the system policy"));
        }
        let file = self
            .layer_file(layer)
            .ok_or_else(|| format!("the {layer} layer has no directory"))?;
        let value = parse_override(raw.trim());
        set_in_file(&file, key, &value)?;
        let note = match self.origin_of(key) {
            Origin::File { layer: l, path } if *l > layer => Some(format!(
                "still overridden by the {l} layer ({})",
                path.display()
            )),
            Origin::Env(var) => Some(format!("still overridden by {var}")),
            Origin::Cli => Some("overridden for this run by a command-line flag".into()),
            _ => None,
        };
        Ok((file, note))
    }
}

/// Set one dotted key in a `config.toml`, creating the file and tables as needed.
pub fn set_in_file(file: &Path, key: &str, value: &Value) -> Result<(), String> {
    let text = match std::fs::read_to_string(file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("cannot read {}: {e}", file.display())),
    };
    let updated = set_in_text(&text, key, value).map_err(|e| format!("{}: {e}", file.display()))?;
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    let tmp = file.with_extension("toml.tmp");
    std::fs::write(&tmp, updated)
        .and_then(|()| std::fs::rename(&tmp, file))
        .map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("cannot write {}: {e}", file.display())
        })
}

/// The edit itself, on text: keeps everything else in the document untouched.
pub(crate) fn set_in_text(text: &str, key: &str, value: &Value) -> Result<String, String> {
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e: toml_edit::TomlError| format!("not valid TOML: {}", e.message()))?;
    let parts: Vec<&str> = key.split('.').collect();
    let Some((leaf, parents)) = parts.split_last() else {
        return Err("empty key".into());
    };
    let new_value = to_edit_value(value)?;
    let mut table = doc.as_table_mut();
    for p in parents {
        let entry = table
            .entry(p)
            .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()));
        table = match entry {
            toml_edit::Item::Table(t) => t,
            _ => return Err(format!("`{p}` in `{key}` is not a table")),
        };
    }
    match table.get_mut(leaf) {
        // Replace the value but keep the comment after it.
        Some(toml_edit::Item::Value(old)) => {
            let decor = old.decor().clone();
            *old = new_value;
            *old.decor_mut() = decor;
        }
        _ => {
            table.insert(leaf, toml_edit::Item::Value(new_value));
        }
    }
    let out = doc.to_string();
    // Reject anything the loader would reject (unknown key, wrong type).
    toml::from_str::<Settings>(&out).map_err(|e| e.message().to_owned())?;
    Ok(out)
}

fn to_edit_value(value: &Value) -> Result<toml_edit::Value, String> {
    if value.is_table() {
        return Err("set the keys inside a table one at a time".into());
    }
    value
        .to_string()
        .parse::<toml_edit::Value>()
        .map_err(|e| e.to_string())
}

/// `(dotted key, description)` for every leaf documented in the settings schema.
fn descriptions() -> Vec<(String, String)> {
    let schema = serde_json::to_value(schemars::schema_for!(Settings)).unwrap_or_default();
    let mut out = Vec::new();
    walk(&schema, &schema, "", &mut out);
    out
}

fn walk(root: &Json, node: &Json, prefix: &str, out: &mut Vec<(String, String)>) {
    let node = resolve(root, node);
    let Some(props) = node.get("properties").and_then(Json::as_object) else {
        return;
    };
    for (name, prop) in props {
        let path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}.{name}")
        };
        if let Some(d) = prop.get("description").and_then(Json::as_str) {
            out.push((path.clone(), d.to_owned()));
        } else if let Some(d) = resolve(root, prop)
            .get("description")
            .and_then(Json::as_str)
        {
            // Enums document themselves on their definition.
            out.push((path.clone(), d.to_owned()));
        }
        walk(root, prop, &path, out);
    }
}

/// Follow a local `$ref` (directly or through `allOf: [{$ref}]`).
fn resolve<'a>(root: &'a Json, node: &'a Json) -> &'a Json {
    let reference = node.get("$ref").and_then(Json::as_str).or_else(|| {
        node.get("allOf")
            .and_then(Json::as_array)
            .and_then(|a| a.first())
            .and_then(|f| f.get("$ref"))
            .and_then(Json::as_str)
    });
    reference
        .and_then(|r| r.strip_prefix("#/"))
        .and_then(|p| root.pointer(&format!("/{p}")))
        .unwrap_or(node)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_keep_comments_and_validate() {
        let text = "# my settings\n[ui]\nascii = true # I like ascii\n\n[model]\n# pick later\n";
        let out = set_in_text(
            text,
            "model.default",
            &Value::String("charm/qwen3.8-flash".into()),
        )
        .expect("edit");
        assert!(out.starts_with("# my settings\n[ui]\nascii = true # I like ascii\n"));
        assert!(
            out.contains("[model]\ndefault = \"charm/qwen3.8-flash\""),
            "{out}"
        );
        assert!(out.contains("# pick later"), "{out}");

        let out = set_in_text(&out, "ui.ascii", &Value::Boolean(false)).expect("edit");
        assert!(out.contains("ascii = false # I like ascii"), "{out}");

        let typo = set_in_text("", "ui.asci", &Value::Boolean(true));
        assert!(typo.is_err_and(|e| e.contains("asci")));
        let wrong = set_in_text("", "ui.ascii", &Value::String("yes".into()));
        assert!(wrong.is_err());
    }

    #[test]
    fn writes_file_atomically() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("sub").join("config.toml");
        set_in_file(&file, "ui.theme", &Value::String("light".into())).expect("write");
        let text = std::fs::read_to_string(&file).expect("read");
        assert_eq!(text, "[ui]\ntheme = \"light\"\n");
        assert!(!file.with_extension("toml.tmp").exists());
    }

    #[test]
    fn schema_descriptions_reach_leaves() {
        let docs = descriptions();
        assert!(
            docs.iter()
                .any(|(k, d)| k == "ui.ascii" && d.contains("ASCII")),
            "{docs:?}"
        );
    }
}
