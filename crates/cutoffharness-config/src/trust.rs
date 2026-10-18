//! Project trust records in `<user data>/trust.toml` (docs/plan/01-configuration.md#project-trust).
//!
//! Scaffold: decisions are read by canonical path. Hashing executable-bearing files, so a
//! changed file re-prompts, lands with the trust prompt in M1.

use std::path::Path;

use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TrustDecision {
    Trusted,
    /// Load `config.toml` only: no item directories that can run executables.
    SettingsOnly,
    Ignored,
}

#[derive(Debug, Default, Deserialize)]
struct TrustFile {
    #[serde(default)]
    project: Vec<TrustRecord>,
}

#[derive(Debug, Deserialize)]
struct TrustRecord {
    path: String,
    decision: TrustDecision,
}

pub(crate) fn lookup(user_data: &Path, workspace_root: &Path) -> Option<TrustDecision> {
    let text = crate::diagnostic::read_text(&user_data.join("trust.toml")).ok()?;
    let file: TrustFile = toml::from_str(&text).ok()?;
    let want = workspace_root.canonicalize().ok()?;
    file.project
        .into_iter()
        .find(|r| Path::new(&r.path).canonicalize().ok().as_deref() == Some(want.as_path()))
        .map(|r| r.decision)
}
