use std::fmt;
use std::path::{Path, PathBuf};

use miette::Diagnostic;
use serde::Deserialize;
use toml::{Table, Value};

use crate::ENV_PREFIX;
use crate::diagnostic::{FileDiagnostic, read_text};
use crate::merge::{self, Origins};
use crate::paths::{Dirs, Env, LayerKind};
use crate::settings::{Policy, ProjectConfigPolicy, Settings};
use crate::trust::{self, ProjectSummary, TrustDecision};

/// Inputs that come from the command line.
#[derive(Debug, Clone, Default)]
pub struct LoadOptions {
    pub cwd: PathBuf,
    /// `-m/--model`.
    pub model: Option<String>,
    /// `-c key=value`, applied in order. Values parse as TOML, falling back to a string.
    pub overrides: Vec<String>,
}

impl LoadOptions {
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        Self {
            cwd: cwd.into(),
            ..Self::default()
        }
    }
}

/// Where an effective setting came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    Default,
    File { layer: LayerKind, path: PathBuf },
    Env(String),
    Cli,
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Default => f.write_str("default"),
            Self::File { layer, path } => write!(f, "{layer}: {}", path.display()),
            Self::Env(var) => write!(f, "env {var}"),
            Self::Cli => f.write_str("cli"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayerStatus {
    Loaded,
    /// No `config.toml` in this layer (its item directories may still exist).
    Missing,
    Failed,
    Skipped(String),
}

#[derive(Debug, Clone)]
pub struct LayerReport {
    pub kind: LayerKind,
    pub dir: Option<PathBuf>,
    pub status: LayerStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectStatus {
    /// No `.quack/` directory in the workspace.
    None,
    Trusted,
    SettingsOnly,
    /// Found but not yet trusted: skipped, and the user is asked.
    Untrusted,
    /// Trusted before, but files that need trust changed since: skipped until the user
    /// looks again.
    Changed,
    Ignored,
    DisabledByPolicy,
}

#[derive(Debug, thiserror::Error, Diagnostic)]
pub enum ConfigError {
    #[error("invalid override `{0}`: expected key=value")]
    OverrideSyntax(String),
    #[error("cannot set `{0}`: a parent key is not a table")]
    OverridePath(String),
    #[error("invalid configuration after applying environment and CLI overrides: {0}")]
    #[diagnostic(help("check -c overrides and QUACK_* variables"))]
    Invalid(String),
}

/// The result of loading settings. Item directories are loaded separately and async.
#[derive(Debug)]
pub struct Loaded {
    pub dirs: Dirs,
    /// The directories as discovered, before the system layer moved the defaults bundle;
    /// a reload starts from these.
    found: Dirs,
    /// What the command line asked for; a reload applies it again.
    pub opts: LoadOptions,
    pub settings: Settings,
    pub origins: Origins,
    pub layers: Vec<LayerReport>,
    pub project: ProjectStatus,
    pub warnings: Vec<String>,
    /// Files that failed to parse. Each one disables only itself.
    pub diagnostics: Vec<FileDiagnostic>,
}

impl Loaded {
    /// Load again from the same directories and command line, with the process
    /// environment: picks up edited files and trust decisions.
    pub fn reload(&self) -> Result<Loaded, ConfigError> {
        self.reload_with(&|k| std::env::var_os(k))
    }

    pub fn reload_with(&self, env: Env<'_>) -> Result<Loaded, ConfigError> {
        load_in(self.found.clone(), env, &self.opts)
    }

    /// What the project's `.quack/` holds, for the trust prompt.
    pub fn project_summary(&self) -> ProjectSummary {
        ProjectSummary::read(&self.dirs.project)
    }

    /// Record the user's trust decision for this workspace in `<user data>/trust.toml`.
    /// It applies from the next load (see [`reload`](Self::reload)).
    pub fn trust_project(&self, decision: TrustDecision) -> Result<PathBuf, String> {
        if self.project == ProjectStatus::DisabledByPolicy {
            return Err("project config is disabled by the system policy".into());
        }
        if !self.dirs.project.is_dir() {
            return Err(format!("{} does not exist", self.dirs.project.display()));
        }
        trust::record(
            &self.dirs.user_data,
            &self.dirs.workspace_root,
            &self.dirs.project,
            decision,
        )
    }

    /// Layers whose item directories (`providers.d`, `mcp.d`, …) are active.
    pub fn item_layers(&self) -> Vec<(LayerKind, PathBuf)> {
        [
            LayerKind::Defaults,
            LayerKind::System,
            LayerKind::User,
            LayerKind::Project,
        ]
        .into_iter()
        .filter(|k| *k != LayerKind::Project || self.project == ProjectStatus::Trusted)
        .filter_map(|k| self.dirs.layer_dir(k).map(|d| (k, d.to_path_buf())))
        .collect()
    }

    pub fn origin_of(&self, key: &str) -> &Origin {
        self.origins.get(key).unwrap_or(&Origin::Default)
    }

    /// Effective settings as TOML, optionally annotated with each value's origin.
    pub fn render(&self, with_origin: bool) -> String {
        let table = Table::try_from(&self.settings).unwrap_or_default();
        if !with_origin {
            return toml::to_string_pretty(&table).unwrap_or_default();
        }
        merge::leaves(&table)
            .into_iter()
            .map(|(k, v)| format!("{k} = {v}  # {}\n", self.origin_of(&k)))
            .collect()
    }
}

/// Load settings using the process environment.
pub fn load(opts: &LoadOptions) -> Result<Loaded, ConfigError> {
    load_with(&|k| std::env::var_os(k), opts)
}

struct LayerFile {
    kind: LayerKind,
    dir: Option<PathBuf>,
    table: Option<(PathBuf, Table)>,
    status: LayerStatus,
}

pub fn load_with(env: Env<'_>, opts: &LoadOptions) -> Result<Loaded, ConfigError> {
    load_in(Dirs::discover_with(env, &opts.cwd), env, opts)
}

fn load_in(found: Dirs, env: Env<'_>, opts: &LoadOptions) -> Result<Loaded, ConfigError> {
    let mut dirs = found.clone();
    let mut warnings = Vec::new();
    let mut diagnostics = Vec::new();

    // The system layer can relocate or disable the defaults bundle, so read it first.
    let system = read_layer(LayerKind::System, Some(&dirs.system), &mut diagnostics);
    if let Some(dir) = system
        .table
        .as_ref()
        .and_then(|(_, t)| merge::get_path(t, "defaults.dir"))
        .and_then(Value::as_str)
        .filter(|d| !d.is_empty())
    {
        dirs.defaults = (dir != "none").then(|| PathBuf::from(dir));
    }
    let defaults = read_layer(
        LayerKind::Defaults,
        dirs.defaults.as_deref(),
        &mut diagnostics,
    );

    let mut merged = Table::try_from(Settings::default()).unwrap_or_default();
    let mut origins = Origins::new();
    let mut reports = Vec::new();

    for mut layer in [defaults, system] {
        apply_layer(&mut layer, &mut merged, &mut origins, &mut warnings);
        reports.push(report(layer));
    }

    // Everything at or below the system layer: the reference for locked keys.
    let base = merged.clone();
    let base_origins = origins.clone();
    let policy: Policy = base
        .get("policy")
        .cloned()
        .and_then(|v| v.try_into().ok())
        .unwrap_or_default();

    let mut user = read_layer(LayerKind::User, Some(&dirs.user_config), &mut diagnostics);
    apply_layer(&mut user, &mut merged, &mut origins, &mut warnings);
    reports.push(report(user));

    let project = project_status(&dirs, policy.project_config);
    let mut project_layer = match project {
        ProjectStatus::Trusted | ProjectStatus::SettingsOnly => {
            read_layer(LayerKind::Project, Some(&dirs.project), &mut diagnostics)
        }
        other => LayerFile {
            kind: LayerKind::Project,
            dir: Some(dirs.project.clone()),
            table: None,
            status: match other {
                ProjectStatus::None => LayerStatus::Missing,
                ProjectStatus::Untrusted => LayerStatus::Skipped("not trusted yet".into()),
                ProjectStatus::Changed => {
                    LayerStatus::Skipped("changed since it was trusted".into())
                }
                ProjectStatus::Ignored => LayerStatus::Skipped("ignored by user".into()),
                _ => LayerStatus::Skipped("disabled by policy".into()),
            },
        },
    };
    apply_layer(&mut project_layer, &mut merged, &mut origins, &mut warnings);
    reports.push(report(project_layer));

    // Environment, then CLI.
    let model_var = format!("{ENV_PREFIX}MODEL");
    if let Some(model) = env(&model_var).and_then(|v| v.into_string().ok()) {
        merge::set_path(&mut merged, "model.default", Value::String(model));
        origins.insert("model.default".into(), Origin::Env(model_var));
    }
    if let Some(model) = &opts.model {
        merge::set_path(&mut merged, "model.default", Value::String(model.clone()));
        origins.insert("model.default".into(), Origin::Cli);
    }
    for raw in &opts.overrides {
        let (key, value) = raw
            .split_once('=')
            .map(|(k, v)| (k.trim(), v.trim()))
            .filter(|(k, _)| !k.is_empty())
            .ok_or_else(|| ConfigError::OverrideSyntax(raw.clone()))?;
        if key == "policy" || key.starts_with("policy.") {
            warnings.push(format!(
                "`-c {key}` ignored: policy is only read from the system layer"
            ));
            continue;
        }
        if !merge::set_path(&mut merged, key, parse_override(value)) {
            return Err(ConfigError::OverridePath(key.to_owned()));
        }
        origins.insert(key.to_owned(), Origin::Cli);
    }

    apply_locks(
        &policy,
        (&base, &base_origins),
        &mut merged,
        &mut origins,
        &mut warnings,
    );

    let mut settings =
        Settings::deserialize(merged).map_err(|e| ConfigError::Invalid(e.message().to_owned()))?;
    if let Some(max) = policy.max_permission_mode
        && settings.permissions.mode > max
    {
        warnings.push(format!(
            "permission mode `{}` exceeds policy maximum; using `{}`",
            settings.permissions.mode.label(),
            max.label()
        ));
        settings.permissions.mode = max;
        match base_origins.get("policy.max_permission_mode") {
            Some(o) => origins.insert("permissions.mode".into(), o.clone()),
            None => origins.remove("permissions.mode"),
        };
    }

    Ok(Loaded {
        dirs,
        found,
        opts: opts.clone(),
        settings,
        origins,
        layers: reports,
        project,
        warnings,
        diagnostics,
    })
}

fn read_layer(kind: LayerKind, dir: Option<&Path>, diags: &mut Vec<FileDiagnostic>) -> LayerFile {
    let Some(dir) = dir else {
        return LayerFile {
            kind,
            dir: None,
            table: None,
            status: LayerStatus::Skipped("not installed".into()),
        };
    };
    let path = dir.join("config.toml");
    let mut layer = LayerFile {
        kind,
        dir: Some(dir.to_path_buf()),
        table: None,
        status: LayerStatus::Missing,
    };
    let text = match read_text(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return layer,
        Err(e) => {
            diags.push(FileDiagnostic::io(&path, &e));
            layer.status = LayerStatus::Failed;
            return layer;
        }
    };
    // Validate against the typed schema first so errors carry spans.
    if let Err(e) = toml::from_str::<Settings>(&text) {
        diags.push(
            FileDiagnostic::from_toml(&path, &text, &e)
                .with_help("this file is ignored until fixed; other layers still load"),
        );
        layer.status = LayerStatus::Failed;
        return layer;
    }
    match toml::from_str::<Table>(&text) {
        Ok(table) => {
            layer.table = Some((path, table));
            layer.status = LayerStatus::Loaded;
        }
        Err(e) => {
            diags.push(FileDiagnostic::from_toml(&path, &text, &e));
            layer.status = LayerStatus::Failed;
        }
    }
    layer
}

fn apply_layer(
    layer: &mut LayerFile,
    merged: &mut Table,
    origins: &mut Origins,
    warnings: &mut Vec<String>,
) {
    let Some((path, mut table)) = layer.table.take() else {
        return;
    };
    if layer.kind != LayerKind::System && table.remove("policy").is_some() {
        warnings.push(format!(
            "{}: [policy] is only honoured in the system layer; ignored",
            path.display()
        ));
    }
    let origin = Origin::File {
        layer: layer.kind,
        path,
    };
    merge::merge(merged, table, &origin, origins, "");
}

fn report(layer: LayerFile) -> LayerReport {
    LayerReport {
        kind: layer.kind,
        dir: layer.dir,
        status: layer.status,
    }
}

fn project_status(dirs: &Dirs, policy: ProjectConfigPolicy) -> ProjectStatus {
    if !dirs.project.is_dir() {
        return ProjectStatus::None;
    }
    match policy {
        ProjectConfigPolicy::Never => ProjectStatus::DisabledByPolicy,
        ProjectConfigPolicy::Always => ProjectStatus::Trusted,
        ProjectConfigPolicy::TrustedOnly => {
            match trust::lookup(&dirs.user_data, &dirs.workspace_root, &dirs.project) {
                Some(f) if !f.current => ProjectStatus::Changed,
                Some(f) => match f.decision {
                    TrustDecision::Trusted => ProjectStatus::Trusted,
                    TrustDecision::SettingsOnly => ProjectStatus::SettingsOnly,
                    TrustDecision::Ignored => ProjectStatus::Ignored,
                },
                // Only text (skills, memory): nothing to decide.
                None if ProjectSummary::read(&dirs.project).is_empty() => ProjectStatus::Trusted,
                None => ProjectStatus::Untrusted,
            }
        }
    }
}

pub(crate) fn parse_override(value: &str) -> Value {
    toml::from_str::<Table>(&format!("v = {value}"))
        .ok()
        .and_then(|mut t| t.remove("v"))
        .unwrap_or_else(|| Value::String(value.to_owned()))
}

fn apply_locks(
    policy: &Policy,
    (base, base_origins): (&Table, &Origins),
    merged: &mut Table,
    origins: &mut Origins,
    warnings: &mut Vec<String>,
) {
    for key in &policy.locked {
        let Some(locked) = merge::get_path(base, key) else {
            warnings.push(format!("policy locks unknown key `{key}`"));
            continue;
        };
        if merge::get_path(merged, key) != Some(locked) {
            let from = origins
                .get(key)
                .map_or_else(|| "a higher layer".into(), ToString::to_string);
            warnings.push(format!(
                "`{key}` is locked by policy; value from {from} ignored"
            ));
            merge::set_path(merged, key, locked.clone());
            match base_origins.get(key) {
                Some(o) => origins.insert(key.clone(), o.clone()),
                None => origins.remove(key),
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::PermissionMode;
    use std::collections::HashMap;
    use std::ffi::OsString;

    struct Fixture {
        _tmp: tempfile::TempDir,
        root: PathBuf,
        vars: HashMap<String, OsString>,
    }

    impl Fixture {
        fn new() -> Self {
            let tmp = tempfile::tempdir().expect("tempdir");
            let root = tmp.path().to_path_buf();
            let mut vars = HashMap::new();
            for (k, sub) in [
                ("QUACK_SYSTEM_DIR", "system"),
                ("QUACK_CONFIG_DIR", "user"),
                ("QUACK_DATA_DIR", "data"),
                ("QUACK_CACHE_DIR", "cache"),
            ] {
                vars.insert(k.to_owned(), root.join(sub).into_os_string());
            }
            vars.insert("QUACK_DEFAULTS_DIR".into(), "none".into());
            std::fs::create_dir_all(root.join("repo").join(".git")).expect("mkdir");
            Self {
                _tmp: tmp,
                root,
                vars,
            }
        }

        fn write(&self, rel: &str, text: &str) {
            let path = self.root.join(rel);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            std::fs::write(path, text).expect("write");
        }

        fn load(&self, opts: impl FnOnce(&mut LoadOptions)) -> Loaded {
            let mut o = LoadOptions::new(self.root.join("repo"));
            opts(&mut o);
            let vars = self.vars.clone();
            load_with(&move |k| vars.get(k).cloned(), &o).expect("load")
        }
    }

    #[test]
    fn empty_environment_yields_compiled_defaults() {
        let f = Fixture::new();
        let l = f.load(|_| {});
        assert_eq!(l.settings, Settings::default());
        assert_eq!(l.project, ProjectStatus::None);
        assert!(l.diagnostics.is_empty());
    }

    #[test]
    fn layers_merge_in_precedence_order() {
        let f = Fixture::new();
        f.write(
            "system/config.toml",
            "[permissions]\nallow = [\"shell:git status*\"]",
        );
        f.write(
            "user/config.toml",
            "[model]\ndefault = \"a/b\"\n[permissions]\nallow.append = [\"shell:cargo test*\"]",
        );
        let l = f.load(|o| o.overrides.push("ui.ascii=true".into()));
        assert_eq!(l.settings.model.default, "a/b");
        assert_eq!(
            l.settings.permissions.allow,
            ["shell:git status*", "shell:cargo test*"]
        );
        assert!(l.settings.ui.ascii);
        assert_eq!(l.origin_of("ui.ascii"), &Origin::Cli);
        assert!(matches!(
            l.origin_of("model.default"),
            Origin::File {
                layer: LayerKind::User,
                ..
            }
        ));
    }

    #[test]
    fn policy_locks_and_caps() {
        let f = Fixture::new();
        f.write(
            "system/config.toml",
            "[network]\noffline = true\n[policy]\nlocked = [\"network.offline\"]\nmax_permission_mode = \"auto-edit\"",
        );
        f.write(
            "user/config.toml",
            "[network]\noffline = false\n[permissions]\nmode = \"full-auto\"\n[policy]\nlocked = []",
        );
        let l = f.load(|o| o.overrides.push("network.offline=false".into()));
        assert!(l.settings.network.offline);
        assert_eq!(l.settings.permissions.mode, PermissionMode::AutoEdit);
        assert_eq!(l.settings.policy.locked, ["network.offline"]);
        assert_eq!(l.warnings.len(), 3, "{:?}", l.warnings);
    }

    #[test]
    fn broken_file_is_reported_with_position_and_skipped() {
        let f = Fixture::new();
        f.write("system/config.toml", "[ui]\nascii = true");
        f.write("user/config.toml", "[ui]\nbogus = 1");
        let l = f.load(|_| {});
        assert!(l.settings.ui.ascii, "other layers still apply");
        assert_eq!(l.diagnostics.len(), 1);
        let line = l.diagnostics[0].one_line();
        assert!(line.contains("config.toml:2:1"), "{line}");
        assert!(line.contains("bogus"), "{line}");
    }

    #[test]
    fn untrusted_project_is_skipped_until_trusted() {
        let f = Fixture::new();
        f.write("repo/.quack/config.toml", "[model]\ndefault = \"proj/m\"");
        let l = f.load(|_| {});
        assert_eq!(l.project, ProjectStatus::Untrusted);
        assert_eq!(l.settings.model.default, "");

        let repo = f.root.join("repo");
        f.write(
            "data/trust.toml",
            &format!(
                "[[project]]\npath = {:?}\ndecision = \"trusted\"",
                repo.display().to_string()
            ),
        );
        let l = f.load(|_| {});
        assert_eq!(l.project, ProjectStatus::Trusted);
        assert_eq!(l.settings.model.default, "proj/m");
    }

    #[test]
    fn bom_is_accepted() {
        let f = Fixture::new();
        f.write("user/config.toml", "\u{feff}[ui]\nascii = true");
        assert!(f.load(|_| {}).settings.ui.ascii);
    }
}
