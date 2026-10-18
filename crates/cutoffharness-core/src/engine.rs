use std::sync::Arc;
use std::time::Duration;

use cutoffharness_config::Loaded;
use cutoffharness_protocol::{Event, InputItem, Op, SessionId, SessionInfo, TurnId, Usage};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::startup::{self, StartupOutcome};

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

#[derive(Debug)]
pub struct Engine {
    config: Arc<Loaded>,
    tx: broadcast::Sender<Event>,
    session: SessionInfo,
    outcome: Option<StartupOutcome>,
    next_turn: u64,
    queued: Vec<Vec<InputItem>>,
    running: Option<(TurnId, CancellationToken)>,
}

impl Engine {
    /// Spawn the engine on the current tokio runtime. Startup runs in the background;
    /// the returned handle is subscribed before any event is sent, so nothing is missed.
    pub fn spawn(config: Arc<Loaded>) -> EngineHandle {
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
            outcome: None,
            next_turn: 0,
            queued: Vec::new(),
            running: None,
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
        let (done_tx, mut done_rx) = mpsc::channel::<TurnId>(8);

        loop {
            tokio::select! {
                outcome = &mut startup, if self.outcome.is_none() => {
                    self.outcome = Some(outcome.unwrap_or_default());
                    self.emit(Event::StartupComplete);
                    for items in std::mem::take(&mut self.queued) {
                        self.start_turn(items, done_tx.clone());
                    }
                }
                Some(turn) = done_rx.recv() => {
                    if self.running.as_ref().is_some_and(|(t, _)| *t == turn) {
                        self.running = None;
                    }
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

    /// Returns false when the engine should stop.
    fn handle(&mut self, op: Op, done: &mpsc::Sender<TurnId>) -> bool {
        match op {
            Op::UserTurn { items, .. } => {
                if self.outcome.is_none() {
                    self.emit(Event::TurnQueued {
                        waiting_for: "startup".into(),
                    });
                    self.queued.push(items);
                } else {
                    self.start_turn(items, done.clone());
                }
            }
            Op::Interrupt => {
                if let Some((_, cancel)) = &self.running {
                    cancel.cancel();
                }
            }
            Op::Shutdown => return false,
            other => self.emit(Event::Warning {
                message: format!("{} is not implemented yet", op_name(&other)),
            }),
        }
        true
    }

    fn start_turn(&mut self, items: Vec<InputItem>, done: mpsc::Sender<TurnId>) {
        if let Some((_, cancel)) = self.running.take() {
            cancel.cancel();
        }
        self.next_turn += 1;
        let turn = TurnId(self.next_turn);
        let cancel = CancellationToken::new();
        self.running = Some((turn, cancel.clone()));
        let reply = stub_reply(&items, self.outcome.as_ref());
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let _ = tx.send(Event::TurnStarted { turn });
            let mut sent = 0u32;
            for chunk in reply.split_inclusive(' ') {
                tokio::select! {
                    () = cancel.cancelled() => {
                        let _ = tx.send(Event::TurnAborted { turn, reason: "interrupted".into() });
                        let _ = done.send(turn).await;
                        return;
                    }
                    () = tokio::time::sleep(Duration::from_millis(12)) => {}
                }
                sent += 1;
                let _ = tx.send(Event::AssistantDelta {
                    text: chunk.to_owned(),
                });
            }
            let usage = Usage {
                prompt_tokens: 0,
                completion_tokens: sent,
                cached_prompt_tokens: None,
            };
            let _ = tx.send(Event::TurnComplete { turn, usage });
            let _ = done.send(turn).await;
        });
    }
}

fn stub_reply(items: &[InputItem], outcome: Option<&StartupOutcome>) -> String {
    let prompt: String = items
        .iter()
        .filter_map(|i| match i {
            InputItem::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ");
    let model = outcome
        .and_then(|o| o.model.as_deref())
        .unwrap_or("no model selected");
    format!(
        "Scaffold engine ({model}): provider adapters land in M1, so nothing was sent to a model. \
         You said: \"{prompt}\""
    )
}

fn op_name(op: &Op) -> &'static str {
    match op {
        Op::UserTurn { .. } => "user_turn",
        Op::Interrupt => "interrupt",
        Op::ApprovalResponse { .. } => "approval_response",
        Op::SetModel { .. } => "set_model",
        Op::Compact => "compact",
        Op::Undo { .. } => "undo",
        Op::ResumeSession { .. } => "resume_session",
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
