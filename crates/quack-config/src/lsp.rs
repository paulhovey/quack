//! `lsp.d/<id>.toml` schema (docs/plan/10-lsp.md#server-file-schema).
//!
//! A language server needs only its command and the file extensions it covers:
//!
//! ```toml
//! command = "rust-analyzer"
//! extensions = ["rs"]
//! ```
//!
//! Nothing is ever installed: a server whose program is not on `PATH` is skipped quietly.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::secret::Secret;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LspServerFile {
    /// Defaults to the file stem.
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Shown in `/lsp`.
    #[serde(default)]
    pub description: String,

    /// Program to start. Found on `PATH`, including `.cmd` files on Windows.
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// File extensions this server covers, without the dot.
    pub extensions: Vec<String>,
    /// Files that mark a project root; the nearest folder holding one is the server's
    /// workspace. Default: the workspace root.
    #[serde(default)]
    pub root_markers: Vec<String>,
    /// LSP language id per extension, when it is not the extension itself
    /// (e.g. `{ rs = "rust", py = "python" }`).
    #[serde(default)]
    pub language_ids: BTreeMap<String, String>,
    /// Extra environment variables; values may be secrets.
    #[serde(default)]
    pub env: BTreeMap<String, Secret>,

    /// Sent with `initialize`.
    #[serde(default)]
    #[schemars(with = "JsonMap")]
    pub initialization_options: toml::Table,
    /// Answers to `workspace/configuration`, as a nested table (e.g. `[settings.python]`).
    #[serde(default)]
    #[schemars(with = "JsonMap")]
    pub settings: toml::Table,

    /// Run the server inside the OS sandbox (see `[sandbox]` in config.toml).
    #[serde(default)]
    pub sandbox: Option<bool>,
    #[serde(default)]
    pub diagnostics: LspDiagnostics,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct LspDiagnostics {
    /// After an edit, wait until the server has been quiet this long.
    pub settle_ms: u64,
    /// Never wait longer than this for diagnostics after an edit.
    pub max_wait_ms: u64,
    /// `error`, `warning`, `info` or `hint`.
    pub min_severity: String,
    /// Diagnostics per file in a tool result.
    pub max_items: usize,
}

impl Default for LspDiagnostics {
    fn default() -> Self {
        Self {
            settle_ms: 800,
            max_wait_ms: 5_000,
            min_severity: "warning".into(),
            max_items: 20,
        }
    }
}

impl LspServerFile {
    /// Whether this server covers a file with this extension (case-insensitive).
    pub fn covers(&self, extension: &str) -> bool {
        self.extensions
            .iter()
            .any(|e| e.trim_start_matches('.').eq_ignore_ascii_case(extension))
    }

    /// The LSP `languageId` for an extension.
    pub fn language_id(&self, extension: &str) -> String {
        let ext = extension.to_ascii_lowercase();
        self.language_ids.get(&ext).cloned().unwrap_or(ext)
    }
}

type JsonMap = serde_json::Map<String, serde_json::Value>;

fn yes() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_file() {
        let f: LspServerFile =
            toml::from_str("command = \"gopls\"\nextensions = [\"go\"]\n").expect("parse");
        assert!(f.enabled && f.covers("GO") && !f.covers("rs"));
        assert_eq!(f.language_id("go"), "go");
        assert_eq!(f.diagnostics.max_wait_ms, 5_000);
        assert!(toml::from_str::<LspServerFile>("command = \"x\"").is_err());
        assert!(toml::from_str::<LspServerFile>("comand = \"x\"\nextensions = []").is_err());
    }

    #[test]
    fn shipped_files_parse() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../defaults/lsp.d");
        let mut n = 0;
        for e in std::fs::read_dir(&dir).expect("defaults/lsp.d") {
            let path = e.expect("entry").path();
            if path.extension().is_some_and(|x| x == "toml") {
                let text = std::fs::read_to_string(&path).expect("read");
                let f: LspServerFile =
                    toml::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
                assert!(!f.extensions.is_empty(), "{}", path.display());
                assert!(
                    text.contains("Install:"),
                    "{} must say how to install the server",
                    path.display()
                );
                n += 1;
            }
        }
        assert!(n >= 8);
    }
}
