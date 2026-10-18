//! One client per (server id, project root), started the first time a covered file is
//! touched. Clients that crash restart up to [`MAX_RESTARTS`] times.

use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use quack_config::LspServerFile;
use serde_json::Value;
use tokio::process::Command;
use tokio::sync::{OnceCell, watch};

use crate::client::{Client, Launch};
use crate::diag::{self, Limits, Severity, Snapshot};

pub const MAX_RESTARTS: u32 = 3;
/// How long a tool waits for a server that is still starting.
pub const TOOL_WAIT: Duration = Duration::from_secs(20);

/// A server file and whether its program was found.
#[derive(Debug, Clone)]
pub struct ServerSpec {
    pub id: String,
    pub file: LspServerFile,
    /// The program's full path, or why it can't be used.
    pub program: Result<PathBuf, String>,
    env: BTreeMap<String, OsString>,
}

impl ServerSpec {
    /// Find the program and resolve the environment. Runs `cmd` secrets, so call it off
    /// the async runtime.
    pub fn resolve(id: String, file: LspServerFile, root: &Path) -> Self {
        let get = |k: &str| std::env::var_os(k);
        let mut env = quack_shell::inherited_env(None);
        let mut problem = None;
        for (k, secret) in &file.env {
            match secret.resolve(&get) {
                Ok(v) => {
                    env.insert(k.clone(), v.into());
                }
                Err(e) => problem = Some(format!("env `{k}`: {e}")),
            }
        }
        let search = env.get("PATH").cloned();
        let program = match problem {
            Some(p) => Err(p),
            None => quack_shell::resolve_program(&file.command, search.as_deref(), root)
                .map_err(|_| format!("`{}` is not installed (not on PATH)", file.command)),
        };
        Self {
            id,
            file,
            program,
            env,
        }
    }

    pub fn installed(&self) -> bool {
        self.program.is_ok()
    }
}

/// Builds a server's command from its program and arguments, e.g. inside the OS sandbox.
/// `Err` refuses to start it.
pub type Prepare =
    Arc<dyn Fn(&ServerSpec, &Path, &[String]) -> Result<Command, String> + Send + Sync>;

type Started = Result<Arc<Client>, String>;
type Cell = Arc<OnceCell<Started>>;

struct Slot {
    cell: Cell,
    restarts: u32,
}

/// What `/lsp` shows for one client or one unused server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub id: String,
    pub command: String,
    pub extensions: Vec<String>,
    /// `not installed`, `idle`, `starting`, `ready`, `indexing`, `failed` or `exited`.
    pub state: String,
    pub detail: String,
    pub root: Option<PathBuf>,
    pub errors: usize,
    pub warnings: usize,
    pub log: Vec<String>,
}

pub struct LspManager {
    specs: Vec<ServerSpec>,
    root: PathBuf,
    slots: Mutex<HashMap<(usize, PathBuf), Slot>>,
    changed: Arc<watch::Sender<u64>>,
    prepare: Prepare,
}

impl std::fmt::Debug for LspManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LspManager")
            .field("specs", &self.specs)
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

impl LspManager {
    pub fn new(specs: Vec<ServerSpec>, root: PathBuf, prepare: Option<Prepare>) -> Arc<Self> {
        Arc::new(Self {
            specs,
            root,
            slots: Mutex::default(),
            changed: Arc::new(watch::channel(0).0),
            prepare: prepare.unwrap_or_else(|| {
                Arc::new(|_, program, args| {
                    let mut cmd = Command::new(program);
                    cmd.args(args);
                    Ok(cmd)
                })
            }),
        })
    }

    pub fn empty() -> Arc<Self> {
        Self::new(Vec::new(), PathBuf::new(), None)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Bumped when a server starts, stops, or reports diagnostics or progress.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }

    pub fn specs(&self) -> &[ServerSpec] {
        &self.specs
    }

    /// Installed servers, for the tool description: `(id, extensions)`.
    pub fn installed(&self) -> Vec<(&str, &[String])> {
        self.specs
            .iter()
            .filter(|s| s.installed())
            .map(|s| (s.id.as_str(), s.file.extensions.as_slice()))
            .collect()
    }

    /// The installed server covering this file.
    fn spec_for(&self, path: &Path) -> Option<usize> {
        let ext = path.extension()?.to_string_lossy();
        self.specs
            .iter()
            .position(|s| s.installed() && s.file.covers(&ext))
    }

    pub fn covers(&self, path: &Path) -> bool {
        self.spec_for(path).is_some()
    }

    /// The nearest folder at or above the file holding one of the server's root markers,
    /// not going above the workspace root; else the workspace root.
    fn project_root(&self, spec: &ServerSpec, path: &Path) -> PathBuf {
        let markers = &spec.file.root_markers;
        let inside = path.starts_with(&self.root);
        let mut dir = path.parent();
        while let Some(d) = dir {
            if !markers.is_empty() && has_marker(d, markers) {
                return d.to_path_buf();
            }
            if inside && d == self.root {
                break;
            }
            dir = d.parent();
        }
        if inside {
            self.root.clone()
        } else {
            path.parent()
                .map_or_else(|| self.root.clone(), Path::to_path_buf)
        }
    }

    /// The client for a file, starting it if needed. `Ok(None)` when no installed server
    /// covers the file.
    pub async fn client(&self, path: &Path) -> Result<Option<Arc<Client>>, String> {
        let Some(idx) = self.spec_for(path) else {
            return Ok(None);
        };
        let root = self.project_root(&self.specs[idx], path);
        self.get(idx, root).await.map(Some)
    }

    /// The client for a file if it is already running; never starts one.
    pub fn running(&self, path: &Path) -> Option<Arc<Client>> {
        let idx = self.spec_for(path)?;
        let root = self.project_root(&self.specs[idx], path);
        let slots = self.slots.lock().ok()?;
        match slots.get(&(idx, root))?.cell.get() {
            Some(Ok(c)) if c.alive() => Some(c.clone()),
            _ => None,
        }
    }

    /// Every running client.
    pub fn clients(&self) -> Vec<Arc<Client>> {
        let Ok(slots) = self.slots.lock() else {
            return Vec::new();
        };
        slots
            .values()
            .filter_map(|s| match s.cell.get() {
                Some(Ok(c)) if c.alive() => Some(c.clone()),
                _ => None,
            })
            .collect()
    }

    async fn get(&self, idx: usize, root: PathBuf) -> Result<Arc<Client>, String> {
        loop {
            let cell = {
                let mut slots = self.slots.lock().map_err(|e| e.to_string())?;
                slots
                    .entry((idx, root.clone()))
                    .or_insert_with(|| Slot {
                        cell: Arc::default(),
                        restarts: 0,
                    })
                    .cell
                    .clone()
            };
            let result = cell
                .get_or_init(|| async {
                    self.bump();
                    let r = self.start(idx, &root).await;
                    if let Err(e) = &r {
                        tracing::warn!(server = %self.specs[idx].id, error = %e, "language server did not start");
                    }
                    r
                })
                .await
                .clone();
            self.bump();
            let client = result?;
            if client.alive() {
                return Ok(client);
            }
            // It exited: start a new one, unless it keeps crashing.
            let restarts = {
                let mut slots = self.slots.lock().map_err(|e| e.to_string())?;
                let slot = slots
                    .get_mut(&(idx, root.clone()))
                    .ok_or("the server was removed")?;
                if Arc::ptr_eq(&slot.cell, &cell) {
                    if slot.restarts >= MAX_RESTARTS {
                        return Err(format!(
                            "{} exited {} times; /lsp shows its log",
                            self.specs[idx].id,
                            slot.restarts + 1
                        ));
                    }
                    slot.restarts += 1;
                    slot.cell = Arc::default();
                }
                slot.restarts
            };
            tokio::time::sleep(Duration::from_millis(250 * u64::from(restarts))).await;
        }
    }

    async fn start(&self, idx: usize, root: &Path) -> Result<Arc<Client>, String> {
        let spec = &self.specs[idx];
        let program = spec.program.clone()?;
        let mut cmd = (self.prepare)(spec, &program, &spec.file.args)?;
        cmd.current_dir(root).env_clear().envs(&spec.env);
        let file = spec.file.clone();
        let launch = Launch {
            id: spec.id.clone(),
            command: cmd,
            root: root.to_path_buf(),
            initialization_options: toml_to_json(&spec.file.initialization_options),
            settings: toml_to_json(&spec.file.settings),
            language_ids: Box::new(move |ext| file.language_id(ext)),
        };
        Client::start(launch, self.changed.clone()).await
    }

    fn bump(&self) {
        self.changed.send_modify(|n| *n += 1);
    }

    /// The file was read: start its server in the background and open the file there,
    /// so the server is warm by the time the file is edited.
    pub fn touch(self: &Arc<Self>, path: &Path) {
        if !self.covers(path) {
            return;
        }
        let this = self.clone();
        let path = path.to_path_buf();
        tokio::spawn(async move {
            if let Ok(Some(c)) = this.client(&path).await {
                let _ = c.sync(&path).await;
            }
        });
    }

    /// Diagnostics before an edit, to compare with afterwards. `None` when no server
    /// covers the file.
    pub async fn before_edit(&self, path: &Path) -> Option<Snapshot> {
        self.spec_for(path)?;
        let Some(c) = self.running(path) else {
            return Some(Snapshot::new());
        };
        // Pick up changes made outside the tools (shell commands, the user).
        let _ = c.sync(path).await;
        Some(c.snapshot())
    }

    /// After an edit: send the new text, wait for diagnostics to settle, and describe
    /// what is new compared with `before`.
    pub async fn after_edit(
        &self,
        path: &Path,
        before: &Snapshot,
        display: &(dyn Fn(&Path) -> String + Sync),
    ) -> Option<String> {
        let idx = self.spec_for(path)?;
        let d = &self.specs[idx].file.diagnostics;
        let max_wait = Duration::from_millis(d.max_wait_ms);
        let client = match tokio::time::timeout(max_wait, self.client(path)).await {
            Ok(Ok(Some(c))) => c,
            Ok(Err(e)) => return Some(format!("diagnostics unavailable: {e}")),
            Ok(Ok(None)) => return None,
            Err(_) => {
                return Some(format!(
                    "diagnostics unavailable: {} is still starting",
                    self.specs[idx].id
                ));
            }
        };
        client
            .changed(path, Duration::from_millis(d.settle_ms), max_wait)
            .await;
        let limits = Limits {
            min_severity: Severity::parse(&d.min_severity),
            max_items: d.max_items.max(1),
        };
        let mut text = diag::report(path, before, &client.snapshot(), limits, display);
        let busy = client.progress();
        if !busy.is_empty() {
            let note = format!(
                "(the language server is still busy: {}; more diagnostics may follow — \
                 the lsp tool's diagnostics operation shows them later)",
                busy.join(", ")
            );
            text = Some(match text {
                Some(t) => format!("{t}\n{note}"),
                None => note,
            });
        }
        text
    }

    /// For `/lsp`: each running client, then each server with none running.
    pub fn status(&self) -> Vec<Status> {
        let slots: Vec<(usize, PathBuf, Option<Started>, u32)> = self
            .slots
            .lock()
            .map(|s| {
                s.iter()
                    .map(|((i, r), slot)| (*i, r.clone(), slot.cell.get().cloned(), slot.restarts))
                    .collect()
            })
            .unwrap_or_default();
        let mut out = Vec::new();
        for (idx, spec) in self.specs.iter().enumerate() {
            let base = Status {
                id: spec.id.clone(),
                command: std::iter::once(spec.file.command.as_str())
                    .chain(spec.file.args.iter().map(String::as_str))
                    .collect::<Vec<_>>()
                    .join(" "),
                extensions: spec.file.extensions.clone(),
                state: String::new(),
                detail: spec.file.description.clone(),
                root: None,
                errors: 0,
                warnings: 0,
                log: Vec::new(),
            };
            let mine: Vec<_> = slots.iter().filter(|s| s.0 == idx).collect();
            if mine.is_empty() {
                out.push(Status {
                    state: if spec.installed() {
                        "idle"
                    } else {
                        "not installed"
                    }
                    .into(),
                    detail: match &spec.program {
                        Ok(p) => p.display().to_string(),
                        Err(e) => e.clone(),
                    },
                    ..base
                });
                continue;
            }
            for (_, root, cell, restarts) in mine {
                let mut s = Status {
                    root: Some(root.clone()),
                    ..base.clone()
                };
                match cell {
                    None => s.state = "starting".into(),
                    Some(Err(e)) => {
                        s.state = "failed".into();
                        s.detail.clone_from(e);
                    }
                    Some(Ok(c)) => {
                        let busy = c.progress();
                        s.state = if !c.alive() {
                            "exited"
                        } else if busy.is_empty() {
                            "ready"
                        } else {
                            "indexing"
                        }
                        .into();
                        if !busy.is_empty() {
                            s.detail = busy.join(", ");
                        } else if *restarts > 0 {
                            s.detail = format!("restarted {restarts} time(s)");
                        }
                        for list in c.snapshot().values() {
                            for d in list {
                                match d.severity {
                                    Severity::Error => s.errors += 1,
                                    Severity::Warning => s.warnings += 1,
                                    _ => {}
                                }
                            }
                        }
                        s.log = c.log();
                    }
                }
                out.push(s);
            }
        }
        out
    }

    /// Stop every server.
    pub async fn shutdown(&self) {
        let cells: Vec<Cell> = self
            .slots
            .lock()
            .map(|mut s| s.drain().map(|(_, slot)| slot.cell).collect())
            .unwrap_or_default();
        let stops = cells.into_iter().filter_map(|c| match c.get() {
            Some(Ok(client)) => Some(client.clone()),
            _ => None,
        });
        let stops: Vec<_> = stops.collect();
        let mut set = tokio::task::JoinSet::new();
        for c in stops {
            set.spawn(async move { c.stop().await });
        }
        while set.join_next().await.is_some() {}
        self.bump();
    }
}

fn has_marker(dir: &Path, markers: &[String]) -> bool {
    markers.iter().any(|m| {
        if m.contains('*') {
            std::fs::read_dir(dir).is_ok_and(|entries| {
                entries
                    .filter_map(Result::ok)
                    .any(|e| quack_config::glob_match(m, &e.file_name().to_string_lossy()))
            })
        } else {
            dir.join(m).exists()
        }
    })
}

fn toml_to_json(t: &toml::Table) -> Value {
    if t.is_empty() {
        return Value::Null;
    }
    serde_json::to_value(t).unwrap_or(Value::Null)
}
