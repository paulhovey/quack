//! Shared modal framework (docs/plan/06-tui.md#modal-framework): title, optional tabs,
//! an always-focused filter line, a list pane, a details pane and a key footer.
//! Every configuration command (`/models`, `/resume`, `/config`, …) is built on this.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Clear, List, ListItem, ListState, Padding, Paragraph, Scrollbar, ScrollbarOrientation,
    ScrollbarState,
};
use unicode_width::UnicodeWidthStr;

use crate::charts::{BarRow, Visual};
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
    /// Charts in the details pane, each before the `detail` line it names.
    pub charts: Vec<(usize, Visual)>,
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

    /// Keep the tab, filter and selection of the modal this one replaces, so a refresh
    /// that arrives while the user looks around does not move them.
    pub fn keep_place(mut self, old: Option<&Modal>) -> Self {
        if let Some(old) = old.filter(|o| o.title == self.title) {
            self.tab = old.tab.min(self.tabs.len().saturating_sub(1));
            self.filter = old.filter.clone();
            self.selected = old.selected;
            self.refilter();
        }
        self
    }

    /// Start with this text in the filter line.
    pub fn with_filter(mut self, text: &str) -> Self {
        self.filter = text.to_owned();
        self.refilter();
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
            .border_style(theme.accent)
            .title(Span::styled(
                format!(" {} ", self.title),
                theme.pill(theme.accent),
            ));
        let inner = block.inner(area);
        f.render_widget(block, area);

        let tabs_h = u16::from(self.tabs.len() > 1);
        // Room to breathe under the title and between the rows, unless the screen is small.
        let gap = u16::from(!narrow);
        let [_, tabs_area, _, filter_area, _, body, footer] = Layout::vertical([
            Constraint::Length(gap),
            Constraint::Length(tabs_h),
            Constraint::Length(gap * tabs_h),
            Constraint::Length(1),
            Constraint::Length(gap),
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .areas(inner);

        if tabs_h > 0 {
            let mut spans = vec![Span::raw(" ")];
            for (i, tab) in self.tabs.iter().enumerate() {
                let style = if i == self.tab {
                    theme.pill(theme.accent)
                } else {
                    theme.dim
                };
                spans.push(Span::styled(format!(" {} ", tab.title), style));
                spans.push(Span::raw(" "));
            }
            f.render_widget(Paragraph::new(Line::from(spans)), tabs_area);
        }
        let total = self.items().len();
        let count = if self.filter.is_empty() {
            format!("{total} ")
        } else {
            format!("{} of {total} ", self.filtered.len())
        };
        let mut filter_line = vec![Span::styled(format!(" {} ", glyphs.prompt), theme.accent)];
        if self.filter.is_empty() {
            filter_line.push(Span::styled("type to filter", theme.dim));
        } else {
            filter_line.push(Span::styled(self.filter.clone(), theme.user));
        }
        f.render_widget(Paragraph::new(Line::from(filter_line)), filter_area);
        f.render_widget(
            Paragraph::new(Line::styled(count, theme.dim).right_aligned()),
            filter_area,
        );
        let prompt_w = u16::try_from(glyphs.prompt.width() + 2).unwrap_or(3);
        let cursor_x = filter_area.x + prompt_w + u16::try_from(self.filter.width()).unwrap_or(0);
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
        self.draw_list(f, list_area, gap, theme, glyphs);
        self.draw_details(f, detail_area, gap, theme, glyphs);

        let keys: Vec<(&str, &str)> = self
            .footer
            .iter()
            .filter(|(k, _)| *k != "tab" || self.tabs.len() > 1)
            .copied()
            .collect();
        f.render_widget(Paragraph::new(crate::ui::key_hints(theme, &keys)), footer);
    }

    /// `top`: blank rows above the first item.
    fn draw_list(&self, f: &mut Frame<'_>, area: Rect, top: u16, theme: &Theme, glyphs: &Glyphs) {
        let block = Block::bordered()
            .border_set(glyphs.border)
            .border_style(theme.border)
            .padding(Padding::top(top));
        let inner = block.inner(area);
        let width = usize::from(inner.width);
        let rows: Vec<ListItem<'_>> = self
            .filtered
            .iter()
            .enumerate()
            .filter_map(|(n, i)| self.items().get(*i).map(|item| (n == self.selected, item)))
            .map(|(on, item)| {
                let soft = if on { theme.sel_text } else { theme.dim };
                let hint = truncate(&item.hint, width / 2);
                let label = truncate(&item.label, width.saturating_sub(hint.width() + 4));
                let hits = if label == item.label {
                    fuzzy::positions(&self.filter, &item.label)
                } else {
                    Vec::new()
                };
                let pad = width.saturating_sub(label.width() + hint.width() + 3);
                let mut spans = vec![
                    Span::styled(if on { glyphs.bar } else { " " }, theme.accent),
                    Span::raw(" "),
                ];
                let base = if on { theme.selected } else { Style::new() };
                spans.extend(theme.highlight(&label, &hits, base));
                spans.push(Span::raw(" ".repeat(pad)));
                spans.push(Span::styled(hint, soft));
                ListItem::new(Line::from(spans))
            })
            .collect();
        let empty = rows.is_empty();
        let list = List::new(rows).block(block).highlight_style(theme.selected);
        let mut state = ListState::default().with_selected((!empty).then_some(self.selected));
        f.render_stateful_widget(list, area, &mut state);
        if self.filtered.len() > usize::from(inner.height) {
            let mut sb = ScrollbarState::new(self.filtered.len()).position(self.selected);
            f.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight)
                    .begin_symbol(None)
                    .end_symbol(None)
                    .thumb_symbol(glyphs.scroll[0])
                    .thumb_style(theme.accent)
                    .track_symbol(Some(glyphs.scroll[1]))
                    .track_style(theme.border),
                area.inner(Margin::new(0, 1)),
                &mut sb,
            );
        }
    }

    fn draw_details(
        &self,
        f: &mut Frame<'_>,
        area: Rect,
        top: u16,
        theme: &Theme,
        glyphs: &Glyphs,
    ) {
        let block = Block::bordered()
            .border_set(glyphs.border)
            .border_style(theme.border)
            .padding(Padding::new(1, 1, top, 0));
        let inner = block.inner(area);
        f.render_widget(block, area);
        match self.selected_item() {
            Some(i) => crate::charts::draw_detail(f, inner, &i.detail, &i.charts, theme),
            None => f.render_widget(Paragraph::new(Line::styled("no matches", theme.dim)), inner),
        }
    }
}

/// Truncate to `max` columns with an ellipsis.
fn truncate(s: &str, max: usize) -> String {
    if s.width() <= max {
        return s.to_owned();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let w = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if used + w + 1 > max {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out
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
                charts: Vec::new(),
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
            charts: Vec::new(),
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
    ascii: bool,
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
            charts: Vec::new(),
        }),
        Some(providers) => {
            // Context windows are drawn against the largest on offer.
            let widest = providers
                .iter()
                .filter(|p| p.available)
                .flat_map(|p| p.models.iter().filter_map(|m| m.context_window))
                .max()
                .unwrap_or(0);
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
                        charts: Vec::new(),
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
                    let mut charts = Vec::new();
                    if let Some(c) = m.context_window.filter(|_| widest > 0) {
                        charts.push((
                            detail.len(),
                            Visual::Meter {
                                label: "window".into(),
                                text: if c == widest {
                                    "largest here".into()
                                } else {
                                    format!(
                                        "{}% of largest",
                                        u64::from(c) * 100 / u64::from(widest)
                                    )
                                },
                                ratio: f64::from(c) / f64::from(widest),
                                style: theme.accent,
                                ascii,
                            },
                        ));
                        detail.push(Line::raw(""));
                    }
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
                        charts,
                    });
                }
            }
            if items.is_empty() {
                items.push(placeholder(
                    "no models ready",
                    "/setup adds one: a server on this machine, a hosted service with its \
                     key, or any server by its URL. /providers lists what is configured.",
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
        charts: Vec::new(),
    }
}

/// The `/tasks` modal: each sub-agent run of this session, newest first, with what it did
/// and its report.
pub(crate) fn tasks(runs: &[crate::app::AgentRun], theme: &Theme) -> Modal {
    use crate::app::AgentItem;
    let items = if runs.is_empty() {
        vec![placeholder(
            "no sub-agents yet",
            "The model starts sub-agents with its task tool, from the agents in agents.d: \
             explore for read-only searches (several at once), general for self-contained \
             changes. Each works with a fresh context and only its report comes back.",
            theme,
        )]
    } else {
        runs.iter()
            .rev()
            .map(|r| {
                let took = r.took.unwrap_or_else(|| r.started.elapsed());
                let state = match r.ok {
                    None => format!("running · {} steps", r.steps()),
                    Some(true) => r.summary.clone(),
                    Some(false) => format!("failed · {}", r.summary),
                };
                let mut detail = vec![
                    Line::styled(format!("{}: {}", r.agent, r.description), theme.accent),
                    Line::raw(""),
                    field(theme, "run", r.id.clone()),
                    field(theme, "model", r.model.clone()),
                    field(
                        theme,
                        "state",
                        format!("{state} · {}", crate::theme::elapsed(took)),
                    ),
                    Line::raw(""),
                ];
                for item in &r.items {
                    match item {
                        AgentItem::Tool(t) => {
                            let (mark, style) = match t.ok {
                                None => ("…", theme.accent),
                                Some(true) => ("✓", theme.ok),
                                Some(false) => ("✗", theme.error),
                            };
                            let result = if t.result.is_empty() {
                                String::new()
                            } else {
                                format!(" · {}", t.result)
                            };
                            detail.push(Line::from(vec![
                                Span::styled(format!("{mark} "), style),
                                Span::raw(t.summary.clone()),
                                Span::styled(result, theme.dim),
                            ]));
                        }
                        AgentItem::Text(text) => {
                            detail.push(Line::raw(""));
                            detail.extend(text.trim().lines().map(|l| Line::raw(l.to_owned())));
                            detail.push(Line::raw(""));
                        }
                    }
                }
                ModalItem {
                    label: format!("{} · {}", r.id, r.description),
                    hint: match r.ok {
                        None => "running".into(),
                        Some(true) => "done".into(),
                        Some(false) => "failed".into(),
                    },
                    detail,
                    action: None,
                    run: false,
                    alt: None,
                    charts: Vec::new(),
                }
            })
            .collect()
    };
    Modal::new(
        "Tasks",
        vec![ModalTab {
            title: "Sub-agents".into(),
            items,
        }],
    )
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
                    charts: Vec::new(),
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
                charts: Vec::new(),
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
            charts: Vec::new(),
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

/// Detail lines with charts set between them.
#[derive(Default)]
struct Detail {
    lines: Vec<Line<'static>>,
    charts: Vec<(usize, Visual)>,
}

impl Detail {
    fn line(&mut self, l: Line<'static>) -> &mut Self {
        self.lines.push(l);
        self
    }

    fn gap(&mut self) -> &mut Self {
        self.line(Line::raw(""))
    }

    /// A chart here, then a blank line.
    fn chart(&mut self, v: Visual) -> &mut Self {
        self.meter(v).gap()
    }

    /// A chart here, with the next one straight under it: for a stack of meters.
    fn meter(&mut self, v: Visual) -> &mut Self {
        self.charts.push((self.lines.len(), v));
        self
    }

    fn item(self, label: &str, hint: String) -> ModalItem {
        ModalItem {
            label: label.to_owned(),
            hint,
            detail: self.lines,
            action: None,
            run: false,
            alt: None,
            charts: self.charts,
        }
    }
}

/// How many tools `/stats` draws bars for; the rest are counted below them.
const TOOL_BARS: usize = 8;

/// The `/stats` modal. `report` is `None` until the engine answers.
pub(crate) fn stats(
    report: Option<&quack_protocol::StatsReport>,
    ascii: bool,
    theme: &Theme,
) -> Modal {
    let tabs = |items| {
        Modal::new(
            "Stats",
            vec![ModalTab {
                title: "Stats".into(),
                items,
            }],
        )
    };
    let Some(r) = report else {
        return tabs(vec![placeholder(
            "counting…",
            "Adding up this session.",
            theme,
        )]);
    };
    let k = |n: u64| crate::theme::tokens(u32::try_from(n).unwrap_or(u32::MAX));
    let time = |ms: u64| crate::theme::elapsed(std::time::Duration::from_millis(ms));
    let pct = |n: u64, of: u64| n * 100 / of.max(1);
    #[allow(clippy::cast_precision_loss)]
    let ratio = |n: u64, of: u64| n as f64 / of.max(1) as f64;
    let on_bar = theme.pill(theme.accent).bg(ratatui::style::Color::Reset);
    let bars = |rows| Visual::Bars {
        rows,
        ascii,
        on_bar,
    };
    let spark = |label: &str, caption: String, data: Vec<u64>, style, height| Visual::Spark {
        label: label.to_owned(),
        caption,
        data,
        style,
        height,
        ascii,
    };
    let meter = |label: &str, text: String, ratio: f64, style| Visual::Meter {
        label: label.to_owned(),
        text,
        ratio,
        style,
        ascii,
    };
    let row = |label: &str, value: u64, text: String, style| BarRow {
        label: label.to_owned(),
        value,
        text,
        style,
    };
    let series = |f: &dyn Fn(&quack_protocol::RequestStat) -> u64| -> Vec<u64> {
        r.history.iter().map(f).collect()
    };
    let peak = |v: &[u64]| v.iter().copied().max().unwrap_or(0);
    let since = if r.resumed {
        "Resumed: tokens and cost include earlier runs; times and tools start at the resume."
    } else {
        "Since this session started."
    };
    let calls: u32 = r.tools.iter().map(|t| t.calls).sum();
    let failed: u32 = r.tools.iter().map(|t| t.failed).sum();
    let sent = series(&|q| u64::from(q.prompt));
    let received = series(&|q| u64::from(q.completion));

    // Overview: the headline figures, then how well the cache and the clock went.
    let mut overview = Detail::default();
    overview
        .line(Line::styled(r.model.clone(), theme.accent))
        .line(Line::styled(since, theme.dim))
        .gap();
    let mut tiles = vec![
        (r.turns.to_string(), "turns".to_owned(), theme.user),
        (k(r.prompt_tokens), "sent".to_owned(), theme.accent),
        (k(r.completion_tokens), "received".to_owned(), theme.magenta),
    ];
    if let Some(c) = r.cost {
        tiles.push((dollars(c), "cost".to_owned(), theme.ok));
    }
    tiles.push((time(r.open_ms), "open".to_owned(), theme.warn));
    overview.chart(Visual::Tiles(tiles));
    if r.cache_reported > 0 {
        overview.meter(meter(
            "cached",
            format!("{}% of sent", pct(r.cached_tokens, r.prompt_tokens)),
            ratio(r.cached_tokens, r.prompt_tokens),
            theme.ok,
        ));
    }
    overview.meter(meter(
        "model",
        format!("{}% of the time", pct(r.model_ms, r.open_ms)),
        ratio(r.model_ms, r.open_ms),
        theme.accent,
    ));
    if calls > 0 {
        let ok = calls - failed;
        overview.meter(meter(
            "tools",
            format!("{ok} of {calls} ok"),
            ratio(u64::from(ok), u64::from(calls)),
            if failed > 0 { theme.warn } else { theme.ok },
        ));
    }
    overview.gap();
    if sent.len() >= 2 {
        overview.chart(spark(
            "sent",
            "by request".into(),
            sent.clone(),
            theme.accent,
            3,
        ));
    }

    // Tokens
    let mut tokens = Detail::default();
    tokens.line(Line::styled("Tokens", theme.accent)).gap();
    let fresh = r.prompt_tokens.saturating_sub(r.cached_tokens);
    let mut rows = vec![row(
        "sent",
        fresh,
        if r.cache_reported > 0 {
            format!("{} fresh", k(fresh))
        } else {
            k(fresh)
        },
        theme.accent,
    )];
    if r.cache_reported > 0 {
        rows.push(row(
            "cached",
            r.cached_tokens,
            format!(
                "{} · {}%",
                k(r.cached_tokens),
                pct(r.cached_tokens, r.prompt_tokens)
            ),
            theme.ok,
        ));
    }
    rows.push(row(
        "received",
        r.completion_tokens,
        k(r.completion_tokens),
        theme.magenta,
    ));
    tokens.chart(bars(rows));
    if sent.len() >= 2 {
        tokens.chart(spark(
            "sent",
            format!("peak {}", k(peak(&sent))),
            sent.clone(),
            theme.accent,
            3,
        ));
        tokens.chart(spark(
            "received",
            format!("peak {}", k(peak(&received))),
            received,
            theme.magenta,
            2,
        ));
    }
    tokens
        .line(field(theme, "requests", r.requests.to_string()))
        .gap();
    tokens.line(Line::styled(
        "Sent counts every request's whole prompt, so it grows faster than the \
         context: that is what a hosted service bills.",
        theme.dim,
    ));
    if r.cache_reported == 0 && r.requests > 0 {
        tokens.line(Line::styled(
            "The server did not say how much of the prompt it had cached.",
            theme.dim,
        ));
    }
    if r.unreported > 0 {
        tokens.line(Line::styled(
            format!(
                "{} request(s) came back without usage and are not counted.",
                r.unreported
            ),
            theme.warn,
        ));
    }

    // Cost: the figures, and the total climbing request by request.
    let cost_hint = r.cost.map_or_else(|| "–".to_owned(), dollars);
    let mut cost = Detail::default();
    cost.line(Line::styled("Cost", theme.accent)).gap();
    match r.cost {
        Some(c) => {
            let mut tiles = vec![(dollars(c), "total".to_owned(), theme.ok)];
            if r.turns > 0 {
                tiles.push((
                    dollars(c / f64::from(r.turns)),
                    "per turn".to_owned(),
                    theme.accent,
                ));
            }
            let priced = r.requests.saturating_sub(r.unpriced + r.unreported);
            if priced > 0 {
                tiles.push((
                    dollars(c / f64::from(priced)),
                    "per request".to_owned(),
                    theme.magenta,
                ));
            }
            cost.chart(Visual::Tiles(tiles));
            let each: Vec<f64> = r.history.iter().map(|q| q.cost.unwrap_or(0.0)).collect();
            if each.len() >= 2 {
                // Requests older than the history start the line where they left it.
                let mut total = (c - each.iter().sum::<f64>()).max(0.0);
                #[allow(clippy::cast_precision_loss)]
                let points: Vec<(f64, f64)> = each
                    .iter()
                    .enumerate()
                    .map(|(i, x)| {
                        total += x;
                        ((i + 1) as f64, total)
                    })
                    .collect();
                let n = points.len();
                cost.chart(if ascii {
                    // Micro-dollars, so the sparkline has whole numbers to scale.
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    let data = points.iter().map(|p| (p.1 * 1e6) as u64).collect();
                    spark("total", "by request".into(), data, theme.ok, 4)
                } else {
                    Visual::Trend {
                        points,
                        style: theme.ok,
                        limit: None,
                        y: ["$0".into(), dollars(c)],
                        x: ["1".into(), format!("{n} requests")],
                        height: 7,
                    }
                });
            }
            if r.unpriced > 0 {
                cost.line(Line::styled(
                    format!(
                        "{} request(s) to models without a price are left out.",
                        r.unpriced
                    ),
                    theme.warn,
                ));
            }
        }
        None => {
            cost.line(Line::raw("No price set for this model."))
                .gap()
                .line(Line::styled(
                    "To see one, add the price per million tokens to the model in its \
                     provider file:",
                    theme.dim,
                ))
                .gap()
                .line(Line::styled(
                    "cost = { input = 0.3, output = 1.2 }",
                    theme.accent,
                ));
        }
    }

    // Time: where it went, then how quick the model was request by request.
    let other = r
        .open_ms
        .saturating_sub(r.model_ms + r.tools_ms + r.approval_ms);
    let total = r.open_ms.max(1);
    let mut timing = Detail::default();
    timing
        .line(Line::styled("Where the time went", theme.accent))
        .gap();
    let rows = [
        ("the model", r.model_ms, theme.accent),
        ("tools", r.tools_ms, theme.magenta),
        ("your approval", r.approval_ms, theme.warn),
        ("between turns", other, theme.dim),
    ]
    .into_iter()
    .filter(|(name, ms, _)| *ms > 0 || *name == "the model")
    .map(|(name, ms, style)| {
        row(
            name,
            ms,
            format!("{} · {}%", time(ms), pct(ms, total)),
            style,
        )
    })
    .collect();
    timing.chart(bars(rows));
    let latency: Vec<u64> = r
        .history
        .iter()
        .filter_map(|q| q.ttft_ms.map(u64::from))
        .collect();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let speeds: Vec<u64> = r
        .history
        .iter()
        .filter_map(|q| q.tokens_per_sec.map(|t| t.round() as u64))
        .collect();
    if let (Some((median, _)), true) = (r.ttft_ms, latency.len() >= 2) {
        timing.chart(spark(
            "latency",
            format!("{} typ.", time(u64::from(median))),
            latency,
            theme.warn,
            2,
        ));
    }
    if let (Some(tps), true) = (r.tokens_per_sec, speeds.len() >= 2) {
        timing.chart(spark("speed", format!("{tps:.0} t/s"), speeds, theme.ok, 2));
    }
    if let Some((median, slowest)) = r.ttft_ms {
        timing.line(field(
            theme,
            "latency",
            format!(
                "{} to first token · {} slowest",
                time(u64::from(median)),
                time(u64::from(slowest))
            ),
        ));
    }
    if let Some(tps) = r.tokens_per_sec {
        timing.line(field(theme, "speed", format!("{tps:.0} tokens/s typical")));
    }
    let sampling = if r.sampling.is_empty() {
        "the server's defaults".to_owned()
    } else {
        let parts: Vec<String> = r.sampling.iter().map(|(k, v)| format!("{k} {v}")).collect();
        parts.join(" · ")
    };
    timing.line(field(theme, "sampling", sampling));

    // Tools: the busiest as bars in their transcript colours.
    let mut tools = Detail::default();
    tools.line(Line::styled("Tools", theme.accent)).gap();
    if r.tools.is_empty() {
        tools.line(Line::styled("No tools called yet.", theme.dim));
    } else {
        let rows = r
            .tools
            .iter()
            .take(TOOL_BARS)
            .map(|t| {
                let text = if t.failed > 0 {
                    format!("{} · {} failed · {}", t.calls, t.failed, time(t.ms))
                } else {
                    format!("{} · {}", t.calls, time(t.ms))
                };
                row(&t.name, u64::from(t.calls), text, theme.tool(&t.name))
            })
            .collect();
        tools.chart(bars(rows));
        if r.tools.len() > TOOL_BARS {
            let rest = &r.tools[TOOL_BARS..];
            tools.line(Line::styled(
                format!(
                    "and {} more, {} calls",
                    rest.len(),
                    rest.iter().map(|t| t.calls).sum::<u32>()
                ),
                theme.dim,
            ));
            tools.gap();
        }
        tools.chart(meter(
            "succeeded",
            format!("{} of {calls}", calls - failed),
            ratio(u64::from(calls - failed), u64::from(calls)),
            if failed > 0 { theme.warn } else { theme.ok },
        ));
    }
    if r.compactions + r.pruned > 0 {
        tools
            .line(field(
                theme,
                "compacted",
                format!("{} time(s)", r.compactions),
            ))
            .line(field(
                theme,
                "pruned",
                format!("{} old result(s)", r.pruned),
            ));
    }

    tabs(vec![
        overview.item("Overview", format!("{} turns", r.turns)),
        tokens.item("Tokens", k(r.prompt_tokens + r.completion_tokens)),
        cost.item("Cost", cost_hint),
        timing.item("Time", time(r.open_ms)),
        tools.item("Tools", format!("{calls} calls")),
    ])
}

/// `$0.0042`, `$0.312`, `$12.40`.
fn dollars(x: f64) -> String {
    if x < 0.1 {
        format!("${x:.4}")
    } else if x < 10.0 {
        format!("${x:.3}")
    } else {
        format!("${x:.2}")
    }
}

/// Width of the `/context` band, in cells.
const BAND: usize = 32;

/// The `/context` modal. `report` is `None` until the engine answers; `growth` is the
/// context used at the end of each turn so far.
///
/// The overview is one band: every part of the request in its own colour and fill, the
/// free room after it, and a mark where compaction starts. Below it, what is left in
/// turns, how fast it has been growing, and what would help.
pub(crate) fn context(
    report: Option<&quack_protocol::ContextReport>,
    growth: &[u32],
    ascii: bool,
    theme: &Theme,
) -> Modal {
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
    let k = crate::theme::tokens;
    let share = |n: u32, of: u32| u64::from(n) * 100 / u64::from(of.max(1));
    let fills: &[&str] = if ascii {
        &["#", "%", "=", "+", "*", "~"]
    } else {
        &["█", "▓", "▒", "▚", "▞", "░"]
    };
    let colours = [
        theme.accent,
        theme.magenta,
        theme.ok,
        theme.warn,
        theme.user,
        theme.error,
    ];
    let paint = |i: usize| (fills[i % fills.len()], colours[i % colours.len()]);

    let mut over = vec![Line::styled(r.model.clone(), theme.accent), Line::raw("")];
    if let Some(n) = &r.note {
        over.push(Line::styled(n.clone(), theme.warn));
        over.push(Line::raw(""));
    }
    let line = r.threshold.or(r.budget).filter(|l| *l > 0);
    if let Some(b) = r.budget.filter(|b| *b > 0) {
        over.push(band(r, b, &paint, ascii, theme));
        let mark = if ascii { "|" } else { "┊" };
        over.push(Line::styled(
            match r.threshold {
                Some(t) => format!("{mark} compacts at {}", k(t)),
                None => "compacts only when the server says it is too long".into(),
            },
            theme.dim,
        ));
        over.push(Line::styled(
            match r.window {
                Some(w) => format!("budget {} of a {} window", k(b), k(w)),
                None => format!("budget {}", k(b)),
            },
            theme.dim,
        ));
        over.push(Line::raw(""));
    }
    if let Some(line) = line {
        let room = line.saturating_sub(r.used);
        let pace = crate::app::turns_left(growth, room);
        let left = match pace {
            _ if room == 0 => "Past the line: the next request is pruned, then compacted.".into(),
            Some(n) => format!(
                "{} left before compaction: about {n} more turn{} at this pace.",
                k(room),
                if n == 1 { "" } else { "s" }
            ),
            None => format!("{} left before compaction.", k(room)),
        };
        over.push(Line::raw(left));
    }
    let mut charts = Vec::new();
    if growth.len() >= 2 && ascii {
        over.push(Line::from(vec![
            Span::styled("by turn   ", theme.dim),
            Span::styled(sparkline(growth, ascii), theme.accent),
        ]));
    } else if growth.len() >= 2 {
        // Context at the end of each turn, rising towards the dashed compaction line.
        over.push(Line::raw(""));
        #[allow(clippy::cast_precision_loss)]
        let points: Vec<(f64, f64)> = growth
            .iter()
            .enumerate()
            .map(|(i, v)| ((i + 1) as f64, f64::from(*v)))
            .collect();
        let top = growth.iter().copied().chain(line).max().unwrap_or(0);
        charts.push((
            over.len(),
            Visual::Trend {
                points,
                style: theme.accent,
                limit: line.map(|l| (f64::from(l), theme.warn)),
                y: ["0".into(), k(top)],
                x: ["turn 1".into(), format!("turn {}", growth.len())],
                height: 6,
            },
        ));
    }
    over.push(Line::raw(""));
    over.push(Line::styled(advice(r), theme.dim));

    let mut items = vec![ModalItem {
        label: "Overview".into(),
        hint: match line {
            Some(l) => format!("{} left", k(l.saturating_sub(r.used))),
            None => k(r.used),
        },
        detail: over,
        action: None,
        run: false,
        alt: None,
        charts,
    }];
    let budget = r.budget.unwrap_or(0);
    items.extend(r.segments.iter().enumerate().map(|(i, seg)| {
        let (fill, colour) = paint(i);
        let mut detail = vec![
            Line::from(vec![
                Span::styled(format!("{fill} "), colour),
                Span::styled(seg.name.clone(), theme.accent),
            ]),
            Line::raw(""),
            field(theme, "tokens", format!("~{}", seg.tokens)),
            Line::raw(""),
        ];
        // Its share of what is used and of the budget, in its band colour.
        #[allow(clippy::cast_precision_loss)]
        let meter = |label: &str, of: u32| Visual::Meter {
            label: label.to_owned(),
            text: format!("{}%", share(seg.tokens, of)),
            ratio: f64::from(seg.tokens) / f64::from(of.max(1)),
            style: colour,
            ascii,
        };
        let mut charts = vec![(detail.len(), meter("of used", r.used))];
        if budget > 0 {
            charts.push((detail.len(), meter("of budget", budget)));
        }
        detail.extend([Line::raw(""), Line::raw(seg.detail.clone())]);
        ModalItem {
            // The fill matches the band, so the list doubles as its legend.
            label: format!("{fill} {}", seg.name),
            hint: k(seg.tokens),
            detail,
            action: None,
            run: false,
            alt: None,
            charts,
        }
    }));
    tabs(items)
}

/// The parts of the request side by side, then the free room, with the compaction mark.
fn band(
    r: &quack_protocol::ContextReport,
    budget: u32,
    paint: &dyn Fn(usize) -> (&'static str, Style),
    ascii: bool,
    theme: &Theme,
) -> Line<'static> {
    let cells = |n: u32| (u64::from(n) * BAND as u64 / u64::from(budget)) as usize;
    let mut spans = Vec::new();
    let mut used = 0;
    for (i, seg) in r.segments.iter().enumerate() {
        // Every part gets at least a cell, so a small one is still visible.
        let n = cells(seg.tokens)
            .max(usize::from(seg.tokens > 0))
            .min(BAND - used);
        if n == 0 {
            continue;
        }
        let (fill, colour) = paint(i);
        spans.push(Span::styled(fill.repeat(n), colour));
        used += n;
    }
    let free = if ascii { "." } else { "·" };
    let mark = r.threshold.map(cells).filter(|m| *m < BAND);
    for cell in used..BAND {
        if Some(cell) == mark {
            spans.push(Span::styled(if ascii { "|" } else { "┊" }, theme.warn));
        } else {
            spans.push(Span::styled(free, theme.border));
        }
    }
    Line::from(spans)
}

/// Context at the end of each turn, as block heights.
fn sparkline(values: &[u32], ascii: bool) -> String {
    let bars: &[char] = if ascii {
        &['_', '.', '-', '=', '#']
    } else {
        &['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█']
    };
    let recent = &values[values.len().saturating_sub(BAND)..];
    let max = recent.iter().copied().max().unwrap_or(0).max(1);
    recent
        .iter()
        .map(|v| bars[(u64::from(*v) * (bars.len() as u64 - 1) / u64::from(max)) as usize])
        .collect()
}

/// One suggestion, from what is taking the room.
fn advice(r: &quack_protocol::ContextReport) -> String {
    let tokens = |name: &str| {
        r.segments
            .iter()
            .find(|s| s.name == name)
            .map_or(0, |s| s.tokens)
    };
    let fixed = tokens("System prompt") + tokens("Tool definitions");
    let results = tokens("Tool results");
    let pct = |n: u32, of: u32| u64::from(n) * 100 / u64::from(of.max(1));
    match r.budget {
        Some(b) if b > 0 && pct(fixed, b) >= 30 => format!(
            "The system prompt and tool definitions take {}% of the budget before \
             anything is said. A smaller profile or fewer MCP servers leaves more room.",
            pct(fixed, b)
        ),
        _ if r.used > 0 && pct(results, r.used) >= 50 => format!(
            "Tool results are {}% of it. Old ones are pruned on their own; \
             /compact [focus] summarizes earlier turns now.",
            pct(results, r.used)
        ),
        _ => "/compact [focus] summarizes earlier turns now; /new starts afresh.".into(),
    }
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
                    charts: Vec::new(),
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
            charts: Vec::new(),
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
                charts: Vec::new(),
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
                    charts: Vec::new(),
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
                charts: Vec::new(),
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
        charts: Vec::new(),
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
        Some(list) => {
            let paint = |status: &CheckStatus| match status {
                CheckStatus::Ok => ("ok", theme.ok),
                CheckStatus::Info => ("info", theme.dim),
                CheckStatus::Warn => ("warn", theme.warn),
                CheckStatus::Fail => ("FAIL", theme.error),
            };
            let count = |s: CheckStatus| list.iter().filter(|c| c.status == s).count();
            let (ok, warn, fail) = (
                count(CheckStatus::Ok),
                count(CheckStatus::Warn),
                count(CheckStatus::Fail),
            );
            let mut summary = Detail::default();
            summary
                .line(Line::styled("Environment checks", theme.accent))
                .gap()
                .chart(Visual::Tiles(vec![
                    (ok.to_string(), "passed".into(), theme.ok),
                    (warn.to_string(), "warnings".into(), theme.warn),
                    (fail.to_string(), "failed".into(), theme.error),
                ]))
                .line(Line::styled(
                    match (fail, warn) {
                        (0, 0) => "Everything checks out.",
                        (0, _) => "Nothing is broken; the warnings say what could be better.",
                        _ => "Pick a failed check to see what to do about it.",
                    },
                    theme.dim,
                ));
            let hint = match (fail, warn) {
                (0, 0) => "all ok".to_owned(),
                (0, w) => format!("{w} warn"),
                (f, _) => format!("{f} failed"),
            };
            let mut items = vec![summary.item("Summary", hint)];
            items.extend(list.iter().map(|c| {
                let (hint, style) = paint(&c.status);
                let mut detail = vec![
                    Line::styled(c.name.trim().to_owned(), theme.accent),
                    Line::raw(""),
                    Line::from(Span::styled(format!(" {hint} "), theme.pill(style))),
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
                    charts: Vec::new(),
                }
            }));
            items
        }
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
            "/setup adds one: a server on this machine, a hosted service with its key, or \
             any server by its URL. Or add a TOML file with a base_url to providers.d in \
             your config folder (`quack config paths` shows where).",
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
                    charts: Vec::new(),
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
                charts: Vec::new(),
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

/// The `/instructions` modal: the AGENTS.md files the model has, in the system prompt or
/// attached when it first worked in their folder. `None` while asking the engine.
pub(crate) fn instructions(
    files: Option<&[quack_protocol::InstructionFile]>,
    theme: &Theme,
) -> Modal {
    let items = match files {
        None => vec![placeholder("reading…", "", theme)],
        Some([]) => vec![placeholder(
            "no instruction files",
            "None was found. quack reads AGENTS.md (and the other names in \
             `[instructions] files`) from your config folder, then from each folder between \
             the git root and the working directory. Files in subfolders are added when the \
             model first works there. /init writes one for this project.",
            theme,
        )],
        Some(files) => files
            .iter()
            .map(|f| {
                let mut detail = vec![
                    Line::styled(f.source.clone(), theme.accent),
                    Line::raw(""),
                    field(
                        theme,
                        "where",
                        if f.in_prompt {
                            "in the system prompt".into()
                        } else {
                            "added to a tool result in its folder".into()
                        },
                    ),
                    field(theme, "size", format!("~{} tokens", f.tokens)),
                    Line::raw(""),
                ];
                detail.extend(f.text.lines().map(|l| Line::raw(l.to_owned())));
                ModalItem {
                    label: f.source.clone(),
                    hint: crate::theme::tokens(f.tokens),
                    detail,
                    action: None,
                    run: false,
                    alt: None,
                    charts: Vec::new(),
                }
            })
            .collect(),
    };
    Modal::new(
        "Instructions",
        vec![ModalTab {
            title: "Instructions".into(),
            items,
        }],
    )
}

/// What `/profiles` shows: the profile in use, whether it was chosen for the session,
/// and every installed profile.
pub(crate) type ProfileList<'a> = (&'a str, bool, &'a [quack_protocol::ProfileInfo]);

/// The `/profiles` modal: each profile previewed for the current model. `None` while
/// asking the engine.
pub(crate) fn profiles(list: Option<ProfileList<'_>>, theme: &Theme) -> Modal {
    let Some((active, forced, profiles)) = list else {
        return Modal::new(
            "Profiles",
            vec![ModalTab {
                title: "Profiles".into(),
                items: vec![placeholder("rendering profiles…", "", theme)],
            }],
        );
    };
    let mut items: Vec<ModalItem> = profiles
        .iter()
        .map(|p| {
            let hint = match (p.id == active, p.matches) {
                (true, _) => "in use".into(),
                (false, true) => "auto pick".into(),
                _ => String::new(),
            };
            let mut detail = vec![
                Line::styled(p.id.clone(), theme.accent),
                Line::raw(""),
                Line::raw(p.description.clone()),
                Line::raw(""),
                field(theme, "source", p.source.clone()),
                field(theme, "edits", p.edit_format.replace('_', " ")),
                field(theme, "tools", p.tools.join(", ")),
                Line::raw(""),
                Line::styled("System prompt, rendered for this model", theme.dim),
            ];
            detail.extend(p.system.lines().map(|l| Line::raw(l.to_owned())));
            ModalItem {
                label: p.id.clone(),
                hint,
                detail,
                action: Some(format!("/profiles {}", p.id)),
                run: true,
                alt: Some(format!("/profiles default {}", p.id)),
                charts: Vec::new(),
            }
        })
        .collect();
    items.push(ModalItem {
        label: "auto".into(),
        hint: if forced { String::new() } else { "on".into() },
        detail: vec![
            Line::styled("auto", theme.accent),
            Line::raw(""),
            Line::raw(
                "Each model's own profile: the one its provider file names, else the \
                 best `[match]` for its id, else `generic`.",
            ),
            Line::raw(""),
            Line::styled("⏎ go back to it for this session", theme.dim),
        ],
        action: Some("/profiles auto".into()),
        run: true,
        alt: None,
        charts: Vec::new(),
    });
    Modal::new(
        "Profiles",
        vec![ModalTab {
            title: "Profiles".into(),
            items,
        }],
    )
    .with_alt("make default")
}

/// The `/commands` modal: custom slash commands from the `commands/` folders.
pub(crate) fn custom_commands(commands: &[quack_protocol::CommandInfo], theme: &Theme) -> Modal {
    let items = if commands.is_empty() {
        vec![placeholder(
            "no custom commands",
            "Put a markdown prompt in commands/ in your config folder (or .quack/commands in \
             a trusted project): commands/review.md is /review, commands/test/flaky.md is \
             /test:flaky. {{ args }} in the file becomes what you type after the name; \
             front matter can give a `description` and `args`.",
            theme,
        )]
    } else {
        commands
            .iter()
            .map(|c| ModalItem {
                label: format!("/{}", c.name),
                hint: c.source.clone(),
                detail: vec![
                    Line::styled(
                        format!("/{} {}", c.name, c.args).trim_end().to_owned(),
                        theme.accent,
                    ),
                    Line::raw(""),
                    Line::raw(c.description.clone()),
                    Line::raw(""),
                    field(theme, "from", c.source.clone()),
                    field(theme, "file", c.path.clone()),
                    Line::raw(""),
                    Line::styled("⏎ type it with a request", theme.dim),
                ],
                action: Some(format!("/{} ", c.name)),
                run: false,
                alt: None,
                charts: Vec::new(),
            })
            .collect()
    };
    Modal::new(
        "Commands",
        vec![ModalTab {
            title: "Commands".into(),
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
                    charts: Vec::new(),
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
                    charts: Vec::new(),
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

/// What `/setup` knows so far.
#[derive(Debug, Default)]
pub(crate) struct SetupView {
    pub local: Scan,
    /// The hosted services in providers.d; `None` while listing.
    pub services: Option<Vec<quack_protocol::ServiceInfo>>,
}

/// Looking for model servers on this machine, which happens only when the user asks.
#[derive(Debug, Default)]
pub(crate) enum Scan {
    #[default]
    NotYet,
    Looking,
    Found(Vec<quack_protocol::DetectedServer>),
}

/// The `/setup` modal: servers on this machine, hosted services that take a key, and any
/// server by its URL.
pub(crate) fn setup(view: &SetupView, theme: &Theme) -> Modal {
    const ELSEWHERE: &str = "For a hosted service, or a server on another machine or \
         port, press tab for the other ways to add one.";
    const LOOKS_AT: &str = "Checks the providers in providers.d that point at this machine \
         (Ollama, llama.cpp, vLLM, LM Studio and SGLang with the defaults bundle). Nothing \
         leaves this machine.";
    let item =
        |label: &str, hint: &str, detail: Vec<Line<'static>>, action: Option<String>| ModalItem {
            label: label.to_owned(),
            hint: hint.to_owned(),
            detail,
            run: action.is_some(),
            action,
            alt: None,
            charts: Vec::new(),
        };
    let local = match &view.local {
        Scan::NotYet => vec![item(
            "find servers on this machine",
            "",
            vec![
                Line::styled(LOOKS_AT, theme.dim),
                Line::raw(""),
                Line::styled("⏎ look now", theme.dim),
                Line::raw(""),
                Line::styled(ELSEWHERE, theme.dim),
            ],
            Some("/setup scan".into()),
        )],
        Scan::Looking => vec![placeholder("looking for servers…", LOOKS_AT, theme)],
        Scan::Found(list) if list.is_empty() => vec![item(
            "no servers found",
            "",
            vec![
                Line::styled(
                    "None of the servers on this machine in providers.d answered. Start \
                     one, for example `ollama serve` or `llama-server --jinja -m \
                     <model.gguf>`, then look again.",
                    theme.dim,
                ),
                Line::raw(""),
                Line::styled("⏎ look again", theme.dim),
                Line::raw(""),
                Line::styled(ELSEWHERE, theme.dim),
            ],
            Some("/setup scan".into()),
        )],
        Scan::Found(list) => list
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
                    item(
                        &format!("{} · {}", s.name, m.id),
                        &m.context_window
                            .map_or_else(String::new, crate::theme::tokens),
                        detail,
                        Some(format!("/setup use {i} {}", m.id)),
                    )
                })
            })
            .collect(),
    };
    let hosted = match view.services.as_deref() {
        None => vec![placeholder(
            "listing services…",
            "Reading the hosted services in providers.d.",
            theme,
        )],
        Some([]) => vec![placeholder(
            "no hosted services configured",
            "providers.d has no hosted services that take a key. The defaults bundle ships \
             a list of them; an install without it (quack-plucked) has none. Add a service \
             under Any server with its OpenAI-compatible URL instead.",
            theme,
        )],
        Some(list) => list
            .iter()
            .map(|s| {
                let mut detail = vec![
                    Line::styled(s.name.clone(), theme.accent),
                    Line::raw(""),
                    field(theme, "provider", s.id.clone()),
                    field(theme, "group", s.group.clone()),
                    field(theme, "url", s.base_url.clone()),
                    Line::raw(""),
                ];
                let action = if s.ready {
                    detail.push(Line::styled("Its key is set.", theme.ok));
                    detail.push(Line::raw(""));
                    detail.push(Line::styled("⏎ pick one of its models", theme.dim));
                    format!("/setup models {}", s.id)
                } else {
                    detail.push(Line::styled(
                        "⏎ enter its API key; quack keeps it in the OS credential store, \
                         and the provider file in your config folder only names it",
                        theme.dim,
                    ));
                    if let Some(env) = &s.env {
                        detail.push(Line::raw(""));
                        detail.push(Line::styled(
                            format!("Or set {env} in your shell and start quack again."),
                            theme.dim,
                        ));
                    }
                    format!("/setup key {}", s.id)
                };
                item(
                    &s.name,
                    if s.ready { "ready" } else { "needs a key" },
                    detail,
                    Some(action),
                )
            })
            .collect(),
    };
    let other = vec![
        item(
            "add a server by its URL",
            "",
            vec![
                Line::styled("Any OpenAI-compatible server", theme.accent),
                Line::raw(""),
                Line::raw(
                    "A model server on another machine or port, or a hosted service that is \
                     not listed. quack asks for its base URL (for example \
                     http://gpu-box:8000/v1) and then its API key, if it needs one.",
                ),
                Line::raw(""),
                Line::styled(
                    "The key goes to the OS credential store; the provider file written to \
                     your config folder only names it.",
                    theme.dim,
                ),
                Line::raw(""),
                Line::styled("⏎ start", theme.dim),
            ],
            Some("/setup url".into()),
        ),
        item(
            "write a provider file yourself",
            "",
            vec![
                Line::raw(
                    "A provider is a TOML file in providers.d in your config folder. Two lines \
                     are enough:",
                ),
                Line::raw(""),
                Line::styled("  base_url = \"http://gpu-box:8000/v1\"", theme.accent),
                Line::styled("  api_key = { env = \"MY_KEY\" }", theme.accent),
                Line::raw(""),
                Line::styled(
                    "`quack config paths` shows where the folder is. Run /reload after \
                     adding one.",
                    theme.dim,
                ),
            ],
            None,
        ),
    ];
    Modal::new(
        "Setup",
        vec![
            ModalTab {
                title: "This machine".into(),
                items: local,
            },
            ModalTab {
                title: "Hosted services".into(),
                items: hosted,
            },
            ModalTab {
                title: "Any server".into(),
                items: other,
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
