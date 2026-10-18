use std::time::{Duration, Instant};

use crossterm::event::{Event as TermEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use cutoffharness_config::Loaded;
use cutoffharness_config::settings::{StartupPanel, UiSettings};
use cutoffharness_protocol::{
    ApprovalDecision, ApprovalRequest, CallId, Event, InputItem, LoadState, ModelId, Op,
    ProviderId, ReplayItem, Subsystem, TurnOverrides,
};

use crate::commands::{self, COMMANDS, Command};
use crate::fuzzy;
use crate::input::Input;
use crate::modal::{self, Modal, ModalOutcome};
use crate::theme::{Glyphs, Theme};

const DOUBLE_CTRL_C: Duration = Duration::from_millis(1500);
/// Spinner advances every N frames (16 ms each).
const SPIN_EVERY: u64 = 5;

/// Static facts the UI shows, captured once at startup.
#[derive(Debug, Clone)]
pub struct AppInfo {
    pub version: String,
    pub cwd: String,
    pub model: Option<String>,
    pub profile: String,
    pub permission_mode: String,
    pub ui: UiSettings,
    pub no_color: bool,
}

impl AppInfo {
    pub fn from_config(config: &Loaded) -> Self {
        let root = config.dirs.workspace_root.display().to_string();
        let home =
            std::env::var(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).unwrap_or_default();
        let cwd = match root.strip_prefix(&home) {
            Some(rest) if !home.is_empty() => format!("~{rest}"),
            _ => root,
        };
        Self {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            cwd,
            model: Some(config.settings.model.default.clone()).filter(|m| !m.is_empty()),
            profile: "empty".into(),
            permission_mode: config.settings.permissions.mode.label().to_owned(),
            ui: config.settings.ui.clone(),
            no_color: std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Entry {
    User(String),
    Assistant(String),
    Reasoning(String),
    Tool(ToolEntry),
    System(String),
    Warning(String),
    Error(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ToolEntry {
    pub call_id: CallId,
    pub summary: String,
    /// `None` while running.
    pub ok: Option<bool>,
    pub result: String,
    /// Live output while running; the full diff or output once done.
    pub detail: String,
}

/// A tool call waiting for the user's decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Approval {
    pub call_id: CallId,
    pub request: ApprovalRequest,
}

#[derive(Debug)]
pub struct App {
    pub(crate) info: AppInfo,
    pub(crate) theme: Theme,
    pub(crate) glyphs: Glyphs,
    pub(crate) startup: Vec<(Subsystem, LoadState)>,
    pub(crate) startup_done: bool,
    pub(crate) transcript: Vec<Entry>,
    pub(crate) input: Input,
    pub(crate) modal: Option<Modal>,
    /// Transcript rows scrolled up from the bottom.
    pub(crate) scroll: usize,
    pub(crate) popup_selected: usize,
    popup_dismissed: bool,
    pub(crate) turn_running: bool,
    pub(crate) queued: bool,
    pub(crate) approval: Option<Approval>,
    /// `(used, budget)` tokens from the last response.
    pub(crate) context: Option<(u32, u32)>,
    panel_collapsed: bool,
    frame: u64,
    dirty: bool,
    quit: bool,
    ops: Vec<Op>,
    last_ctrl_c: Option<Instant>,
}

impl App {
    pub fn new(info: AppInfo) -> Self {
        Self {
            theme: Theme::resolve(&info.ui.theme, info.no_color),
            glyphs: Glyphs::new(info.ui.ascii),
            startup: Subsystem::ALL
                .iter()
                .map(|s| (*s, LoadState::Pending))
                .collect(),
            startup_done: false,
            transcript: Vec::new(),
            input: Input::default(),
            modal: None,
            scroll: 0,
            popup_selected: 0,
            popup_dismissed: false,
            turn_running: false,
            queued: false,
            approval: None,
            context: None,
            panel_collapsed: false,
            frame: 0,
            dirty: true,
            quit: false,
            ops: Vec::new(),
            last_ctrl_c: None,
            info,
        }
    }

    pub fn should_quit(&self) -> bool {
        self.quit
    }

    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    pub fn take_ops(&mut self) -> Vec<Op> {
        std::mem::take(&mut self.ops)
    }

    pub(crate) fn spinner(&self) -> &'static str {
        if self.info.ui.reduced_motion {
            return self.glyphs.pending;
        }
        let frames = self.glyphs.spinner;
        frames[(self.frame / SPIN_EVERY) as usize % frames.len()]
    }

    pub(crate) fn startup_issues(&self) -> usize {
        self.startup
            .iter()
            .filter(|(_, s)| {
                matches!(
                    s,
                    LoadState::Failed { .. } | LoadState::NeedsAttention { .. }
                )
            })
            .count()
    }

    pub(crate) fn panel_visible(&self) -> bool {
        match self.info.ui.startup_panel {
            StartupPanel::Always => true,
            StartupPanel::Never => false,
            StartupPanel::Auto => {
                let all_fine = self.startup_done && self.startup_issues() == 0;
                !self.panel_collapsed && !all_fine
            }
        }
    }

    /// Commands matching the input while it is a bare `/word`.
    pub(crate) fn popup_matches(&self) -> Vec<&'static Command> {
        let text = self.input.text();
        if self.modal.is_some()
            || self.popup_dismissed
            || !text.starts_with('/')
            || text.contains(char::is_whitespace)
        {
            return Vec::new();
        }
        let query = &text[1..];
        let mut hits: Vec<&Command> = fuzzy::filter(query, COMMANDS.iter().map(|c| c.name))
            .into_iter()
            .map(|i| &COMMANDS[i])
            .collect();
        if let Some(alias) = COMMANDS.iter().find(|c| c.aliases.contains(&query)) {
            hits.retain(|c| c.name != alias.name);
            hits.insert(0, alias);
        }
        hits
    }

    pub fn on_tick(&mut self) {
        self.frame = self.frame.wrapping_add(1);
        let animating = self.turn_running
            || self.queued
            || self
                .startup
                .iter()
                .any(|(_, s)| matches!(s, LoadState::Running { .. }));
        if animating && !self.info.ui.reduced_motion && self.frame.is_multiple_of(SPIN_EVERY) {
            self.dirty = true;
        }
    }

    pub fn on_engine(&mut self, event: Event) {
        self.dirty = true;
        match event {
            Event::StartupProgress { subsystem, state } => {
                if let Some(row) = self.startup.iter_mut().find(|(s, _)| *s == subsystem) {
                    row.1 = state;
                }
            }
            Event::StartupComplete => self.startup_done = true,
            Event::TurnQueued { waiting_for } => {
                self.queued = true;
                self.note(format!("queued, waiting for {waiting_for}"));
            }
            Event::SessionConfigured { session } => {
                self.info.model = session.model;
                self.info.profile = session.profile;
            }
            Event::ModelCatalog { providers, current } => {
                // Only refresh a /models modal that is still open.
                if self.modal.as_ref().is_some_and(|m| m.title == "Models") {
                    self.modal = Some(modal::models(
                        Some(&providers),
                        current.as_deref(),
                        &self.theme,
                    ));
                }
            }
            Event::SessionResumed { session, items } => {
                self.panel_collapsed = true;
                let short: String = session.id.0.chars().take(12).collect();
                self.note(format!("resumed session {short} ({} entries)", items.len()));
                for item in items {
                    self.transcript.push(match item {
                        ReplayItem::User { text } => Entry::User(text),
                        ReplayItem::Assistant { text } => Entry::Assistant(text),
                        ReplayItem::Tool {
                            summary,
                            ok,
                            result,
                        } => Entry::Tool(ToolEntry {
                            call_id: CallId(String::new()),
                            summary,
                            ok: Some(ok),
                            result,
                            detail: String::new(),
                        }),
                    });
                }
            }
            Event::TurnStarted { .. } => {
                self.queued = false;
                self.turn_running = true;
            }
            Event::AssistantDelta { text } => match self.transcript.last_mut() {
                Some(Entry::Assistant(buf)) => buf.push_str(&text),
                _ => self
                    .transcript
                    .push(Entry::Assistant(text.trim_start().to_owned())),
            },
            Event::AssistantRewrite { text } => {
                let last = self
                    .transcript
                    .iter()
                    .rposition(|e| matches!(e, Entry::Assistant(_)));
                if let Some(i) = last {
                    self.transcript[i] = Entry::Assistant(text);
                }
            }
            Event::ReasoningDelta { text } => match self.transcript.last_mut() {
                Some(Entry::Reasoning(buf)) => buf.push_str(&text),
                _ => self
                    .transcript
                    .push(Entry::Reasoning(text.trim_start().to_owned())),
            },
            Event::ToolCallBegin { call } => self.transcript.push(Entry::Tool(ToolEntry {
                call_id: call.call_id,
                summary: call.summary,
                ok: None,
                result: String::new(),
                detail: String::new(),
            })),
            Event::ToolCallOutputDelta { call_id, chunk } => {
                if let Some(t) = self.tool_mut(&call_id) {
                    t.detail.push_str(&chunk);
                }
            }
            Event::ToolCallEnd { call_id, result } => {
                if let Some(t) = self.tool_mut(&call_id) {
                    t.ok = Some(result.ok);
                    t.result = result.summary;
                    t.detail = result.detail.unwrap_or_default();
                }
            }
            Event::ApprovalRequest { call_id, request } => {
                self.approval = Some(Approval { call_id, request });
            }
            Event::ContextUpdate { used, budget, .. } => self.context = Some((used, budget)),
            Event::TurnComplete { .. } => {
                self.turn_running = false;
                self.approval = None;
            }
            Event::TurnAborted { reason, .. } => {
                self.turn_running = false;
                self.approval = None;
                self.note(reason);
            }
            Event::Warning { message } => self.transcript.push(Entry::Warning(message)),
            Event::Error { error } => self.transcript.push(Entry::Error(error.message)),
            _ => self.dirty = false,
        }
    }

    pub fn on_terminal(&mut self, event: TermEvent) {
        match event {
            // Windows reports Release events too; act on Press and Repeat only.
            TermEvent::Key(key) if key.kind != KeyEventKind::Release => self.on_key(key),
            TermEvent::Paste(text) => {
                match &mut self.modal {
                    Some(m) => m.paste(&text),
                    None => self.input.insert_str(&text),
                }
                self.dirty = true;
            }
            TermEvent::Resize(..) => self.dirty = true,
            _ => {}
        }
    }

    fn on_key(&mut self, key: KeyEvent) {
        self.dirty = true;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('c') {
            return self.ctrl_c();
        }
        if self.approval.is_some() {
            return self.on_approval_key(key);
        }
        if let Some(modal) = &mut self.modal {
            match modal.on_key(key) {
                ModalOutcome::None => {}
                ModalOutcome::Close => self.modal = None,
                ModalOutcome::Insert(text) => {
                    self.modal = None;
                    self.input.set(&text);
                    self.popup_dismissed = true;
                }
                ModalOutcome::Run(text) => {
                    self.modal = None;
                    self.execute(&text);
                }
            }
            return;
        }
        let popup = self.popup_matches();
        match key.code {
            KeyCode::Esc if !popup.is_empty() => self.popup_dismissed = true,
            KeyCode::Esc if self.turn_running || self.queued => self.ops.push(Op::Interrupt),
            KeyCode::Esc => {}
            KeyCode::Enter
                if key
                    .modifiers
                    .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) =>
            {
                self.input.insert('\n');
            }
            KeyCode::Char('j') if ctrl => self.input.insert('\n'),
            KeyCode::Enter => self.submit(popup.get(self.popup_selected).copied()),
            KeyCode::Tab => {
                if let Some(cmd) = popup.get(self.popup_selected) {
                    self.input.set(&format!("/{} ", cmd.name));
                }
            }
            KeyCode::Up if !popup.is_empty() => {
                self.popup_selected = self.popup_selected.saturating_sub(1);
            }
            KeyCode::Down if !popup.is_empty() => {
                self.popup_selected = (self.popup_selected + 1).min(popup.len() - 1);
            }
            KeyCode::Up => self.input.history_prev(),
            KeyCode::Down => self.input.history_next(),
            KeyCode::PageUp => self.scroll += 10,
            KeyCode::PageDown => self.scroll = self.scroll.saturating_sub(10),
            KeyCode::Left => self.input.left(),
            KeyCode::Right => self.input.right(),
            KeyCode::Home => self.input.home(),
            KeyCode::End => self.input.end(),
            KeyCode::Char('a') if ctrl => self.input.home(),
            KeyCode::Char('e') if ctrl => self.input.end(),
            KeyCode::Char('u') if ctrl => self.input.clear(),
            KeyCode::Char('p') if ctrl => self.modal = Some(modal::help(&self.theme)),
            KeyCode::Backspace => self.edited(Input::backspace),
            KeyCode::Delete => self.edited(Input::delete),
            KeyCode::Char(c) if !ctrl => {
                self.input.insert(c);
                self.popup_dismissed = false;
                self.popup_selected = 0;
            }
            _ => {}
        }
    }

    fn tool_mut(&mut self, id: &CallId) -> Option<&mut ToolEntry> {
        self.transcript.iter_mut().rev().find_map(|e| match e {
            Entry::Tool(t) if &t.call_id == id => Some(t),
            _ => None,
        })
    }

    fn on_approval_key(&mut self, key: KeyEvent) {
        let decision = match key.code {
            KeyCode::Char('y' | 'Y') | KeyCode::Enter => ApprovalDecision::AllowOnce,
            KeyCode::Char('a' | 'A') => ApprovalDecision::AllowSession,
            KeyCode::Char('n' | 'N') | KeyCode::Esc => ApprovalDecision::Deny,
            KeyCode::PageUp => {
                self.scroll += 10;
                return;
            }
            KeyCode::PageDown => {
                self.scroll = self.scroll.saturating_sub(10);
                return;
            }
            _ => return,
        };
        if let Some(a) = self.approval.take() {
            self.ops.push(Op::ApprovalResponse {
                call_id: a.call_id,
                decision,
            });
        }
    }

    fn edited(&mut self, f: fn(&mut Input)) {
        f(&mut self.input);
        self.popup_dismissed = false;
        self.popup_selected = 0;
    }

    fn ctrl_c(&mut self) {
        if self.modal.take().is_some() {
            return;
        }
        if self.approval.is_some() {
            self.approval = None;
            self.ops.push(Op::Interrupt);
            return;
        }
        if !self.input.is_empty() {
            self.input.clear();
        } else if self.turn_running || self.queued {
            self.ops.push(Op::Interrupt);
        } else if self
            .last_ctrl_c
            .is_some_and(|t| t.elapsed() < DOUBLE_CTRL_C)
        {
            self.quit = true;
        } else {
            self.last_ctrl_c = Some(Instant::now());
            self.note("press Ctrl-C again to quit".into());
        }
    }

    fn submit(&mut self, popup_choice: Option<&'static Command>) {
        if let Some(cmd) = popup_choice {
            self.input.clear();
            return self.run_command(cmd, "");
        }
        let text = self.input.submit();
        if text.trim().is_empty() {
            return;
        }
        self.scroll = 0;
        if text.starts_with('/') {
            return self.execute(&text);
        }
        self.panel_collapsed = true;
        self.transcript.push(Entry::User(text.clone()));
        self.ops.push(Op::UserTurn {
            items: vec![InputItem::text(text)],
            overrides: TurnOverrides::default(),
        });
    }

    /// Run a `/command args` line.
    fn execute(&mut self, text: &str) {
        let rest = text.trim().trim_start_matches('/');
        let (name, args) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
        match commands::find(name) {
            Some(cmd) => self.run_command(cmd, args.trim()),
            None => self.note(format!("unknown command /{name}; /help lists commands")),
        }
    }

    fn run_command(&mut self, cmd: &Command, args: &str) {
        self.popup_selected = 0;
        match cmd.name {
            "models" if !args.is_empty() => match args.split_once('/') {
                Some((provider, model)) if !provider.is_empty() && !model.is_empty() => {
                    self.note(format!("switching to {args}…"));
                    self.ops.push(Op::SetModel {
                        provider: ProviderId(provider.to_owned()),
                        model: ModelId(model.to_owned()),
                    });
                }
                _ => self.note("usage: /models <provider>/<model>".into()),
            },
            "models" => {
                self.modal = Some(modal::models(None, self.info.model.as_deref(), &self.theme));
                self.ops.push(Op::ListModels);
            }
            "help" => self.modal = Some(modal::help(&self.theme)),
            "quit" => self.quit = true,
            "new" => {
                self.transcript.clear();
                self.note("new session (sessions are saved from M1)".into());
            }
            name => self.note(format!(
                "/{name} is planned for {}",
                cmd.pending.unwrap_or("a later milestone")
            )),
        }
    }

    fn note(&mut self, text: String) {
        self.transcript.push(Entry::System(text));
    }
}

#[cfg(test)]
pub(crate) use tests::info as test_info;

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn info() -> AppInfo {
        AppInfo {
            version: "0.1.0".into(),
            cwd: "~/src/project".into(),
            model: None,
            profile: "empty".into(),
            permission_mode: "ask".into(),
            ui: UiSettings::default(),
            no_color: false,
        }
    }

    fn press(app: &mut App, code: KeyCode) {
        app.on_terminal(TermEvent::Key(KeyEvent::new(code, KeyModifiers::NONE)));
    }

    fn type_str(app: &mut App, s: &str) {
        for c in s.chars() {
            press(app, KeyCode::Char(c));
        }
    }

    #[test]
    fn release_events_are_ignored() {
        let mut app = App::new(info());
        let mut ev = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE);
        ev.kind = KeyEventKind::Release;
        app.on_terminal(TermEvent::Key(ev));
        assert!(app.input.is_empty());
    }

    #[test]
    fn slash_popup_runs_help_and_quit() {
        let mut app = App::new(info());
        type_str(&mut app, "/hel");
        assert_eq!(app.popup_matches().first().map(|c| c.name), Some("help"));
        press(&mut app, KeyCode::Enter);
        assert!(app.modal.is_some());
        press(&mut app, KeyCode::Esc);
        assert!(app.modal.is_none());
        type_str(&mut app, "/exit");
        assert_eq!(app.popup_matches().first().map(|c| c.name), Some("quit"));
        press(&mut app, KeyCode::Enter);
        assert!(app.should_quit());
    }

    #[test]
    fn messages_become_user_turns_and_stream_back() {
        let mut app = App::new(info());
        type_str(&mut app, "hello");
        press(&mut app, KeyCode::Enter);
        assert!(matches!(app.take_ops().as_slice(), [Op::UserTurn { .. }]));
        app.on_engine(Event::TurnStarted {
            turn: cutoffharness_protocol::TurnId(1),
        });
        app.on_engine(Event::AssistantDelta { text: "hi ".into() });
        app.on_engine(Event::AssistantDelta {
            text: "there".into(),
        });
        assert_eq!(
            app.transcript.last(),
            Some(&Entry::Assistant("hi there".into()))
        );
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.take_ops(), vec![Op::Interrupt]);
    }

    #[test]
    fn models_modal_lists_catalog_and_switches() {
        use cutoffharness_protocol::{ModelInfo, ProviderModels};
        let mut app = App::new(info());
        type_str(&mut app, "/models");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.take_ops(), vec![Op::ListModels]);
        app.on_engine(Event::ModelCatalog {
            providers: vec![
                ProviderModels {
                    id: "charm".into(),
                    available: true,
                    note: "2 models".into(),
                    models: vec![
                        ModelInfo {
                            id: "glm-5.3".into(),
                            alias: None,
                            context_window: Some(1_000_000),
                        },
                        ModelInfo {
                            id: "qwen3.8-flash".into(),
                            alias: None,
                            context_window: None,
                        },
                    ],
                },
                ProviderModels {
                    id: "novita".into(),
                    available: false,
                    note: "set NOVITA_API_KEY to use this provider".into(),
                    models: Vec::new(),
                },
            ],
            current: Some("charm/glm-5.3".into()),
        });
        type_str(&mut app, "qwen");
        press(&mut app, KeyCode::Enter);
        assert!(app.modal.is_none());
        assert_eq!(
            app.take_ops(),
            vec![Op::SetModel {
                provider: ProviderId("charm".into()),
                model: ModelId("qwen3.8-flash".into()),
            }]
        );
    }

    #[test]
    fn double_ctrl_c_quits() {
        let mut app = App::new(info());
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        app.on_terminal(TermEvent::Key(ctrl_c));
        assert!(!app.should_quit());
        app.on_terminal(TermEvent::Key(ctrl_c));
        assert!(app.should_quit());
    }
}
