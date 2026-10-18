use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::{APP_NAME, ENV_PREFIX, PROJECT_DIR_NAME};

/// Source of environment variables. Injected so tests never mutate the process
/// environment (`set_var` is `unsafe` in edition 2024).
pub type Env<'a> = &'a dyn Fn(&str) -> Option<OsString>;

/// A configuration layer, in precedence order (lowest first).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LayerKind {
    /// The separately shipped defaults bundle (docs/plan/14-packaging-and-distribution.md).
    Defaults,
    System,
    User,
    Project,
}

impl LayerKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Defaults => "defaults",
            Self::System => "system",
            Self::User => "user",
            Self::Project => "project",
        }
    }
}

impl fmt::Display for LayerKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// Resolved directories for one run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dirs {
    /// Defaults bundle, if one is installed and not disabled.
    pub defaults: Option<PathBuf>,
    pub system: PathBuf,
    pub user_config: PathBuf,
    pub user_data: PathBuf,
    pub user_cache: PathBuf,
    /// Git root containing the cwd, or the cwd itself.
    pub workspace_root: PathBuf,
    /// `<workspace_root>/.cutoffharness`.
    pub project: PathBuf,
}

impl Dirs {
    /// Resolve directories from the real process environment.
    pub fn discover(cwd: &Path) -> Self {
        Self::discover_with(&|k| std::env::var_os(k), cwd)
    }

    pub fn discover_with(env: Env<'_>, cwd: &Path) -> Self {
        let var = |name: &str| env(&format!("{ENV_PREFIX}{name}")).filter(|v| !v.is_empty());
        let home = env(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
            .map(PathBuf::from)
            .unwrap_or_default();
        let platform = platform_dirs(env, &home);

        let user_data = var("DATA_DIR").map_or(platform.data, PathBuf::from);
        let defaults = match var("DEFAULTS_DIR") {
            Some(v) if v == "none" => None,
            Some(v) => Some(PathBuf::from(v)),
            None => default_bundle_candidates(&user_data)
                .into_iter()
                .find(|p| p.is_dir()),
        };
        let workspace_root = find_workspace_root(cwd);
        Self {
            defaults,
            system: var("SYSTEM_DIR").map_or(platform.system, PathBuf::from),
            user_config: var("CONFIG_DIR").map_or(platform.config, PathBuf::from),
            user_cache: var("CACHE_DIR").map_or(platform.cache, PathBuf::from),
            user_data,
            project: workspace_root.join(PROJECT_DIR_NAME),
            workspace_root,
        }
    }

    pub fn layer_dir(&self, kind: LayerKind) -> Option<&Path> {
        match kind {
            LayerKind::Defaults => self.defaults.as_deref(),
            LayerKind::System => Some(&self.system),
            LayerKind::User => Some(&self.user_config),
            LayerKind::Project => Some(&self.project),
        }
    }

    pub fn log_dir(&self) -> PathBuf {
        self.user_cache.join("logs")
    }
}

struct PlatformDirs {
    system: PathBuf,
    config: PathBuf,
    data: PathBuf,
    cache: PathBuf,
}

/// XDG variables are only honoured when absolute, per the spec.
#[cfg(not(windows))]
fn xdg(env: Env<'_>, name: &str) -> Option<PathBuf> {
    env(name).map(PathBuf::from).filter(|p| p.is_absolute())
}

#[cfg(windows)]
fn platform_dirs(env: Env<'_>, home: &Path) -> PlatformDirs {
    let dir = |name: &str, fallback: &[&str]| {
        env(name)
            .map(PathBuf::from)
            .unwrap_or_else(|| fallback.iter().fold(home.to_path_buf(), |p, c| p.join(c)))
    };
    let local = dir("LOCALAPPDATA", &["AppData", "Local"]).join(APP_NAME);
    PlatformDirs {
        system: env("ProgramData")
            .map_or_else(|| PathBuf::from(r"C:\ProgramData"), PathBuf::from)
            .join(APP_NAME),
        config: dir("APPDATA", &["AppData", "Roaming"]).join(APP_NAME),
        cache: local.join("cache"),
        data: local,
    }
}

#[cfg(target_os = "macos")]
fn platform_dirs(env: Env<'_>, home: &Path) -> PlatformDirs {
    // Terminal tools conventionally use ~/.config on macOS; Library is for data only.
    let support = home.join("Library").join("Application Support");
    PlatformDirs {
        system: PathBuf::from("/Library/Application Support").join(APP_NAME),
        config: xdg(env, "XDG_CONFIG_HOME")
            .unwrap_or_else(|| home.join(".config"))
            .join(APP_NAME),
        data: xdg(env, "XDG_DATA_HOME").unwrap_or(support).join(APP_NAME),
        cache: xdg(env, "XDG_CACHE_HOME")
            .unwrap_or_else(|| home.join("Library").join("Caches"))
            .join(APP_NAME),
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
fn platform_dirs(env: Env<'_>, home: &Path) -> PlatformDirs {
    PlatformDirs {
        system: PathBuf::from("/etc").join(APP_NAME),
        config: xdg(env, "XDG_CONFIG_HOME")
            .unwrap_or_else(|| home.join(".config"))
            .join(APP_NAME),
        data: xdg(env, "XDG_DATA_HOME")
            .unwrap_or_else(|| home.join(".local").join("share"))
            .join(APP_NAME),
        cache: xdg(env, "XDG_CACHE_HOME")
            .unwrap_or_else(|| home.join(".cache"))
            .join(APP_NAME),
    }
}

/// Where an installed defaults bundle may live, most specific first.
fn default_bundle_candidates(user_data: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(exe_dir) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
    {
        // Homebrew / tarball: <prefix>/bin/../share/cutoffharness/defaults
        if let Some(prefix) = exe_dir.parent() {
            out.push(prefix.join("share").join(APP_NAME).join("defaults"));
        }
        // MSI / zip: next to the executable
        out.push(exe_dir.join("defaults"));
    }
    if cfg!(unix) {
        out.push(PathBuf::from("/usr/share").join(APP_NAME).join("defaults"));
        out.push(
            PathBuf::from("/usr/local/share")
                .join(APP_NAME)
                .join("defaults"),
        );
    }
    out.push(user_data.join("defaults"));
    out
}

/// The nearest ancestor of `cwd` containing `.git` (directory or worktree file), else `cwd`.
pub fn find_workspace_root(cwd: &Path) -> PathBuf {
    cwd.ancestors()
        .find(|dir| dir.join(".git").exists())
        .unwrap_or(cwd)
        .to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> + use<> {
        let map: HashMap<String, OsString> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), OsString::from(v)))
            .collect();
        move |k| map.get(k).cloned()
    }

    #[test]
    fn env_overrides_win() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg = tmp.path().join("cfg");
        let sys = tmp.path().join("sys");
        let env = env_of(&[
            ("CUTOFFHARNESS_CONFIG_DIR", cfg.to_str().unwrap_or_default()),
            ("CUTOFFHARNESS_SYSTEM_DIR", sys.to_str().unwrap_or_default()),
            ("CUTOFFHARNESS_DEFAULTS_DIR", "none"),
        ]);
        let dirs = Dirs::discover_with(&env, tmp.path());
        assert_eq!(dirs.user_config, cfg);
        assert_eq!(dirs.system, sys);
        assert_eq!(dirs.defaults, None);
    }

    #[test]
    fn workspace_root_is_git_root() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let nested = tmp.path().join("a").join("b");
        std::fs::create_dir_all(&nested).expect("mkdir");
        std::fs::create_dir(tmp.path().join(".git")).expect("mkdir .git");
        assert_eq!(find_workspace_root(&nested), tmp.path());
        let dirs = Dirs::discover_with(&env_of(&[]), &nested);
        assert_eq!(dirs.project, tmp.path().join(".cutoffharness"));
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    #[test]
    fn relative_xdg_is_ignored() {
        let env = env_of(&[("HOME", "/home/u"), ("XDG_CONFIG_HOME", "relative")]);
        let dirs = Dirs::discover_with(&env, Path::new("/"));
        assert_eq!(dirs.user_config, Path::new("/home/u/.config/cutoffharness"));
    }
}
