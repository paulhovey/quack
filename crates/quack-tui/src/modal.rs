//! Shared modal framework (docs/plan/06-tui.md#modal-framework): title, optional tabs,
//! an always-focused filter line, a list pane, a details pane and a key footer.
//! Every configuration command (`/models`, `/resume`, `/config`, …) is built on this.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Padding, Paragraph, Tabs, Wrap};

use crate::commands::{COMMANDS, KEYS};
use crate::fuzzy;
use crate::theme::{Glyphs, Theme};

/// Below this size the modal goes full-screen with a single column.
const MIN_SPLIT: (u16, u16) = (80, 24);

#[derive(Debug, Clone)]
pub(crate) struct ModalItem {
    pub label: String,
    /// Short right-aligned text in the list (state, milestone, size).
    pub hint: String,
    pub detail: Vec<Line<'static>>,
    /// Text inserted into the input when the item is activated, if any.
    pub action: Option<String>,
    /// Run `action` as a command immediately instead of inserting it.
    pub run: bool,
    /// Command run by the modal's secondary key (Ctrl-S), if any.
    pub alt: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct ModalTab {
    pub title: String,
    pub items: Vec<ModalItem>,
}

#[derive(Debug)]
pub(crate) struct Modal {
    pub title: String,
    tabs: Vec<ModalTab>,
    tab: usize,
    filter: String,
    filtered: Vec<usize>,
    selected: usize,
    footer: Vec<(&'static str, &'static str)>,
}

/// The secondary action key, shown in the footer when a modal has one.
const ALT_KEY: &str = "^s";

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ModalOutcome {
    None,
    Close,
    /// Close and put this text in the input.
    Insert(String),
    /// Close and run this command.
    Run(String),
}

impl Modal {
    pub fn new(title: impl Into<String>, tabs: Vec<ModalTab>) -> Self {
        let mut m = Self {
            title: title.into(),
            tabs,
            tab: 0,
            filter: String::new(),
            filtered: Vec::new(),
            selected: 0,
            footer: vec![
                ("↑↓", "move"),
                ("tab", "switch tab"),
                ("⏎", "use"),
                ("esc", "close"),
            ],
        };
        m.refilter();
        m
    }

    /// Name the secondary action (Ctrl-S) in the footer.
    pub fn with_alt(mut self, what: &'static str) -> Self {
        self.footer.insert(self.footer.len() - 1, (ALT_KEY, what));
        self
    }

    fn items(&self) -> &[ModalItem] {
        self.tabs.get(self.tab).map_or(&[], |t| t.items.as_slice())
    }

    fn refilter(&mut self) {
        self.filtered = fuzzy::filter(&self.filter, self.items().iter().map(|i| i.label.as_str()));
        self.selected = self.selected.min(self.filtered.len().saturating_sub(1));
    }

    pub fn selected_item(&self) -> Option<&ModalItem> {
        self.filtered
            .get(self.selected)
            .and_then(|i| self.items().get(*i))
    }

    pub fn paste(&mut self, text: &str) {
        self.filter
            .push_str(text.lines().next().unwrap_or_default());
        self.refilter();
    }

    pub fn on_key(&mut self, key: KeyEvent) -> ModalOutcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc if !self.filter.is_empty() => {
                self.filter.clear();
                self.refilter();
            }
            KeyCode::Esc => return ModalOutcome::Close,
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Char('p') if ctrl => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down => self.move_down(),
            KeyCode::Char('n') if ctrl => self.move_down(),
            KeyCode::Char('s') if ctrl => {
                if let Some(cmd) = self.selected_item().and_then(|i| i.alt.clone()) {
                    return ModalOutcome::Run(cmd);
                }
            }
            KeyCode::PageUp => self.selected = self.selected.saturating_sub(10),
            KeyCode::PageDown => {
                self.selected = (self.selected + 10).min(self.filtered.len().saturating_sub(1));
            }
            KeyCode::Tab | KeyCode::BackTab if self.tabs.len() > 1 => {
                let n = self.tabs.len();
                self.tab = if key.code == KeyCode::Tab {
                    (self.tab + 1) % n
                } else {
                    (self.tab + n - 1) % n
                };
                self.selected = 0;
                self.refilter();
            }
            KeyCode::Enter => {
                return match self.selected_item() {
                    Some(ModalItem {
                        action: Some(text),
                        run,
                        ..
                    }) => {
                        if *run {
                            ModalOutcome::Run(text.clone())
                        } else {
                            ModalOutcome::Insert(text.clone())
                        }
                    }
                    _ => ModalOutcome::None,
                };
            }
            KeyCode::Backspace => {
                self.filter.pop();
                self.refilter();
            }
            KeyCode::Char(c) if !ctrl => {
                self.filter.push(c);
                self.selected = 0;
                self.refilter();
            }
            _ => {}
        }
        ModalOutcome::None
    }

    fn move_down(&mut self) {
        self.selected = (self.selected + 1).min(self.filtered.len().saturating_sub(1));
    }

    pub fn draw(&self, f: &mut Frame<'_>, theme: &Theme, glyphs: &Glyphs) {
        let screen = f.area();
        let narrow = screen.width < MIN_SPLIT.0 || screen.height < MIN_SPLIT.1;
        let area = if narrow {
            screen
        } else {
            centered(screen, 90, 85)
        };
        f.render_widget(Clear, area);
        let block = Block::bordered()
            .border_set(glyphs.border)
            .border_style(theme.border)
            .title(Span::styled(format!(" {} ", self.title), theme.accent));
        let inner = block.inner(area);
        f.render_widget(block, area);

        let tabs_h = u16::from(self.tabs.len() > 1);
        let [tabs_area, filter_area, body, footer] = Layout::vertical([
            Constraint::Length(tabs_h),
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .areas(inner);

        if tabs_h > 0 {
            let titles: Vec<String> = self.tabs.iter().map(|t| t.title.clone()).collect();
            f.render_widget(
                Tabs::new(titles)
                    .select(self.tab)
                    .style(theme.dim)
                    .highlight_style(theme.accent.add_modifier(Modifier::BOLD)),
                tabs_area,
            );
        }
        let filter_line = Line::from(vec![
            Span::styled(" filter: ", theme.dim),
            Span::raw(self.filter.clone()),
        ]);
        f.render_widget(Paragraph::new(filter_line), filter_area);
        let cursor_x = filter_area.x + 9 + u16::try_from(self.filter.chars().count()).unwrap_or(0);
        f.set_cursor_position((
            cursor_x.min(filter_area.right().saturating_sub(1)),
            filter_area.y,
        ));

        let (list_area, detail_area) = if narrow {
            let [l, d] = Layout::vertical([Constraint::Percentage(60), Constraint::Percentage(40)])
                .areas(body);
            (l, d)
        } else {
            let [l, d] =
                Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)])
                    .areas(body);
            (l, d)
        };
        self.draw_list(f, list_area, theme, glyphs);
        self.draw_details(f, detail_area, theme, glyphs);

        let mut spans = vec![Span::raw(" ")];
        for (key, what) in &self.footer {
            if *key == "tab" && self.tabs.len() < 2 {
                continue;
            }
            spans.push(Span::styled(*key, theme.accent));
            spans.push(Span::styled(format!(" {what}   "), theme.dim));
        }
        f.render_widget(Paragraph::new(Line::from(spans)), footer);
    }

    fn draw_list(&self, f: &mut Frame<'_>, area: Rect, theme: &Theme, glyphs: &Glyphs) {
        let block = Block::bordered()
            .border_set(glyphs.border)
            .border_style(theme.border);
        let width = usize::from(block.inner(area).width);
        let rows: Vec<ListItem<'_>> = self
            .filtered
            .iter()
            .filter_map(|i| self.items().get(*i))
            .map(|item| {
                let used = item.label.chars().count() + item.hint.chars().count() + 1;
                let pad = " ".repeat(width.saturating_sub(used + 1));
                ListItem::new(Line::from(vec![
                    Span::raw(format!(" {}", item.label)),
                    Span::raw(pad),
                    Span::styled(item.hint.clone(), theme.dim),
                ]))
            })
            .collect();
        let empty = rows.is_empty();
        let list = List::new(rows).block(block).highlight_style(theme.selected);
        let mut state = ListState::default().with_selected((!empty).then_some(self.selected));
        f.render_stateful_widget(list, area, &mut state);
    }

    fn draw_details(&self, f: &mut Frame<'_>, area: Rect, theme: &Theme, glyphs: &Glyphs) {
        let block = Block::bordered()
            .border_set(glyphs.border)
            .border_style(theme.border)
            .padding(Padding::horizontal(1));
        let lines = self.selected_item().map_or_else(
            || vec![Line::styled("no matches", theme.dim)],
            |i| i.detail.clone(),
        );
        f.render_widget(
            Paragraph::new(lines)
                .block(block)
                .wrap(Wrap { trim: false }),
            area,
        );
    }
}

pub(crate) fn centered(area: Rect, pct_w: u16, pct_h: u16) -> Rect {
    let w = area.width * pct_w / 100;
    let h = area.height * pct_h / 100;
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}

/// The `/help` modal: commands and keys.
pub(crate) fn help(theme: &Theme) -> Modal {
    let field = |k: &str, v: String| {
        Line::from(vec![
            Span::styled(format!("{k:<10}"), theme.dim),
            Span::raw(v),
        ])
    };
    let commands = COMMANDS
        .iter()
        .map(|c| {
            let mut detail = vec![
                Line::styled(format!("/{} {}", c.name, c.args), theme.accent),
                Line::raw(""),
                Line::raw(c.description),
                Line::raw(""),
            ];
            if !c.aliases.is_empty() {
                let aliases: Vec<String> = c.aliases.iter().map(|a| format!("/{a}")).collect();
                detail.push(field("aliases", aliases.join(", ")));
            }
            detail.push(field(
                "status",
                c.pending
                    .map_or_else(|| "available".into(), |m| format!("planned for {m}")),
            ));
            ModalItem {
                label: format!("/{}", c.name),
                hint: c.pending.unwrap_or_default().to_owned(),
                detail,
                action: Some(format!(
                    "/{}{}",
                    c.name,
                    if c.args.is_empty() { "" } else { " " }
                )),
                run: false,
                alt: None,
            }
        })
        .collect();
    let keys = KEYS
        .iter()
        .map(|(k, what)| ModalItem {
            label: (*k).to_owned(),
            hint: String::new(),
            detail: vec![
                Line::styled(*k, theme.accent),
                Line::raw(""),
                Line::raw(*what),
            ],
            action: None,
            run: false,
            alt: None,
        })
        .collect();
    Modal::new(
        "Help",
        vec![
            ModalTab {
                title: "Commands".into(),
                items: commands,
            },
            ModalTab {
                title: "Keys".into(),
                items: keys,
            },
        ],
    )
}

/// The `/models` modal. `catalog` is `None` while the engine is still listing models.
pub(crate) fn models(
    catalog: Option<&[quack_protocol::ProviderModels]>,
    current: Option<&str>,
    theme: &Theme,
) -> Modal {
    let field = |k: &str, v: String| {
        Line::from(vec![
            Span::styled(format!("{k:<10}"), theme.dim),
            Span::raw(v),
        ])
    };
    let mut items = Vec::new();
    match catalog {
        None => items.push(ModalItem {
            label: "listing models…".into(),
            hint: String::new(),
            detail: vec![Line::styled(
                "Asking each provider for its models.",
                theme.dim,
            )],
            action: None,
            run: false,
            alt: None,
        }),
        Some(providers) => {
            // The bundle's providers that are not set up are listed in /providers.
            for p in providers.iter().filter(|p| p.available || !p.bundled) {
                if !p.available {
                    items.push(ModalItem {
                        label: format!("{} (unavailable)", p.id),
                        hint: String::new(),
                        detail: vec![
                            Line::styled(p.id.clone(), theme.accent),
                            Line::raw(""),
                            Line::raw(p.note.clone()),
                        ],
                        action: None,
                        run: false,
                        alt: None,
                    });
                    continue;
                }
                for m in &p.models {
                    let name = m.alias.clone().unwrap_or_else(|| m.id.clone());
                    let reference = format!("{}/{}", p.id, name);
                    let is_current = current == Some(reference.as_str());
                    let ctx = m
                        .context_window
                        .map_or_else(String::new, crate::theme::tokens);
                    let mut detail = vec![
                        Line::styled(reference.clone(), theme.accent),
                        Line::raw(""),
                        field("provider", p.id.clone()),
                        field("model id", m.id.clone()),
                        field(
                            "context",
                            m.context_window
                                .map_or_else(|| "unknown".into(), |c| format!("{c} tokens")),
                        ),
                        Line::raw(""),
                    ];
                    detail.push(if is_current {
                        Line::styled("in use", theme.ok)
                    } else {
                        Line::styled("⏎ switch to this model for the session", theme.dim)
                    });
                    detail.push(Line::styled(
                        "^s save as the default model (user config.toml)",
                        theme.dim,
                    ));
                    items.push(ModalItem {
                        label: if is_current {
                            format!("{reference} ●")
                        } else {
                            reference.clone()
                        },
                        hint: ctx,
                        detail,
                        action: Some(format!("/models {reference}")),
                        run: true,
                        alt: Some(format!("/models default {reference}")),
                    });
                }
            }
            if items.is_empty() {
                items.push(placeholder(
                    "no models ready",
                    "Start a server on this machine and run /setup, or run `quack \
                     setup` in a terminal to give a hosted service its key. /providers lists \
                     what is configured.",
                    theme,
                ));
            }
        }
    }
    Modal::new(
        "Models",
        vec![ModalTab {
            title: "Models".into(),
            items,
        }],
    )
    .with_alt("save as default")
}

fn field(theme: &Theme, k: &str, v: String) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{k:<10}"), theme.dim),
        Span::raw(v),
    ])
}

fn placeholder(label: &str, detail: &str, theme: &Theme) -> ModalItem {
    ModalItem {
        label: label.to_owned(),
        hint: String::new(),
        detail: vec![Line::styled(detail.to_owned(), theme.dim)],
        action: None,
        run: false,
        alt: None,
    }
}

/// The `/resume` modal. `sessions` is `None` while the engine is listing them.
pub(crate) fn sessions(
    sessions: Option<&[quack_protocol::SessionSummary]>,
    theme: &Theme,
) -> Modal {
    let items = match sessions {
        None => vec![placeholder(
            "listing sessions…",
            "Reading this project's sessions.",
            theme,
        )],
        Some([]) => vec![placeholder(
            "no saved sessions",
            "Sessions are saved as you work; there are none for this project yet.",
            theme,
        )],
        Some(list) => list
            .iter()
            .map(|s| {
                let title = if s.title.is_empty() {
                    "(untitled)".to_owned()
                } else {
                    s.title.clone()
                };
                let mut detail = vec![
                    Line::styled(title.clone(), theme.accent),
                    Line::raw(""),
                    field(theme, "id", s.id.0.clone()),
                    field(theme, "started", s.started.clone()),
                    field(theme, "updated", s.age.clone()),
                    field(theme, "turns", s.turns.to_string()),
                    field(theme, "model", s.model.clone()),
                    Line::raw(""),
                ];
                detail.push(if s.current {
                    Line::styled("this session", theme.ok)
                } else {
                    Line::styled("⏎ resume it (the current session stays saved)", theme.dim)
                });
                ModalItem {
                    label: if s.current {
                        format!("{title} ●")
                    } else {
                        title
                    },
                    hint: format!("{} · {}t", s.age, s.turns),
                    detail,
                    action: (!s.current).then(|| format!("/resume {}", s.id)),
                    run: true,
                    alt: None,
                }
            })
            .collect(),
    };
    Modal::new(
        "Sessions",
        vec![ModalTab {
            title: "Sessions".into(),
            items,
        }],
    )
}

/// The `/config` modal: every setting with its value and where it came from.
pub(crate) fn config(
    rows: &[quack_config::SettingRow],
    files: &[(String, String)],
    theme: &Theme,
) -> Modal {
    let settings = rows
        .iter()
        .map(|r| {
            let mut detail = vec![Line::styled(r.key.clone(), theme.accent), Line::raw("")];
            if !r.description.is_empty() {
                detail.push(Line::raw(r.description.clone()));
                detail.push(Line::raw(""));
            }
            detail.push(field(theme, "value", r.value.clone()));
            detail.push(field(theme, "from", r.origin.clone()));
            detail.push(Line::raw(""));
            let (action, run) = if r.locked {
                detail.push(Line::styled("locked by the system policy", theme.warn));
                (None, false)
            } else if r.is_bool {
                let flipped = if r.value == "true" { "false" } else { "true" };
                detail.push(Line::styled(
                    format!("⏎ set to {flipped} in the user config.toml"),
                    theme.dim,
                ));
                (Some(format!("/config set {} {flipped}", r.key)), true)
            } else {
                detail.push(Line::styled(
                    "⏎ edit the value, then press ⏎ to save it to the user config.toml",
                    theme.dim,
                ));
                (Some(format!("/config set {} {}", r.key, r.value)), false)
            };
            let mut hint: String = r.value.chars().take(24).collect();
            if r.value.chars().count() > 24 {
                hint.push('…');
            }
            ModalItem {
                label: r.key.clone(),
                hint,
                detail,
                action,
                run,
                alt: None,
            }
        })
        .collect();
    let layers = files
        .iter()
        .map(|(layer, status)| ModalItem {
            label: layer.clone(),
            hint: String::new(),
            detail: vec![
                Line::styled(layer.clone(), theme.accent),
                Line::raw(""),
                Line::raw(status.clone()),
            ],
            action: None,
            run: false,
            alt: None,
        })
        .collect();
    Modal::new(
        "Config",
        vec![
            ModalTab {
                title: "Settings".into(),
                items: settings,
            },
            ModalTab {
                title: "Layers".into(),
                items: layers,
            },
        ],
    )
}

/// The `/context` modal. `report` is `None` until the engine answers.
pub(crate) fn context(report: Option<&quack_protocol::ContextReport>, theme: &Theme) -> Modal {
    let tabs = |items| {
        Modal::new(
            "Context",
            vec![ModalTab {
                title: "Context".into(),
                items,
            }],
        )
    };
    let Some(r) = report else {
        return tabs(vec![placeholder(
            "measuring…",
            "Estimating what the next request will hold.",
            theme,
        )]);
    };
    let k = |n: u32| {
        if n >= 10_000 {
            format!("{}k", n / 1000)
        } else {
            n.to_string()
        }
    };
    let share = |n: u32| match r.budget {
        Some(b) if b > 0 => format!("{}%", u64::from(n) * 100 / u64::from(b)),
        _ => String::new(),
    };
    let mut total = vec![Line::styled(r.model.clone(), theme.accent), Line::raw("")];
    if let Some(b) = r.budget.filter(|b| *b > 0) {
        const WIDTH: u32 = 30;
        let filled = (r.used.min(b) * WIDTH / b) as usize;
        total.push(Line::raw(format!(
            "{}{} {}",
            "█".repeat(filled),
            "░".repeat(WIDTH as usize - filled),
            share(r.used)
        )));
        total.push(Line::raw(""));
    }
    let unknown = || "unknown".to_owned();
    total.extend([
        field(theme, "used", format!("~{} tokens", r.used)),
        field(
            theme,
            "budget",
            r.budget.map_or_else(unknown, |b| b.to_string()),
        ),
        field(
            theme,
            "window",
            r.window.map_or_else(unknown, |w| w.to_string()),
        ),
        field(
            theme,
            "compacts",
            r.threshold
                .map_or_else(|| "on overflow only".into(), |t| format!("above ~{t}")),
        ),
        field(theme, "strategy", r.strategy.clone()),
        Line::raw(""),
        Line::styled("/compact [focus] summarizes earlier turns now", theme.dim),
    ]);
    if let Some(n) = &r.note {
        total.insert(2, Line::styled(n.clone(), theme.warn));
        total.insert(3, Line::raw(""));
    }
    let mut items = vec![ModalItem {
        label: "Total".into(),
        hint: match r.budget {
            Some(b) => format!("{} / {}", k(r.used), k(b)),
            None => k(r.used),
        },
        detail: total,
        action: None,
        run: false,
        alt: None,
    }];
    items.extend(r.segments.iter().map(|s| {
        ModalItem {
            label: s.name.clone(),
            hint: format!("{} {}", k(s.tokens), share(s.tokens))
                .trim()
                .to_owned(),
            detail: vec![
                Line::styled(s.name.clone(), theme.accent),
                Line::raw(""),
                field(theme, "tokens", format!("~{}", s.tokens)),
                field(theme, "of budget", share(s.tokens)),
                Line::raw(""),
                Line::raw(s.detail.clone()),
            ],
            action: None,
            run: false,
            alt: None,
        }
    }));
    tabs(items)
}

/// The `/memory` modal. `notes` is `None` while the engine reads them.
pub(crate) fn memory(notes: Option<&[quack_protocol::MemoryNote]>, theme: &Theme) -> Modal {
    let items = match notes {
        None => vec![placeholder(
            "reading notes…",
            "Reading memory notes.",
            theme,
        )],
        Some([]) => vec![placeholder(
            "no notes yet",
            "The model saves notes with the memory tool when something will matter in later \
             sessions. Notes are markdown files you can edit.",
            theme,
        )],
        Some(list) => list
            .iter()
            .map(|n| {
                let mut detail = vec![
                    Line::styled(n.name.clone(), theme.accent),
                    Line::raw(""),
                    Line::raw(n.description.clone()),
                    Line::raw(""),
                    field(theme, "scope", n.scope.clone()),
                    field(theme, "file", n.path.clone()),
                    Line::raw(""),
                ];
                // The body, without the front matter shown above.
                let body = n
                    .text
                    .strip_prefix("---")
                    .and_then(|r| r.split_once("\n---"))
                    .map_or(n.text.as_str(), |(_, b)| b);
                detail.extend(body.trim().lines().map(|l| Line::raw(l.to_owned())));
                ModalItem {
                    label: n.name.clone(),
                    hint: n.scope.clone(),
                    detail,
                    action: None,
                    run: false,
                    alt: Some(format!("/memory delete {}/{}", n.scope, n.name)),
                }
            })
            .collect(),
    };
    Modal::new(
        "Memory",
        vec![ModalTab {
            title: "Notes".into(),
            items,
        }],
    )
    .with_alt("delete")
}

/// The `/permissions` modal: modes to switch to, and the configured rules.
pub(crate) fn permissions(current: &str, rules: &[(String, String)], theme: &Theme) -> Modal {
    const MODES: &[(&str, &str)] = &[
        (
            "read-only",
            "Reads only. Edits are refused; commands run only when an allow rule matches.",
        ),
        (
            "plan",
            "Look around and propose a plan. Edits are refused; read-only commands run.",
        ),
        (
            "ask",
            "Reads in the workspace run; edits and commands ask first.",
        ),
        (
            "auto-edit",
            "Edits inside the workspace run; commands ask first.",
        ),
        (
            "full-auto",
            "Everything runs without asking, inside the sandbox when it is on, except the \
             built-in deny list and deny rules. Leaving the sandbox still asks.",
        ),
        (
            "yolo",
            "Nothing asks and the sandbox is off. Only the built-in deny list (rm -rf /, \
             disk wipes, …) and your deny rules still apply.",
        ),
    ];
    let modes = MODES
        .iter()
        .map(|(name, what)| ModalItem {
            label: if *name == current {
                format!("{name} ●")
            } else {
                (*name).to_owned()
            },
            hint: String::new(),
            detail: vec![
                Line::styled(*name, theme.accent),
                Line::raw(""),
                Line::raw(*what),
                Line::raw(""),
                Line::styled(
                    if *name == current {
                        "the current mode"
                    } else {
                        "⏎ switch to it for this session"
                    },
                    theme.dim,
                ),
            ],
            action: Some(format!("/permissions {name}")),
            run: true,
            alt: None,
        })
        .collect();
    let rule_items = if rules.is_empty() {
        vec![placeholder(
            "no rules",
            "Add allow, ask and deny rules under [permissions] in config.toml, e.g. \
             allow = [\"shell:cargo test*\"]. /config shows where each setting comes from.",
            theme,
        )]
    } else {
        rules
            .iter()
            .map(|(list, rule)| ModalItem {
                label: rule.clone(),
                hint: list.clone(),
                detail: vec![
                    Line::styled(rule.clone(), theme.accent),
                    Line::raw(""),
                    field(theme, "list", list.clone()),
                    Line::raw(""),
                    Line::styled(
                        "Order: built-in deny list, deny, session approvals, allow, ask, then the mode.",
                        theme.dim,
                    ),
                ],
                action: None,
                run: false,
                alt: None,
            })
            .collect()
    };
    Modal::new(
        "Permissions",
        vec![
            ModalTab {
                title: "Mode".into(),
                items: modes,
            },
            ModalTab {
                title: "Rules".into(),
                items: rule_items,
            },
        ],
    )
}

/// The `/rewind` picker. `points` is `None` while the engine lists them.
pub(crate) fn rewind(points: Option<&[quack_protocol::RewindPoint]>, theme: &Theme) -> Modal {
    let items = match points {
        None => vec![placeholder("listing messages…", "", theme)],
        Some([]) => vec![placeholder(
            "no messages yet",
            "Once you have sent a message, /rewind can go back to before it.",
            theme,
        )],
        Some(points) => points
            .iter()
            .map(|p| {
                let first = p.text.lines().find(|l| !l.trim().is_empty()).unwrap_or_default();
                let mut detail = vec![Line::styled(format!("{} back", p.back), theme.accent), Line::raw("")];
                detail.extend(p.text.lines().take(12).map(|l| Line::raw(l.to_owned())));
                detail.push(Line::raw(""));
                if p.files.is_empty() {
                    detail.push(Line::styled("No files changed by write or edit since then.", theme.dim));
                } else {
                    detail.push(Line::styled("Files put back:", theme.dim));
                    detail.extend(p.files.iter().map(|f| Line::raw(format!("  {f}"))));
                }
                detail.push(Line::raw(""));
                detail.push(Line::styled(
                    format!(
                        "⏎ conversation and files · ^s conversation only · /rewind {} files for files only. \
                         Changes made by shell commands are not tracked.",
                        p.back
                    ),
                    theme.dim,
                ));
                ModalItem {
                    label: first.chars().take(100).collect(),
                    hint: match p.files.len() {
                        0 => String::new(),
                        1 => "1 file".into(),
                        n => format!("{n} files"),
                    },
                    detail,
                    action: Some(format!("/rewind {} both", p.back)),
                    run: true,
                    alt: Some(format!("/rewind {} conversation", p.back)),
                }
            })
            .collect(),
    };
    Modal::new(
        "Rewind",
        vec![ModalTab {
            title: "Messages".into(),
            items,
        }],
    )
    .with_alt("conversation only")
}

/// The `/trust` modal: what the workspace's `.quack/` holds and the three choices.
pub(crate) fn trust(pending: Option<&TrustPrompt>, theme: &Theme) -> Modal {
    const CHOICES: &[(&str, &str, &str)] = &[
        (
            "trusted",
            "Trust",
            "Load everything: settings, providers, MCP servers, hooks and language servers. \
             MCP servers and hooks start programs from this repo's files.",
        ),
        (
            "settings-only",
            "Trust settings only",
            "Load config.toml, but nothing that starts a program or adds a provider.",
        ),
        (
            "ignored",
            "Ignore",
            "Never load this repo's .quack/, and don't ask again.",
        ),
    ];
    let mut about = match pending {
        Some(p) => vec![
            Line::styled(
                if p.changed {
                    "Changed since you trusted it"
                } else {
                    "Project config found"
                },
                theme.warn,
            ),
            Line::raw(""),
            field(theme, "folder", p.path.clone()),
            field(theme, "holds", p.summary.clone()),
        ],
        None => vec![
            Line::styled("This workspace's .quack/", theme.accent),
            Line::raw(""),
            Line::raw("No decision is waiting. Choosing again replaces the stored one."),
        ],
    };
    about.push(Line::raw(""));
    about.push(Line::styled(
        "Decisions are kept in trust.toml in the user data folder. If the files change, \
         you are asked again.",
        theme.dim,
    ));
    let mut items: Vec<ModalItem> = CHOICES
        .iter()
        .map(|(id, label, what)| {
            let mut detail = vec![
                Line::styled(*label, theme.accent),
                Line::raw(""),
                Line::raw(*what),
                Line::raw(""),
            ];
            detail.extend(about.iter().cloned());
            ModalItem {
                label: (*label).to_owned(),
                hint: String::new(),
                detail,
                action: Some(format!("/trust {id}")),
                run: true,
                alt: None,
            }
        })
        .collect();
    items.push(ModalItem {
        label: "Decide later".into(),
        hint: String::new(),
        detail: vec![
            Line::styled("Decide later", theme.accent),
            Line::raw(""),
            Line::raw("Keep skipping it for now. /trust brings this back."),
        ],
        action: Some("/trust later".into()),
        run: true,
        alt: None,
    });
    Modal::new(
        "Trust",
        vec![ModalTab {
            title: "Project".into(),
            items,
        }],
    )
}

/// A `.quack/` waiting for a decision (`Event::ProjectTrust`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TrustPrompt {
    pub path: String,
    pub summary: String,
    pub changed: bool,
}

/// The `/doctor` modal. `checks` is `None` while they run.
pub(crate) fn doctor(checks: Option<&[quack_protocol::DoctorCheck]>, theme: &Theme) -> Modal {
    use quack_protocol::CheckStatus;
    let items = match checks {
        None => vec![placeholder(
            "checking…",
            "Running environment checks and contacting each configured provider.",
            theme,
        )],
        Some(list) => list
            .iter()
            .map(|c| {
                let (hint, style) = match c.status {
                    CheckStatus::Ok => ("ok", theme.ok),
                    CheckStatus::Info => ("info", theme.dim),
                    CheckStatus::Warn => ("warn", theme.warn),
                    CheckStatus::Fail => ("FAIL", theme.error),
                };
                let mut detail = vec![
                    Line::styled(c.name.trim().to_owned(), theme.accent),
                    Line::raw(""),
                    Line::styled(hint, style),
                    Line::raw(""),
                ];
                detail.extend(c.detail.lines().map(|l| Line::raw(l.to_owned())));
                ModalItem {
                    label: c.name.clone(),
                    hint: hint.to_owned(),
                    detail,
                    action: None,
                    run: false,
                    alt: None,
                }
            })
            .collect(),
    };
    Modal::new(
        "Doctor",
        vec![ModalTab {
            title: "Checks".into(),
            items,
        }],
    )
}

/// The `/providers` modal: each provider file with its state. `None` while probing.
pub(crate) fn providers(
    catalog: Option<&[quack_protocol::ProviderModels]>,
    current: Option<&str>,
    theme: &Theme,
) -> Modal {
    let items = match catalog {
        None => vec![placeholder(
            "checking providers…",
            "Asking each provider for its models.",
            theme,
        )],
        Some([]) => vec![placeholder(
            "no providers",
            "/setup finds model servers on this machine; `quack setup` in a \
             terminal also adds hosted services and servers elsewhere. Or add a TOML file \
             with a base_url to providers.d in your config folder (`quack config \
             paths` shows where).",
            theme,
        )],
        Some(list) => list
            .iter()
            .map(|p| {
                let in_use = current.is_some_and(|c| c.starts_with(&format!("{}/", p.id)));
                let mut detail = vec![
                    Line::styled(p.id.clone(), theme.accent),
                    Line::raw(""),
                    if p.available {
                        Line::styled("usable", theme.ok)
                    } else if p.bundled {
                        Line::styled("offered by the defaults bundle; not set up", theme.dim)
                    } else {
                        Line::styled("unavailable", theme.warn)
                    },
                ];
                if !p.note.is_empty() {
                    detail.push(Line::raw(p.note.clone()));
                }
                if in_use {
                    detail.push(Line::styled("the current model is from here", theme.dim));
                }
                detail.push(Line::raw(""));
                detail.extend(p.models.iter().take(30).map(|m| {
                    Line::raw(format!(
                        "  {}{}",
                        m.alias
                            .as_ref()
                            .map_or_else(|| m.id.clone(), |a| format!("{a} ({})", m.id)),
                        m.context_window.map_or_else(String::new, |c| format!(
                            " · {}",
                            crate::theme::tokens(c)
                        ))
                    ))
                }));
                if p.models.len() > 30 {
                    detail.push(Line::styled(
                        format!("  … {} more in /models", p.models.len() - 30),
                        theme.dim,
                    ));
                }
                ModalItem {
                    label: if in_use {
                        format!("{} ●", p.id)
                    } else {
                        p.id.clone()
                    },
                    hint: if p.available {
                        format!("{} models", p.models.len())
                    } else if p.bundled {
                        "not set up".into()
                    } else {
                        "unavailable".into()
                    },
                    detail,
                    action: None,
                    run: false,
                    alt: None,
                }
            })
            .collect(),
    };
    Modal::new(
        "Providers",
        vec![ModalTab {
            title: "Providers".into(),
            items,
        }],
    )
}

/// The `/skills` modal: installed skills; ⏎ puts `/<name> ` in the input.
pub(crate) fn skills(skills: &[quack_protocol::SkillInfo], theme: &Theme) -> Modal {
    let items = if skills.is_empty() {
        vec![placeholder(
            "no skills",
            "Put a skill folder with a SKILL.md in .agents/skills (this project), \
             ~/.agents/skills or skills/ in your config folder.",
            theme,
        )]
    } else {
        skills
            .iter()
            .map(|s| ModalItem {
                label: s.name.clone(),
                hint: s.source.split(' ').next().unwrap_or_default().to_owned(),
                detail: vec![
                    Line::styled(format!("/{}", s.name), theme.accent),
                    Line::raw(""),
                    Line::raw(s.description.clone()),
                    Line::raw(""),
                    field(theme, "from", s.source.clone()),
                    field(theme, "folder", s.dir.clone()),
                    Line::raw(""),
                    Line::styled(
                        "⏎ run it with a request; the model also loads it when relevant",
                        theme.dim,
                    ),
                ],
                action: Some(format!("/{} ", s.name)),
                run: false,
                alt: None,
            })
            .collect()
    };
    Modal::new(
        "Skills",
        vec![ModalTab {
            title: "Skills".into(),
            items,
        }],
    )
}

/// The `/mcp` modal: each server with its state, tools and recent stderr. `None` while
/// asking the engine.
pub(crate) fn mcp(servers: Option<&[quack_protocol::McpServerInfo]>, theme: &Theme) -> Modal {
    let items = match servers {
        None => vec![placeholder("checking servers…", "", theme)],
        Some([]) => vec![placeholder(
            "no MCP servers",
            "Add a TOML file to mcp.d in your config folder (`quack config paths` \
             shows where), for example\n\ncommand = \"npx\"\nargs = [\"-y\", \"@modelcontextprotocol/server-everything\"]\n\n\
             or, for a remote server, url = \"https://…/mcp\". The defaults bundle has examples.",
            theme,
        )],
        Some(list) => list
            .iter()
            .map(|s| {
                let style = match s.state.as_str() {
                    "ready" => theme.ok,
                    "failed" => theme.error,
                    _ => theme.warn,
                };
                let mut detail = vec![Line::styled(s.id.clone(), style), Line::raw("")];
                if !s.description.is_empty() {
                    detail.push(Line::raw(s.description.clone()));
                    detail.push(Line::raw(""));
                }
                detail.push(field(theme, "state", s.state.clone()));
                if !s.error.is_empty() {
                    detail.push(Line::styled(s.error.clone(), theme.error));
                }
                detail.push(field(theme, s.transport.as_str(), s.target.clone()));
                if s.state == "ready" {
                    detail.push(field(
                        theme,
                        "tools",
                        format!(
                            "{} · ~{} tokens{}",
                            s.tools.len(),
                            s.tool_tokens,
                            if s.deferred {
                                " · found with tool_search (too large to send every time)"
                            } else {
                                ""
                            }
                        ),
                    ));
                    detail.push(Line::raw(""));
                    detail.extend(s.tools.iter().map(|t| Line::raw(format!("  {t}"))));
                }
                if !s.log.is_empty() {
                    detail.push(Line::raw(""));
                    detail.push(Line::styled("recent output", theme.dim));
                    let skip = s.log.len().saturating_sub(12);
                    detail.extend(
                        s.log
                            .iter()
                            .skip(skip)
                            .map(|l| Line::styled(format!("  {l}"), theme.dim)),
                    );
                }
                detail.push(Line::raw(""));
                detail.push(Line::styled("^s restart this server", theme.dim));
                ModalItem {
                    label: s.id.clone(),
                    hint: if s.state == "ready" {
                        format!("{} tools", s.tools.len())
                    } else {
                        s.state.clone()
                    },
                    detail,
                    action: None,
                    run: false,
                    alt: Some(format!("/mcp restart {}", s.id)),
                }
            })
            .collect(),
    };
    Modal::new(
        "MCP",
        vec![ModalTab {
            title: "Servers".into(),
            items,
        }],
    )
    .with_alt("restart")
}

/// The `/lsp` modal: each language server with its state and diagnostics. `None` while
/// asking the engine.
pub(crate) fn lsp(servers: Option<&[quack_protocol::LspServerInfo]>, theme: &Theme) -> Modal {
    let items = match servers {
        None => vec![placeholder("checking servers…", "", theme)],
        Some([]) => vec![placeholder(
            "no language servers",
            "Add a TOML file to lsp.d in your config folder (`quack config paths` shows \
             where), for example\n\ncommand = \"gopls\"\nextensions = [\"go\"]\n\nThe defaults \
             bundle has files for the main languages. quack never installs a server.",
            theme,
        )],
        Some(list) => list
            .iter()
            .map(|s| {
                let style = match s.state.as_str() {
                    "ready" | "idle" => theme.ok,
                    "failed" | "exited" => theme.error,
                    "not installed" => theme.dim,
                    _ => theme.warn,
                };
                let mut detail = vec![Line::styled(s.id.clone(), style), Line::raw("")];
                detail.push(field(theme, "state", s.state.clone()));
                if !s.detail.is_empty() {
                    let st = if s.state == "failed" {
                        theme.error
                    } else {
                        theme.dim
                    };
                    detail.push(Line::styled(s.detail.clone(), st));
                }
                detail.push(field(theme, "command", s.command.clone()));
                detail.push(field(
                    theme,
                    "files",
                    format!(".{}", s.extensions.join(" .")),
                ));
                if !s.root.is_empty() {
                    detail.push(field(theme, "project", s.root.clone()));
                }
                if matches!(s.state.as_str(), "ready" | "indexing") {
                    detail.push(field(
                        theme,
                        "problems",
                        format!("{} errors · {} warnings", s.errors, s.warnings),
                    ));
                }
                if s.state == "idle" {
                    detail.push(Line::raw(""));
                    detail.push(Line::styled(
                        "Starts when the agent reads or edits a file it covers.",
                        theme.dim,
                    ));
                }
                if !s.log.is_empty() {
                    detail.push(Line::raw(""));
                    detail.push(Line::styled("recent output", theme.dim));
                    let skip = s.log.len().saturating_sub(12);
                    detail.extend(
                        s.log
                            .iter()
                            .skip(skip)
                            .map(|l| Line::styled(format!("  {l}"), theme.dim)),
                    );
                }
                let label = if s.root.is_empty() || s.root == "." {
                    s.id.clone()
                } else {
                    format!("{} · {}", s.id, s.root)
                };
                ModalItem {
                    label,
                    hint: match s.state.as_str() {
                        "ready" if s.errors + s.warnings > 0 => {
                            format!("✗{} ⚠{}", s.errors, s.warnings)
                        }
                        other => other.to_owned(),
                    },
                    detail,
                    action: None,
                    run: false,
                    alt: None,
                }
            })
            .collect(),
    };
    Modal::new(
        "LSP",
        vec![ModalTab {
            title: "Servers".into(),
            items,
        }],
    )
}

/// The `/setup` modal: model servers found on this machine, one row per model. `None`
/// while looking.
pub(crate) fn setup(servers: Option<&[quack_protocol::DetectedServer]>, theme: &Theme) -> Modal {
    const ELSEWHERE: &str = "For a hosted service, or a server on another machine or port, \
         run `quack setup` in a terminal.";
    let items = match servers {
        None => vec![placeholder(
            "looking for servers…",
            "Checking the providers in providers.d that point at this machine (Ollama, \
             llama.cpp, vLLM, LM Studio and SGLang out of the box). Nothing leaves this \
             machine.",
            theme,
        )],
        Some([]) => vec![ModalItem {
            label: "no servers found".into(),
            hint: String::new(),
            detail: vec![
                Line::styled(
                    "None of the servers on this machine in providers.d answered. Start \
                     one, for example `ollama serve` or `llama-server --jinja -m \
                     <model.gguf>`, then run /setup again.",
                    theme.dim,
                ),
                Line::raw(""),
                Line::styled(ELSEWHERE, theme.dim),
            ],
            action: None,
            run: false,
            alt: None,
        }],
        Some(list) => list
            .iter()
            .enumerate()
            .flat_map(|(i, s)| {
                s.models.iter().map(move |m| {
                    let mut detail = vec![
                        Line::styled(format!("{} · {}", s.name, m.id), theme.accent),
                        Line::raw(""),
                        field(theme, "server", s.base_url.clone()),
                        field(
                            theme,
                            "context",
                            m.context_window
                                .map_or_else(|| "unknown".into(), |c| format!("{c} tokens")),
                        ),
                        Line::raw(""),
                    ];
                    if !s.note.is_empty() {
                        detail.push(Line::raw(s.note.clone()));
                        detail.push(Line::raw(""));
                    }
                    detail.push(match &s.configured {
                        Some(id) => Line::styled(
                            format!("⏎ use it (already set up as provider `{id}`)"),
                            theme.dim,
                        ),
                        None => Line::styled(
                            format!(
                                "⏎ add providers.d/{}.toml to your config folder and use this \
                                 model; it becomes the default if none is set",
                                s.id
                            ),
                            theme.dim,
                        ),
                    });
                    detail.push(Line::raw(""));
                    detail.push(Line::styled(ELSEWHERE, theme.dim));
                    ModalItem {
                        label: format!("{} · {}", s.name, m.id),
                        hint: m
                            .context_window
                            .map_or_else(String::new, crate::theme::tokens),
                        detail,
                        action: Some(format!("/setup use {i} {}", m.id)),
                        run: true,
                        alt: None,
                    }
                })
            })
            .collect(),
    };
    Modal::new(
        "Setup",
        vec![ModalTab {
            title: "Servers on this machine".into(),
            items,
        }],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEventKind;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new_with_kind(code, KeyModifiers::NONE, KeyEventKind::Press)
    }

    #[test]
    fn filter_tabs_and_activation() {
        let mut m = help(&Theme::resolve("dark", false));
        for c in "resu".chars() {
            m.on_key(key(KeyCode::Char(c)));
        }
        assert_eq!(m.selected_item().map(|i| i.label.as_str()), Some("/resume"));
        assert_eq!(
            m.on_key(key(KeyCode::Enter)),
            ModalOutcome::Insert("/resume ".into())
        );
        assert_eq!(
            m.on_key(key(KeyCode::Esc)),
            ModalOutcome::None,
            "first esc clears filter"
        );
        m.on_key(key(KeyCode::Tab));
        assert_eq!(m.selected_item().map(|i| i.label.as_str()), Some("Enter"));
        assert_eq!(m.on_key(key(KeyCode::Esc)), ModalOutcome::Close);
    }
}
