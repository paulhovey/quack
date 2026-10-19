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
    let panel_rows = if app.panel_visible() {
        u16::try_from(app.startup.len()).unwrap_or(0) + 1
    } else {
        0
    };
    let input_rows = u16::try_from(app.input.line_count().min(MAX_INPUT_LINES)).unwrap_or(1) + 2;
    let plan_rows = u16::from(plan_open(app));
    let [header, panel, transcript, plan, input, status] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(panel_rows),
        Constraint::Min(1),
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
    if plan_rows > 0 {
        draw_plan(f, app, plan);
    }
    draw_input(f, app, input);
    draw_status(f, app, status);
    draw_popup(f, app, transcript);
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
        let k = crate::theme::tokens;
        if budget > 0 {
            #[allow(clippy::cast_precision_loss)]
            let frac = used as f32 / budget as f32;
            let style = if frac >= 0.8 {
                t.error
            } else if frac >= 0.5 {
                t.warn
            } else {
                t.ok
            };
            left.push(Span::styled(" · ", t.dim));
            left.push(Span::styled(
                crate::theme::meter(frac, 6, app.info.ui.ascii),
                style,
            ));
            left.push(Span::styled(format!(" {}/{}", k(used), k(budget)), t.dim));
        } else {
            left.push(Span::styled(format!(" · {} ctx", k(used)), t.dim));
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
        app.modal = Some(crate::modal::context(Some(&report), &app.theme));
        insta::assert_snapshot!(render(&app, 100, 30));
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
