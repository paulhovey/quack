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

use std::path::PathBuf;

use crate::paths::Env;

/// Why a secret could not be resolved. Messages name the source but never the value.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SecretError {
    #[error("environment variable `{0}` is not set")]
    EnvMissing(String),
    #[error("cannot read `{path}`: {reason}")]
    File { path: String, reason: String },
    #[error("command `{cmd}` failed: {reason}")]
    Cmd { cmd: String, reason: String },
    #[error("keyring secrets are not supported yet (use `env`, `file` or `cmd`)")]
    KeyringUnsupported,
    #[error("`{0}` resolved to an empty value")]
    Empty(String),
}

impl Secret {
    /// Resolve to the secret value. `cmd` runs a process, so call this off the async runtime.
    pub fn resolve(&self, env: Env<'_>) -> Result<String, SecretError> {
        let (value, source) = match self {
            Self::Plain(v) => (v.clone(), "value".to_owned()),
            Self::Env { env: name, format } => {
                let raw = env(name)
                    .and_then(|v| v.into_string().ok())
                    .ok_or_else(|| SecretError::EnvMissing(name.clone()))?;
                let v = match format {
                    Some(f) => f.replace("{}", &raw),
                    None => raw,
                };
                (v, format!("env {name}"))
            }
            Self::File { file } => {
                let path = expand_home(file, env);
                let v = std::fs::read_to_string(&path).map_err(|e| SecretError::File {
                    path: file.clone(),
                    reason: e.to_string(),
                })?;
                (v.trim().to_owned(), format!("file {file}"))
            }
            Self::Cmd { cmd } => {
                let shown = cmd.join(" ");
                let (program, args) = cmd.split_first().ok_or_else(|| SecretError::Cmd {
                    cmd: shown.clone(),
                    reason: "empty command".into(),
                })?;
                let out = std::process::Command::new(program)
                    .args(args)
                    .stdin(std::process::Stdio::null())
                    .output()
                    .map_err(|e| SecretError::Cmd {
                        cmd: shown.clone(),
                        reason: e.to_string(),
                    })?;
                if !out.status.success() {
                    return Err(SecretError::Cmd {
                        cmd: shown,
                        reason: format!("exit status {}", out.status),
                    });
                }
                (
                    String::from_utf8_lossy(&out.stdout).trim().to_owned(),
                    format!("cmd {shown}"),
                )
            }
            Self::Keyring { .. } => return Err(SecretError::KeyringUnsupported),
        };
        if value.is_empty() {
            return Err(SecretError::Empty(source));
        }
        Ok(value)
    }
}

/// Expand a leading `~` to the home directory (`%USERPROFILE%` on Windows).
pub fn expand_home(path: &str, env: Env<'_>) -> PathBuf {
    let rest = path
        .strip_prefix("~/")
        .or_else(|| path.strip_prefix("~\\"))
        .or_else(|| (path == "~").then_some(""));
    match (
        rest,
        env(if cfg!(windows) { "USERPROFILE" } else { "HOME" }),
    ) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => PathBuf::from(path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn env_and_format() {
        let env = |k: &str| (k == "K").then(|| OsString::from("abc"));
        let s = Secret::Env {
            env: "K".into(),
            format: Some("Token {}".into()),
        };
        assert_eq!(s.resolve(&env), Ok("Token abc".into()));
        let missing = Secret::Env {
            env: "NOPE".into(),
            format: None,
        };
        assert_eq!(
            missing.resolve(&env),
            Err(SecretError::EnvMissing("NOPE".into()))
        );
    }

    #[test]
    fn file_is_trimmed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("k");
        std::fs::write(&path, "secret\n").expect("write");
        let s = Secret::File {
            file: path.display().to_string(),
        };
        assert_eq!(s.resolve(&|_| None), Ok("secret".into()));
    }
}
