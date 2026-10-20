use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use quack_protocol::LoadState;

use quack_config::settings::ShowReasoning;
use quack_protocol::ApprovalRequest;

use crate::app::{App, Entry, PlanState, PopupItem, ToolEntry};
use crate::fuzzy;
use crate::wrap::wrap;

const MAX_INPUT_LINES: usize = 6;
const MAX_POPUP_ROWS: usize = 8;

pub(crate) fn draw(f: &mut Frame<'_>, app: &App) {
    // The question or plan review bar takes rows from the transcript, keeping a few; the
    // startup panel gives way to it.
    let choices = choices(app, usize::from(f.area().width).saturating_sub(2));
    let panel_rows = if app.panel_visible() && choices.is_none() {
        u16::try_from(app.startup.len()).unwrap_or(0) + 1
    } else {
        0
    };
    let input_rows = u16::try_from(app.input.line_count().min(MAX_INPUT_LINES)).unwrap_or(1) + 2;
    let plan_rows = if app.plan_expanded && !app.plan.is_empty() {
        // The whole plan, up to a third of the screen.
        let cap = (f.area().height / 3).max(2);
        u16::try_from(app.plan.len() + 1)
            .unwrap_or(u16::MAX)
            .min(cap)
    } else {
        u16::from(plan_open(app))
    };
    let choice_rows = choices.as_ref().map_or(0, |c| {
        let room = f
            .area()
            .height
            .saturating_sub(1 + panel_rows + plan_rows + input_rows + 1 + 3);
        u16::try_from(c.height()).unwrap_or(u16::MAX).min(room)
    });
    let [header, panel, transcript, choice_area, plan, input, status] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(panel_rows),
        Constraint::Min(1),
        Constraint::Length(choice_rows),
        Constraint::Length(plan_rows),
        Constraint::Length(input_rows),
        Constraint::Length(1),
    ])
    .areas(f.area());

    draw_header(f, app, header);
    if panel_rows > 0 {
        draw_panel(f, app, panel);
    }
    draw_transcript(f, app, transcript);
    if plan_rows > 1 {
        draw_plan_list(f, app, plan);
    } else if plan_rows > 0 {
        draw_plan(f, app, plan);
    }
    draw_input(f, app, input);
    draw_status(f, app, status);
    draw_popup(f, app, transcript);
    if let Some(c) = choices {
        draw_choices(f, app, choice_area, c);
    }
    draw_approval(f, app, transcript);
    if let Some(modal) = &app.modal {
        modal.draw(f, &app.theme, &app.glyphs);
    }
}

fn draw_header(f: &mut Frame<'_>, app: &App, area: Rect) {
    let t = &app.theme;
    let version = format!(" {}", app.info.version);
    let branch = app
        .info
        .branch
        .as_ref()
        .map(|b| format!("{}{b} ", app.glyphs.branch))
        .unwrap_or_default();
    let room = usize::from(area.width)
        .saturating_sub(" quack".width() + version.width() + 3 + branch.width() + 1);
    let cwd = &app.info.cwd;
    // Keep the end of a long path: the project folder is the useful part.
    let cwd = if cwd.width() > room && room > 1 {
        let tail: String = cwd
            .chars()
            .rev()
            .take(room - 1)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        format!("…{tail}")
    } else {
        cwd.clone()
    };
    let mut spans = vec![Span::raw(" ")];
    spans.extend(t.gradient_spans("quack"));
    spans.push(Span::styled(version, t.dim));
    spans.push(Span::styled(format!(" · {cwd}"), t.dim));
    let used: usize = spans.iter().map(|s| s.content.width()).sum();
    let pad = usize::from(area.width).saturating_sub(used + branch.width());
    if !branch.is_empty() && pad > 0 {
        spans.push(Span::raw(" ".repeat(pad)));
        spans.push(Span::styled(branch, t.magenta));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_panel(f: &mut Frame<'_>, app: &App, area: Rect) {
    let t = &app.theme;
    let g = &app.glyphs;
    let lines: Vec<Line<'_>> = app
        .startup
        .iter()
        .map(|(subsystem, state)| {
            let (icon, style, text): (&str, Style, &str) = match state {
                LoadState::Pending => (g.pending, t.dim, ""),
                LoadState::Running { detail } => (app.spinner(), t.accent, detail),
                LoadState::Ready { detail } => (g.ok, t.ok, detail),
                LoadState::NeedsAttention { detail } => (g.attention, t.warn, detail),
                LoadState::Failed { error } => (g.fail, t.error, error),
                LoadState::Disabled { reason } => (g.disabled, t.dim, reason),
            };
            Line::from(vec![
                Span::styled(format!(" {icon} "), style),
                Span::raw(format!("{:<13}", subsystem.label())),
                Span::styled(
                    text.to_owned(),
                    if matches!(state, LoadState::Disabled { .. }) {
                        t.dim
                    } else {
                        Style::new()
                    },
                ),
            ])
        })
        .collect();
    f.render_widget(Paragraph::new(lines), area);
}

/// Diff and output lines shown under a finished tool call.
const TOOL_DETAIL_LINES: usize = 24;
/// Live output lines shown while a tool runs, and for failures.
const TOOL_LIVE_LINES: usize = 6;
/// Output lines shown under a successful command.
const TOOL_OK_LINES: usize = 3;

fn transcript_rows(app: &App, width: usize) -> Vec<Line<'static>> {
    let t = &app.theme;
    let mut rows = Vec::new();
    let body_width = width.saturating_sub(3);
    for (i, entry) in app.transcript.iter().enumerate() {
        let (prefix, prefix_style, body_style, text) = match entry {
            Entry::User(s) => {
                // A tinted band, so each turn's start is easy to find when scrolling.
                for (i, row) in wrap(s, body_width).into_iter().enumerate() {
                    let lead = if i == 0 {
                        format!(" {} ", app.glyphs.prompt)
                    } else {
                        "   ".into()
                    };
                    let pad = width.saturating_sub(3 + row.width());
                    rows.push(Line::from(vec![
                        Span::styled(lead, t.accent.patch(t.user_bg)),
                        Span::styled(row, t.user.patch(t.user_bg)),
                        Span::styled(" ".repeat(pad), t.user_bg),
                    ]));
                }
                rows.push(Line::raw(""));
                continue;
            }
            Entry::TurnSummary(s) => {
                let rule = app.glyphs.rule;
                let text = format!("{rule}{rule} {s} ");
                let fill = width.saturating_sub(3 + text.width()).min(8);
                rows.push(Line::styled(
                    format!("   {text}{}", rule.repeat(fill)),
                    t.dim,
                ));
                rows.push(Line::raw(""));
                continue;
            }
            Entry::Assistant(s) if s.trim().is_empty() => continue,
            Entry::Assistant(s) => {
                let lead_w = 2 + app.glyphs.assistant.width();
                let body_w = width.saturating_sub(lead_w);
                for (i, row) in crate::markdown::render(s, body_w, t, app.info.ui.ascii)
                    .into_iter()
                    .enumerate()
                {
                    let lead = if i == 0 {
                        format!(" {} ", app.glyphs.assistant)
                    } else {
                        " ".repeat(lead_w)
                    };
                    let mut spans = vec![Span::styled(lead, t.accent)];
                    spans.extend(row);
                    rows.push(Line::from(spans));
                }
                rows.push(Line::raw(""));
                continue;
            }
            Entry::Reasoning(s) => match app.show_reasoning {
                ShowReasoning::Hidden => continue,
                ShowReasoning::Collapsed => {
                    let words = s.split_whitespace().count();
                    let italic = t.dim.add_modifier(Modifier::ITALIC);
                    let live = app.turn_running && i + 1 == app.transcript.len();
                    let what = if live {
                        format!("thinking… {words} words")
                    } else {
                        format!("thought for {words} words")
                    };
                    (
                        app.glyphs.think,
                        t.magenta,
                        italic,
                        format!("{what} · ctrl-t shows"),
                    )
                }
                ShowReasoning::Expanded => (
                    app.glyphs.think,
                    t.magenta,
                    t.dim.add_modifier(Modifier::ITALIC),
                    s.trim().to_owned(),
                ),
            },
            Entry::Tool(tool) => {
                tool_rows(app, tool, body_width, &mut rows);
                // Consecutive tool calls read as one block.
                if !matches!(app.transcript.get(i + 1), Some(Entry::Tool(_)) | None) {
                    rows.push(Line::raw(""));
                }
                continue;
            }
            Entry::System(s) => (" ", t.dim, t.dim, s.clone()),
            Entry::Warning(s) => ("!", t.warn, t.warn, s.clone()),
            Entry::Error(s) => (app.glyphs.fail, t.error, t.error, s.clone()),
        };
        for (i, row) in wrap(&text, body_width).into_iter().enumerate() {
            let lead = if i == 0 {
                format!(" {prefix} ")
            } else {
                "   ".into()
            };
            rows.push(Line::from(vec![
                Span::styled(lead, prefix_style),
                Span::styled(row, body_style),
            ]));
        }
        rows.push(Line::raw(""));
    }
    rows
}

fn tool_rows(app: &App, tool: &ToolEntry, width: usize, rows: &mut Vec<Line<'static>>) {
    let t = &app.theme;
    let (icon, style) = match tool.ok {
        None => (app.spinner(), t.accent),
        Some(true) => (app.glyphs.ok, t.ok),
        Some(false) => (app.glyphs.fail, t.error),
    };
    // How long it took (or has been running), when that is long enough to matter.
    let time = match (tool.took, tool.started) {
        (Some(d), _) => Some(d),
        (None, Some(s)) if tool.ok.is_none() => Some(s.elapsed()),
        _ => None,
    }
    .filter(|d| d.as_secs() >= 1)
    .map(|d| format!("  {}", crate::theme::elapsed(d)))
    .unwrap_or_default();
    let summary = truncate(&tool.summary, width.saturating_sub(2 + time.width()));
    // The tool's name stands out in its colour; the rest is its argument.
    let (verb, rest) = match summary.split_once(' ') {
        Some((v, r)) => (v.to_owned(), format!(" {r}")),
        None => (summary.clone(), String::new()),
    };
    let mut head = vec![
        Span::styled(format!(" {icon} "), style),
        Span::styled(verb, t.tool(&tool.name).add_modifier(Modifier::BOLD)),
        Span::raw(rest),
    ];
    if !tool.result.is_empty() && !tool.summary.contains(&tool.result) {
        let room = width.saturating_sub(summary.width() + 3 + time.width());
        let result_style = if tool.ok == Some(false) {
            t.error
        } else {
            t.dim
        };
        head.push(Span::styled(
            format!(" · {}", truncate(&tool.result, room)),
            result_style,
        ));
    }
    head.push(Span::styled(time, t.dim));
    rows.push(Line::from(head));

    if tool.name == "todo" && tool.ok == Some(true) {
        for (state, text) in crate::app::parse_plan(&tool.detail) {
            let (mark, style) = plan_mark(app, state);
            let text_style = match state {
                PlanState::Done => t.dim.add_modifier(Modifier::CROSSED_OUT),
                PlanState::Active => Style::new().add_modifier(Modifier::BOLD),
                PlanState::Pending => Style::new(),
            };
            rows.push(Line::from(vec![
                Span::styled(format!("     {mark} "), style),
                Span::styled(truncate(&text, width.saturating_sub(6)), text_style),
            ]));
        }
        return;
    }

    if tool.name == "ask_user" && tool.ok == Some(true) {
        let mut q = "";
        for l in tool.detail.lines() {
            if let Some(rest) = l.strip_prefix("Q: ") {
                q = rest;
            } else if let Some(a) = l.strip_prefix("A: ") {
                let q = truncate(q, width.saturating_sub(10) / 2);
                let used = q.width() + 9;
                rows.push(Line::from(vec![
                    Span::styled(format!("     {q} → "), t.dim),
                    Span::raw(truncate(a, width.saturating_sub(used))),
                ]));
            }
        }
        return;
    }

    if let Some(run) = app.agents.iter().rev().find(|a| a.call_id == tool.call_id) {
        agent_rows(app, run, tool, width, rows);
        return;
    }

    let is_diff = tool.detail.starts_with("--- ");
    let lines: Vec<&str> = tool.detail.lines().collect();
    let shown: Vec<&str> = if tool.ok.is_none() {
        lines[lines.len().saturating_sub(TOOL_LIVE_LINES)..].to_vec()
    } else if is_diff {
        // Skip the ---/+++ header; hunks carry the information.
        lines
            .iter()
            .skip(2)
            .take(TOOL_DETAIL_LINES)
            .copied()
            .collect()
    } else if tool.ok == Some(false) {
        lines[lines.len().saturating_sub(TOOL_LIVE_LINES)..].to_vec()
    } else {
        // Successful command output: the tail is usually what matters.
        lines[lines.len().saturating_sub(TOOL_OK_LINES)..].to_vec()
    };
    let hidden = if is_diff {
        lines.len().saturating_sub(2 + shown.len())
    } else {
        0
    };
    for l in shown {
        let style = if !is_diff {
            t.dim
        } else if l.starts_with('+') {
            t.ok
        } else if l.starts_with('-') {
            t.error
        } else if l.starts_with("@@") {
            t.accent
        } else {
            t.dim
        };
        let gutter = if is_diff { app.glyphs.gutter } else { " " };
        rows.push(Line::from(vec![
            Span::raw("    "),
            Span::styled(gutter, style),
            Span::styled(
                truncate(&l.replace('\t', "    "), width.saturating_sub(3)),
                style,
            ),
        ]));
    }
    if hidden > 0 {
        rows.push(Line::styled(
            format!("     … {hidden} more diff lines"),
            t.dim,
        ));
    }
}

/// Under a `task` call: what its sub-agent is doing, then the start of its report.
fn agent_rows(
    app: &App,
    run: &crate::app::AgentRun,
    tool: &ToolEntry,
    width: usize,
    rows: &mut Vec<Line<'static>>,
) {
    use crate::app::AgentItem;
    let t = &app.theme;
    let row = |rows: &mut Vec<Line<'static>>, text: &str, style: Style| {
        rows.push(Line::from(vec![
            Span::styled("     › ", t.dim),
            Span::styled(truncate(text, width.saturating_sub(7)), style),
        ]));
    };
    if tool.ok.is_none() {
        let start = run.items.len().saturating_sub(TOOL_LIVE_LINES);
        for item in &run.items[start..] {
            match item {
                AgentItem::Tool(c) => {
                    let line = if c.result.is_empty() {
                        c.summary.clone()
                    } else {
                        format!("{} · {}", c.summary, c.result)
                    };
                    let style = if c.ok == Some(false) { t.error } else { t.dim };
                    row(rows, &line, style);
                }
                AgentItem::Text(text) => {
                    if let Some(l) = text.lines().rev().find(|l| !l.trim().is_empty()) {
                        row(rows, l.trim(), t.dim.add_modifier(Modifier::ITALIC));
                    }
                }
            }
        }
        return;
    }
    let report: Vec<&str> = tool
        .detail
        .lines()
        .filter(|l| !l.trim().is_empty())
        .collect();
    for l in report.iter().take(TOOL_OK_LINES) {
        row(rows, l.trim(), t.dim);
    }
    let more = report.len().saturating_sub(TOOL_OK_LINES);
    let more = if more > 0 {
        format!("{more} more lines · ")
    } else {
        String::new()
    };
    rows.push(Line::styled(
        format!("       {more}/tasks shows what it did"),
        t.dim,
    ));
}

fn draw_transcript(f: &mut Frame<'_>, app: &App, area: Rect) {
    if app.transcript.is_empty() {
        f.render_widget(Paragraph::new(welcome(app, area)), area);
        return;
    }
    // Only the visible rows are handed to ratatui. Per-message layout caching lands in M1.
    // The last column is kept for the scrollbar.
    let rows = transcript_rows(app, usize::from(area.width.saturating_sub(1)));
    let height = usize::from(area.height);
    let total = rows.len();
    let max = total.saturating_sub(height);
    // Scrolled up while a reply streams in: move with the new rows so the view holds still.
    let (drawn, _) = app.view.get();
    let mut scroll = app.scroll.get();
    if scroll > 0 && drawn > 0 && total > drawn {
        scroll = scroll.saturating_add(total - drawn);
    }
    let scroll = scroll.min(max);
    app.scroll.set(scroll);
    app.view.set((total, height));

    let end = total - scroll;
    let start = end.saturating_sub(height);
    f.render_widget(Paragraph::new(rows[start..end].to_vec()), area);

    if max > 0 {
        let mut state = ScrollbarState::new(max)
            .position(max - scroll)
            .viewport_content_length(height);
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .thumb_symbol(app.glyphs.scroll[0])
                .thumb_style(app.theme.dim)
                .track_symbol(Some(app.glyphs.scroll[1]))
                .track_style(app.theme.border),
            area,
            &mut state,
        );
    }
    if scroll > 0 {
        // What is below, and the way back to it, on the bottom row.
        let arrow = if app.info.ui.ascii { "v" } else { "↓" };
        let hint = format!(" {arrow} {scroll} more lines · ctrl-end jumps to the latest ");
        let w = u16::try_from(hint.width())
            .unwrap_or(u16::MAX)
            .min(area.width);
        let at = Rect {
            x: area.right().saturating_sub(w + 1),
            y: area.bottom() - 1,
            width: w,
            height: 1,
        };
        f.render_widget(
            Paragraph::new(Span::styled(hint, app.theme.pill(app.theme.accent))),
            at,
        );
    }
}

fn draw_input(f: &mut Frame<'_>, app: &App, area: Rect) {
    let block = Block::bordered()
        .border_set(app.glyphs.border)
        .border_style(app.theme.border);
    let inner = block.inner(area);
    let (cur_line, cur_col) = app.input.cursor_line_col();
    let first = cur_line.saturating_sub(MAX_INPUT_LINES - 1);
    let lines: Vec<Line<'_>> = app
        .input
        .text()
        .split('\n')
        .enumerate()
        .skip(first)
        .take(MAX_INPUT_LINES)
        .map(|(i, l)| {
            let lead = if i == 0 {
                format!("{} ", app.glyphs.prompt)
            } else {
                "  ".into()
            };
            Line::from(vec![
                Span::styled(lead, app.theme.accent),
                Span::raw(l.to_owned()),
            ])
        })
        .collect();
    f.render_widget(Paragraph::new(lines).block(block), area);
    if app.modal.is_none() {
        let line_text = app
            .input
            .text()
            .split('\n')
            .nth(cur_line)
            .unwrap_or_default();
        let col_w: usize = line_text.chars().take(cur_col).collect::<String>().width();
        let x = inner.x + 2 + u16::try_from(col_w).unwrap_or(0);
        let y = inner.y + u16::try_from(cur_line - first).unwrap_or(0);
        f.set_cursor_position((x.min(inner.right().saturating_sub(1)), y));
    }
}

fn draw_status(f: &mut Frame<'_>, app: &App, area: Rect) {
    let t = &app.theme;
    let mode = &app.info.permission_mode;
    let label = if mode == "yolo" {
        "YOLO".to_owned()
    } else {
        mode.clone()
    };
    let mut left: Vec<Span<'_>> = vec![
        Span::raw(" "),
        Span::styled(format!(" {label} "), t.pill(t.mode(mode))),
        Span::raw(" "),
    ];
    match &app.info.model {
        Some(m) => left.push(Span::raw(m.clone())),
        None => left.push(Span::styled("no model", t.warn)),
    }
    if app.info.profile != "empty" {
        left.push(Span::styled(format!(" · {}", app.info.profile), t.dim));
    }
    if let Some((used, budget)) = app.context {
        // Not a gauge: how much is left before compaction, and how long that lasts.
        let k = crate::theme::tokens;
        left.push(Span::styled(" · ", t.dim));
        match app.room() {
            Some((room, turns)) => {
                let line = app.compact_at.unwrap_or(budget).max(1);
                #[allow(clippy::cast_precision_loss)]
                let frac = used as f32 / line as f32;
                let style = if frac >= 0.85 {
                    t.error
                } else if frac >= 0.6 {
                    t.warn
                } else {
                    t.dim
                };
                let text = match turns {
                    _ if room == 0 => "compacts next turn".to_owned(),
                    Some(0) => format!("{} left · compacts soon", k(room)),
                    Some(1) => format!("{} left · ~1 turn", k(room)),
                    Some(n) => format!("{} left · ~{n} turns", k(room)),
                    None => format!("{} left", k(room)),
                };
                left.push(Span::styled(text, style));
            }
            None => left.push(Span::styled(format!("{} in context", k(used)), t.dim)),
        }
    }
    if let Some(tps) = app.speed {
        left.push(Span::styled(format!(" · {tps:.0} t/s"), t.dim));
    }
    let mut right: Vec<Span<'_>> = Vec::new();
    if app.approval.is_some() {
        right.push(Span::styled("waiting for approval", t.warn));
    } else if app.turn_running {
        let clock = app
            .turn_started
            .map(|s| s.elapsed())
            .filter(|d| d.as_secs() >= 1)
            .map(|d| format!(" · {}", crate::theme::elapsed(d)))
            .unwrap_or_default();
        right.push(Span::styled(
            format!("{} {}{clock}", app.spinner(), activity(app)),
            t.accent,
        ));
        right.push(Span::styled(" · esc to interrupt", t.dim));
    } else if app.queued {
        right.push(Span::styled(format!("{} queued", app.spinner()), t.warn));
    } else if !app.startup_done {
        right.push(Span::styled(format!("{} starting", app.spinner()), t.dim));
    }
    let issues = app.startup_issues();
    if issues > 0 && !app.panel_visible() {
        right.push(Span::styled(
            format!("  ! {issues} startup issue(s)"),
            t.warn,
        ));
    }
    right.push(Span::styled("  /help ", t.dim));
    let right_w: usize = right.iter().map(|s| s.content.width()).sum();
    let left_w: usize = left.iter().map(|s| s.content.width()).sum();
    let pad = usize::from(area.width).saturating_sub(left_w + right_w);
    let mut spans = left;
    spans.push(Span::raw(" ".repeat(pad)));
    spans.extend(right);
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_popup(f: &mut Frame<'_>, app: &App, above: Rect) {
    let matches = app.popup_matches();
    if matches.is_empty() {
        return;
    }
    let t = &app.theme;
    let g = &app.glyphs;
    let query = app.input.text().get(1..).unwrap_or_default().to_owned();
    let width = above.width;
    let inner_w = usize::from(width.saturating_sub(2));
    let selected = app.popup_selected.min(matches.len() - 1);

    let label = |i: &PopupItem| format!("/{}", i.name);
    let col = matches
        .iter()
        .map(|i| {
            label(i).width()
                + if i.args.is_empty() {
                    0
                } else {
                    i.args.width() + 1
                }
        })
        .max()
        .unwrap_or(0)
        .clamp(10, 30);
    let tag = |i: &PopupItem| match (i.command, i.hint) {
        (None, _) => "skill",
        (Some(_), Some(_)) => "soon",
        _ => "",
    };
    let room = |i: &PopupItem| {
        let tag_w = if tag(i).is_empty() {
            0
        } else {
            tag(i).width() + 2
        };
        inner_w.saturating_sub((2 + col + 2).max(2 + label(i).width() + 1) + tag_w + 1)
    };
    // Under the list: what the selected row could not show (the rest of its description,
    // aliases, when it lands), sized for the tallest so the popup doesn't jump.
    let about_w = inner_w.saturating_sub(3).max(10);
    let about = |i: &PopupItem| {
        let mut parts = Vec::new();
        if i.description.width() > room(i) {
            parts.push(i.description.clone());
        }
        if let Some(c) = i.command
            && !c.aliases.is_empty()
        {
            let also: Vec<String> = c.aliases.iter().map(|a| format!("/{a}")).collect();
            parts.push(format!("also {}", also.join(", ")));
        }
        if let Some(m) = i.command.and_then(|c| c.pending) {
            parts.push(format!("coming in {m}"));
        }
        if parts.is_empty() {
            return Vec::new();
        }
        let mut lines = wrap(&parts.join(" · "), about_w);
        lines.truncate(2);
        lines
    };
    let rows = matches.len().min(MAX_POPUP_ROWS);
    let first = selected.saturating_sub(rows - 1);
    let about_h = matches
        .iter()
        .skip(first)
        .take(rows)
        .map(|i| about(i).len())
        .max()
        .unwrap_or(0);
    let mut about_lines = about(&matches[selected]);
    about_lines.resize(about_h, String::new());

    let extra = if about_h > 0 { about_h + 1 } else { 0 };
    let height = u16::try_from(rows + extra + 2).unwrap_or(u16::MAX);
    if above.height < height {
        return;
    }
    let area = Rect {
        x: above.x,
        y: above.bottom() - height,
        width,
        height,
    };

    let mut lines: Vec<Line<'_>> = matches
        .iter()
        .enumerate()
        .skip(first)
        .take(rows)
        .map(|(i, item)| {
            let on = i == selected;
            let row = if on { t.selected } else { Style::new() };
            let soft = if on { t.sel_text } else { t.dim };
            let tag = tag(item);
            let tag_style = if item.command.is_none() {
                t.magenta
            } else {
                soft
            };
            let name = label(item);
            let hits: Vec<usize> = fuzzy::positions(&query, &item.name)
                .into_iter()
                .map(|p| p + 1)
                .collect();
            let mut spans = vec![
                Span::styled(if on { g.bar } else { " " }, row.patch(t.accent)),
                Span::styled(" ", row),
            ];
            spans.extend(t.highlight(
                &name,
                &hits,
                row.patch(t.accent).add_modifier(Modifier::BOLD),
            ));
            let mut used = 2 + name.width();
            if !item.args.is_empty() {
                let args = truncate(&item.args, col.saturating_sub(name.width() + 1).max(3));
                used += args.width() + 1;
                spans.push(Span::styled(format!(" {args}"), row.patch(soft)));
            }
            let pad = (2 + col + 2).saturating_sub(used);
            spans.push(Span::styled(" ".repeat(pad), row));
            used += pad;
            let tag_w = if tag.is_empty() { 0 } else { tag.width() + 2 };
            let desc_style = if on {
                t.selected.remove_modifier(Modifier::BOLD)
            } else if item.hint.is_some() && item.command.is_some() {
                t.dim
            } else {
                Style::new()
            };
            let desc = truncate(&item.description, room(item));
            used += desc.width();
            spans.push(Span::styled(desc, desc_style));
            let fill = inner_w.saturating_sub(used + tag_w);
            spans.push(Span::styled(" ".repeat(fill), row));
            if !tag.is_empty() {
                spans.push(Span::styled(format!("{tag} "), row.patch(tag_style)));
                spans.push(Span::styled(" ", row));
            }
            Line::from(spans)
        })
        .collect();
    if about_h > 0 {
        lines.push(Line::styled(g.rule.repeat(inner_w), t.border));
    }
    for (n, l) in about_lines.into_iter().enumerate() {
        let lead = if n == 0 && !l.is_empty() {
            g.assistant
        } else {
            " "
        };
        lines.push(Line::from(vec![
            Span::styled(format!(" {lead} "), t.accent),
            Span::styled(l, t.sel_text),
        ]));
    }

    let count = if matches.len() > rows {
        format!(" {} of {} ", selected + 1, matches.len())
    } else {
        format!(" {} ", matches.len())
    };
    let keys = key_hints(
        t,
        &[
            ("↑↓", "move"),
            ("tab", "complete"),
            ("⏎", "run"),
            ("esc", "close"),
        ],
    );
    f.render_widget(Clear, area);
    let block = Block::bordered()
        .border_set(g.border)
        .border_style(t.border)
        .title_top(Line::from(Span::styled(" commands ", t.pill(t.accent))))
        .title_top(Line::styled(count, t.dim).right_aligned())
        .title_bottom(keys.right_aligned());
    f.render_widget(Paragraph::new(lines).block(block), area);
}

/// `key what · key what`, for popup and modal footers.
pub(crate) fn key_hints(t: &crate::theme::Theme, keys: &[(&str, &str)]) -> Line<'static> {
    let mut spans = vec![Span::raw(" ")];
    for (i, (key, what)) in keys.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" · ", t.border));
        }
        spans.push(Span::styled(
            (*key).to_owned(),
            t.accent.add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(format!(" {what}"), t.dim));
    }
    spans.push(Span::raw(" "));
    Line::from(spans)
}

/// Bottom overlay asking to allow a tool call.
fn draw_approval(f: &mut Frame<'_>, app: &App, above: Rect) {
    let Some(approval) = &app.approval else {
        return;
    };
    let t = &app.theme;
    let (title, body): (String, Vec<Line<'_>>) = match &approval.request {
        ApprovalRequest::Shell {
            command,
            cwd,
            outside_sandbox,
        } => (
            match (cwd.as_str(), outside_sandbox) {
                (".", false) => " Run this command? ".to_owned(),
                (".", true) => " Run this command outside the sandbox? ".to_owned(),
                (_, false) => format!(" Run this command in {cwd}? "),
                (_, true) => format!(" Run this command in {cwd}, outside the sandbox? "),
            },
            command
                .lines()
                .map(|l| Line::from(vec![Span::styled("$ ", t.accent), Span::raw(l.to_owned())]))
                .collect(),
        ),
        ApprovalRequest::Edit { path, diff } => (
            format!(" Apply this change to {path}? "),
            diff.lines()
                .skip(2)
                .map(|l| {
                    let style = if l.starts_with('+') {
                        t.ok
                    } else if l.starts_with('-') {
                        t.error
                    } else if l.starts_with("@@") {
                        t.accent
                    } else {
                        t.dim
                    };
                    Line::styled(l.replace('\t', "    "), style)
                })
                .collect(),
        ),
        ApprovalRequest::Tool { name, arguments } => (
            format!(" Allow {name}? "),
            vec![Line::styled(arguments.clone(), t.dim)],
        ),
    };
    let max_body = usize::from(above.height.saturating_sub(4)).max(1);
    let shown = body.len().min(max_body);
    let height = u16::try_from(shown).unwrap_or(0) + 3;
    let area = Rect {
        x: above.x,
        y: above.bottom().saturating_sub(height),
        width: above.width,
        height: height.min(above.height),
    };
    let mut lines: Vec<Line<'_>> = body.into_iter().take(shown).collect();
    lines.push(Line::from(vec![
        Span::styled(" y ", t.pill(t.ok)),
        Span::raw(" allow  "),
        Span::styled(" a ", t.pill(t.accent)),
        Span::raw(" allow for this session  "),
        Span::styled(" n ", t.pill(t.error)),
        Span::raw(" deny  "),
        Span::styled("ctrl-c interrupts", t.dim),
    ]));
    f.render_widget(Clear, area);
    let block = Block::bordered()
        .title(Span::styled(title, t.pill(t.warn)))
        .border_set(app.glyphs.border)
        .border_style(t.warn);
    f.render_widget(Paragraph::new(lines).block(block), area);
}

/// The question or plan review bar, built before layout so it can take its rows.
struct Choices<'a> {
    title: String,
    /// The question, or the plan; it gives way first when rows are short.
    body: Vec<Line<'a>>,
    /// The choices and the keys line.
    rows: Vec<Line<'a>>,
}

impl Choices<'_> {
    /// Rows wanted, borders included.
    fn height(&self) -> usize {
        let spacer = usize::from(!self.body.is_empty());
        self.body.len() + spacer + self.rows.len() + 2
    }
}

/// Numbered choices above the input: the model's questions (`ask_user`), or the plan
/// review once a plan-mode turn ends. The input stays live for typed answers.
fn choices(app: &App, width: usize) -> Option<Choices<'_>> {
    if app.approval.is_some() {
        return None;
    }
    let t = &app.theme;
    let (arrows, left) = if app.info.ui.ascii {
        ("up/down", "left")
    } else {
        ("↑↓", "←")
    };
    // (label, description, recommended)
    let mut lead: Vec<Line<'_>> = Vec::new();
    let mut options: Vec<(String, Option<String>, bool)> = Vec::new();
    let (title, body, selected, own, keys) = if let Some(a) = &app.asking {
        let q = a.question();
        let n = a.questions.len();
        let count = if n > 1 {
            format!("{}/{n}", a.current + 1)
        } else {
            String::new()
        };
        let title = match (q.header.is_empty(), count.is_empty()) {
            (true, true) => " Question ".to_owned(),
            (true, false) => format!(" Question {count} "),
            (false, true) => format!(" {} ", q.header),
            (false, false) => format!(" {} · {count} ", q.header),
        };
        // The question is part of the choices, never cut.
        let body: Vec<Line<'_>> = Vec::new();
        lead = wrap(&q.question, width.saturating_sub(1))
            .into_iter()
            .map(|l| Line::styled(format!(" {l}"), Style::new().add_modifier(Modifier::BOLD)))
            .collect();
        for (i, o) in q.options.iter().enumerate() {
            options.push((
                o.label.clone(),
                o.description.clone(),
                q.recommended == Some(i),
            ));
        }
        let own = if app.input.is_empty() {
            "Type your own answer below"
        } else {
            "Your own answer (Enter sends it)"
        };
        let back = if a.current > 0 {
            format!("  {left} back")
        } else {
            String::new()
        };
        let keys = format!(
            " {arrows} choose  enter confirm  esc skip{back}  ctrl-s stop asking  ctrl-c interrupts"
        );
        (title, body, a.selected, Some(own), keys)
    } else {
        let selected = app.review?;
        let body: Vec<Line<'_>> = app
            .plan
            .iter()
            .map(|(state, text)| {
                let (mark, style) = plan_mark(app, *state);
                Line::from(vec![
                    Span::styled(format!(" {mark} "), style),
                    Span::raw(truncate(text, width.saturating_sub(4))),
                ])
            })
            .collect();
        for (_, label) in app.review_choices() {
            options.push((label, None, false));
        }
        let keys =
            format!(" {arrows} choose  enter confirm  esc keep planning  or type what to change");
        (
            " Ready to carry out this plan? ".to_owned(),
            body,
            selected,
            None,
            keys,
        )
    };

    let mut rows = lead;
    for (i, (label, description, recommended)) in options.iter().enumerate() {
        let on = i == selected;
        let lead = if on { app.glyphs.bar } else { " " };
        let style = if on {
            t.accent.add_modifier(Modifier::BOLD)
        } else {
            Style::new()
        };
        let mut spans = vec![
            Span::styled(lead.to_owned(), t.accent),
            Span::styled(format!("{}. {label}", i + 1), style),
        ];
        if *recommended {
            spans.push(Span::styled(" (recommended)", t.ok));
        }
        if let Some(d) = description {
            let used: usize = spans.iter().map(|s| s.content.width()).sum();
            let room = width.saturating_sub(used + 3);
            if room > 8 {
                spans.push(Span::styled(format!(" · {}", truncate(d, room)), t.dim));
            }
        }
        rows.push(Line::from(spans));
    }
    if let Some(own) = own {
        let on = selected == options.len();
        let lead = if on { app.glyphs.bar } else { " " };
        let style = if on { t.accent } else { t.dim };
        rows.push(Line::from(vec![
            Span::styled(lead.to_owned(), t.accent),
            Span::styled(format!("{}. {own}", options.len() + 1), style),
        ]));
    }
    rows.push(Line::styled(truncate(&keys, width), t.dim));
    Some(Choices { title, body, rows })
}

fn draw_choices(f: &mut Frame<'_>, app: &App, area: Rect, c: Choices<'_>) {
    if area.height == 0 {
        return;
    }
    let t = &app.theme;
    // The choices first; a long body (a plan) gives way, keeping its first lines.
    let room = usize::from(area.height)
        .saturating_sub(2 + c.rows.len() + 1)
        .min(c.body.len());
    let mut lines: Vec<Line<'_>> = c.body.into_iter().take(room).collect();
    if !lines.is_empty() {
        lines.push(Line::raw(""));
    }
    lines.extend(c.rows);
    let title = c.title;
    let block = Block::bordered()
        .title(Span::styled(title, t.pill(t.accent)))
        .border_set(app.glyphs.border)
        .border_style(t.accent);
    f.render_widget(Paragraph::new(lines).block(block), area);
}

/// What the model is doing right now, for the status bar.
fn activity(app: &App) -> String {
    match app.transcript.last() {
        Some(Entry::Reasoning(_)) => "thinking".into(),
        Some(Entry::Assistant(_)) => "writing".into(),
        Some(Entry::Tool(t)) if t.ok.is_none() => format!("running {}", t.name),
        _ => "working".into(),
    }
}

fn plan_mark(app: &App, state: PlanState) -> (&'static str, Style) {
    let [done, active, pending] = app.glyphs.todo;
    match state {
        PlanState::Done => (done, app.theme.ok),
        PlanState::Active => (active, app.theme.warn),
        PlanState::Pending => (pending, app.theme.dim),
    }
}

/// The plan strip shows while the model's plan has unfinished items.
fn plan_open(app: &App) -> bool {
    app.plan.iter().any(|(s, _)| *s != PlanState::Done)
}

/// One row above the input: progress through the plan and the step in hand.
fn draw_plan(f: &mut Frame<'_>, app: &App, area: Rect) {
    let t = &app.theme;
    let total = app.plan.len();
    let done = app
        .plan
        .iter()
        .filter(|(s, _)| *s == PlanState::Done)
        .count();
    let (state, step) = app
        .plan
        .iter()
        .find(|(s, _)| *s == PlanState::Active)
        .or_else(|| app.plan.iter().find(|(s, _)| *s == PlanState::Pending))
        .map(|(s, text)| (*s, text.as_str()))
        .unwrap_or((PlanState::Pending, ""));
    let (mark, mark_style) = plan_mark(app, state);
    #[allow(clippy::cast_precision_loss)]
    let meter = crate::theme::meter(
        done as f32 / total.max(1) as f32,
        total.min(10),
        app.info.ui.ascii,
    );
    let lead = vec![
        Span::raw(" "),
        Span::styled(" plan ", t.pill(t.ok)),
        Span::raw(" "),
        Span::styled(meter, t.ok),
        Span::styled(format!(" {done}/{total}  "), t.dim),
        Span::styled(format!("{mark} "), mark_style),
    ];
    let used: usize = lead.iter().map(|s| s.content.width()).sum();
    let mut spans = lead;
    spans.push(Span::raw(truncate(
        step,
        usize::from(area.width).saturating_sub(used + 1),
    )));
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// `/todo`: every step, under the one-line summary. Done steps scroll off first when the
/// plan is longer than the room.
fn draw_plan_list(f: &mut Frame<'_>, app: &App, area: Rect) {
    let [head, list] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
    draw_plan(f, app, head);
    let room = usize::from(list.height);
    let skip = app
        .plan
        .iter()
        .take_while(|(s, _)| *s == PlanState::Done)
        .count()
        .min(app.plan.len().saturating_sub(room));
    let width = usize::from(list.width).saturating_sub(6);
    let lines: Vec<Line<'static>> = app
        .plan
        .iter()
        .skip(skip)
        .take(room)
        .map(|(state, text)| {
            let (mark, style) = plan_mark(app, *state);
            let text_style = match state {
                PlanState::Done => app.theme.dim,
                PlanState::Active => style,
                PlanState::Pending => Style::default(),
            };
            Line::from(vec![
                Span::raw("   "),
                Span::styled(format!("{mark} "), style),
                Span::styled(truncate(text, width), text_style),
            ])
        })
        .collect();
    f.render_widget(Paragraph::new(lines), list);
}

/// The empty transcript: a duck, and the few things worth knowing first.
fn welcome(app: &App, area: Rect) -> Vec<Line<'static>> {
    let t = &app.theme;
    let hint = |key: &str, what: &str| {
        vec![
            Span::styled(key.to_owned(), t.accent),
            Span::styled(format!(" {what}"), t.dim),
        ]
    };
    let mut title = t.gradient_spans("quack");
    title.push(Span::styled(
        "  a coding agent for open-weight models",
        t.dim,
    ));
    let model = match &app.info.model {
        Some(m) => vec![
            Span::styled("model ", t.dim),
            Span::raw(m.clone()),
            Span::styled(" · /models to switch", t.dim),
        ],
        None => vec![
            Span::styled("no model yet", t.warn),
            Span::styled(" · /setup adds a provider", t.dim),
        ],
    };
    let right: Vec<Vec<Span<'static>>> = vec![
        title,
        vec![],
        model,
        [
            hint("/", "commands  "),
            hint("shift+tab", "modes  "),
            hint("/plan", "plan first"),
        ]
        .concat(),
        [
            hint("/help", "everything  "),
            hint("esc esc", "rewind  "),
            hint("/yolo", "no prompts"),
        ]
        .concat(),
    ];
    // Too narrow or short for the duck: just the tips.
    if area.width < 64 || area.height < 7 {
        let mut lines = vec![Line::raw("")];
        lines.extend(right.into_iter().skip(2).map(|mut r| {
            r.insert(0, Span::raw("   "));
            Line::from(r)
        }));
        return lines;
    }
    let body = t.warn;
    let beak = t.gradient.get(1).map_or(t.warn, |c| {
        Style::new().fg(ratatui::style::Color::Rgb(c.0, c.1, c.2))
    });

    let step = if app.info.ui.reduced_motion {
        0
    } else {
        (app.frame() / 8) % 16
    };

    let (shift, eye, beak_lower, tail, wave_idx) = match step {
        // 0: default / static pose
        0 => (0, "(o )", "_>", " /  ", 0),
        1 => (0, "(o )", "_>", " /  ", 1),
        2 => (0, "(o )", "_>", " |  ", 2),
        3 => (0, "(o )", "_>", " /  ", 3),
        // 4..9: glide forward & quack
        4 => (1, "(o )", "_>", " / ", 0),
        5 => (1, "(o )", "_>", " | ", 1),
        6 => (1, "(o )", "_>", " / ", 2),
        7 => (1, "(o )", " >", " / ", 3), // quack!
        8 => (1, "(o )", " >", " / ", 0), // quack!
        9 => (1, "(o )", "_>", " / ", 1),
        // 10..15: glide back, blink & rest
        10 => (0, "(o )", "_>", " |  ", 2),
        11 => (0, "(o )", "_>", " /  ", 3),
        12 => (0, "(o )", "_>", " /  ", 0),
        13 => (0, "(- )", "_>", " /  ", 1), // blink!
        14 => (0, "(o )", "_>", " /  ", 2),
        _ => (0, "(o )", "_>", " /  ", 3),
    };

    let water = if app.info.ui.ascii {
        match wave_idx {
            1 | 3 => "~-~-~-~-~-",
            2 => "-~-~-~-~-~",
            _ => "~~~~~~~~~~",
        }
    } else {
        match wave_idx {
            1 | 3 => "~≈~≈~≈~≈~≈",
            2 => "≈~≈~≈~≈~≈~",
            _ => "~~~~~~~~~~",
        }
    };

    let duck: [Vec<Span<'static>>; 5] = if shift == 0 {
        [
            vec![Span::styled("      __      ", body)],
            vec![
                Span::styled("    ", body),
                Span::styled("<", beak),
                Span::styled(format!("{eye}___  "), body),
            ],
            vec![
                Span::styled("     ( .", body),
                Span::styled(beak_lower, beak),
                Span::styled(tail, body),
            ],
            vec![Span::styled("      `---'   ", body)],
            vec![
                Span::styled("   ", t.accent),
                Span::styled(format!("{water} "), t.accent),
            ],
        ]
    } else {
        [
            vec![Span::styled("       __     ", body)],
            vec![
                Span::styled("     ", body),
                Span::styled("<", beak),
                Span::styled(format!("{eye}___ "), body),
            ],
            vec![
                Span::styled("      ( .", body),
                Span::styled(beak_lower, beak),
                Span::styled(tail, body),
            ],
            vec![Span::styled("       `---'  ", body)],
            vec![
                Span::styled("   ", t.accent),
                Span::styled(format!("{water} "), t.accent),
            ],
        ]
    };
    let mut lines = vec![Line::raw("")];
    for (d, r) in duck.into_iter().zip(right) {
        let mut spans = d;
        if !r.is_empty() {
            spans.push(Span::raw("  "));
        }
        spans.extend(r);
        lines.push(Line::from(spans));
    }
    lines
}

/// Truncate to `max` columns with an ellipsis.
fn truncate(s: &str, max: usize) -> String {
    if s.width() <= max {
        return s.to_owned();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let w = c.width().unwrap_or(0);
        if used + w + 1 > max {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_info;
    use quack_protocol::{Event, Subsystem};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn render(app: &App, w: u16, h: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).expect("terminal");
        terminal.draw(|f| draw(f, app)).expect("draw");
        terminal.backend().to_string()
    }

    fn started_app() -> App {
        let mut app = App::new(test_info());
        let rows = [
            (
                Subsystem::Config,
                LoadState::Ready {
                    detail: "2 config files · 1 providers · 3ms".into(),
                },
            ),
            (
                Subsystem::Project,
                LoadState::Disabled {
                    reason: "no .quack/ in this workspace".into(),
                },
            ),
            (
                Subsystem::Provider,
                LoadState::NeedsAttention {
                    detail: "no model selected · set [model].default or use /models".into(),
                },
            ),
            (
                Subsystem::Profile,
                LoadState::Ready {
                    detail: "empty (compiled-in)".into(),
                },
            ),
            (
                Subsystem::Tokenizer,
                LoadState::Running {
                    detail: "selecting tokenizer".into(),
                },
            ),
            (
                Subsystem::Lsp,
                LoadState::Failed {
                    error: "rust-analyzer: not found on PATH".into(),
                },
            ),
        ];
        for (subsystem, state) in rows {
            app.on_engine(Event::StartupProgress { subsystem, state });
        }
        app
    }

    #[test]
    fn startup_panel_80x24() {
        insta::assert_snapshot!(render(&started_app(), 80, 24));
    }

    #[test]
    fn help_modal_100x30() {
        let mut app = started_app();
        app.modal = Some(crate::modal::help(&app.theme));
        insta::assert_snapshot!(render(&app, 100, 30));
    }

    #[test]
    fn context_modal_100x30() {
        use quack_protocol::{ContextReport, ContextSegment};
        let mut app = started_app();
        let seg = |name: &str, tokens, detail: &str| ContextSegment {
            name: name.into(),
            tokens,
            detail: detail.into(),
        };
        let report = ContextReport {
            model: "charm/qwen3.8-flash".into(),
            window: Some(131_072),
            budget: Some(111_411),
            threshold: Some(89_128),
            used: 41_870,
            strategy: "auto".into(),
            segments: vec![
                seg("System prompt", 1_210, "profile generic, with instructions"),
                seg("Tool definitions", 1_650, "7 tools"),
                seg("Summary", 2_400, "earlier conversation, summarized"),
                seg("Your messages", 610, "6 messages"),
                seg("Model replies and tool calls", 4_300, "31 messages"),
                seg("Tool results", 31_700, "29 results, 8 pruned"),
            ],
            note: None,
        };
        let growth = [9_000, 16_000, 22_500, 30_000, 41_870];
        app.modal = Some(crate::modal::context(
            Some(&report),
            &growth,
            false,
            &app.theme,
        ));
        insta::assert_snapshot!(render(&app, 100, 30));
        // The biggest part, with its share drawn in its band colour.
        for _ in 0..6 {
            key(
                &mut app,
                crossterm::event::KeyCode::Down,
                crossterm::event::KeyModifiers::NONE,
            );
        }
        let s = render(&app, 100, 30);
        assert!(s.contains("of budget"), "{s}");
        insta::assert_snapshot!("context_modal_segment_100x30", s);
    }

    #[test]
    fn stats_modal_100x30() {
        use crossterm::event::{KeyCode, KeyModifiers};
        use quack_protocol::{RequestStat, StatsReport, ToolStat};
        let mut app = App::new(test_info());
        app.on_engine(Event::StartupComplete);
        let tool = |name: &str, calls, failed, ms| ToolStat {
            name: name.into(),
            calls,
            failed,
            ms,
        };
        let report = StatsReport {
            model: "charm/qwen3.8-flash".into(),
            turns: 6,
            requests: 23,
            unreported: 0,
            prompt_tokens: 412_000,
            completion_tokens: 9_800,
            cached_tokens: 301_000,
            cache_reported: 23,
            cost: Some(0.1374),
            unpriced: 0,
            ttft_ms: Some((640, 2_900)),
            tokens_per_sec: Some(52.0),
            open_ms: 600_000,
            model_ms: 210_000,
            tools_ms: 45_000,
            approval_ms: 30_000,
            tools: vec![
                tool("read", 9, 0, 400),
                tool("shell", 6, 2, 40_000),
                tool("edit", 4, 0, 4_600),
            ],
            compactions: 1,
            pruned: 8,
            resumed: false,
            sampling: vec![("temperature".into(), 0.7), ("top_k".into(), 20.0)],
            // A context that grows, with a dip at the compaction.
            history: (0..23_u32)
                .map(|i| RequestStat {
                    prompt: 8_000 + (i % 14) * 2_000,
                    cached: 6_000 + (i % 14) * 1_500,
                    completion: 200 + (i * 37) % 700,
                    cost: Some(0.002 + f64::from(i % 14) * 0.0005),
                    ttft_ms: Some(400 + (i * 113) % 900),
                    tokens_per_sec: Some(40.0 + f64::from((i * 7) % 20) as f32),
                })
                .collect(),
        };
        app.modal = Some(crate::modal::stats(Some(&report), false, &app.theme));
        insta::assert_snapshot!(render(&app, 100, 30));
        for pane in ["tokens", "cost", "time", "tools"] {
            key(&mut app, KeyCode::Down, KeyModifiers::NONE);
            let s = render(&app, 100, 30);
            if pane == "time" {
                assert!(s.contains("Where the time went"), "{s}");
                assert!(s.contains("the model"), "{s}");
                assert!(s.contains("35%"), "model is 210 of 600 s: {s}");
            }
            insta::assert_snapshot!(format!("stats_modal_{pane}_100x30"), s);
        }

        // Small terminals squeeze the charts or drop them, without panicking.
        for (w, h) in [(60, 20), (40, 12), (20, 8)] {
            for _ in 0..5 {
                render(&app, w, h);
                key(&mut app, KeyCode::Up, KeyModifiers::NONE);
            }
        }

        // ASCII mode draws the same charts from plain characters.
        app.modal = Some(crate::modal::stats(Some(&report), true, &app.theme));
        for _ in 0..5 {
            let s = render(&app, 100, 30);
            let fancy = s
                .chars()
                .find(|c| matches!(c, '▁'..='█' | '━' | '\u{2800}'..='\u{28ff}'));
            assert_eq!(fancy, None, "{s}");
            key(&mut app, KeyCode::Down, KeyModifiers::NONE);
        }
    }

    #[test]
    fn doctor_and_models_modals_100x30() {
        use crossterm::event::{KeyCode, KeyModifiers};
        use quack_protocol::{CheckStatus, DoctorCheck, ModelInfo, ProviderModels};
        let mut app = started_app();
        let check = |name: &str, status, detail: &str| DoctorCheck {
            name: name.into(),
            status,
            detail: detail.into(),
        };
        let checks = [
            check("config", CheckStatus::Ok, "2 files"),
            check("charm", CheckStatus::Ok, "ready"),
            check("ollama", CheckStatus::Warn, "not running"),
            check("sandbox", CheckStatus::Fail, "Landlock is not available"),
        ];
        app.modal = Some(crate::modal::doctor(Some(&checks), &app.theme));
        let s = render(&app, 100, 30);
        assert!(s.contains("Summary") && s.contains("1 failed"), "{s}");
        insta::assert_snapshot!("doctor_modal_100x30", s);

        let model = |id: &str, window| ModelInfo {
            id: id.into(),
            alias: None,
            context_window: window,
        };
        let catalog = [ProviderModels {
            bundled: false,
            id: "charm".into(),
            available: true,
            note: String::new(),
            models: vec![
                model("glm-5.3", Some(1_000_000)),
                model("qwen3.8-flash", Some(262_144)),
            ],
        }];
        app.modal = Some(crate::modal::models(
            Some(&catalog),
            Some("charm/glm-5.3"),
            false,
            &app.theme,
        ));
        key(&mut app, KeyCode::Down, KeyModifiers::NONE);
        let s = render(&app, 100, 30);
        assert!(s.contains("26% of largest"), "{s}");
        insta::assert_snapshot!("models_modal_100x30", s);
    }

    #[test]
    fn instructions_modal_100x30() {
        use quack_protocol::InstructionFile;
        let mut app = started_app();
        let file = |source: &str, in_prompt, text: &str| InstructionFile {
            source: source.into(),
            in_prompt,
            tokens: 1_200,
            text: text.into(),
        };
        let files = [
            file(
                "AGENTS.md",
                true,
                "# Project\n\nRun `cargo test` before finishing.",
            ),
            file("web/AGENTS.md", false, "Use pnpm."),
        ];
        app.modal = Some(crate::modal::instructions(Some(&files), &app.theme));
        let s = render(&app, 100, 30);
        assert!(
            s.contains("in the system prompt") && s.contains("Run `cargo test`"),
            "{s}"
        );
        insta::assert_snapshot!("instructions_modal_100x30", s);
        app.modal = Some(crate::modal::instructions(Some(&[]), &app.theme));
        let s = render(&app, 100, 30);
        assert!(s.contains("no instruction files"), "{s}");
    }

    #[test]
    fn profiles_modal_100x30() {
        use crossterm::event::{KeyCode, KeyModifiers};
        use quack_protocol::{Event, Op, ProfileInfo};
        let mut app = started_app();
        let profile = |id: &str, matches, tools: &[&str], system: &str| ProfileInfo {
            id: id.into(),
            description: format!("The {id} profile"),
            source: "Original".into(),
            matches,
            edit_format: "str_replace".into(),
            tools: tools.iter().map(|t| (*t).to_owned()).collect(),
            system: system.into(),
        };
        for c in "/profiles".chars() {
            key(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
        }
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.take_ops(), vec![Op::ListProfiles]);
        app.on_engine(Event::Profiles {
            active: "generic".into(),
            forced: false,
            profiles: vec![
                profile(
                    "generic",
                    true,
                    &["read", "edit", "shell"],
                    "You are a coding agent.\nBe brief.",
                ),
                profile(
                    "qwen-code",
                    false,
                    &["read_file", "edit", "run_shell_command"],
                    "You are an interactive agent.",
                ),
            ],
        });
        key(&mut app, KeyCode::Down, KeyModifiers::NONE);
        let s = render(&app, 100, 30);
        assert!(s.contains("read_file, edit, run_shell_command"), "{s}");
        assert!(s.contains("You are an interactive agent."), "{s}");
        insta::assert_snapshot!("profiles_modal_100x30", s);
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(
            app.take_ops(),
            vec![Op::SetProfile {
                id: "qwen-code".into()
            }]
        );
    }

    #[test]
    fn status_bar_shows_room_left_in_turns() {
        let mut app = started_app();
        app.context = Some((60_000, 104_000));
        app.compact_at = Some(83_200);
        let s = render(&app, 100, 24);
        assert!(s.contains("23.2k left"), "{s}");
        assert!(!s.contains("turns"), "no pace without turns: {s}");
        app.growth = vec![30_000, 38_000, 44_000, 52_000, 60_000];
        let s = render(&app, 100, 24);
        assert!(s.contains("23.2k left · ~2 turns"), "{s}");
        app.context = Some((90_000, 104_000));
        let s = render(&app, 100, 24);
        assert!(s.contains("compacts next turn"), "{s}");
    }

    #[test]
    fn config_modal_100x30() {
        use quack_config::SettingRow;
        let mut app = started_app();
        let row = |key: &str, value: &str, origin: &str, description: &str, is_bool| SettingRow {
            key: key.into(),
            value: value.into(),
            origin: origin.into(),
            description: description.into(),
            is_bool,
            locked: false,
        };
        let rows = vec![
            row(
                "model.default",
                "\"charm/qwen3.8-flash\"",
                "user: ~/.config/quack/config.toml",
                "`<provider-id>/<model-id>` or `<provider-id>/<alias>`. Empty: pick with `/models`.",
                false,
            ),
            row(
                "ui.ascii",
                "false",
                "default",
                "Use ASCII instead of box-drawing and spinner glyphs.",
                true,
            ),
        ];
        app.modal = Some(crate::modal::config(&rows, &[], &app.theme));
        insta::assert_snapshot!(render(&app, 100, 30));
    }

    #[test]
    fn help_modal_narrow_60x20() {
        let mut app = started_app();
        app.modal = Some(crate::modal::help(&app.theme));
        insta::assert_snapshot!(render(&app, 60, 20));
    }

    #[test]
    fn tool_calls_and_approval_80x24() {
        use crate::app::Approval;
        use quack_protocol::{CallId, ToolCallInfo, ToolResultSummary};
        let mut app = App::new(test_info());
        app.on_engine(Event::StartupComplete);
        app.transcript.push(Entry::User("rename the helper".into()));
        app.on_engine(Event::ReasoningDelta {
            text: "Need to find the helper first.".into(),
        });
        app.on_engine(Event::ToolCallBegin {
            call: ToolCallInfo {
                call_id: CallId("1".into()),
                name: "grep".into(),
                summary: "grep fn helper".into(),
            },
        });
        app.on_engine(Event::ToolCallEnd {
            call_id: CallId("1".into()),
            result: ToolResultSummary {
                ok: true,
                summary: "1 matches in 1 files".into(),
                detail: None,
            },
        });
        app.on_engine(Event::ToolCallBegin {
            call: ToolCallInfo {
                call_id: CallId("2".into()),
                name: "edit".into(),
                summary: "edit src/lib.rs".into(),
            },
        });
        app.on_engine(Event::ToolCallEnd {
            call_id: CallId("2".into()),
            result: ToolResultSummary {
                ok: true,
                summary: "+1 -1".into(),
                detail: Some("--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,2 +1,2 @@\n-fn helper() {}\n+fn assist() {}\n fn main() {}\n".into()),
            },
        });
        app.approval = Some(Approval {
            call_id: CallId("3".into()),
            request: ApprovalRequest::Shell {
                command: "cargo test".into(),
                cwd: ".".into(),
                outside_sandbox: false,
            },
        });
        app.context = Some((12_345, 104_000));
        insta::assert_snapshot!(render(&app, 80, 24));
    }

    #[test]
    fn todo_pane_lists_the_whole_plan_80x24() {
        use crate::app::PlanState;
        use crossterm::event::{KeyCode, KeyModifiers};
        let mut app = started_app();
        app.plan = vec![
            (PlanState::Done, "read the parser".into()),
            (PlanState::Active, "cache the token table".into()),
            (PlanState::Pending, "run the benchmarks".into()),
        ];
        let s = render(&app, 80, 24);
        assert!(!s.contains("run the benchmarks"), "only the summary: {s}");
        for c in "/todo".chars() {
            key(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
        }
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        let s = render(&app, 80, 24);
        assert!(
            s.contains("read the parser") && s.contains("run the benchmarks"),
            "{s}"
        );
        insta::assert_snapshot!("todo_pane_80x24", s);
    }

    #[test]
    fn plan_strip_and_turn_summary_80x24() {
        use quack_protocol::{CallId, ToolCallInfo, ToolResultSummary};
        let mut info = test_info();
        info.model = Some("charm/qwen3.8-flash".into());
        info.permission_mode = "auto-edit".into();
        let mut app = App::new(info);
        app.on_engine(Event::StartupComplete);
        app.transcript
            .push(Entry::User("make the parser faster".into()));
        let call = |app: &mut App,
                    id: &str,
                    name: &str,
                    summary: &str,
                    ok,
                    result: &str,
                    detail: Option<&str>| {
            app.on_engine(Event::ToolCallBegin {
                call: ToolCallInfo {
                    call_id: CallId(id.into()),
                    name: name.into(),
                    summary: summary.into(),
                },
            });
            app.on_engine(Event::ToolCallEnd {
                call_id: CallId(id.into()),
                result: ToolResultSummary {
                    ok,
                    summary: result.into(),
                    detail: detail.map(Into::into),
                },
            });
        };
        call(
            &mut app,
            "1",
            "todo",
            "plan 1/3 done · profile the hot loop",
            true,
            "plan 1/3 done",
            Some("[x] read the parser\n[~] profile the hot loop\n[ ] cache token lookups"),
        );
        call(
            &mut app,
            "2",
            "shell",
            "shell cargo bench",
            false,
            "exit 101",
            Some("error: no bench target"),
        );
        app.transcript.push(Entry::Assistant(
            "There is no bench target yet; adding one.".into(),
        ));
        app.transcript.push(Entry::TurnSummary(
            "14s · 2 tools · 1.2k tokens out · 48 t/s".into(),
        ));
        app.context = Some((70_000, 104_000));
        app.speed = Some(48.0);
        insta::assert_snapshot!(render(&app, 80, 24));
    }

    #[test]
    fn a_sub_agent_shows_under_its_task_and_in_tasks() {
        use crate::app::PlanState;
        use crossterm::event::{KeyCode, KeyModifiers};
        use quack_protocol::{CallId, ToolCallInfo, ToolResultSummary};
        let mut app = started_app();
        app.on_engine(Event::StartupComplete);
        app.on_engine(Event::ToolCallBegin {
            call: ToolCallInfo {
                call_id: CallId("t1".into()),
                name: "Task".into(),
                summary: "explore: find the parser".into(),
            },
        });
        app.on_engine(Event::Plan {
            checklist: "[~] find the parser\n[ ] fix it".into(),
        });
        assert_eq!(app.plan[0].0, PlanState::Active);
        app.on_engine(Event::AgentStarted {
            id: "explore-1".into(),
            agent: "explore".into(),
            description: "find the parser".into(),
            call_id: CallId("t1".into()),
            model: "mock/small".into(),
        });
        let inner = |e: Event| Event::Agent {
            id: "explore-1".into(),
            inner: Box::new(e),
        };
        app.on_engine(inner(Event::ToolCallBegin {
            call: ToolCallInfo {
                call_id: CallId("explore-1/c1".into()),
                name: "grep".into(),
                summary: "grep fn parse".into(),
            },
        }));
        app.on_engine(inner(Event::ToolCallEnd {
            call_id: CallId("explore-1/c1".into()),
            result: ToolResultSummary {
                ok: true,
                summary: "3 matches".into(),
                detail: None,
            },
        }));
        app.on_engine(inner(Event::AssistantDelta {
            text: "Reading src/parse.rs".into(),
        }));
        let s = render(&app, 80, 24);
        assert!(
            s.contains("› grep fn parse · 3 matches") && s.contains("› Reading src/parse.rs"),
            "{s}"
        );
        app.on_engine(inner(Event::AssistantRewrite {
            text: "The parser is parse() in src/parse.rs:12.".into(),
        }));
        app.on_engine(Event::AgentFinished {
            id: "explore-1".into(),
            ok: true,
            summary: "2 steps".into(),
        });
        app.on_engine(Event::ToolCallEnd {
            call_id: CallId("t1".into()),
            result: ToolResultSummary {
                ok: true,
                summary: "2 steps".into(),
                detail: Some("The parser is parse() in src/parse.rs:12.".into()),
            },
        });
        let s = render(&app, 80, 24);
        assert!(
            s.contains("› The parser is parse() in src/parse.rs:12.")
                && s.contains("/tasks shows what it did"),
            "{s}"
        );
        for c in "/tasks".chars() {
            key(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
        }
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        let s = render(&app, 100, 30);
        assert!(
            s.contains("explore-1 · find the parser")
                && s.contains("mock/small")
                && s.contains("grep fn parse · 3 matches")
                && s.contains("src/parse.rs:12"),
            "{s}"
        );
    }

    #[test]
    fn question_and_plan_review_bars() {
        use crate::app::PlanState;
        use quack_protocol::{CallId, Question, QuestionOption, TurnId, Usage};
        let mut app = started_app();
        app.on_engine(Event::Questions {
            call_id: CallId("q1".into()),
            questions: vec![
                Question {
                    header: "Storage".into(),
                    question: "Where should the login tokens live?".into(),
                    options: vec![
                        QuestionOption {
                            label: "OS keyring".into(),
                            description: Some("safest; needs secret-tool on Linux".into()),
                        },
                        QuestionOption {
                            label: "A dotfile".into(),
                            description: None,
                        },
                    ],
                    recommended: Some(0),
                },
                Question {
                    header: String::new(),
                    question: "Which database?".into(),
                    options: Vec::new(),
                    recommended: None,
                },
            ],
        });
        let s = render(&app, 80, 24);
        assert!(
            s.contains("Storage · 1/2") && s.contains("(recommended)"),
            "{s}"
        );
        insta::assert_snapshot!("question_bar_80x24", s);

        app.asking = None;
        app.on_engine(Event::PermissionMode {
            mode: "plan".into(),
        });
        app.plan = vec![
            (PlanState::Pending, "add the token store".into()),
            (PlanState::Pending, "wire the login command".into()),
            (PlanState::Pending, "test it against the mock server".into()),
        ];
        app.on_engine(Event::TurnComplete {
            turn: TurnId(1),
            usage: Usage {
                prompt_tokens: 1,
                completion_tokens: 1,
                cached_prompt_tokens: None,
            },
        });
        let s = render(&app, 100, 30);
        assert!(
            s.contains("Ready to carry out this plan?") && s.contains("wire the login command"),
            "{s}"
        );
        insta::assert_snapshot!("plan_review_bar_100x30", s);
    }

    #[test]
    fn turn_complete_adds_a_summary() {
        use quack_protocol::{TurnId, Usage};
        let mut app = App::new(test_info());
        app.on_engine(Event::TurnStarted { turn: TurnId(1) });
        app.on_engine(Event::TurnComplete {
            turn: TurnId(1),
            usage: Usage {
                prompt_tokens: 10,
                completion_tokens: 1234,
                cached_prompt_tokens: None,
            },
        });
        let Some(Entry::TurnSummary(s)) = app.transcript.last() else {
            panic!("no summary: {:?}", app.transcript);
        };
        assert!(s.ends_with("s · 1.2k tokens out"), "{s}");
    }

    #[test]
    fn slash_popup_and_transcript_ascii() {
        let mut info = test_info();
        info.ui.ascii = true;
        let mut app = App::new(info);
        app.on_engine(Event::StartupComplete);
        app.transcript
            .push(Entry::User("fix the failing test".into()));
        app.transcript.push(Entry::Assistant(
            "Looking at the test output now; the assertion compares a usize to a u32.".into(),
        ));
        app.input.set("/re");
        insta::assert_snapshot!(render(&app, 80, 24));
    }

    #[test]
    fn slash_popup_highlights_matched_letters() {
        let mut app = App::new(test_info());
        app.on_engine(Event::StartupComplete);
        app.input.set("/rw");
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("terminal");
        terminal.draw(|f| draw(f, &app)).expect("draw");
        let buf = terminal.backend().buffer();
        let row = (0..24)
            .find(|y| {
                (0..80)
                    .map(|x| buf[(x, *y)].symbol())
                    .collect::<String>()
                    .contains("/rewind")
            })
            .expect("rewind row");
        let slash = (0..80u16)
            .find(|x| buf[(*x, row)].symbol() == "/" && buf[(x + 1, row)].symbol() == "r")
            .expect("slash");
        let underlined = |x: u16| buf[(x, row)].modifier.contains(Modifier::UNDERLINED);
        let r = slash + 1;
        assert!(underlined(r), "r of /rewind is a match");
        assert!(!underlined(r + 1), "e is not");
        assert!(underlined(r + 2), "w is a match");
    }

    fn key(app: &mut App, code: crossterm::event::KeyCode, mods: crossterm::event::KeyModifiers) {
        app.on_terminal(crossterm::event::Event::Key(
            crossterm::event::KeyEvent::new(code, mods),
        ));
    }

    #[test]
    fn transcript_scrolls_to_top_and_bottom_and_holds_still_while_streaming() {
        use crossterm::event::{KeyCode, KeyModifiers};
        let mut app = started_app();
        for n in 1..=40 {
            app.transcript.push(Entry::User(format!("message {n}")));
        }
        let s = render(&app, 80, 24);
        assert!(s.contains("message 40") && !s.contains("message 1 "), "{s}");
        assert!(!s.contains("more lines"), "no hint at the bottom");

        key(&mut app, KeyCode::Home, KeyModifiers::CONTROL);
        let s = render(&app, 80, 24);
        assert!(s.contains("message 1 ") && !s.contains("message 40"), "{s}");
        assert!(s.contains("more lines · ctrl-end"), "{s}");

        // New output arrives below: the top stays in view.
        app.transcript.push(Entry::Assistant("a new reply".into()));
        let s = render(&app, 80, 24);
        assert!(s.contains("message 1 "), "{s}");

        key(&mut app, KeyCode::PageDown, KeyModifiers::NONE);
        let s = render(&app, 80, 24);
        assert!(!s.contains("message 1 "), "{s}");

        key(&mut app, KeyCode::End, KeyModifiers::CONTROL);
        let s = render(&app, 80, 24);
        assert!(
            s.contains("a new reply") && !s.contains("more lines"),
            "{s}"
        );

        // Plain Home and End still move the input cursor.
        key(&mut app, KeyCode::Up, KeyModifiers::SHIFT);
        assert_eq!(app.scroll.get(), 1);
        key(&mut app, KeyCode::End, KeyModifiers::NONE);
        assert_eq!(app.scroll.get(), 1);
    }

    #[test]
    fn ctrl_t_shows_and_hides_thinking() {
        use crossterm::event::{KeyCode, KeyModifiers};
        let mut app = started_app();
        app.transcript.push(Entry::User("hi".into()));
        app.on_engine(Event::ReasoningDelta {
            text: "The user greets me; greet back.".into(),
        });
        app.transcript.push(Entry::Assistant("Hello!".into()));
        let s = render(&app, 80, 24);
        assert!(s.contains("thought for 6 words · ctrl-t shows"), "{s}");
        assert!(!s.contains("greet back"));

        key(&mut app, KeyCode::Char('t'), KeyModifiers::CONTROL);
        let s = render(&app, 80, 24);
        assert!(s.contains("The user greets me; greet back."), "{s}");

        app.execute("/thinking hide");
        let s = render(&app, 80, 24);
        assert!(!s.contains("greet back"), "{s}");
        assert!(s.contains("thought for 6 words"), "{s}");
    }

    #[test]
    fn welcome_duck_animates_and_honors_reduced_motion() {
        let mut app = App::new(test_info());
        app.on_engine(Event::StartupComplete);

        // Frame 0: static duck, closed beak, open eye, resting wave
        let s0 = render(&app, 80, 24);
        assert!(s0.contains("<(o )___"));
        assert!(s0.contains("._> /"));
        assert!(s0.contains("~~~~~~~~~~"));

        // Advance 56 frames (step 7): quack with open beak and water ripple
        for _ in 0..56 {
            app.on_tick();
        }
        let s_quack = render(&app, 80, 24);
        assert!(s_quack.contains("<(o )___"));
        assert!(s_quack.contains(". > /"));
        assert!(s_quack.contains("~≈~≈~≈~≈~≈"));

        // Advance to step 13 (frame 104 = 56 + 48 more): eye blink
        for _ in 0..48 {
            app.on_tick();
        }
        let s_blink = render(&app, 80, 24);
        assert!(s_blink.contains("<(- )___"));

        // With reduced motion, step is locked at 0
        let mut info_rm = test_info();
        info_rm.ui.reduced_motion = true;
        let mut app_rm = App::new(info_rm);
        app_rm.on_engine(Event::StartupComplete);
        for _ in 0..104 {
            app_rm.on_tick();
        }
        let s_rm = render(&app_rm, 80, 24);
        assert!(s_rm.contains("<(o )___"));
        assert!(s_rm.contains("._> /"));
        assert!(s_rm.contains("~~~~~~~~~~"));
    }
}
