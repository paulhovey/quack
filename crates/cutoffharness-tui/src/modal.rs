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
    catalog: Option<&[cutoffharness_protocol::ProviderModels]>,
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
            for p in providers {
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
    sessions: Option<&[cutoffharness_protocol::SessionSummary]>,
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
    rows: &[cutoffharness_config::SettingRow],
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
