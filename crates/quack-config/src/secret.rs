use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A secret value. Secrets never have to be written into TOML in plain text.
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
    /// OS credential store entry `"<service>/<user>"`, or `"<user>"` under the
    /// `quack` service. Store one with `quack keyring set <name>`.
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
    #[error("this build has no keyring support (use `env`, `file` or `cmd`)")]
    KeyringUnsupported,
    #[error("keyring `{name}`: {reason}")]
    Keyring { name: String, reason: String },
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
            Self::Keyring { keyring } => (keyring_get(keyring)?, format!("keyring {keyring}")),
        };
        if value.is_empty() {
            return Err(SecretError::Empty(source));
        }
        Ok(value)
    }
}

/// `"<service>/<user>"` or `"<user>"` → `(service, user)`.
pub fn keyring_parts(name: &str) -> (&str, &str) {
    match name.split_once('/') {
        Some((service, user)) if !service.is_empty() && !user.is_empty() => (service, user),
        _ => (crate::APP_NAME, name),
    }
}

#[cfg(feature = "keyring")]
fn keyring_error(name: &str) -> impl Fn(String) -> SecretError + '_ {
    move |reason| SecretError::Keyring {
        name: name.to_owned(),
        reason,
    }
}

#[cfg(feature = "keyring")]
fn keyring_get(name: &str) -> Result<String, SecretError> {
    let (service, user) = keyring_parts(name);
    crate::keyring::get(service, user)
        .map_err(keyring_error(name))?
        .ok_or_else(|| {
            keyring_error(name)(format!("nothing stored; run `quack keyring set {name}`"))
        })
}

#[cfg(not(feature = "keyring"))]
fn keyring_get(_name: &str) -> Result<String, SecretError> {
    Err(SecretError::KeyringUnsupported)
}

/// Store a secret in the OS credential store.
#[cfg(feature = "keyring")]
pub fn keyring_set(name: &str, value: &str) -> Result<(), SecretError> {
    let (service, user) = keyring_parts(name);
    crate::keyring::set(service, user, value).map_err(keyring_error(name))
}

#[cfg(not(feature = "keyring"))]
pub fn keyring_set(_name: &str, _value: &str) -> Result<(), SecretError> {
    Err(SecretError::KeyringUnsupported)
}

/// Remove a secret from the OS credential store.
#[cfg(feature = "keyring")]
pub fn keyring_delete(name: &str) -> Result<(), SecretError> {
    let (service, user) = keyring_parts(name);
    crate::keyring::delete(service, user).map_err(keyring_error(name))
}

#[cfg(not(feature = "keyring"))]
pub fn keyring_delete(_name: &str) -> Result<(), SecretError> {
    Err(SecretError::KeyringUnsupported)
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

    /// Uses the real OS store, so it only runs on request:
    /// `cargo test -p quack-config --features keyring -- --ignored keyring`.
    #[cfg(feature = "keyring")]
    #[test]
    #[ignore = "touches the OS credential store"]
    fn keyring_round_trip() {
        let name = "quack-selftest/round-trip";
        keyring_set(name, "dummy-value").expect("set");
        let secret = Secret::Keyring {
            keyring: name.into(),
        };
        let got = secret.resolve(&|_| None);
        keyring_delete(name).expect("delete");
        assert_eq!(got.as_deref(), Ok("dummy-value"));
        assert!(matches!(
            secret.resolve(&|_| None),
            Err(SecretError::Keyring { .. })
        ));
    }

    #[test]
    fn keyring_names() {
        assert_eq!(keyring_parts("corp/vllm"), ("corp", "vllm"));
        assert_eq!(keyring_parts("novita"), ("quack", "novita"));
        assert_eq!(keyring_parts("/x"), ("quack", "/x"));
    }
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
