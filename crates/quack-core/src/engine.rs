use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use quack_config::hooks::HookEvent;
use quack_config::settings::PermissionMode;
use quack_config::{LayerKind, Loaded};
use quack_lsp::LspManager;
use quack_mcp::McpManager;
use quack_protocol::{
    EngineError, EngineErrorKind, Event, InputItem, InstructionFile, Op, SessionId, SessionInfo,
    SessionSummary, TurnId, WriteScope,
};
use quack_providers::Message;
use quack_tools::Permissions;
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::handoff;
use crate::hooks::{Hooks, Sink, Target};
use crate::session::{self, Resume, SharedLog};
use crate::startup::{self, Setup, StartupOutcome};
use crate::turn::{self, Approvals, Questions, TurnCtx};
use quack_instructions::agents_md::Nested;
use quack_protocol::MemoryNote;
use quack_store::checkpoint::Checkpoints;
use quack_store::memory::Scope;

const EVENT_CAPACITY: usize = 4096;

/// Appended to user messages in plan mode when the profile has no `plan.md`.
const PLAN_PROMPT: &str = "[Plan mode] Do not change anything yet. Explore with read-only \
tools first. If a decision would change the plan in a big way (scope, approach, a trade-off, \
an unclear requirement), ask the user with ask_user: put the questions in one call, give \
options with your recommendation first, and don't ask what the code or docs can answer. Ask \
only a few, and none when the task is clear. Then record the plan with the todo tool (concrete \
steps, all pending) and reply with a short summary: the decisions made, which files change, \
and how to check the result. The user will accept the plan or ask for changes.";
const OP_CAPACITY: usize = 64;

/// A client's connection to a running engine.
#[derive(Debug)]
pub struct EngineHandle {
    pub ops: mpsc::Sender<Op>,
    pub events: broadcast::Receiver<Event>,
    tx: broadcast::Sender<Event>,
    config: watch::Receiver<Arc<Loaded>>,
    task: JoinHandle<()>,
}

impl EngineHandle {
    /// Another receiver for the same event stream (e.g. a logger next to the UI).
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }

    /// The config in use; it changes after `Event::ConfigReloaded`.
    pub fn config(&self) -> Arc<Loaded> {
        self.config.borrow().clone()
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
    /// `/reload`, or a trust decision.
    Reload,
    /// `/plan accept`, into this mode.
    Accept(String),
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
    /// The session's own setup, while plan mode uses the `[model] planner`'s.
    editor: Option<Arc<Setup>>,
    /// History length at the planner's first turn: its messages come after it.
    plan_start: Option<usize>,
}

/// Which slot a model switch fills: the session's model, or the planner in plan mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Session,
    Planner,
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
    config_tx: watch::Sender<Arc<Loaded>>,
    /// A reload is reading files or rebuilding the session; turns wait for it.
    reloading: bool,
    /// The planner's setup is being built; turns wait for it.
    switching: bool,
    tx: broadcast::Sender<Event>,
    session: SessionInfo,
    started: bool,
    problem: Option<String>,
    convo: Option<Conversation>,
    next_turn: u64,
    queued: VecDeque<Queued>,
    running: Option<(TurnId, CancellationToken)>,
    approvals: Approvals,
    questions: Questions,
    resume: Resume,
    /// Created at the first turn (no empty sessions) or reopened on resume.
    log: Option<SharedLog>,
    turns: u32,
    title: Option<String>,
    switch_tx: Option<mpsc::Sender<(Role, Result<Setup, String>)>>,
    /// A mode set before startup finished, applied once a model is ready.
    pending_mode: Option<String>,
    /// A `/sandbox` setting sent before startup finished.
    pending_sandbox: Option<bool>,
    /// Told to the model at the start of the next turn, e.g. that the user undid an edit.
    notes: Vec<String>,
    /// MCP servers, shared by every model the session switches to.
    mcp: Arc<crate::mcp::Mcp>,
    /// Language servers, likewise.
    lsp: Arc<LspManager>,
    /// This session's usage, for `/stats`.
    stats: Arc<Mutex<crate::stats::Stats>>,
    /// `hooks.d`, read at startup and on `/reload`.
    hooks: Arc<Hooks>,
    /// The `session_start` hooks running, for the session's first prompt.
    start_context: Option<JoinHandle<Vec<String>>>,
    /// Sub-agent setups for the current config (`pool()`).
    pool: Option<Arc<crate::agent::Pool>>,
    /// Sub-agent runs in this session, for their ids.
    agent_runs: Arc<std::sync::atomic::AtomicU64>,
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
        let (config_tx, config_rx) = watch::channel(config.clone());
        let session = SessionInfo {
            id: new_session_id(),
            cwd: config.dirs.workspace_root.display().to_string(),
            model: Some(config.settings.model.default.clone()).filter(|m| !m.is_empty()),
            profile: "empty".into(),
        };
        let engine = Self {
            config,
            config_tx,
            reloading: false,
            switching: false,
            tx: tx.clone(),
            session,
            started: false,
            problem: None,
            convo: None,
            next_turn: 0,
            queued: VecDeque::new(),
            running: None,
            approvals: Approvals::default(),
            questions: Questions::default(),
            resume,
            log: None,
            turns: 0,
            title: None,
            switch_tx: None,
            pending_mode: None,
            pending_sandbox: None,
            notes: Vec::new(),
            stats: Arc::default(),
            hooks: Arc::default(),
            start_context: None,
            pool: None,
            agent_runs: Arc::default(),
            mcp: crate::mcp::Mcp::new(McpManager::new(
                Vec::new(),
                PathBuf::new(),
                Default::default(),
            )),
            lsp: LspManager::empty(),
        };
        let task = tokio::spawn(engine.run(ops_rx));
        EngineHandle {
            ops,
            events,
            tx,
            config: config_rx,
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
        self.start_lsp().await;
        self.load_hooks().await;
        let mut startup = tokio::spawn(startup::run(
            self.config.clone(),
            self.tx.clone(),
            self.mcp.clone(),
            self.lsp.clone(),
            None,
        ));
        let (done_tx, mut done_rx) = mpsc::channel::<(TurnId, Vec<Message>)>(8);
        let (switch_tx, mut switch_rx) = mpsc::channel::<(Role, Result<Setup, String>)>(2);
        self.switch_tx = Some(switch_tx);
        let (reload_tx, mut reload_rx) = mpsc::channel::<Reload>(2);

        loop {
            tokio::select! {
                outcome = &mut startup, if !self.started => {
                    self.started = true;
                    self.on_started(outcome.unwrap_or_default());
                    self.emit(Event::StartupComplete);
                    self.next_queued(&done_tx, &reload_tx);
                }
                Some((role, result)) = switch_rx.recv() => {
                    match role {
                        Role::Session => self.on_switched(result),
                        Role::Planner => self.on_planner(result),
                    }
                    self.next_queued(&done_tx, &reload_tx);
                }
                Some(step) = reload_rx.recv() => {
                    match step {
                        Reload::Read(result) => self.on_reread(result, &reload_tx).await,
                        Reload::Ready(outcome) => {
                            self.on_reloaded(*outcome);
                            self.reloading = false;
                            self.emit(Event::StartupComplete);
                        }
                    }
                    self.next_queued(&done_tx, &reload_tx);
                }
                Some((turn, history)) = done_rx.recv() => {
                    if let Some(c) = &mut self.convo {
                        c.history = Some(history);
                    }
                    self.sync_log();
                    if self.running.as_ref().is_some_and(|(t, _)| *t == turn) {
                        self.running = None;
                    }
                    self.next_queued(&done_tx, &reload_tx);
                }
                op = ops.recv() => {
                    let Some(op) = op else { break };
                    if !self.handle(op, &done_tx, &reload_tx) {
                        break;
                    }
                }
            }
        }
        if let Some((_, cancel)) = self.running.take() {
            cancel.cancel();
        }
        startup.abort();
        if let Some(end) = self.session_hooks(HookEvent::SessionEnd, "quit") {
            let _ = end.await;
        }
        let _ = tokio::time::timeout(
            Duration::from_secs(3),
            futures::future::join(self.mcp.manager.shutdown(), self.lsp.shutdown()),
        )
        .await;
        self.emit(Event::ShutdownComplete);
    }

    /// Read `hooks.d`.
    async fn load_hooks(&mut self) {
        let config = self.config.clone();
        let (hooks, warnings) = tokio::task::spawn_blocking(move || crate::hooks::load(&config))
            .await
            .unwrap_or_default();
        for w in warnings {
            self.warn(w);
        }
        self.hooks = Arc::new(hooks);
    }

    /// Start the `session_start` or `session_end` hooks in the background. `source` says
    /// why: `startup`, `resume`, `new`, `quit` or `switch`.
    fn session_hooks(&self, event: HookEvent, source: &str) -> Option<JoinHandle<Vec<String>>> {
        if !self.hooks.any(event) {
            return None;
        }
        let (hooks, tx) = (self.hooks.clone(), self.tx.clone());
        let session = self.session.id.0.clone();
        let fields = serde_json::json!({ "source": source });
        Some(tokio::spawn(async move {
            let emit = move |e: Event| {
                let _ = tx.send(e);
            };
            let sink = Sink {
                emit: &emit,
                log: None,
                session: &session,
            };
            let cancel = CancellationToken::new();
            hooks
                .run(event, fields, Target::None, &sink, &cancel)
                .await
                .context
        }))
    }

    /// Read `lsp.d` and find the servers' programs; they start when a file is opened.
    async fn start_lsp(&mut self) {
        let config = self.config.clone();
        let (specs, warnings) = tokio::task::spawn_blocking(move || crate::lsp::load(&config))
            .await
            .unwrap_or_default();
        for w in warnings {
            self.warn(w);
        }
        self.lsp = crate::lsp::manager(&self.config, specs);
        crate::lsp::report(self.lsp.clone(), self.tx.clone());
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
        let manager = McpManager::with_sandbox(
            specs,
            self.config.dirs.workspace_root.clone(),
            self.config.settings.network.clone(),
            crate::sandbox::server_policy(&self.config),
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
        let resumed = resume != Resume::New;
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
            editor: None,
            plan_start: None,
        });
        match self.pending_mode.take() {
            Some(mode) => self.set_mode(&mode),
            None => self.emit_mode(),
        }
        if let Some(off) = self.pending_sandbox.take() {
            self.set_sandbox(off);
        }
        let source = if resumed && self.log.is_some() {
            "resume"
        } else {
            "startup"
        };
        self.start_context = self.session_hooks(HookEvent::SessionStart, source);
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
        self.stats = Arc::new(Mutex::new(crate::stats::Stats::from_log(&records)));
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
        // The new session starts outside plan mode, on the session's own model.
        self.restore_editor();
        let Some(model) = self.convo.as_ref().map(|c| c.setup.model_ref.clone()) else {
            return self.warn("no model is ready; sessions open once one is".into());
        };
        if target.as_ref() == Some(&self.session.id) {
            return self.warn("already in this session".into());
        }
        let source = if target.is_some() { "resume" } else { "new" };
        if let Some(end) = self.session_hooks(HookEvent::SessionEnd, "switch") {
            drop(end); // runs on; nothing waits for it
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
                self.stats = Arc::default();
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
        self.start_context = self.session_hooks(HookEvent::SessionStart, source);
    }

    fn list_sessions(&self) {
        let now = quack_store::time::unix_now();
        let sessions = session::project_sessions(&self.config)
            .list()
            .into_iter()
            .map(|e| SessionSummary {
                current: e.id == self.session.id.0,
                id: SessionId(e.id),
                title: e.title,
                model: e.model,
                turns: e.turns,
                age: quack_store::time::age(e.updated, now),
                started: quack_store::time::date(e.started),
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
                        "unknown mode `{m}`; use read-only, ask, auto-edit, full-auto, yolo or plan"
                    ));
                }
            },
        };
        let was_plan = c.perms.lock().is_ok_and(|p| p.plan);
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
        match (was_plan, parsed.is_none()) {
            (false, true) => self.start_planner(),
            (true, false) => {
                self.restore_editor();
            }
            _ => {}
        }
        self.emit_mode();
    }

    fn sandbox_locked(&self) -> bool {
        let locked = &self.config.settings.policy.locked;
        locked.iter().any(|k| k == "sandbox" || k == "sandbox.mode")
    }

    /// `/sandbox on|off`: run shell commands without the OS sandbox this session, whatever
    /// the permission mode, or go back to `[sandbox] mode`.
    fn set_sandbox(&mut self, off: bool) {
        let Some(c) = &self.convo else {
            return self.warn("no model is ready; the sandbox setting applies once one is".into());
        };
        if off && self.sandbox_locked() {
            self.warn("`sandbox.mode` is locked by the system policy; the sandbox stays on".into());
            return self.emit(Event::Sandbox { off: false });
        }
        if let Ok(mut p) = c.perms.lock() {
            p.sandbox_off = off;
        }
        if !off {
            match &c.setup.tool_ctx.sandbox {
                quack_tools::SandboxState::On(_) => {}
                quack_tools::SandboxState::Off => self.warn(format!(
                    "[sandbox] mode = \"{}\", so commands still run without a sandbox · set it in config.toml",
                    self.config.settings.sandbox.mode.label()
                )),
                quack_tools::SandboxState::Unavailable(why) => self.warn(format!(
                    "the sandbox is not available ({why}); every shell command asks first"
                )),
            }
        }
        self.emit(Event::Sandbox { off });
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

    /// The user messages still in the history, most recent first, with the tool calls
    /// made after each.
    fn rewind_points(&self) -> Vec<Point> {
        let Some(history) = self.convo.as_ref().and_then(|c| c.history.as_deref()) else {
            return Vec::new();
        };
        let changes = self
            .checkpoints()
            .map(|cp| cp.changes())
            .unwrap_or_default();
        let root = &self.config.dirs.workspace_root;
        let mut points = Vec::new();
        let mut calls: Vec<String> = Vec::new();
        for (index, m) in history.iter().enumerate().rev() {
            match m {
                Message::Assistant { tool_calls, .. } => {
                    calls.extend(tool_calls.iter().map(|c| c.id.clone()));
                }
                Message::User { content }
                    if !content.starts_with(quack_context::compact::SUMMARY_MARKER) =>
                {
                    let mut files: Vec<String> = Vec::new();
                    for c in changes.iter().filter(|c| calls.contains(&c.call_id)) {
                        let shown = c
                            .path
                            .strip_prefix(root)
                            .unwrap_or(&c.path)
                            .display()
                            .to_string()
                            .replace('\\', "/");
                        if !files.contains(&shown) {
                            files.push(shown);
                        }
                    }
                    points.push(Point {
                        point: quack_protocol::RewindPoint {
                            back: u32::try_from(points.len() + 1).unwrap_or(u32::MAX),
                            text: content.clone(),
                            files,
                        },
                        index,
                        calls: calls.clone(),
                    });
                }
                _ => {}
            }
        }
        points
    }

    /// `/rewind`: put files back and/or cut the conversation back to before a user message.
    /// When files changed again since, nothing happens without `force`.
    fn rewind(&mut self, back: u32, conversation: bool, files: bool, force: bool) {
        if self.running.is_some() {
            return self.warn("wait for the current turn to finish before /rewind".into());
        }
        let Some(point) = self
            .rewind_points()
            .into_iter()
            .find(|p| p.point.back == back)
        else {
            return self.warn(format!("no user message {back} back to rewind to"));
        };
        let root = self.config.dirs.workspace_root.clone();
        let show = |p: &PathBuf| {
            p.strip_prefix(&root)
                .unwrap_or(p)
                .display()
                .to_string()
                .replace('\\', "/")
        };
        let mut restored = Vec::new();
        if files && let Some(cp) = self.checkpoints() {
            let changes: Vec<_> = cp
                .changes()
                .into_iter()
                .filter(|c| point.calls.contains(&c.call_id))
                .collect();
            let conflicts = cp.conflicts(&changes);
            if !force && !conflicts.is_empty() {
                self.emit(Event::FilesRestored {
                    restored: Vec::new(),
                    conflicts: conflicts.iter().map(show).collect(),
                });
                return self.warn(format!(
                    "nothing rewound: files changed since; /rewind {back} --force overwrites them"
                ));
            }
            match cp.restore(&changes, force) {
                Ok(r) => {
                    restored = r.restored.iter().map(show).collect();
                    self.emit(Event::FilesRestored {
                        restored: restored.clone(),
                        conflicts: r.conflicts.iter().map(show).collect(),
                    });
                }
                Err(e) => return self.warn(format!("rewind failed: {e}")),
            }
        }
        if !conversation {
            if !restored.is_empty() {
                self.notes.push(format!(
                    "[The user put {} back to how they were before their message “{}”.]",
                    restored.join(", "),
                    point.point.text.lines().next().unwrap_or_default()
                ));
            }
            return;
        }
        let Some(c) = &mut self.convo else { return };
        let Some(history) = &mut c.history else {
            return;
        };
        let dropped = history[point.index..]
            .iter()
            .filter(|m| matches!(m, Message::User { .. }))
            .count();
        history.truncate(point.index);
        c.nested = nested(&self.config, &c.setup, history);
        self.turns = self
            .turns
            .saturating_sub(u32::try_from(dropped).unwrap_or(u32::MAX));
        session::record(
            self.log.as_ref(),
            "rewind",
            serde_json::json!({ "to": point.index }),
        );
        self.sync_log();
        self.emit(Event::Rewound {
            back,
            text: point.point.text,
        });
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
        let added =
            match crate::setup::add_provider(&self.config, id, base_url, api, "Added by setup.") {
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
        self.switch_to(Role::Session, added.id, model);
    }

    /// Build the setup for `provider/model` in the background; `on_switched` (or
    /// `on_planner`) gets it.
    fn switch_to(&self, role: Role, provider: String, model: String) {
        if let Some(done) = self.switch_tx.clone() {
            let config = self.config.clone();
            let tx = self.tx.clone();
            let mcp = self.mcp.clone();
            let lsp = self.lsp.clone();
            tokio::spawn(async move {
                let r = startup::switch(config, tx, mcp, lsp, provider, model).await;
                let _ = done.send((role, r)).await;
            });
        }
    }

    /// `/instructions`: the files in the system prompt, then the ones attached to tool
    /// results (seen in the history when no turn holds it).
    fn list_instructions(&self) {
        let file = |(source, text): (String, String), in_prompt| InstructionFile {
            tokens: quack_providers::tokens::estimate(&text),
            source,
            in_prompt,
            text,
        };
        let mut files: Vec<InstructionFile> = Vec::new();
        if let Some(c) = &self.convo {
            files.extend(
                quack_instructions::agents_md::blocks(&c.setup.vars.instructions)
                    .into_iter()
                    .map(|b| file(b, true)),
            );
            for m in c.history.iter().flatten() {
                if let Message::Tool { content, .. } = m {
                    for b in quack_instructions::agents_md::blocks(content) {
                        if !files.iter().any(|f| f.source == b.0) {
                            files.push(file(b, false));
                        }
                    }
                }
            }
        }
        self.emit(Event::Instructions { files });
    }

    /// A warning, for an `Op` handler that then carries on with the next op.
    fn warn_continue(&self, message: &str) -> bool {
        self.warn(message.to_owned());
        true
    }

    /// `/profiles <id>`: rebuild the session's setup with that profile for the same model.
    /// It goes into the command-line overrides, so a reload keeps it.
    fn set_profile(&mut self, id: String) {
        if self.running.is_some() {
            return self
                .warn("wait for the current turn to finish before switching profiles".into());
        }
        let Some((provider, model)) = self
            .convo
            .as_ref()
            .map(|c| (c.setup.provider.id.clone(), c.setup.model.id.clone()))
        else {
            return self.warn("no model is ready yet".into());
        };
        let value = if id == "auto" { String::new() } else { id };
        let mut config = (*self.config).clone();
        config.settings.model.profile.clone_from(&value);
        config
            .opts
            .overrides
            .retain(|o| !o.starts_with("model.profile="));
        config.opts.overrides.push(format!(
            "model.profile={}",
            serde_json::Value::String(value)
        ));
        let config = Arc::new(config);
        self.config = config.clone();
        self.config_tx.send_replace(config);
        self.switch_to(Role::Session, provider, model);
    }

    fn on_switched(&mut self, result: Result<Setup, String>) {
        let mut setup = match result {
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
        if self.config.settings.model.remember_last
            && let Err(e) =
                quack_config::save_last_model(&self.config.dirs.user_data, &setup.model_ref)
        {
            self.warn(format!("cannot remember the model for next time: {e}"));
        }
        session::record(
            self.log.as_ref(),
            "model_switch",
            serde_json::json!({ "to": setup.model_ref, "profile": setup.profile.id }),
        );
        match &mut self.convo {
            // Keep the history, the plan and the session's approvals.
            Some(c) => {
                setup.tool_ctx.plan = c.setup.tool_ctx.plan.clone();
                c.setup = Arc::new(setup);
            }
            None => {
                self.convo = Some(Conversation {
                    perms: Arc::new(Mutex::new(setup.permissions.clone())),
                    nested: nested(&self.config, &setup, &[]),
                    setup: Arc::new(setup),
                    history: Some(Vec::new()),
                    editor: None,
                    plan_start: None,
                });
                self.problem = None;
            }
        }
        self.emit(Event::SessionConfigured {
            session: self.session.clone(),
        });
    }

    /// Plan mode began: plan with the `[model] planner`, when one is set and it is not
    /// the model in use already.
    fn start_planner(&mut self) {
        let planner = self.config.settings.model.planner.clone();
        let Some(c) = &self.convo else { return };
        if planner.is_empty() || c.editor.is_some() || planner == c.setup.model_ref {
            return;
        }
        let Some((provider, model)) = planner.split_once('/') else {
            return self.warn(format!(
                "model.planner `{planner}` is not <provider>/<model>; planning with {}",
                c.setup.model_ref
            ));
        };
        self.switching = true;
        self.switch_to(Role::Planner, provider.to_owned(), model.to_owned());
    }

    /// The planner's setup is ready. The session's own setup waits in `editor` until plan
    /// mode ends; the todo list is shared, so the plan carries over.
    fn on_planner(&mut self, result: Result<Setup, String>) {
        self.switching = false;
        let mut setup = match result {
            Ok(s) => s,
            Err(e) => {
                return self.warn(format!(
                    "cannot plan with {}: {e}; planning with the session's model",
                    self.config.settings.model.planner
                ));
            }
        };
        let Some(c) = &mut self.convo else { return };
        // Plan mode ended while the planner was loading.
        if c.editor.is_some() || !c.perms.lock().is_ok_and(|p| p.plan) {
            return;
        }
        setup.tool_ctx.plan = c.setup.tool_ctx.plan.clone();
        c.editor = Some(std::mem::replace(&mut c.setup, Arc::new(setup)));
        c.plan_start = None;
        self.announce_model("planner");
    }

    /// Plan mode ended: go back to the session's own model. Returns where the planner's
    /// messages start in the history, when it wrote any.
    fn restore_editor(&mut self) -> Option<usize> {
        let c = self.convo.as_mut()?;
        let editor = c.editor.take()?;
        c.setup = editor;
        let start = c.plan_start.take();
        self.announce_model("session");
        start
    }

    /// Tell the clients and the log which model the conversation now uses.
    fn announce_model(&mut self, role: &str) {
        let Some(setup) = self.convo.as_ref().map(|c| c.setup.clone()) else {
            return;
        };
        self.session.model = Some(setup.model_ref.clone());
        self.session.profile = setup.profile.id.clone();
        session::record(
            self.log.as_ref(),
            "model_switch",
            serde_json::json!({ "to": setup.model_ref, "profile": setup.profile.id, "role": role }),
        );
        self.emit(Event::SessionConfigured {
            session: self.session.clone(),
        });
    }

    /// `/plan accept`: leave plan mode for `mode` and carry out the plan. After a planner,
    /// its messages give way to one brief, so the session's model reads only that on top of
    /// the history it already has cached.
    fn accept_plan(&mut self, mode: &str, done: mpsc::Sender<(TurnId, Vec<Message>)>) {
        if serde_json::from_value::<PermissionMode>(serde_json::Value::from(mode)).is_err() {
            return self.warn(format!(
                "cannot carry out the plan in `{mode}`; use read-only, ask, auto-edit, full-auto or yolo"
            ));
        }
        let Some(c) = &mut self.convo else {
            return self.warn("no model is ready yet".into());
        };
        if !c.perms.lock().is_ok_and(|p| p.plan) {
            return self.warn("not in plan mode; /plan starts it".into());
        }
        let brief = match (&c.editor, c.plan_start, c.history.as_mut()) {
            (Some(_), Some(start), Some(history)) => {
                let planner = &c.setup;
                let start = start.min(history.len());
                let note = plan_note(planner);
                let ask = planner.surface.name_of("ask_user");
                let text = handoff::brief(&history[start..], &note, ask, &planner.model_ref);
                history.truncate(start);
                Some((start, planner.model_ref.clone(), text))
            }
            _ => None,
        };
        self.set_mode(mode);
        let text = match brief {
            Some((start, planner, text)) => {
                session::record(
                    self.log.as_ref(),
                    "handoff",
                    serde_json::json!({ "to": start, "from": planner }),
                );
                text
            }
            None => "Carry out the plan above.".to_owned(),
        };
        self.start_turn(vec![InputItem::text(text)], done);
    }

    fn warn(&self, message: String) {
        self.emit(Event::Warning { message });
    }

    /// The sub-agent pool, made again when a reload replaced the config or the servers.
    fn pool(&mut self) -> Arc<crate::agent::Pool> {
        match &self.pool {
            Some(p) if p.current(&self.config, &self.mcp, &self.lsp) => p.clone(),
            _ => {
                let p = Arc::new(crate::agent::Pool::new(
                    self.config.clone(),
                    self.mcp.clone(),
                    self.lsp.clone(),
                    self.agent_runs.clone(),
                ));
                self.pool = Some(p.clone());
                p
            }
        }
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
    fn handle(
        &mut self,
        op: Op,
        done: &mpsc::Sender<(TurnId, Vec<Message>)>,
        reload: &mpsc::Sender<Reload>,
    ) -> bool {
        match op {
            Op::UserTurn { items, .. } => {
                if self.reloading {
                    self.emit(Event::TurnQueued {
                        waiting_for: "the config reload".into(),
                    });
                    self.queued.push_back(Queued::Turn(items));
                } else if self.switching {
                    self.emit(Event::TurnQueued {
                        waiting_for: "the planner model".into(),
                    });
                    self.queued.push_back(Queued::Turn(items));
                } else if !self.started {
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
            Op::AnswerQuestions {
                call_id,
                answers,
                stop_asking,
            } => {
                let waiting = self
                    .questions
                    .lock()
                    .ok()
                    .and_then(|mut p| p.remove(&call_id.0));
                match waiting {
                    Some(tx) => {
                        let _ = tx.send(quack_tools::Answers {
                            answers,
                            stop_asking,
                        });
                    }
                    None => self.emit(Event::Warning {
                        message: format!("no pending question for call {}", call_id.0),
                    }),
                }
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
            Op::ListProfiles => {
                let Some(setup) = self.convo.as_ref().map(|c| c.setup.clone()) else {
                    return self.warn_continue("no model is ready yet; profiles are shown for one");
                };
                let config = self.config.clone();
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let event =
                        tokio::task::spawn_blocking(move || startup::profiles(&config, &setup))
                            .await;
                    if let Ok(event) = event {
                        let _ = tx.send(event);
                    }
                });
            }
            Op::SetProfile { id } => self.set_profile(id),
            Op::ListInstructions => self.list_instructions(),
            Op::SetModel { provider, model } => {
                if self.running.is_some() {
                    self.warn("wait for the current turn to finish before switching models".into());
                } else {
                    self.switch_to(Role::Session, provider.0, model.0);
                }
            }
            // A client may send these the moment it sees `TurnComplete`, before the turn
            // task has handed the history back, so they wait in the queue like turns do.
            Op::ResumeSession { id } => self.queue_switch(Some(id), done, reload),
            Op::NewSession => self.queue_switch(None, done, reload),
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
                self.next_queued(done, reload);
            }
            Op::ListSessions => self.list_sessions(),
            Op::ListMemory => self.list_memory(),
            Op::ListRewindPoints => {
                let points = self.rewind_points().into_iter().map(|p| p.point).collect();
                self.emit(Event::RewindPoints { points });
            }
            Op::Rewind {
                back,
                conversation,
                files,
                force,
            } => self.rewind(back, conversation, files, force),
            Op::Undo { force, .. } => self.undo(force),
            Op::RunDoctor => {
                let config = self.config.clone();
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let checks = crate::doctor::run(config).await;
                    let _ = tx.send(Event::DoctorReport { checks });
                });
            }
            Op::InitInstructions => {
                let prompt = init_prompt(&self.config);
                return self.handle(
                    Op::UserTurn {
                        items: vec![InputItem::text(prompt)],
                        overrides: Default::default(),
                    },
                    done,
                    reload,
                );
            }
            Op::ListLsp => self.emit(Event::LspServers {
                servers: crate::lsp::info(&self.lsp),
            }),
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
            Op::SetSandbox { off } if !self.started => self.pending_sandbox = Some(off),
            Op::SetSandbox { off } => self.set_sandbox(off),
            Op::AcceptPlan { mode } => {
                self.queued.push_back(Queued::Accept(mode));
                self.next_queued(done, reload);
            }
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
            Op::GetStats => {
                let model = self
                    .convo
                    .as_ref()
                    .map(|c| c.setup.model_ref.clone())
                    .or_else(|| self.session.model.clone())
                    .unwrap_or_default();
                let mut report = self
                    .stats
                    .lock()
                    .map(|s| s.report(&model, self.turns))
                    .unwrap_or_default();
                if let Some(c) = &self.convo {
                    let s = turn::sampling(&c.setup);
                    report.sampling = [
                        ("temperature", s.temperature),
                        ("top_p", s.top_p),
                        ("top_k", s.top_k.map(f64::from)),
                        ("min_p", s.min_p),
                        ("repetition_penalty", s.repetition_penalty),
                    ]
                    .into_iter()
                    .filter_map(|(k, v)| Some((k.to_owned(), v?)))
                    .collect();
                }
                self.emit(Event::Stats { report });
            }
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
            Op::ReloadConfig => {
                self.queued.push_back(Queued::Reload);
                self.next_queued(done, reload);
            }
            Op::TrustProject { decision } => {
                let saved = quack_config::TrustDecision::parse(&decision)
                    .ok_or_else(|| {
                        format!(
                            "unknown decision `{decision}`; use trusted, settings-only or ignored"
                        )
                    })
                    .and_then(|d| self.config.trust_project(d));
                match saved {
                    Ok(_) => {
                        self.queued.push_back(Queued::Reload);
                        self.next_queued(done, reload);
                    }
                    Err(message) => self.emit(Event::Error {
                        error: EngineError {
                            kind: EngineErrorKind::Config,
                            message: format!("cannot save the trust decision: {message}"),
                        },
                    }),
                }
            }
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
        reload: &mpsc::Sender<Reload>,
    ) {
        self.queued.push_back(Queued::Switch(target));
        self.next_queued(done, reload);
    }

    fn next_queued(
        &mut self,
        done: &mpsc::Sender<(TurnId, Vec<Message>)>,
        reload: &mpsc::Sender<Reload>,
    ) {
        while self.started && self.running.is_none() && !self.reloading && !self.switching {
            match self.queued.pop_front() {
                Some(Queued::Turn(items)) => self.start_turn(items, done.clone()),
                Some(Queued::Switch(target)) => self.switch_session(target),
                Some(Queued::Compact(focus)) => self.start_compact(focus, done.clone()),
                Some(Queued::Reload) => self.begin_reload(reload.clone()),
                Some(Queued::Accept(mode)) => self.accept_plan(&mode, done.clone()),
                None => break,
            }
        }
    }

    /// Read the config files again off the async threads.
    fn begin_reload(&mut self, reload: mpsc::Sender<Reload>) {
        self.reloading = true;
        let config = self.config.clone();
        tokio::spawn(async move {
            let result = tokio::task::spawn_blocking(move || config.reload())
                .await
                .map_err(|e| e.to_string())
                .and_then(|r| r.map_err(|e| e.to_string()));
            let _ = reload.send(Reload::Read(result.map(Box::new))).await;
        });
    }

    /// The new config is read: restart MCP servers on it and run startup again, keeping
    /// the model in use.
    async fn on_reread(
        &mut self,
        result: Result<Box<Loaded>, String>,
        reload: &mpsc::Sender<Reload>,
    ) {
        let config = match result {
            Ok(c) => Arc::new(*c),
            Err(message) => {
                self.reloading = false;
                return self.emit(Event::Error {
                    error: EngineError {
                        kind: EngineErrorKind::Config,
                        message: format!("cannot reload the config: {message}"),
                    },
                });
            }
        };
        self.config = config.clone();
        self.config_tx.send_replace(config.clone());
        let _ = tokio::time::timeout(
            Duration::from_secs(3),
            futures::future::join(self.mcp.manager.shutdown(), self.lsp.shutdown()),
        )
        .await;
        self.start_mcp().await;
        self.start_lsp().await;
        self.load_hooks().await;
        self.emit(Event::ConfigReloaded);
        // The session's own model, also while the planner is in use.
        let keep = self.convo.as_ref().map(|c| {
            let s = c.editor.as_ref().unwrap_or(&c.setup);
            (s.provider.id.clone(), s.model.id.clone())
        });
        let tx = self.tx.clone();
        let mcp = self.mcp.clone();
        let lsp = self.lsp.clone();
        let reload = reload.clone();
        tokio::spawn(async move {
            let outcome = startup::run(config, tx, mcp, lsp, keep).await;
            let _ = reload.send(Reload::Ready(Box::new(outcome))).await;
        });
    }

    /// Startup after a reload finished: the session continues on the new setup, with its
    /// history and permission mode. Approvals given for the session are dropped, since the
    /// rules they were given under may have changed.
    fn on_reloaded(&mut self, outcome: StartupOutcome) {
        let Some(setup) = outcome.setup else {
            self.problem = outcome.problem;
            if self.convo.is_some() {
                self.warn(
                    "the reloaded config has no usable model; keeping the previous one".into(),
                );
            }
            return;
        };
        self.problem = None;
        self.session.model = Some(setup.model_ref.clone());
        self.session.profile = setup.profile.id.clone();
        let mut perms = setup.permissions.clone();
        let history = match self.convo.take() {
            Some(c) => {
                if let Ok(old) = c.perms.lock() {
                    perms.mode = self
                        .config
                        .settings
                        .policy
                        .max_permission_mode
                        .map_or(old.mode, |cap| old.mode.min(cap));
                    perms.plan = old.plan;
                    // A reload that locks `sandbox.mode` turns the sandbox back on.
                    perms.sandbox_off = old.sandbox_off && !self.sandbox_locked();
                    if old.sandbox_off && !perms.sandbox_off {
                        self.warn("`sandbox.mode` is now locked by the system policy; the sandbox is back on".into());
                        self.emit(Event::Sandbox { off: false });
                    }
                }
                c.history.unwrap_or_default()
            }
            None => Vec::new(),
        };
        self.convo = Some(Conversation {
            perms: Arc::new(Mutex::new(perms)),
            nested: nested(&self.config, &setup, &history),
            setup: Arc::new(setup),
            history: Some(history),
            editor: None,
            plan_start: None,
        });
        session::record(
            self.log.as_ref(),
            "reload",
            serde_json::json!({ "model": self.session.model }),
        );
        self.emit(Event::SessionConfigured {
            session: self.session.clone(),
        });
        self.emit_mode();
        if self
            .convo
            .as_ref()
            .is_some_and(|c| c.perms.lock().is_ok_and(|p| p.plan))
        {
            self.start_planner();
        }
    }

    /// Run `/compact` like a turn: it owns the history until it is done and Esc stops it.
    fn start_compact(&mut self, focus: Option<String>, done: mpsc::Sender<(TurnId, Vec<Message>)>) {
        let pool = self.pool();
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
            questions: self.questions.clone(),
            tx: self.tx.clone(),
            cancel,
            log: self.log.clone(),
            main_log: self.log.clone(),
            checkpoints: None,
            stats: self.stats.clone(),
            hooks: self.hooks.clone(),
            session: self.session.id.0.clone(),
            agent: None,
            max_steps: turn::MAX_STEPS,
            pool,
            prompt_lock: Arc::default(),
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
        if convo.editor.is_some() {
            convo.plan_start.get_or_insert(history.len());
        }
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
            format!("{prompt}\n\n{}", plan_note(&setup))
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
        let pool = self.pool();
        let ctx = TurnCtx {
            turn,
            setup,
            perms,
            nested,
            approvals: self.approvals.clone(),
            questions: self.questions.clone(),
            tx: self.tx.clone(),
            cancel,
            log: self.log.clone(),
            main_log: self.log.clone(),
            checkpoints: self.checkpoints(),
            stats: self.stats.clone(),
            hooks: self.hooks.clone(),
            session: self.session.id.0.clone(),
            agent: None,
            max_steps: turn::MAX_STEPS,
            pool,
            prompt_lock: Arc::default(),
        };
        let prompt = turn::Prompt {
            text: prompt,
            typed,
            start: self.start_context.take(),
        };
        tokio::spawn(async move {
            let history = turn::run(ctx, history, prompt).await;
            let _ = done.send((turn, history)).await;
        });
    }
}

/// A rewind point and where it is in the history.
#[derive(Debug)]
struct Point {
    point: quack_protocol::RewindPoint,
    /// Index of the user message in the history.
    index: usize,
    /// Tool calls made after it.
    calls: Vec<String>,
}

/// Steps of a reload, reported back to the engine loop.
#[derive(Debug)]
enum Reload {
    Read(Result<Box<Loaded>, String>),
    Ready(Box<StartupOutcome>),
}

/// The request appended to each message in plan mode: the profile's `plan.md` (or the
/// default), and when read-only sub-agents can run, a pointer to them.
fn plan_note(setup: &Setup) -> String {
    let note = setup.profile.plan_prompt.as_deref().unwrap_or(PLAN_PROMPT);
    let explorers: Vec<&str> = setup
        .tool_ctx
        .agents
        .iter()
        .filter(|a| a.read_only)
        .map(|a| a.id.as_str())
        .collect();
    match setup.surface.name_of("task") {
        Some(task) if !explorers.is_empty() => format!(
            "{note} To explore widely without filling your context, hand searches to \
             read-only agents ({}) with {task}; several started in one reply run in parallel.",
            explorers.join(", ")
        ),
        _ => note.to_owned(),
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
        // A custom command: its prompt with the arguments filled in.
        return match quack_instructions::commands::expand(&setup.commands, &prompt) {
            Some(Ok(text)) => (text, None),
            Some(Err(e)) => {
                let message = format!("/{name}: {e}");
                (prompt, Some(message))
            }
            None => (prompt, None),
        };
    };
    match quack_tools::expand_skill(skill) {
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

/// What `/init` asks the model to do: merge other agents' files into AGENTS.md, update
/// an existing AGENTS.md, or write one from scratch.
fn init_prompt(config: &Loaded) -> String {
    let root = &config.dirs.workspace_root;
    let others = crate::env::unmerged(config);
    let has_agents = root.join("AGENTS.md").is_file();
    let guide = "Keep it short and concrete: exact build, test and lint commands, the project \
                 layout, conventions that are not obvious from the code, and pitfalls. Do not \
                 invent anything you have not checked.";
    if !others.is_empty() {
        format!(
            "Consolidate this project's agent instructions into AGENTS.md at the workspace \
             root. Read {}{}. Write one AGENTS.md that keeps every instruction that still \
             applies, drops duplicates, and is organized under short headings; if two files \
             contradict each other, keep both points and tell me so I can decide. {guide} Do \
             not change or delete the other files. When you are done, list them and suggest \
             replacing each with a single line `@AGENTS.md`, so those tools keep reading the \
             same instructions.",
            others.join(", "),
            if has_agents {
                " and the existing AGENTS.md"
            } else {
                ""
            },
        )
    } else if has_agents {
        format!(
            "Review AGENTS.md at the workspace root against the repository as it is now, and \
             update it where it is wrong, missing something important, or out of date. {guide}"
        )
    } else {
        format!(
            "Create AGENTS.md at the workspace root: instructions for coding agents working in \
             this repository. Explore the repository first. {guide}"
        )
    }
}

fn op_name(op: &Op) -> &'static str {
    match op {
        Op::UserTurn { .. } => "user_turn",
        Op::Interrupt => "interrupt",
        Op::ApprovalResponse { .. } => "approval_response",
        Op::AnswerQuestions { .. } => "answer_questions",
        Op::SetModel { .. } => "set_model",
        Op::ListProfiles => "list_profiles",
        Op::ListInstructions => "list_instructions",
        Op::SetProfile { .. } => "set_profile",
        Op::ListModels => "list_models",
        Op::Compact { .. } => "compact",
        Op::Undo { .. } => "undo",
        Op::ResumeSession { .. } => "resume_session",
        Op::NewSession => "new_session",
        Op::ListSessions => "list_sessions",
        Op::GetContext => "get_context",
        Op::GetStats => "get_stats",
        Op::ListMemory => "list_memory",
        Op::RunDoctor => "run_doctor",
        Op::DetectServers => "detect_servers",
        Op::ListMcp => "list_mcp",
        Op::ListLsp => "list_lsp",
        Op::InitInstructions => "init_instructions",
        Op::RestartMcp { .. } => "restart_mcp",
        Op::AddProvider { .. } => "add_provider",
        Op::SetPermissionMode { .. } => "set_permission_mode",
        Op::SetSandbox { .. } => "set_sandbox",
        Op::AcceptPlan { .. } => "accept_plan",
        Op::DeleteMemory { .. } => "delete_memory",
        Op::SetTitle { .. } => "set_title",
        Op::SaveSetting { .. } => "save_setting",
        Op::ForkSession { .. } => "fork_session",
        Op::ListRewindPoints => "list_rewind_points",
        Op::Rewind { .. } => "rewind",
        Op::ReloadConfig => "reload_config",
        Op::TrustProject { .. } => "trust_project",
        Op::Shutdown => "shutdown",
    }
}

/// Time-ordered id. Replaced by UUID v7 when `quack-store` lands.
fn new_session_id() -> SessionId {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    SessionId(format!("{nanos:032x}"))
}
