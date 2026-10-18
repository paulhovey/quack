use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use cutoffharness_config::{LayerKind, Loaded};
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

const EVENT_CAPACITY: usize = 4096;
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
}

/// The conversation once a model is ready.
#[derive(Debug)]
struct Conversation {
    setup: Arc<Setup>,
    perms: Arc<Mutex<Permissions>>,
    /// `None` while a turn task owns it.
    history: Option<Vec<Message>>,
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
        let mut startup = tokio::spawn(startup::run(self.config.clone(), self.tx.clone()));
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
        self.emit(Event::ShutdownComplete);
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
            setup: Arc::new(setup),
            history: Some(history),
        });
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
            c.history = Some(history);
        }
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
                    tokio::spawn(async move {
                        let r = startup::switch(config, tx, provider.0, model.0).await;
                        let _ = done.send(r).await;
                    });
                }
            }
            // A client may send these the moment it sees `TurnComplete`, before the turn
            // task has handed the history back, so they wait in the queue like turns do.
            Op::ResumeSession { id } => self.queue_switch(Some(id), done),
            Op::NewSession => self.queue_switch(None, done),
            Op::ListSessions => self.list_sessions(),
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
                None => break,
            }
        }
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
        self.ensure_log();
        self.turns += 1;
        if self.title.is_none() {
            let first: String = prompt
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
            approvals: self.approvals.clone(),
            tx: self.tx.clone(),
            cancel,
            log: self.log.clone(),
        };
        tokio::spawn(async move {
            let history = turn::run(ctx, history, prompt).await;
            let _ = done.send((turn, history)).await;
        });
    }
}

fn op_name(op: &Op) -> &'static str {
    match op {
        Op::UserTurn { .. } => "user_turn",
        Op::Interrupt => "interrupt",
        Op::ApprovalResponse { .. } => "approval_response",
        Op::SetModel { .. } => "set_model",
        Op::ListModels => "list_models",
        Op::Compact => "compact",
        Op::Undo { .. } => "undo",
        Op::ResumeSession { .. } => "resume_session",
        Op::NewSession => "new_session",
        Op::ListSessions => "list_sessions",
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
