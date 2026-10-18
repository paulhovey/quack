use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use cutoffharness_protocol::LoadState;

use crate::app::{App, Entry};
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
    let [header, panel, transcript, input, status] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(panel_rows),
        Constraint::Min(1),
        Constraint::Length(input_rows),
        Constraint::Length(1),
    ])
    .areas(f.area());

    draw_header(f, app, header);
    if panel_rows > 0 {
        draw_panel(f, app, panel);
    }
    draw_transcript(f, app, transcript);
    draw_input(f, app, input);
    draw_status(f, app, status);
    draw_popup(f, app, transcript);
    if let Some(modal) = &app.modal {
        modal.draw(f, &app.theme, &app.glyphs);
    }
}

fn draw_header(f: &mut Frame<'_>, app: &App, area: Rect) {
    let line = Line::from(vec![
        Span::styled(
            format!(" cutoffharness {}", app.info.version),
            app.theme.accent,
        ),
        Span::styled(format!(" · {}", app.info.cwd), app.theme.dim),
    ]);
    f.render_widget(Paragraph::new(line), area);
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

fn transcript_rows(app: &App, width: usize) -> Vec<Line<'static>> {
    let t = &app.theme;
    let mut rows = Vec::new();
    for entry in &app.transcript {
        let (prefix, prefix_style, body_style, text) = match entry {
            Entry::User(s) => (app.glyphs.prompt, t.user, t.user, s),
            Entry::Assistant(s) => (app.glyphs.assistant, t.accent, Style::new(), s),
            Entry::System(s) => (" ", t.dim, t.dim, s),
            Entry::Warning(s) => ("!", t.warn, t.warn, s),
            Entry::Error(s) => (app.glyphs.fail, t.error, t.error, s),
        };
        for (i, row) in wrap(text, width.saturating_sub(3)).into_iter().enumerate() {
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

fn draw_transcript(f: &mut Frame<'_>, app: &App, area: Rect) {
    if app.transcript.is_empty() {
        let hint = Line::styled(
            "   Type a message, or / for commands. /help lists everything.",
            app.theme.dim,
        );
        f.render_widget(Paragraph::new(vec![Line::raw(""), hint]), area);
        return;
    }
    // Only the visible rows are handed to ratatui. Per-message layout caching lands in M1.
    let rows = transcript_rows(app, usize::from(area.width));
    let height = usize::from(area.height);
    let end = rows
        .len()
        .saturating_sub(app.scroll.min(rows.len().saturating_sub(height)));
    let start = end.saturating_sub(height);
    f.render_widget(Paragraph::new(rows[start..end].to_vec()), area);
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
    let model = app.info.model.as_deref().unwrap_or("no model");
    let left = format!(
        " {model} · profile: {} · {}",
        app.info.profile, app.info.permission_mode
    );
    let mut right: Vec<Span<'_>> = Vec::new();
    if app.turn_running {
        right.push(Span::styled(
            format!("{} working · esc to interrupt", app.spinner()),
            t.accent,
        ));
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
    let pad = usize::from(area.width).saturating_sub(left.width() + right_w);
    let mut spans = vec![Span::styled(left, t.dim), Span::raw(" ".repeat(pad))];
    spans.extend(right);
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_popup(f: &mut Frame<'_>, app: &App, above: Rect) {
    let matches = app.popup_matches();
    if matches.is_empty() {
        return;
    }
    let rows = matches.len().min(MAX_POPUP_ROWS);
    let height = u16::try_from(rows).unwrap_or(0) + 2;
    let width = above.width.min(72);
    if above.height < height {
        return;
    }
    let area = Rect {
        x: above.x,
        y: above.bottom() - height,
        width,
        height,
    };
    let selected = app.popup_selected.min(matches.len() - 1);
    let first = selected.saturating_sub(rows - 1);
    let lines: Vec<Line<'_>> = matches
        .iter()
        .enumerate()
        .skip(first)
        .take(rows)
        .map(|(i, cmd)| {
            let name = format!(" /{:<14}", format!("{} {}", cmd.name, cmd.args).trim_end());
            let hint = cmd.pending.map(|m| format!(" ({m})")).unwrap_or_default();
            let room = usize::from(width).saturating_sub(2 + name.width() + hint.width() + 1);
            let desc = truncate(cmd.description, room);
            let style = if i == selected {
                app.theme.selected
            } else {
                Style::new()
            };
            Line::from(vec![
                Span::styled(name, style.patch(app.theme.accent)),
                Span::styled(format!(" {desc}"), style),
                Span::styled(hint, style.patch(app.theme.dim)),
            ])
        })
        .collect();
    f.render_widget(Clear, area);
    let block = Block::bordered()
        .border_set(app.glyphs.border)
        .border_style(app.theme.border);
    f.render_widget(Paragraph::new(lines).block(block), area);
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
    use cutoffharness_protocol::{Event, Subsystem};
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
                    reason: "no .cutoffharness/ in this workspace".into(),
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
    fn help_modal_narrow_60x20() {
        let mut app = started_app();
        app.modal = Some(crate::modal::help(&app.theme));
        insta::assert_snapshot!(render(&app, 60, 20));
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
}
