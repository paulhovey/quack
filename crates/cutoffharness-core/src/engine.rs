use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cutoffharness_config::settings::PermissionMode;
use cutoffharness_config::{LayerKind, Loaded};
use cutoffharness_mcp::McpManager;
use cutoffharness_protocol::{
    EngineError, EngineErrorKind, Event, InputItem, Op, SessionId, SessionInfo, SessionSummary,
    TurnId, WriteScope,
};
use cutoffharness_providers::Message;
use cutoffharness_tools::Permissions;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::session::{self, Resume, SharedLog};
use crate::startup::{self, Setup, StartupOutcome};
use crate::turn::{self, Approvals, TurnCtx};
use cutoffharness_instructions::agents_md::Nested;
use cutoffharness_protocol::MemoryNote;
use cutoffharness_store::checkpoint::Checkpoints;
use cutoffharness_store::memory::Scope;

const EVENT_CAPACITY: usize = 4096;

/// Appended to user messages in plan mode when the profile has no `plan.md`.
const PLAN_PROMPT: &str = "[Plan mode] Do not change anything yet. Explore with read-only \
tools, then reply with a short numbered plan: which files to change, what to change in each, \
and how to check the result. The user will accept the plan or ask for changes.";
const OP_CAPACITY: usize = 64;

/// A client's connection to a running engine.
#[derive(Debug)]
pub struct EngineHandle {
    pub ops: mpsc::Sender<Op>,
    pub events: broadcast::Receiver<Event>,
    tx: broadcast::Sender<Event>,
    task: JoinHandle<()>,
}

impl EngineHandle {
    /// Another receiver for the same event stream (e.g. a logger next to the UI).
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }

    pub async fn submit(&self, op: Op) -> bool {
        self.ops.send(op).await.is_ok()
    }

    /// Ask the engine to stop and wait for it.
    pub async fn shutdown(self) {
        let _ = self.ops.send(Op::Shutdown).await;
        let _ = self.task.await;
    }
}

/// Work that waits for startup or the running turn.
#[derive(Debug)]
enum Queued {
    Turn(Vec<InputItem>),
    /// Resume a saved session, or start a new one with `None`.
    Switch(Option<SessionId>),
    /// `/compact`, with an optional focus.
    Compact(Option<String>),
}

/// The conversation once a model is ready.
#[derive(Debug)]
struct Conversation {
    setup: Arc<Setup>,
    perms: Arc<Mutex<Permissions>>,
    /// Nested AGENTS.md files already shown in this session.
    nested: Arc<Nested>,
    /// `None` while a turn task owns it.
    history: Option<Vec<Message>>,
}

/// Nested instruction tracking for a session: the startup chain, plus any nested file
/// already attached to a tool result in `history` (after a resume).
fn nested(config: &Loaded, setup: &Setup, history: &[Message]) -> Arc<Nested> {
    let n = Nested::new(
        &config.dirs.workspace_root,
        crate::env::rules(config),
        setup.instruction_dirs.iter().cloned(),
    );
    for m in history {
        if let Message::Tool { content, .. } = m {
            n.mark_shown(content);
        }
    }
    Arc::new(n)
}

#[derive(Debug)]
pub struct Engine {
    config: Arc<Loaded>,
    tx: broadcast::Sender<Event>,
    session: SessionInfo,
    started: bool,
    problem: Option<String>,
    convo: Option<Conversation>,
    next_turn: u64,
    queued: VecDeque<Queued>,
    running: Option<(TurnId, CancellationToken)>,
    approvals: Approvals,
    resume: Resume,
    /// Created at the first turn (no empty sessions) or reopened on resume.
    log: Option<SharedLog>,
    turns: u32,
    title: Option<String>,
    switch_tx: Option<mpsc::Sender<Result<Setup, String>>>,
    /// A mode set before startup finished, applied once a model is ready.
    pending_mode: Option<String>,
    /// Told to the model at the start of the next turn, e.g. that the user undid an edit.
    notes: Vec<String>,
    /// MCP servers, shared by every model the session switches to.
    mcp: Arc<crate::mcp::Mcp>,
}

impl Engine {
    /// Spawn the engine on the current tokio runtime. Startup runs in the background;
    /// the returned handle is subscribed before any event is sent, so nothing is missed.
    pub fn spawn(config: Arc<Loaded>) -> EngineHandle {
        Self::spawn_with(config, Resume::New)
    }

    /// Like [`spawn`](Self::spawn), resuming a saved session.
    pub fn spawn_with(config: Arc<Loaded>, resume: Resume) -> EngineHandle {
        let (tx, events) = broadcast::channel(EVENT_CAPACITY);
        let (ops, ops_rx) = mpsc::channel(OP_CAPACITY);
        let session = SessionInfo {
            id: new_session_id(),
            cwd: config.dirs.workspace_root.display().to_string(),
            model: Some(config.settings.model.default.clone()).filter(|m| !m.is_empty()),
            profile: "empty".into(),
        };
        let engine = Self {
            config,
            tx: tx.clone(),
            session,
            started: false,
            problem: None,
            convo: None,
            next_turn: 0,
            queued: VecDeque::new(),
            running: None,
            approvals: Approvals::default(),
            resume,
            log: None,
            turns: 0,
            title: None,
            switch_tx: None,
            pending_mode: None,
            notes: Vec::new(),
            mcp: crate::mcp::Mcp::new(McpManager::new(
                Vec::new(),
                PathBuf::new(),
                Default::default(),
            )),
        };
        let task = tokio::spawn(engine.run(ops_rx));
        EngineHandle {
            ops,
            events,
            tx,
            task,
        }
    }

    fn emit(&self, event: Event) {
        // No receivers just means no client is listening right now.
        let _ = self.tx.send(event);
    }

    async fn run(mut self, mut ops: mpsc::Receiver<Op>) {
        self.emit(Event::SessionConfigured {
            session: self.session.clone(),
        });
        self.start_mcp().await;
        let mut startup = tokio::spawn(startup::run(
            self.config.clone(),
            self.tx.clone(),
            self.mcp.clone(),
        ));
        let (done_tx, mut done_rx) = mpsc::channel::<(TurnId, Vec<Message>)>(8);
        let (switch_tx, mut switch_rx) = mpsc::channel::<Result<Setup, String>>(2);
        self.switch_tx = Some(switch_tx);

        loop {
            tokio::select! {
                outcome = &mut startup, if !self.started => {
                    self.started = true;
                    self.on_started(outcome.unwrap_or_default());
                    self.emit(Event::StartupComplete);
                    self.next_queued(&done_tx);
                }
                Some(result) = switch_rx.recv() => self.on_switched(result),
                Some((turn, history)) = done_rx.recv() => {
                    if let Some(c) = &mut self.convo {
                        c.history = Some(history);
                    }
                    self.sync_log();
                    if self.running.as_ref().is_some_and(|(t, _)| *t == turn) {
                        self.running = None;
                    }
                    self.next_queued(&done_tx);
                }
                op = ops.recv() => {
                    let Some(op) = op else { break };
                    if !self.handle(op, &done_tx) {
                        break;
                    }
                }
            }
        }
        if let Some((_, cancel)) = self.running.take() {
            cancel.cancel();
        }
        startup.abort();
        let _ = tokio::time::timeout(Duration::from_secs(3), self.mcp.manager.shutdown()).await;
        self.emit(Event::ShutdownComplete);
    }

    /// Read `mcp.d` and connect the servers in the background.
    async fn start_mcp(&mut self) {
        let config = self.config.clone();
        let (specs, warnings) = tokio::task::spawn_blocking(move || crate::mcp::load(&config))
            .await
            .unwrap_or_default();
        for w in warnings {
            self.warn(w);
        }
        let manager = McpManager::new(
            specs,
            self.config.dirs.workspace_root.clone(),
            self.config.settings.network.clone(),
        );
        manager.start();
        crate::mcp::report(manager.clone(), self.tx.clone());
        self.mcp = crate::mcp::Mcp::new(manager);
    }

    fn on_started(&mut self, outcome: StartupOutcome) {
        self.problem = outcome.problem;
        let Some(setup) = outcome.setup else {
            if self.resume != Resume::New {
                self.warn("cannot resume: no model is ready".into());
            }
            return;
        };
        self.session.model = Some(setup.model_ref.clone());
        self.session.profile = setup.profile.id.clone();
        let resume = std::mem::take(&mut self.resume);
        let history = self
            .open_saved(&resume, &setup.model_ref)
            .unwrap_or_else(|| {
                if resume != Resume::New {
                    self.warn("starting a new session".into());
                }
                Vec::new()
            });
        self.emit(Event::SessionConfigured {
            session: self.session.clone(),
        });
        self.convo = Some(Conversation {
            perms: Arc::new(Mutex::new(setup.permissions.clone())),
            nested: nested(&self.config, &setup, &history),
            setup: Arc::new(setup),
            history: Some(history),
        });
        match self.pending_mode.take() {
            Some(mode) => self.set_mode(&mode),
            None => self.emit_mode(),
        }
    }

    /// Load the requested session, if any, and reopen its log for appending. `None` when
    /// there is nothing to load or it failed (with a warning).
    fn open_saved(&mut self, resume: &Resume, model: &str) -> Option<Vec<Message>> {
        let entry = match session::find(&self.config, resume) {
            Ok(Some(e)) => e,
            Ok(None) => return None,
            Err(e) => {
                self.warn(format!("cannot resume: {e}"));
                return None;
            }
        };
        let ps = session::project_sessions(&self.config);
        let records = match ps.read(&entry) {
            Ok(r) => r,
            Err(e) => {
                self.warn(format!("cannot read session {}: {e}", entry.id));
                return None;
            }
        };
        let (history, items, turns) = session::replay(&records);
        self.log = None;
        match ps.reopen(entry.clone()) {
            Ok(log) => {
                let log = Arc::new(Mutex::new(log));
                session::record(
                    Some(&log),
                    "resume",
                    serde_json::json!({ "model": model, "version": env!("CARGO_PKG_VERSION") }),
                );
                self.log = Some(log);
            }
            Err(e) => self.warn(format!(
                "session log is read-only ({e}); new turns will not be saved"
            )),
        }
        self.session.id = SessionId(entry.id.clone());
        self.turns = turns;
        self.title = Some(entry.title.clone()).filter(|t| !t.is_empty());
        self.emit(Event::SessionResumed {
            session: self.session.clone(),
            items,
        });
        Some(history)
    }

    /// Replace the conversation with a saved session or an empty one, keeping the model.
    fn switch_session(&mut self, target: Option<SessionId>) {
        let Some(model) = self.convo.as_ref().map(|c| c.setup.model_ref.clone()) else {
            return self.warn("no model is ready; sessions open once one is".into());
        };
        if target.as_ref() == Some(&self.session.id) {
            return self.warn("already in this session".into());
        }
        let history = match target {
            Some(id) => {
                self.sync_log();
                match self.open_saved(&Resume::Id(id.0), &model) {
                    Some(h) => h,
                    None => return,
                }
            }
            None => {
                self.sync_log();
                self.log = None;
                self.session.id = new_session_id();
                self.turns = 0;
                self.title = None;
                Vec::new()
            }
        };
        if let Some(c) = &mut self.convo {
            // Session approvals belong to the session they were given in.
            c.perms = Arc::new(Mutex::new(c.setup.permissions.clone()));
            c.nested = nested(&self.config, &c.setup, &history);
            c.history = Some(history);
        }
        self.emit_mode();
        self.emit(Event::SessionConfigured {
            session: self.session.clone(),
        });
    }

    fn list_sessions(&self) {
        let now = cutoffharness_store::time::unix_now();
        let sessions = session::project_sessions(&self.config)
            .list()
            .into_iter()
            .map(|e| SessionSummary {
                current: e.id == self.session.id.0,
                id: SessionId(e.id),
                title: e.title,
                model: e.model,
                turns: e.turns,
                age: cutoffharness_store::time::age(e.updated, now),
                started: cutoffharness_store::time::date(e.started),
            })
            .collect();
        self.emit(Event::SessionList { sessions });
    }

    fn set_mode(&mut self, mode: &str) {
        let Some(c) = &self.convo else {
            return self.warn("no model is ready; the mode applies once one is".into());
        };
        let parsed = match mode {
            "plan" => None,
            m => match serde_json::from_value::<PermissionMode>(serde_json::Value::from(m)) {
                Ok(p) => Some(p),
                Err(_) => {
                    return self.warn(format!(
                        "unknown mode `{m}`; use read-only, ask, auto-edit, full-auto or plan"
                    ));
                }
            },
        };
        if let Ok(mut p) = c.perms.lock() {
            match parsed {
                None => p.plan = true,
                Some(m) => {
                    let capped = self
                        .config
                        .settings
                        .policy
                        .max_permission_mode
                        .map_or(m, |cap| m.min(cap));
                    if capped != m {
                        self.warn(format!(
                            "{} is not allowed by policy; using {}",
                            m.label(),
                            capped.label()
                        ));
                    }
                    p.mode = capped;
                    p.plan = false;
                }
            }
        }
        self.emit_mode();
    }

    fn emit_mode(&self) {
        let Some(mode) = self.convo.as_ref().and_then(|c| {
            c.perms.lock().ok().map(|p| {
                if p.plan {
                    "plan".to_owned()
                } else {
                    p.mode.label().to_owned()
                }
            })
        }) else {
            return;
        };
        self.emit(Event::PermissionMode { mode });
    }

    fn checkpoints(&self) -> Option<Arc<Checkpoints>> {
        let dir = self.log.as_ref()?.lock().ok()?.dir().join("checkpoints");
        Some(Arc::new(Checkpoints::open(dir)))
    }

    fn undo(&mut self, force: bool) {
        if self.running.is_some() {
            return self.warn("wait for the current turn to finish before /undo".into());
        }
        let Some(cp) = self.checkpoints() else {
            return self.warn("nothing to undo in this session".into());
        };
        let last = cp.last_call();
        if last.is_empty() {
            return self.warn(
                "nothing to undo: only changes made with write and edit are tracked, not shell commands"
                    .into(),
            );
        }
        let root = &self.config.dirs.workspace_root;
        let show = |p: &PathBuf| {
            p.strip_prefix(root)
                .unwrap_or(p)
                .display()
                .to_string()
                .replace('\\', "/")
        };
        match cp.restore(&last, force) {
            Ok(r) => {
                let restored: Vec<String> = r.restored.iter().map(show).collect();
                if !restored.is_empty() {
                    self.notes.push(format!(
                        "[The user undid your last change to {}; the file is back to how it was before that change.]",
                        restored.join(", ")
                    ));
                }
                self.emit(Event::FilesRestored {
                    restored,
                    conflicts: r.conflicts.iter().map(show).collect(),
                });
            }
            Err(e) => self.warn(format!("undo failed: {e}")),
        }
    }

    fn list_memory(&self) {
        let store = crate::env::memory_store(&self.config);
        let mut notes = Vec::new();
        for scope in [Scope::Project, Scope::Global] {
            for (name, description) in store.list(scope) {
                notes.push(MemoryNote {
                    scope: scope.label().into(),
                    path: store
                        .dir(scope)
                        .join("notes")
                        .join(format!("{name}.md"))
                        .display()
                        .to_string(),
                    text: store.read(scope, &name).unwrap_or_default(),
                    name,
                    description,
                });
            }
        }
        self.emit(Event::MemoryNotes { notes });
    }

    fn save_setting(&self, key: &str, value: &str, scope: &WriteScope) {
        let layer = match scope {
            WriteScope::User => LayerKind::User,
            WriteScope::Project => LayerKind::Project,
            WriteScope::Session => {
                return self.warn("settings are saved to the user or project layer".into());
            }
        };
        match self.config.save_setting(layer, key, value) {
            Ok((path, note)) => self.emit(Event::SettingSaved {
                key: key.to_owned(),
                value: value.to_owned(),
                path: path.display().to_string(),
                note,
            }),
            Err(message) => self.emit(Event::Error {
                error: EngineError {
                    kind: EngineErrorKind::Config,
                    message: format!("cannot save {key}: {message}"),
                },
            }),
        }
    }

    /// `/setup`: write the provider file, then switch to the model like `/models` does.
    fn add_provider(&mut self, id: &str, base_url: &str, api: Option<&str>, model: String) {
        if self.running.is_some() {
            return self
                .warn("wait for the current turn to finish before adding a provider".into());
        }
        let comment = "Found on this machine by setup.";
        let added = match crate::setup::add_provider(&self.config, id, base_url, api, comment) {
            Ok(a) => a,
            Err(message) => {
                return self.emit(Event::Error {
                    error: EngineError {
                        kind: EngineErrorKind::Config,
                        message: format!("cannot add the provider: {message}"),
                    },
                });
            }
        };
        self.emit(Event::ProviderAdded {
            id: added.id.clone(),
            path: added.path.map(|p| p.display().to_string()),
        });
        let reference = format!("{}/{model}", added.id);
        match crate::setup::default_model_if_unset(&self.config, &reference) {
            Ok(Some(path)) => self.emit(Event::SettingSaved {
                key: "model.default".into(),
                value: serde_json::Value::from(reference).to_string(),
                path: path.display().to_string(),
                note: None,
            }),
            Ok(None) => {}
            Err(e) => self.warn(format!("cannot save the default model: {e}")),
        }
        if let Some(done) = self.switch_tx.clone() {
            let config = self.config.clone();
            let tx = self.tx.clone();
            let mcp = self.mcp.clone();
            tokio::spawn(async move {
                let r = startup::switch(config, tx, mcp, added.id, model).await;
                let _ = done.send(r).await;
            });
        }
    }

    fn on_switched(&mut self, result: Result<Setup, String>) {
        let setup = match result {
            Ok(s) => s,
            Err(e) => {
                self.emit(Event::Error {
                    error: EngineError {
                        kind: EngineErrorKind::Provider,
                        message: format!("cannot switch model: {e}"),
                    },
                });
                return;
            }
        };
        self.session.model = Some(setup.model_ref.clone());
        self.session.profile = setup.profile.id.clone();
        session::record(
            self.log.as_ref(),
            "model_switch",
            serde_json::json!({ "to": setup.model_ref, "profile": setup.profile.id }),
        );
        match &mut self.convo {
            // Keep the history and the session's approvals.
            Some(c) => c.setup = Arc::new(setup),
            None => {
                self.convo = Some(Conversation {
                    perms: Arc::new(Mutex::new(setup.permissions.clone())),
                    nested: nested(&self.config, &setup, &[]),
                    setup: Arc::new(setup),
                    history: Some(Vec::new()),
                });
                self.problem = None;
            }
        }
        self.emit(Event::SessionConfigured {
            session: self.session.clone(),
        });
    }

    fn warn(&self, message: String) {
        self.emit(Event::Warning { message });
    }

    /// Create the session log on the first turn.
    fn ensure_log(&mut self) {
        if self.log.is_some() {
            return;
        }
        let ps = session::project_sessions(&self.config);
        let model = self.session.model.clone().unwrap_or_default();
        match std::fs::create_dir_all(&ps.dir).and_then(|()| ps.create(&self.session.id.0, &model))
        {
            Ok(log) => {
                let log = Arc::new(Mutex::new(log));
                session::record(
                    Some(&log),
                    "session_start",
                    serde_json::json!({
                        "version": env!("CARGO_PKG_VERSION"),
                        "os": std::env::consts::OS,
                        "cwd": self.session.cwd,
                        "model": model,
                        "profile": self.session.profile,
                    }),
                );
                self.log = Some(log);
            }
            Err(e) => self.warn(format!(
                "cannot save this session ({e}); continuing without a log"
            )),
        }
    }

    fn sync_log(&mut self) {
        if let Some(log) = &self.log
            && let Ok(mut l) = log.lock()
            && let Err(e) = l.sync(self.title.as_deref(), self.turns)
        {
            tracing::warn!(error = %e, "could not sync session log");
        }
    }

    /// Returns false when the engine should stop.
    fn handle(&mut self, op: Op, done: &mpsc::Sender<(TurnId, Vec<Message>)>) -> bool {
        match op {
            Op::UserTurn { items, .. } => {
                if !self.started {
                    self.emit(Event::TurnQueued {
                        waiting_for: "startup".into(),
                    });
                    self.queued.push_back(Queued::Turn(items));
                } else if self.running.is_some() {
                    self.emit(Event::TurnQueued {
                        waiting_for: "the current turn".into(),
                    });
                    self.queued.push_back(Queued::Turn(items));
                } else {
                    self.start_turn(items, done.clone());
                }
            }
            Op::Interrupt => {
                if let Some((_, cancel)) = &self.running {
                    cancel.cancel();
                }
                self.queued.clear();
            }
            Op::ApprovalResponse { call_id, decision } => {
                let waiting = self
                    .approvals
                    .lock()
                    .ok()
                    .and_then(|mut p| p.remove(&call_id.0));
                match waiting {
                    Some(tx) => {
                        let _ = tx.send(decision);
                    }
                    None => self.emit(Event::Warning {
                        message: format!("no pending approval for call {}", call_id.0),
                    }),
                }
            }
            Op::ListModels => {
                let config = self.config.clone();
                let tx = self.tx.clone();
                let current = self.session.model.clone();
                tokio::spawn(async move {
                    let providers = crate::models::catalog(config).await;
                    let _ = tx.send(Event::ModelCatalog { providers, current });
                });
            }
            Op::SetModel { provider, model } => {
                if self.running.is_some() {
                    self.warn("wait for the current turn to finish before switching models".into());
                } else if let Some(done) = self.switch_tx.clone() {
                    let config = self.config.clone();
                    let tx = self.tx.clone();
                    let mcp = self.mcp.clone();
                    tokio::spawn(async move {
                        let r = startup::switch(config, tx, mcp, provider.0, model.0).await;
                        let _ = done.send(r).await;
                    });
                }
            }
            // A client may send these the moment it sees `TurnComplete`, before the turn
            // task has handed the history back, so they wait in the queue like turns do.
            Op::ResumeSession { id } => self.queue_switch(Some(id), done),
            Op::NewSession => self.queue_switch(None, done),
            Op::Compact { focus } => {
                if !self.started || self.running.is_some() {
                    self.emit(Event::TurnQueued {
                        waiting_for: if self.started {
                            "the current turn".into()
                        } else {
                            "startup".into()
                        },
                    });
                }
                self.queued.push_back(Queued::Compact(focus));
                self.next_queued(done);
            }
            Op::ListSessions => self.list_sessions(),
            Op::ListMemory => self.list_memory(),
            Op::Undo { force, .. } => self.undo(force),
            Op::RunDoctor => {
                let config = self.config.clone();
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let checks = crate::doctor::run(config).await;
                    let _ = tx.send(Event::DoctorReport { checks });
                });
            }
            Op::ListMcp => self.emit(Event::McpServers {
                servers: self
                    .mcp
                    .info(self.convo.as_ref().and_then(|c| c.setup.budget)),
            }),
            Op::RestartMcp { id } => {
                let mcp = self.mcp.clone();
                let tx = self.tx.clone();
                let budget = self.convo.as_ref().and_then(|c| c.setup.budget);
                tokio::spawn(async move {
                    if let Err(message) = mcp.manager.restart(&id).await {
                        let _ = tx.send(Event::Warning { message });
                    }
                    let _ = tx.send(Event::McpServers {
                        servers: mcp.info(budget),
                    });
                });
            }
            Op::DetectServers => {
                let config = self.config.clone();
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let servers = crate::setup::detect(config).await;
                    let _ = tx.send(Event::ServersDetected { servers });
                });
            }
            Op::AddProvider {
                id,
                base_url,
                api,
                model,
            } => self.add_provider(&id, &base_url, api.as_deref(), model),
            Op::SetPermissionMode { mode } if !self.started => self.pending_mode = Some(mode),
            Op::SetPermissionMode { mode } => self.set_mode(&mode),
            Op::DeleteMemory { scope, name } => {
                let store = crate::env::memory_store(&self.config);
                let result = Scope::parse(&scope)
                    .ok_or_else(|| format!("unknown scope `{scope}`"))
                    .and_then(|s| store.delete(s, &name));
                match result {
                    Ok(()) => self.list_memory(),
                    Err(e) => self.warn(format!("cannot delete note: {e}")),
                }
            }
            Op::GetContext => match &self.convo {
                Some(c) => self.emit(Event::ContextReport {
                    report: crate::context::report(&c.setup, c.history.as_deref()),
                }),
                None => self.warn("no model is ready, so there is no context yet".into()),
            },
            Op::SetTitle { title } => {
                let title = title.trim().to_owned();
                session::record(
                    self.log.as_ref(),
                    "title",
                    serde_json::json!({ "title": title }),
                );
                self.title = Some(title).filter(|t| !t.is_empty());
                self.sync_log();
            }
            Op::SaveSetting { key, value, scope } => self.save_setting(&key, &value, &scope),
            Op::Shutdown => return false,
            other => self.emit(Event::Warning {
                message: format!("{} is not implemented yet", op_name(&other)),
            }),
        }
        true
    }

    fn queue_switch(
        &mut self,
        target: Option<SessionId>,
        done: &mpsc::Sender<(TurnId, Vec<Message>)>,
    ) {
        self.queued.push_back(Queued::Switch(target));
        self.next_queued(done);
    }

    fn next_queued(&mut self, done: &mpsc::Sender<(TurnId, Vec<Message>)>) {
        while self.started && self.running.is_none() {
            match self.queued.pop_front() {
                Some(Queued::Turn(items)) => self.start_turn(items, done.clone()),
                Some(Queued::Switch(target)) => self.switch_session(target),
                Some(Queued::Compact(focus)) => self.start_compact(focus, done.clone()),
                None => break,
            }
        }
    }

    /// Run `/compact` like a turn: it owns the history until it is done and Esc stops it.
    fn start_compact(&mut self, focus: Option<String>, done: mpsc::Sender<(TurnId, Vec<Message>)>) {
        let Some(convo) = &mut self.convo else {
            return self.warn("no model is ready to write a summary".into());
        };
        let Some(mut history) = convo.history.take() else {
            return;
        };
        self.next_turn += 1;
        let turn = TurnId(self.next_turn);
        let cancel = CancellationToken::new();
        self.running = Some((turn, cancel.clone()));
        let ctx = TurnCtx {
            turn,
            setup: convo.setup.clone(),
            perms: convo.perms.clone(),
            nested: convo.nested.clone(),
            approvals: self.approvals.clone(),
            tx: self.tx.clone(),
            cancel,
            log: self.log.clone(),
            checkpoints: None,
        };
        tokio::spawn(async move {
            ctx.emit(Event::TurnStarted { turn });
            let result =
                crate::context::compact_now(&ctx, &mut history, 1, focus.as_deref(), "/compact")
                    .await;
            ctx.emit(match result {
                Ok(()) => Event::TurnComplete {
                    turn,
                    usage: Default::default(),
                },
                Err(reason) => Event::TurnAborted { turn, reason },
            });
            let _ = done.send((turn, history)).await;
        });
    }

    fn start_turn(&mut self, items: Vec<InputItem>, done: mpsc::Sender<(TurnId, Vec<Message>)>) {
        self.next_turn += 1;
        let turn = TurnId(self.next_turn);
        let prompt = items
            .iter()
            .filter_map(|i| match i {
                InputItem::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");

        let Some(convo) = &mut self.convo else {
            let message = match &self.problem {
                Some(p) => format!("No model is ready: {p}"),
                None => "No model is ready.".to_owned(),
            };
            self.emit(Event::TurnStarted { turn });
            self.emit(Event::Error {
                error: EngineError {
                    kind: EngineErrorKind::Config,
                    message,
                },
            });
            self.emit(Event::TurnAborted {
                turn,
                reason: "no model".into(),
            });
            return;
        };
        let Some(history) = convo.history.take() else {
            return;
        };
        let setup = convo.setup.clone();
        let perms = convo.perms.clone();
        let nested = convo.nested.clone();
        let typed = prompt.clone();
        let (prompt, skill_error) = expand_skill(&setup, prompt);
        let prompt = if self.notes.is_empty() {
            prompt
        } else {
            format!("{}\n\n{prompt}", std::mem::take(&mut self.notes).join("\n"))
        };
        if let Some(e) = skill_error {
            self.warn(e);
        }
        // Plan mode asks for a plan at the end of the message, after the cached prefix.
        let prompt = if perms.lock().is_ok_and(|p| p.plan) {
            let note = setup.profile.plan_prompt.as_deref().unwrap_or(PLAN_PROMPT);
            format!("{prompt}\n\n{note}")
        } else {
            prompt
        };
        self.ensure_log();
        self.turns += 1;
        if self.title.is_none() {
            let first: String = typed
                .lines()
                .next()
                .unwrap_or_default()
                .chars()
                .take(80)
                .collect();
            self.title = Some(first);
        }
        let cancel = CancellationToken::new();
        self.running = Some((turn, cancel.clone()));
        let ctx = TurnCtx {
            turn,
            setup,
            perms,
            nested,
            approvals: self.approvals.clone(),
            tx: self.tx.clone(),
            cancel,
            log: self.log.clone(),
            checkpoints: self.checkpoints(),
        };
        tokio::spawn(async move {
            let history = turn::run(ctx, history, prompt).await;
            let _ = done.send((turn, history)).await;
        });
    }
}

/// `/<skill> [args]` becomes the skill's instructions followed by the request. Anything
/// else (including unknown `/words`) is sent as typed.
fn expand_skill(setup: &Setup, prompt: String) -> (String, Option<String>) {
    let Some(rest) = prompt.strip_prefix('/') else {
        return (prompt, None);
    };
    let (name, args) = rest
        .split_once(char::is_whitespace)
        .map_or((rest, ""), |(n, a)| (n, a.trim()));
    let Some(skill) = setup.tool_ctx.skills.iter().find(|s| s.name == name) else {
        return (prompt, None);
    };
    match cutoffharness_tools::expand_skill(skill) {
        Ok(text) => {
            let request = if args.is_empty() {
                "Use this skill now."
            } else {
                args
            };
            (format!("{text}\n\n{request}"), None)
        }
        Err(e) => {
            let message = format!("skill /{name}: {e}");
            (prompt, Some(message))
        }
    }
}

fn op_name(op: &Op) -> &'static str {
    match op {
        Op::UserTurn { .. } => "user_turn",
        Op::Interrupt => "interrupt",
        Op::ApprovalResponse { .. } => "approval_response",
        Op::SetModel { .. } => "set_model",
        Op::ListModels => "list_models",
        Op::Compact { .. } => "compact",
        Op::Undo { .. } => "undo",
        Op::ResumeSession { .. } => "resume_session",
        Op::NewSession => "new_session",
        Op::ListSessions => "list_sessions",
        Op::GetContext => "get_context",
        Op::ListMemory => "list_memory",
        Op::RunDoctor => "run_doctor",
        Op::DetectServers => "detect_servers",
        Op::ListMcp => "list_mcp",
        Op::RestartMcp { .. } => "restart_mcp",
        Op::AddProvider { .. } => "add_provider",
        Op::SetPermissionMode { .. } => "set_permission_mode",
        Op::DeleteMemory { .. } => "delete_memory",
        Op::SetTitle { .. } => "set_title",
        Op::SaveSetting { .. } => "save_setting",
        Op::ForkSession { .. } => "fork_session",
        Op::ReloadConfig => "reload_config",
        Op::Shutdown => "shutdown",
    }
}

/// Time-ordered id. Replaced by UUID v7 when `cutoffharness-store` lands.
fn new_session_id() -> SessionId {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    SessionId(format!("{nanos:032x}"))
}
