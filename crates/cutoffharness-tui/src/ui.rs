use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use cutoffharness_protocol::LoadState;

use cutoffharness_config::settings::ShowReasoning;
use cutoffharness_protocol::ApprovalRequest;

use crate::app::{App, Entry, ToolEntry};
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
    draw_approval(f, app, transcript);
    if let Some(modal) = &app.modal {
        modal.draw(f, &app.theme, &app.glyphs);
    }
}

fn draw_header(f: &mut Frame<'_>, app: &App, area: Rect) {
    let name = format!(" cutoffharness {}", app.info.version);
    let room = usize::from(area.width).saturating_sub(name.width() + 3);
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
    let line = Line::from(vec![
        Span::styled(name, app.theme.accent),
        Span::styled(format!(" · {cwd}"), app.theme.dim),
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
            Entry::User(s) => (app.glyphs.prompt, t.user, t.user, s.clone()),
            Entry::Assistant(s) if s.trim().is_empty() => continue,
            Entry::Assistant(s) => {
                for (i, row) in crate::markdown::render(s, body_width, t, app.info.ui.ascii)
                    .into_iter()
                    .enumerate()
                {
                    let lead = if i == 0 {
                        format!(" {} ", app.glyphs.assistant)
                    } else {
                        "   ".into()
                    };
                    let mut spans = vec![Span::styled(lead, t.accent)];
                    spans.extend(row);
                    rows.push(Line::from(spans));
                }
                rows.push(Line::raw(""));
                continue;
            }
            Entry::Reasoning(s) => match app.info.ui.show_reasoning {
                ShowReasoning::Hidden => continue,
                ShowReasoning::Collapsed => {
                    let words = s.split_whitespace().count();
                    ("~", t.dim, t.dim, format!("thought for {words} words"))
                }
                ShowReasoning::Expanded => ("~", t.dim, t.dim, s.trim().to_owned()),
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
    let mut head = vec![
        Span::styled(format!(" {icon} "), style),
        Span::raw(truncate(&tool.summary, width.saturating_sub(2))),
    ];
    if !tool.result.is_empty() {
        let room = width.saturating_sub(tool.summary.width() + 3);
        head.push(Span::styled(
            format!(" · {}", truncate(&tool.result, room)),
            t.dim,
        ));
    }
    rows.push(Line::from(head));

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
        rows.push(Line::from(vec![
            Span::raw("     "),
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
    let mut left = format!(
        " {model} · profile: {} · {}",
        app.info.profile, app.info.permission_mode
    );
    if let Some((used, budget)) = app.context {
        let k = crate::theme::tokens;
        if budget > 0 {
            left.push_str(&format!(" · {}/{} ctx", k(used), k(budget)));
        } else {
            left.push_str(&format!(" · {} ctx", k(used)));
        }
    }
    if let Some(tps) = app.speed {
        left.push_str(&format!(" · {tps:.0} t/s"));
    }
    let mut right: Vec<Span<'_>> = Vec::new();
    if app.approval.is_some() {
        right.push(Span::styled("waiting for approval", t.warn));
    } else if app.turn_running {
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
            let hint = cmd.hint.map(|m| format!(" ({m})")).unwrap_or_default();
            let room = usize::from(width).saturating_sub(2 + name.width() + hint.width() + 1);
            let desc = truncate(&cmd.description, room);
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

/// Bottom overlay asking to allow a tool call.
fn draw_approval(f: &mut Frame<'_>, app: &App, above: Rect) {
    let Some(approval) = &app.approval else {
        return;
    };
    let t = &app.theme;
    let (title, body): (String, Vec<Line<'_>>) = match &approval.request {
        ApprovalRequest::Shell { command, cwd } => (
            if cwd == "." {
                " Run this command? ".to_owned()
            } else {
                format!(" Run this command in {cwd}? ")
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
        Span::styled(" y ", t.selected),
        Span::raw(" allow  "),
        Span::styled(" a ", t.selected),
        Span::raw(" allow for this session  "),
        Span::styled(" n ", t.selected),
        Span::raw(" deny  "),
        Span::styled("ctrl-c interrupts", t.dim),
    ]));
    f.render_widget(Clear, area);
    let block = Block::bordered()
        .title(Span::styled(title, t.warn))
        .border_set(app.glyphs.border)
        .border_style(t.warn);
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
    fn context_modal_100x30() {
        use cutoffharness_protocol::{ContextReport, ContextSegment};
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
        use cutoffharness_config::SettingRow;
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
                "user: ~/.config/cutoffharness/config.toml",
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
        use cutoffharness_protocol::{CallId, ToolCallInfo, ToolResultSummary};
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
            },
        });
        app.context = Some((12_345, 104_000));
        insta::assert_snapshot!(render(&app, 80, 24));
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
