use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A secret value. Secrets never have to be written into TOML in plain text.
/// Resolution (running `cmd`, reading the keyring) lands with the providers in M1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum Secret {
    /// Plain value. Allowed, but warns when the file is world-readable or inside a repo.
    Plain(String),
    Env {
        env: String,
        /// Format string with `{}` for the value, e.g. `"Bearer {}"`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        format: Option<String>,
    },
    /// Command whose trimmed stdout is the secret. Only honoured from system, user or trusted project layers.
    Cmd {
        cmd: Vec<String>,
    },
    Keyring {
        keyring: String,
    },
    File {
        file: String,
    },
}
