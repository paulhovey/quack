use std::cell::Cell;
use std::time::{Duration, Instant};

use crossterm::event::{
    Event as TermEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind,
};
use quack_config::settings::{ShowReasoning, StartupPanel, UiSettings};
use quack_config::{LayerStatus, Loaded, SettingRow};
use quack_protocol::{
    ApprovalDecision, ApprovalRequest, CallId, Event, InputItem, LoadState, ModelId, Op,
    ProviderId, ReplayItem, SessionId, Subsystem, TurnOverrides, WriteScope,
};

use crate::commands::{self, COMMANDS, Command};
use crate::fuzzy;
use crate::input::Input;
use crate::modal::{self, Modal, ModalOutcome};
use crate::theme::{Glyphs, Theme};

const DOUBLE_CTRL_C: Duration = Duration::from_millis(1500);
const DOUBLE_ESC: Duration = Duration::from_millis(600);
/// Spinner advances every N frames (16 ms each).
const SPIN_EVERY: u64 = 5;
/// Welcome duck advances animation every N frames (16 ms each).
const DUCK_ANIM_EVERY: u64 = 8;
/// Transcript rows one mouse-wheel notch scrolls.
const WHEEL_ROWS: usize = 3;

/// Static facts the UI shows, captured once at startup.
#[derive(Debug, Clone)]
pub struct AppInfo {
    pub version: String,
    pub cwd: String,
    /// The checked-out git branch, or a short commit when detached.
    pub branch: Option<String>,
    pub model: Option<String>,
    pub profile: String,
    pub permission_mode: String,
    pub ui: UiSettings,
    pub no_color: bool,
    /// Effective settings for `/config`.
    pub settings: Vec<SettingRow>,
    /// `(layer, status and file)` rows for `/config`.
    pub layers: Vec<(String, String)>,
    /// `(list, rule)` from `[permissions]`, e.g. `("allow", "shell:git status*")`.
    pub rules: Vec<(String, String)>,
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
            branch: git_branch(&config.dirs.workspace_root),
            cwd,
            model: Some(config.settings.model.default.clone()).filter(|m| !m.is_empty()),
            profile: "empty".into(),
            permission_mode: config.settings.permissions.mode.label().to_owned(),
            ui: config.settings.ui.clone(),
            no_color: std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()),
            settings: config.setting_rows(),
            rules: {
                let p = &config.settings.permissions;
                [("deny", &p.deny), ("ask", &p.ask), ("allow", &p.allow)]
                    .into_iter()
                    .flat_map(|(k, list)| list.iter().map(move |r| (k.to_owned(), r.clone())))
                    .collect()
            },
            layers: config
                .layers
                .iter()
                .map(|l| {
                    let status = match &l.status {
                        LayerStatus::Loaded => "loaded".to_owned(),
                        LayerStatus::Missing => "no config.toml".to_owned(),
                        LayerStatus::Failed => "failed to parse (see config check)".to_owned(),
                        LayerStatus::Skipped(why) => format!("skipped: {why}"),
                    };
                    let file = l.dir.as_ref().map_or_else(String::new, |d| {
                        format!("\n{}", d.join("config.toml").display())
                    });
                    (l.kind.label().to_owned(), format!("{status}{file}"))
                })
                .collect(),
        }
    }
}

/// The branch in `.git/HEAD` of `dir` or a parent; worktrees and submodules point at
/// their git dir with a `gitdir:` file.
fn git_branch(dir: &std::path::Path) -> Option<String> {
    let git = dir
        .ancestors()
        .map(|d| d.join(".git"))
        .find(|g| g.exists())?;
    let git = if git.is_file() {
        let link = std::fs::read_to_string(&git).ok()?;
        let target = link.strip_prefix("gitdir:")?.trim();
        git.parent()?.join(target)
    } else {
        git
    };
    let head = std::fs::read_to_string(git.join("HEAD")).ok()?;
    let head = head.trim();
    Some(match head.strip_prefix("ref: refs/heads/") {
        Some(branch) => branch.to_owned(),
        None => head.chars().take(7).collect(),
    })
}

/// A row of the slash popup: a built-in command or a skill.
#[derive(Debug, Clone)]
pub(crate) struct PopupItem {
    pub name: String,
    pub args: String,
    pub description: String,
    /// Shown after the description, e.g. a milestone or `skill`.
    pub hint: Option<&'static str>,
    /// `None` for skills.
    pub command: Option<&'static Command>,
}

impl PopupItem {
    fn command(c: &'static Command) -> Self {
        Self {
            name: c.name.to_owned(),
            args: c.args.to_owned(),
            description: c.description.to_owned(),
            hint: c.pending,
            command: Some(c),
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
    /// What a finished turn took, shown as a dim rule.
    TurnSummary(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ToolEntry {
    pub call_id: CallId,
    pub name: String,
    pub summary: String,
    /// `None` while running.
    pub ok: Option<bool>,
    pub result: String,
    /// Live output while running; the full diff or output once done.
    pub detail: String,
    pub started: Option<Instant>,
    pub took: Option<Duration>,
}

/// A plan item's state, from the `todo` tool's `[x]`/`[~]`/`[ ]` marks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlanState {
    Done,
    Active,
    Pending,
}

/// Plan items from the `todo` tool's rendered checklist.
pub(crate) fn parse_plan(text: &str) -> Vec<(PlanState, String)> {
    text.lines()
        .filter_map(|l| {
            let (mark, rest) = (l.get(..3)?, l.get(3..)?.trim());
            let state = match mark {
                "[x]" => PlanState::Done,
                "[~]" => PlanState::Active,
                "[ ]" => PlanState::Pending,
                _ => return None,
            };
            Some((state, rest.to_owned()))
        })
        .collect()
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
    /// Transcript rows scrolled up from the bottom. Drawing clamps it and keeps the view
    /// still while new rows arrive below, so it lives in a `Cell`.
    pub(crate) scroll: Cell<usize>,
    /// `(rows, height)` of the transcript as last drawn.
    pub(crate) view: Cell<(usize, usize)>,
    /// How reasoning shows right now: `ui.show_reasoning`, flipped by Ctrl-T.
    pub(crate) show_reasoning: ShowReasoning,
    pub(crate) popup_selected: usize,
    popup_dismissed: bool,
    pub(crate) turn_running: bool,
    pub(crate) queued: bool,
    pub(crate) approval: Option<Approval>,
    /// `(used, budget)` tokens from the last response.
    pub(crate) context: Option<(u32, u32)>,
    /// Generation speed of the last response, tokens per second.
    pub(crate) speed: Option<f32>,
    /// When the running turn started, and how many tools it has called.
    pub(crate) turn_started: Option<Instant>,
    turn_tools: u32,
    /// The model's current plan, from its last `todo` call.
    pub(crate) plan: Vec<(PlanState, String)>,
    panel_collapsed: bool,
    /// A `/resume` is in flight: clear the transcript when it lands.
    resuming: bool,
    /// The mode to go back to when plan mode ends.
    mode_before_plan: String,
    /// The mode `/yolo` turns back to.
    mode_before_yolo: String,
    /// Text for the terminal's clipboard (OSC 52), written by the event loop.
    clipboard: Option<String>,
    /// Installed skills, runnable as `/<name>`.
    skills: Vec<quack_protocol::SkillInfo>,
    /// Servers found by the last `/setup`.
    detected: Vec<quack_protocol::DetectedServer>,
    /// A `.quack/` waiting for a trust decision.
    trust: Option<modal::TrustPrompt>,
    frame: u64,
    dirty: bool,
    quit: bool,
    ops: Vec<Op>,
    last_ctrl_c: Option<Instant>,
    /// For Esc Esc, which opens the rewind picker.
    last_esc: Option<Instant>,
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
            scroll: Cell::new(0),
            view: Cell::new((0, 0)),
            show_reasoning: info.ui.show_reasoning,
            popup_selected: 0,
            popup_dismissed: false,
            turn_running: false,
            queued: false,
            approval: None,
            context: None,
            speed: None,
            turn_started: None,
            turn_tools: 0,
            plan: Vec::new(),
            panel_collapsed: false,
            resuming: false,
            mode_before_plan: String::new(),
            mode_before_yolo: String::new(),
            clipboard: None,
            skills: Vec::new(),
            detected: Vec::new(),
            trust: None,
            frame: 0,
            dirty: true,
            quit: false,
            ops: Vec::new(),
            last_ctrl_c: None,
            last_esc: None,
            info,
        }
    }

    pub fn should_quit(&self) -> bool {
        self.quit
    }

    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    pub fn take_clipboard(&mut self) -> Option<String> {
        self.clipboard.take()
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

    pub(crate) fn frame(&self) -> u64 {
        self.frame
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
    pub(crate) fn popup_matches(&self) -> Vec<PopupItem> {
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
        let mut items: Vec<PopupItem> = hits.into_iter().map(PopupItem::command).collect();
        items.extend(
            fuzzy::filter(query, self.skills.iter().map(|s| s.name.as_str()))
                .into_iter()
                .map(|i| PopupItem {
                    name: self.skills[i].name.clone(),
                    args: "[request]".into(),
                    description: self.skills[i].description.clone(),
                    hint: Some("skill"),
                    command: None,
                }),
        );
        items
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
        // The welcome duck animates on startup while the transcript is empty.
        if self.transcript.is_empty()
            && !self.info.ui.reduced_motion
            && self.frame.is_multiple_of(DUCK_ANIM_EVERY)
        {
            self.dirty = true;
        }
        // The elapsed-time clock in the status bar.
        if self.turn_running && self.frame.is_multiple_of(60) {
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
            Event::ConfigReloaded => {
                self.startup_done = false;
                self.panel_collapsed = false;
                self.trust = None;
                self.note("config reloaded".into());
            }
            Event::ProjectTrust {
                path,
                summary,
                changed,
            } => {
                let prompt = modal::TrustPrompt {
                    path,
                    summary,
                    changed,
                };
                // Offer the choice once it arrives, unless the user is busy with something.
                if self.modal.is_none() && self.approval.is_none() {
                    self.modal = Some(modal::trust(Some(&prompt), &self.theme));
                }
                self.trust = Some(prompt);
            }
            Event::TurnQueued { waiting_for } => {
                self.queued = true;
                self.note(format!("queued, waiting for {waiting_for}"));
            }
            Event::SessionConfigured { session } => {
                self.info.model = session.model;
                self.info.profile = session.profile;
            }
            Event::ModelCatalog { providers, current }
                if self.modal.as_ref().is_some_and(|m| m.title == "Providers") =>
            {
                self.modal = Some(modal::providers(
                    Some(&providers),
                    current.as_deref(),
                    &self.theme,
                ));
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
            Event::Skills { skills } => self.skills = skills,
            Event::FilesRestored {
                restored,
                conflicts,
            } => {
                if !restored.is_empty() {
                    self.note(format!(
                        "undone: {} (the model is told at the start of your next message)",
                        restored.join(", ")
                    ));
                }
                if !conflicts.is_empty() {
                    self.transcript.push(Entry::Warning(format!(
                        "not undone, changed since the edit: {}. /undo --force overwrites them",
                        conflicts.join(", ")
                    )));
                }
            }
            Event::RewindPoints { points } => {
                if self.modal.as_ref().is_some_and(|m| m.title == "Rewind") {
                    self.modal = Some(modal::rewind(Some(&points), &self.theme));
                }
            }
            Event::Rewound { back, text } => {
                // Cut the transcript at the same user message, counted from the end.
                let at = self
                    .transcript
                    .iter()
                    .enumerate()
                    .rev()
                    .filter(|(_, e)| matches!(e, Entry::User(_)))
                    .nth(back.saturating_sub(1) as usize)
                    .map(|(i, _)| i);
                if let Some(i) = at {
                    self.transcript.truncate(i);
                }
                self.context = None;
                if self.input.is_empty() {
                    self.input.set(&text);
                }
                self.note("rewound; the message is back in the input to edit or send again".into());
            }
            Event::McpServers { servers } => {
                if self.modal.as_ref().is_some_and(|m| m.title == "MCP") {
                    self.modal = Some(modal::mcp(Some(&servers), &self.theme));
                }
            }
            Event::LspServers { servers } => {
                if self.modal.as_ref().is_some_and(|m| m.title == "LSP") {
                    self.modal = Some(modal::lsp(Some(&servers), &self.theme));
                }
            }
            Event::ServersDetected { servers } => {
                if self.modal.as_ref().is_some_and(|m| m.title == "Setup") {
                    self.modal = Some(modal::setup(Some(&servers), &self.theme));
                }
                self.detected = servers;
            }
            Event::ProviderAdded { id, path } => self.note(match path {
                Some(path) => format!("added provider {id} ({path}); switching…"),
                None => format!("using the existing provider {id}; switching…"),
            }),
            Event::DoctorReport { checks } => {
                if self.modal.as_ref().is_some_and(|m| m.title == "Doctor") {
                    self.modal = Some(modal::doctor(Some(&checks), &self.theme));
                }
            }
            Event::MemoryNotes { notes } => {
                if self.modal.as_ref().is_some_and(|m| m.title == "Memory") {
                    self.modal = Some(modal::memory(Some(&notes), &self.theme));
                }
            }
            Event::ContextReport { report } => {
                if self.modal.as_ref().is_some_and(|m| m.title == "Context") {
                    self.modal = Some(modal::context(Some(&report), &self.theme));
                }
            }
            Event::SessionList { sessions } => {
                if self.modal.as_ref().is_some_and(|m| m.title == "Sessions") {
                    self.modal = Some(modal::sessions(Some(&sessions), &self.theme));
                }
            }
            Event::SettingSaved {
                key,
                value,
                path,
                note,
            } => self.on_setting_saved(&key, &value, &path, note),
            Event::SessionResumed { session, items } => {
                if std::mem::take(&mut self.resuming) {
                    self.transcript.clear();
                    self.context = None;
                }
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
                            name: summary.split_whitespace().next().unwrap_or_default().to_owned(),
                            summary,
                            ok: Some(ok),
                            result,
                            detail: String::new(),
                            started: None,
                            took: None,
                        }),
                        ReplayItem::Compacted => {
                            Entry::System("── earlier conversation summarized ──".into())
                        }
                    });
                }
            }
            Event::TurnStarted { .. } => {
                self.queued = false;
                self.turn_running = true;
                self.turn_started = Some(Instant::now());
                self.turn_tools = 0;
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
            Event::ToolCallBegin { call } => {
                self.turn_tools += 1;
                self.transcript.push(Entry::Tool(ToolEntry {
                    call_id: call.call_id,
                    name: call.name,
                    summary: call.summary,
                    ok: None,
                    result: String::new(),
                    detail: String::new(),
                    started: Some(Instant::now()),
                    took: None,
                }));
            }
            Event::ToolCallOutputDelta { call_id, chunk } => {
                if let Some(t) = self.tool_mut(&call_id) {
                    t.detail.push_str(&chunk);
                }
            }
            Event::ToolCallEnd { call_id, result } => {
                let mut plan = None;
                if let Some(t) = self.tool_mut(&call_id) {
                    t.ok = Some(result.ok);
                    t.result = result.summary;
                    t.detail = result.detail.unwrap_or_default();
                    t.took = t.started.map(|s| s.elapsed());
                    if t.name == "todo" && result.ok {
                        plan = Some(parse_plan(&t.detail));
                    }
                }
                if let Some(plan) = plan {
                    self.plan = plan;
                }
            }
            Event::ApprovalRequest { call_id, request } => {
                self.approval = Some(Approval { call_id, request });
            }
            Event::ContextUpdate { used, budget, .. } => self.context = Some((used, budget)),
            Event::Speed { tokens_per_sec, .. } => {
                if tokens_per_sec.is_some() {
                    self.speed = tokens_per_sec;
                }
            }
            Event::ContextPruned { report } => self.note(format!(
                "pruned {} old tool result{} (~{} tokens)",
                report.items.len(),
                if report.items.len() == 1 { "" } else { "s" },
                report.removed_tokens
            )),
            Event::Compacting { reason } => {
                self.note(format!("summarizing earlier conversation ({reason})…"))
            }
            Event::Compacted {
                summary_tokens,
                replaced,
            } => self.note(format!(
                "── earlier conversation summarized: {replaced} messages → ~{summary_tokens} tokens ──"
            )),
            Event::TurnComplete { usage, .. } => {
                self.turn_running = false;
                self.approval = None;
                if let Some(started) = self.turn_started.take() {
                    let mut parts = vec![crate::theme::elapsed(started.elapsed())];
                    match self.turn_tools {
                        0 => {}
                        1 => parts.push("1 tool".into()),
                        n => parts.push(format!("{n} tools")),
                    }
                    if usage.completion_tokens > 0 {
                        parts.push(format!(
                            "{} tokens out",
                            crate::theme::tokens(usage.completion_tokens)
                        ));
                    }
                    if let Some(tps) = self.speed {
                        parts.push(format!("{tps:.0} t/s"));
                    }
                    self.transcript.push(Entry::TurnSummary(parts.join(" · ")));
                }
                if self.info.permission_mode == "plan" {
                    self.note(
                        "plan mode · /plan accept carries out the plan; reply to revise it".into(),
                    );
                }
            }
            Event::PermissionMode { mode } => {
                if mode == "plan" && self.info.permission_mode != "plan" {
                    self.note("plan mode: the model looks around and proposes a plan; nothing is changed (/plan off leaves)".into());
                }
                if mode == "yolo" && self.info.permission_mode != "yolo" {
                    self.note("yolo: nothing asks and the sandbox is off; only the built-in deny list and your deny rules still apply · /yolo turns it off".into());
                } else if mode != "yolo" && self.info.permission_mode == "yolo" {
                    self.note(format!("yolo off · {mode} mode"));
                }
                self.info.permission_mode = mode;
            }
            Event::TurnAborted { reason, .. } => {
                self.turn_running = false;
                self.turn_started = None;
                self.approval = None;
                self.note(reason);
            }
            Event::Notice { message } => self.transcript.push(Entry::System(message)),
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
            TermEvent::Mouse(m) if self.modal.is_none() => match m.kind {
                MouseEventKind::ScrollUp => self.scroll_up(WHEEL_ROWS),
                MouseEventKind::ScrollDown => self.scroll_down(WHEEL_ROWS),
                _ => {}
            },
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
            KeyCode::Esc => {
                if self
                    .last_esc
                    .take()
                    .is_some_and(|t| t.elapsed() < DOUBLE_ESC)
                {
                    self.open_rewind();
                } else {
                    self.last_esc = Some(Instant::now());
                }
            }
            KeyCode::Enter
                if key
                    .modifiers
                    .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) =>
            {
                self.input.insert('\n');
            }
            KeyCode::Char('j') if ctrl => self.input.insert('\n'),
            KeyCode::Enter => self.submit(popup.get(self.popup_selected).cloned()),
            KeyCode::BackTab => self.cycle_mode(),
            KeyCode::Tab => {
                if let Some(cmd) = popup.get(self.popup_selected) {
                    self.input.set(&format!("/{} ", cmd.name));
                }
            }
            _ if self.scroll_key(key) => {}
            KeyCode::Char('t') if ctrl => self.toggle_reasoning(None),
            KeyCode::Up if !popup.is_empty() => {
                self.popup_selected = self.popup_selected.saturating_sub(1);
            }
            KeyCode::Down if !popup.is_empty() => {
                self.popup_selected = (self.popup_selected + 1).min(popup.len() - 1);
            }
            KeyCode::Up => self.input.history_prev(),
            KeyCode::Down => self.input.history_next(),
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

    /// Transcript scrolling keys, shared by the prompt and the approval bar. Returns
    /// whether `key` was one.
    fn scroll_key(&mut self, key: KeyEvent) -> bool {
        let page = self.view.get().1.saturating_sub(2).max(1);
        let jump = key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::SHIFT);
        match key.code {
            KeyCode::PageUp => self.scroll_up(page),
            KeyCode::PageDown => self.scroll_down(page),
            KeyCode::Up if key.modifiers.contains(KeyModifiers::SHIFT) => self.scroll_up(1),
            KeyCode::Down if key.modifiers.contains(KeyModifiers::SHIFT) => self.scroll_down(1),
            // Drawing clamps this to the first row.
            KeyCode::Home if jump => self.scroll.set(usize::MAX),
            KeyCode::End if jump => self.scroll.set(0),
            _ => return false,
        }
        self.dirty = true;
        true
    }

    fn scroll_up(&mut self, rows: usize) {
        let (total, height) = self.view.get();
        let max = total.saturating_sub(height);
        self.scroll.set((self.scroll.get() + rows).min(max));
        self.dirty = true;
    }

    fn scroll_down(&mut self, rows: usize) {
        self.scroll.set(self.scroll.get().saturating_sub(rows));
        self.dirty = true;
    }

    /// Show (`Some(true)`), hide (`Some(false)`) or flip the model's reasoning. Hiding goes
    /// back to `ui.show_reasoning`, or to a one-line summary when that is `expanded`.
    fn toggle_reasoning(&mut self, show: Option<bool>) {
        let show = show.unwrap_or(self.show_reasoning != ShowReasoning::Expanded);
        self.show_reasoning = match (show, self.info.ui.show_reasoning) {
            (true, _) => ShowReasoning::Expanded,
            (false, ShowReasoning::Expanded) => ShowReasoning::Collapsed,
            (false, configured) => configured,
        };
        // Without any reasoning on screen the change is invisible; say what happened.
        if !self
            .transcript
            .iter()
            .any(|e| matches!(e, Entry::Reasoning(_)))
        {
            let state = if show { "shown" } else { "hidden" };
            self.note(format!(
                "thinking will be {state} (Ctrl-T); nothing to show yet, \
                 and only some models send it"
            ));
        }
        self.dirty = true;
    }

    /// Whether the terminal should capture the mouse (`ui.mouse`), for wheel scrolling.
    pub fn wants_mouse(&self) -> bool {
        self.info.ui.mouse
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
            _ if self.scroll_key(key) => return,
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

    fn submit(&mut self, popup_choice: Option<PopupItem>) {
        match popup_choice {
            Some(PopupItem {
                command: Some(cmd), ..
            }) => {
                self.input.clear();
                return self.run_command(cmd, "");
            }
            // A skill: let the user add a request first.
            Some(item) => {
                self.input.set(&format!("/{} ", item.name));
                self.popup_dismissed = true;
                return;
            }
            None => {}
        }
        let text = self.input.submit();
        if text.trim().is_empty() {
            return;
        }
        self.scroll.set(0);
        if text.starts_with('/') {
            return self.execute(&text);
        }
        self.send(text);
    }

    fn send(&mut self, text: String) {
        self.panel_collapsed = true;
        self.transcript.push(Entry::User(text.clone()));
        self.ops.push(Op::UserTurn {
            items: vec![InputItem::text(text)],
            overrides: TurnOverrides::default(),
        });
    }

    fn set_mode(&mut self, mode: &str) {
        if mode == "plan" && self.info.permission_mode != "plan" {
            self.mode_before_plan = self.info.permission_mode.clone();
        }
        self.ops.push(Op::SetPermissionMode {
            mode: mode.to_owned(),
        });
    }

    /// `/yolo`: switch to yolo, or back to the mode it was turned on from.
    fn toggle_yolo(&mut self, args: &str) {
        let on = match args.trim() {
            "on" => true,
            "off" => false,
            _ => self.info.permission_mode != "yolo",
        };
        if on {
            if self.info.permission_mode != "yolo" {
                self.mode_before_yolo = self.info.permission_mode.clone();
            }
            self.set_mode("yolo");
        } else {
            let back = match self.mode_before_yolo.as_str() {
                "" | "yolo" => "ask".to_owned(),
                m => m.to_owned(),
            };
            self.set_mode(&back);
        }
    }

    /// Leave plan mode for the mode it was entered from.
    fn end_plan(&mut self) {
        let back = match self.mode_before_plan.as_str() {
            "" | "plan" => "ask".to_owned(),
            m => m.to_owned(),
        };
        self.set_mode(&back);
    }

    /// Shift-Tab: ask → auto-edit → plan → ask. Other modes join the cycle at `ask`.
    fn cycle_mode(&mut self) {
        let next = match self.info.permission_mode.as_str() {
            "ask" => "auto-edit",
            "auto-edit" => "plan",
            "plan" => {
                self.mode_before_plan.clear();
                "ask"
            }
            _ => "ask",
        };
        self.set_mode(next);
    }

    /// Run a `/command args` line.
    pub(crate) fn execute(&mut self, text: &str) {
        let rest = text.trim().trim_start_matches('/');
        let (name, args) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
        match commands::find(name) {
            Some(cmd) => self.run_command(cmd, args.trim()),
            // The engine expands `/<skill> args` into the skill's instructions.
            None if self.skills.iter().any(|s| s.name == name) => self.send(text.trim().to_owned()),
            None => self.note(format!("unknown command /{name}; /help lists commands")),
        }
    }

    fn run_command(&mut self, cmd: &Command, args: &str) {
        self.popup_selected = 0;
        match cmd.name {
            "models" if args.starts_with("default ") => {
                let reference = args["default ".len()..].trim();
                self.save_setting("model.default", &toml_string(reference));
            }
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
            "resume" if !args.is_empty() => {
                self.resuming = true;
                self.ops.push(Op::ResumeSession {
                    id: SessionId(args.to_owned()),
                });
            }
            "resume" => {
                self.modal = Some(modal::sessions(None, &self.theme));
                self.ops.push(Op::ListSessions);
            }
            "new" => {
                if self.turn_running {
                    return self.note("wait for the current turn to finish first".into());
                }
                self.transcript.clear();
                self.context = None;
                self.ops.push(Op::NewSession);
                self.note("new session; the previous one is saved (/resume)".into());
            }
            "title" if args.is_empty() => self.note("usage: /title <text>".into()),
            "title" => {
                self.ops.push(Op::SetTitle {
                    title: args.to_owned(),
                });
                self.note(format!("session renamed to “{args}”"));
            }
            "config" if args.is_empty() => {
                self.modal = Some(modal::config(
                    &self.info.settings,
                    &self.info.layers,
                    &self.theme,
                ));
            }
            "config" => {
                let rest = args.strip_prefix("set").map(str::trim_start);
                match rest.and_then(|r| r.split_once(char::is_whitespace)) {
                    Some((key, value)) if !value.trim().is_empty() => {
                        self.save_setting(key, value.trim());
                    }
                    _ => self.note("usage: /config set <key> <value>".into()),
                }
            }
            "memory" if args.is_empty() => {
                self.modal = Some(modal::memory(None, &self.theme));
                self.ops.push(Op::ListMemory);
            }
            "memory" => match args
                .strip_prefix("delete")
                .map(str::trim)
                .and_then(|r| r.split_once('/'))
            {
                Some((scope, name)) if !name.is_empty() => {
                    self.ops.push(Op::DeleteMemory {
                        scope: scope.to_owned(),
                        name: name.to_owned(),
                    });
                    self.note(format!("deleted {scope} note {name}"));
                }
                _ => self.note("usage: /memory, or /memory delete <global|project>/<name>".into()),
            },
            "providers" => {
                self.modal = Some(modal::providers(
                    None,
                    self.info.model.as_deref(),
                    &self.theme,
                ));
                self.ops.push(Op::ListModels);
            }
            "mcp" if args.starts_with("restart ") => {
                let id = args["restart ".len()..].trim().to_owned();
                self.note(format!("restarting MCP server {id}…"));
                self.ops.push(Op::RestartMcp { id });
            }
            "mcp" => {
                self.modal = Some(modal::mcp(None, &self.theme));
                self.ops.push(Op::ListMcp);
            }
            "lsp" => {
                self.modal = Some(modal::lsp(None, &self.theme));
                self.ops.push(Op::ListLsp);
            }
            "init" => {
                self.note("asking the model to write AGENTS.md…".into());
                self.ops.push(Op::InitInstructions);
            }
            "setup" if args.starts_with("use ") => {
                let mut parts = args["use ".len()..].splitn(2, ' ');
                let server = parts
                    .next()
                    .and_then(|i| i.parse::<usize>().ok())
                    .and_then(|i| self.detected.get(i));
                match (server, parts.next()) {
                    (Some(s), Some(model)) => self.ops.push(Op::AddProvider {
                        id: s.id.clone(),
                        base_url: s.base_url.clone(),
                        api: s.api.clone(),
                        model: model.to_owned(),
                    }),
                    _ => self.note("pick a server and model from /setup".into()),
                }
            }
            "setup" => {
                self.modal = Some(modal::setup(None, &self.theme));
                self.ops.push(Op::DetectServers);
            }
            "undo" => self.ops.push(Op::Undo {
                checkpoint: None,
                force: args == "--force",
            }),
            "skills" => self.modal = Some(modal::skills(&self.skills, &self.theme)),
            "doctor" => {
                self.modal = Some(modal::doctor(None, &self.theme));
                self.ops.push(Op::RunDoctor);
            }
            "thinking" => match args {
                "" => self.toggle_reasoning(None),
                "show" | "on" => self.toggle_reasoning(Some(true)),
                "hide" | "off" => self.toggle_reasoning(Some(false)),
                _ => self.note("usage: /thinking [show|hide]".into()),
            },
            "copy" => {
                let last = self.transcript.iter().rev().find_map(|e| match e {
                    Entry::Assistant(t) if !t.trim().is_empty() => Some(t.clone()),
                    _ => None,
                });
                let Some(text) = last else {
                    return self.note("nothing to copy yet".into());
                };
                let (what, text) = match args {
                    "code" => match last_code_block(&text) {
                        Some(code) => ("the last code block", code),
                        None => return self.note("the last response has no code block".into()),
                    },
                    _ => ("the last response", text),
                };
                self.note(format!(
                    "copied {what} ({} characters) through the terminal",
                    text.chars().count()
                ));
                self.clipboard = Some(text);
            }
            "permissions" if args.is_empty() => {
                self.modal = Some(modal::permissions(
                    &self.info.permission_mode,
                    &self.info.rules,
                    &self.theme,
                ));
            }
            "permissions" => self.set_mode(args),
            "yolo" => self.toggle_yolo(args),
            "plan" => match args {
                "" => self.set_mode("plan"),
                "accept" | "go" => {
                    if self.info.permission_mode != "plan" {
                        return self.note("not in plan mode; /plan starts it".into());
                    }
                    self.end_plan();
                    self.send("Carry out the plan above.".into());
                }
                "off" | "exit" => self.end_plan(),
                task => {
                    self.set_mode("plan");
                    self.send(task.to_owned());
                }
            },
            "context" => {
                self.modal = Some(modal::context(None, &self.theme));
                self.ops.push(Op::GetContext);
            }
            "compact" => self.ops.push(Op::Compact {
                focus: Some(args.to_owned()).filter(|a| !a.is_empty()),
            }),
            "rewind" if args.is_empty() => self.open_rewind(),
            "rewind" => {
                let mut words = args.split_whitespace();
                let back = words.next().and_then(|n| n.parse::<u32>().ok());
                let mut what = "both";
                let mut force = false;
                for w in words {
                    match w {
                        "--force" => force = true,
                        w => what = w,
                    }
                }
                match (back, what) {
                    (Some(back), "both" | "conversation" | "files") if back > 0 => {
                        self.ops.push(Op::Rewind {
                            back,
                            conversation: what != "files",
                            files: what != "conversation",
                            force,
                        });
                    }
                    _ => self.note("usage: /rewind [n] [both|conversation|files] [--force]".into()),
                }
            }
            "reload" => {
                self.note("reloading config…".into());
                self.ops.push(Op::ReloadConfig);
            }
            "trust" => match args {
                "" => self.modal = Some(modal::trust(self.trust.as_ref(), &self.theme)),
                "later" => {
                    self.note("skipping the project config for now; /trust to decide".into())
                }
                decision @ ("trusted" | "settings-only" | "ignored") => {
                    self.note(format!("project config: {decision}; reloading…"));
                    self.ops.push(Op::TrustProject {
                        decision: decision.to_owned(),
                    });
                }
                _ => self.note("usage: /trust [trusted|settings-only|ignored]".into()),
            },
            "help" => self.modal = Some(modal::help(&self.theme)),
            "quit" => self.quit = true,
            name => self.note(format!(
                "/{name} is planned for {}",
                cmd.pending.unwrap_or("a later milestone")
            )),
        }
    }

    fn open_rewind(&mut self) {
        if self.turn_running {
            return self.note("wait for the current turn to finish before rewinding".into());
        }
        self.modal = Some(modal::rewind(None, &self.theme));
        self.ops.push(Op::ListRewindPoints);
    }

    /// Settings from a reloaded config. What the engine reports (model, profile, mode)
    /// stays as it is until the engine says otherwise.
    pub fn reconfigure(&mut self, info: AppInfo) {
        if info.ui.show_reasoning != self.info.ui.show_reasoning {
            self.show_reasoning = info.ui.show_reasoning;
        }
        let keep = (
            self.info.model.take(),
            std::mem::take(&mut self.info.profile),
            std::mem::take(&mut self.info.permission_mode),
        );
        self.info = info;
        (
            self.info.model,
            self.info.profile,
            self.info.permission_mode,
        ) = keep;
        self.theme = Theme::resolve(&self.info.ui.theme, self.info.no_color);
        self.glyphs = Glyphs::new(self.info.ui.ascii);
        self.dirty = true;
    }

    fn note(&mut self, text: String) {
        self.transcript.push(Entry::System(text));
    }

    fn save_setting(&mut self, key: &str, value: &str) {
        self.ops.push(Op::SaveSetting {
            key: key.to_owned(),
            value: value.to_owned(),
            scope: WriteScope::User,
        });
    }

    fn on_setting_saved(&mut self, key: &str, value: &str, path: &str, note: Option<String>) {
        // Settings the UI owns apply now; the rest on the next start.
        let live = match key {
            "ui.ascii" => {
                self.info.ui.ascii = value == "true";
                self.glyphs = Glyphs::new(self.info.ui.ascii);
                true
            }
            "ui.reduced_motion" => {
                self.info.ui.reduced_motion = value == "true";
                true
            }
            "ui.theme" => {
                self.info.ui.theme = value.trim_matches('"').to_owned();
                self.theme = Theme::resolve(&self.info.ui.theme, self.info.no_color);
                true
            }
            _ => false,
        };
        if let Some(row) = self.info.settings.iter_mut().find(|r| r.key == key) {
            row.value = value.to_owned();
            row.origin = format!("user: {path}");
        }
        let when = if live {
            ""
        } else {
            " (takes effect on the next start)"
        };
        let mut text = format!("saved {key} = {value} to {path}{when}");
        if let Some(n) = note {
            text.push_str("; ");
            text.push_str(&n);
        }
        self.note(text);
    }
}

/// A TOML string literal, so ids like `true` or `1.5` stay strings.
fn toml_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(test)]
pub(crate) use tests::info as test_info;

/// The contents of the last fenced code block in `text`.
fn last_code_block(text: &str) -> Option<String> {
    let mut last = None;
    let mut current: Option<Vec<&str>> = None;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            match current.take() {
                Some(lines) => last = Some(lines.join("\n")),
                None => current = Some(Vec::new()),
            }
        } else if let Some(lines) = &mut current {
            lines.push(line);
        }
    }
    last
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn info() -> AppInfo {
        AppInfo {
            version: "0.1.0".into(),
            cwd: "~/src/project".into(),
            branch: Some("main".into()),
            model: None,
            profile: "empty".into(),
            permission_mode: "ask".into(),
            ui: UiSettings::default(),
            no_color: false,
            settings: Vec::new(),
            layers: Vec::new(),
            rules: Vec::new(),
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
    fn copy_last_response_or_code_block() {
        let mut app = App::new(info());
        type_str(&mut app, "/copy");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.take_clipboard(), None);
        app.on_engine(Event::AssistantDelta {
            text: "Run:\n```sh\ncargo test\n```\nthen\n```\nb\n```".into(),
        });
        type_str(&mut app, "/copy code");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.take_clipboard().as_deref(), Some("b"));
        type_str(&mut app, "/copy");
        press(&mut app, KeyCode::Enter);
        assert!(app.take_clipboard().is_some_and(|t| t.starts_with("Run:")));
        assert_eq!(
            last_code_block("```\na\n```\n```\nb\nc\n```").as_deref(),
            Some("b\nc")
        );
    }

    #[test]
    fn plan_mode_cycle_and_accept() {
        let mut app = App::new(info());
        let mode = |m: &str| Op::SetPermissionMode { mode: m.into() };
        press(&mut app, KeyCode::BackTab);
        assert_eq!(app.take_ops(), vec![mode("auto-edit")]);
        app.on_engine(Event::PermissionMode {
            mode: "auto-edit".into(),
        });
        type_str(&mut app, "/plan");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.take_ops(), vec![mode("plan")]);
        app.on_engine(Event::PermissionMode {
            mode: "plan".into(),
        });
        type_str(&mut app, "/plan accept");
        press(&mut app, KeyCode::Enter);
        let ops = app.take_ops();
        assert_eq!(
            ops[0],
            mode("auto-edit"),
            "back to the mode plan mode started from"
        );
        assert!(
            matches!(&ops[1], Op::UserTurn { items, .. } if items == &[InputItem::text("Carry out the plan above.")])
        );
    }

    #[test]
    fn yolo_toggles_back_to_the_previous_mode() {
        let mut app = App::new(info());
        let mode = |m: &str| Op::SetPermissionMode { mode: m.into() };
        app.on_engine(Event::PermissionMode {
            mode: "auto-edit".into(),
        });
        type_str(&mut app, "/yolo");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.take_ops(), vec![mode("yolo")]);
        app.on_engine(Event::PermissionMode {
            mode: "yolo".into(),
        });
        type_str(&mut app, "/yolo");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.take_ops(), vec![mode("auto-edit")]);
        type_str(&mut app, "/yolo off");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.take_ops(), vec![mode("auto-edit")]);
    }

    #[test]
    fn trust_prompt_opens_and_sends_the_decision() {
        let mut app = App::new(info());
        app.on_engine(Event::ProjectTrust {
            path: "/r/.quack".into(),
            summary: "1 MCP server".into(),
            changed: false,
        });
        assert_eq!(app.modal.as_ref().map(|m| m.title.as_str()), Some("Trust"));
        press(&mut app, KeyCode::Enter);
        assert!(app.modal.is_none());
        assert_eq!(
            app.take_ops(),
            vec![Op::TrustProject {
                decision: "trusted".into()
            }]
        );
        type_str(&mut app, "/reload");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.take_ops(), vec![Op::ReloadConfig]);
    }

    #[test]
    fn double_esc_opens_rewind_and_rewound_cuts_the_transcript() {
        let mut app = App::new(info());
        for text in ["one", "two", "three"] {
            app.on_engine(Event::AssistantDelta { text: "x".into() });
            app.transcript.push(Entry::User(text.into()));
        }
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.modal.as_ref().map(|m| m.title.as_str()), Some("Rewind"));
        assert_eq!(app.take_ops(), vec![Op::ListRewindPoints]);
        app.modal = None;
        app.on_engine(Event::Rewound {
            back: 2,
            text: "two".into(),
        });
        assert!(!app.transcript.contains(&Entry::User("two".into())));
        assert!(app.transcript.contains(&Entry::User("one".into())));
        assert_eq!(app.input.text(), "two");
        type_str(&mut app, "");
        app.input.clear();
        type_str(&mut app, "/rewind 1 files --force");
        press(&mut app, KeyCode::Enter);
        assert_eq!(
            app.take_ops(),
            vec![Op::Rewind {
                back: 1,
                conversation: false,
                files: true,
                force: true
            }]
        );
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
        assert_eq!(
            app.popup_matches().first().map(|c| c.name.as_str()),
            Some("help")
        );
        press(&mut app, KeyCode::Enter);
        assert!(app.modal.is_some());
        press(&mut app, KeyCode::Esc);
        assert!(app.modal.is_none());
        type_str(&mut app, "/exit");
        assert_eq!(
            app.popup_matches().first().map(|c| c.name.as_str()),
            Some("quit")
        );
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
            turn: quack_protocol::TurnId(1),
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
        use quack_protocol::{ModelInfo, ProviderModels};
        let mut app = App::new(info());
        type_str(&mut app, "/models");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.take_ops(), vec![Op::ListModels]);
        app.on_engine(Event::ModelCatalog {
            providers: vec![
                ProviderModels {
                    bundled: false,
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
                    bundled: false,
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
    fn models_modal_ctrl_s_saves_default() {
        use quack_protocol::{ModelInfo, ProviderModels};
        let mut app = App::new(info());
        type_str(&mut app, "/models");
        press(&mut app, KeyCode::Enter);
        app.take_ops();
        app.on_engine(Event::ModelCatalog {
            providers: vec![ProviderModels {
                bundled: false,
                id: "charm".into(),
                available: true,
                note: String::new(),
                models: vec![ModelInfo {
                    id: "true".into(),
                    alias: None,
                    context_window: None,
                }],
            }],
            current: None,
        });
        app.on_terminal(TermEvent::Key(KeyEvent::new(
            KeyCode::Char('s'),
            KeyModifiers::CONTROL,
        )));
        assert!(app.modal.is_none());
        assert_eq!(
            app.take_ops(),
            vec![Op::SaveSetting {
                key: "model.default".into(),
                value: "\"charm/true\"".into(),
                scope: WriteScope::User,
            }]
        );
        app.on_engine(Event::SettingSaved {
            key: "model.default".into(),
            value: "\"charm/true\"".into(),
            path: "/u/config.toml".into(),
            note: None,
        });
        assert!(
            matches!(app.transcript.last(), Some(Entry::System(t)) if t.contains("next start"))
        );
    }

    #[test]
    fn resume_picker_switches_and_replaces_transcript() {
        use quack_protocol::{SessionInfo, SessionSummary};
        let mut app = App::new(info());
        app.transcript.push(Entry::User("old".into()));
        type_str(&mut app, "/resume");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.take_ops(), vec![Op::ListSessions]);
        let summary = |id: &str, title: &str, current| SessionSummary {
            id: SessionId(id.into()),
            title: title.into(),
            model: "charm/x".into(),
            turns: 2,
            age: "1h ago".into(),
            started: "2026-10-17".into(),
            current,
        };
        app.on_engine(Event::SessionList {
            sessions: vec![
                summary("aaa", "now", true),
                summary("bbb", "fix the parser", false),
            ],
        });
        type_str(&mut app, "parser");
        press(&mut app, KeyCode::Enter);
        assert_eq!(
            app.take_ops(),
            vec![Op::ResumeSession {
                id: SessionId("bbb".into())
            }]
        );
        app.on_engine(Event::SessionResumed {
            session: SessionInfo {
                id: SessionId("bbb".into()),
                cwd: String::new(),
                model: None,
                profile: "generic".into(),
            },
            items: vec![ReplayItem::User {
                text: "fix it".into(),
            }],
        });
        assert!(!app.transcript.contains(&Entry::User("old".into())));
        assert!(app.transcript.contains(&Entry::User("fix it".into())));
    }

    #[test]
    fn config_set_saves_to_user_layer_and_applies_ui_live() {
        let mut app = App::new(info());
        type_str(&mut app, "/config set ui.ascii true");
        press(&mut app, KeyCode::Enter);
        assert_eq!(
            app.take_ops(),
            vec![Op::SaveSetting {
                key: "ui.ascii".into(),
                value: "true".into(),
                scope: WriteScope::User,
            }]
        );
        app.on_engine(Event::SettingSaved {
            key: "ui.ascii".into(),
            value: "true".into(),
            path: "/u/config.toml".into(),
            note: None,
        });
        assert!(app.info.ui.ascii);
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
