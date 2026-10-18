//! Configuration: per-OS layer directories, `config.toml` layering with policy locks,
//! project trust, and one-file-per-item directories such as `providers.d/`.
//!
//! See docs/plan/01-configuration.md. Precedence, lowest to highest:
//! compiled defaults → defaults bundle → system → user → project (trusted only) → env → CLI,
//! then keys locked by the system layer's `[policy]` are reset to the system value.

mod diagnostic;
mod edit;
mod items;
#[cfg(feature = "keyring")]
mod keyring;
mod list;
mod load;
pub mod mcp;
mod merge;
mod paths;
pub mod profile;
pub mod provider;
pub mod schema;
mod secret;
pub mod settings;
mod sha256;
mod trust;

pub use diagnostic::FileDiagnostic;
pub use edit::{SettingRow, set_in_file};
pub use items::{Item, ItemState, glob_match, is_loopback, load_dir_items, load_items, url_host};
pub use load::{
    ConfigError, LayerReport, LayerStatus, LoadOptions, Loaded, Origin, ProjectStatus, load,
    load_with,
};
pub use mcp::McpServerFile;
pub use paths::{Dirs, Env, LayerKind, find_workspace_root};
pub use provider::ProviderFile;
pub use secret::{Secret, SecretError, expand_home, keyring_delete, keyring_parts, keyring_set};
pub use settings::Settings;
pub use trust::{ProjectSummary, TrustDecision};

/// Product name, used for directory names. Kept in one place for the eventual rename.
pub const APP_NAME: &str = "quack";
/// Prefix for environment variables.
pub const ENV_PREFIX: &str = "QUACK_";
/// Name of the project-layer directory inside a repository.
pub const PROJECT_DIR_NAME: &str = ".quack";

/// Load `mcp.d/*.toml` from every active layer, merged by id. A project's files load
/// only when the project is trusted.
pub fn load_mcp(loaded: &Loaded) -> Vec<Item<McpServerFile>> {
    let mut items = load_items::<McpServerFile>(&loaded.item_layers(), "mcp.d");
    items::apply_toggles(
        &mut items,
        &loaded.settings.mcp.disabled,
        &loaded.settings.policy.mcp,
        |f| (!f.url.is_empty()).then_some(f.url.as_str()),
    );
    items
}

/// Load `providers.d/*.toml` from every active layer, merged by id.
pub fn load_providers(loaded: &Loaded) -> Vec<Item<ProviderFile>> {
    let mut items = load_items::<ProviderFile>(&loaded.item_layers(), "providers.d");
    items::apply_toggles(
        &mut items,
        &loaded.settings.providers.disabled,
        &loaded.settings.policy.providers,
        |f| Some(f.base_url.as_str()),
    );
    items
}
