//! What quack remembers between runs, in `<user data>/state.toml`: not configuration, so
//! it never lives in a config layer, and deleting the file only forgets.

use std::path::{Path, PathBuf};

use crate::load::{Loaded, Origin};
use crate::paths::LayerKind;

const FILE: &str = "state.toml";

/// The model last switched to, as `<provider>/<model>`.
pub fn last_model(user_data: &Path) -> Option<String> {
    let text = std::fs::read_to_string(user_data.join(FILE)).ok()?;
    let doc: toml::Table = text.parse().ok()?;
    doc.get("last_model")?
        .as_str()
        .filter(|m| m.contains('/'))
        .map(str::to_owned)
}

/// Remember `model` for the next start. Other keys and comments in the file are kept.
pub fn save_last_model(user_data: &Path, model: &str) -> Result<PathBuf, String> {
    let file = user_data.join(FILE);
    let text = match std::fs::read_to_string(&file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            "# Remembered by quack between runs. Safe to delete.\n".to_owned()
        }
        Err(e) => return Err(format!("cannot read {}: {e}", file.display())),
    };
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e| format!("{}: {e}", file.display()))?;
    if doc.get("last_model").and_then(|v| v.as_str()) == Some(model) {
        return Ok(file);
    }
    doc["last_model"] = toml_edit::value(model);
    std::fs::create_dir_all(user_data).map_err(|e| format!("{}: {e}", user_data.display()))?;
    // Write beside it and rename, so two quacks never leave a half-written file.
    let tmp = file.with_extension("toml.new");
    std::fs::write(&tmp, doc.to_string())
        .and_then(|()| std::fs::rename(&tmp, &file))
        .map_err(|e| format!("cannot write {}: {e}", file.display()))?;
    Ok(file)
}

impl Loaded {
    /// The remembered model to start with instead of `model.default`, if any. Only a
    /// personal default gives way to it: a project's choice, `QUACK_MODEL`, `-m` and a
    /// policy lock all win, and `model.remember_last = false` turns it off.
    pub fn remembered_model(&self) -> Option<String> {
        if !self.settings.model.remember_last {
            return None;
        }
        let key = "model.default";
        let locks = &self.settings.policy.locked;
        if locks.iter().any(|k| k == key || k == "model") {
            return None;
        }
        if matches!(
            self.origin_of(key),
            Origin::Env(_)
                | Origin::Cli
                | Origin::File {
                    layer: LayerKind::Project,
                    ..
                }
        ) {
            return None;
        }
        last_model(&self.dirs.user_data).filter(|m| *m != self.settings.model.default)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saves_reads_and_keeps_other_keys() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("data");
        assert_eq!(last_model(&dir), None);
        save_last_model(&dir, "charm-hyper/glm-5").expect("save");
        assert_eq!(last_model(&dir).as_deref(), Some("charm-hyper/glm-5"));

        let file = dir.join(FILE);
        let text = std::fs::read_to_string(&file).expect("read");
        std::fs::write(&file, format!("{text}# mine\nother = 1\n")).expect("write");
        save_last_model(&dir, "ollama/qwen3").expect("save");
        let text = std::fs::read_to_string(&file).expect("read");
        assert!(text.contains("last_model = \"ollama/qwen3\""), "{text}");
        assert!(
            text.contains("# mine") && text.contains("other = 1"),
            "{text}"
        );
    }

    #[test]
    fn ignores_a_broken_or_odd_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join(FILE), "last_model = ").expect("write");
        assert_eq!(last_model(tmp.path()), None);
        std::fs::write(tmp.path().join(FILE), "last_model = \"no-slash\"").expect("write");
        assert_eq!(last_model(tmp.path()), None);
    }
}
