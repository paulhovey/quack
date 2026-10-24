//! Markdown to styled, wrapped rows for assistant messages.
//!
//! Headings, emphasis, inline code, fenced code (not wrapped, truncated instead), lists,
//! block quotes, tables and rules. Code in a known language is highlighted once `highlight.rs` has
//! it, and plain until then.

use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::highlight::Highlights;
use crate::theme::Theme;

/// One logical line before wrapping.
#[derive(Debug, Default)]
struct Logical {
    spans: Vec<(String, Style)>,
    /// Prefix for the first row (list bullet, quote bar).
    first: String,
    /// Prefix for wrapped rows.
    rest: String,
    /// Code lines are truncated, not wrapped.
    nowrap: bool,
}

#[derive(Debug, Default)]
struct TableState {
    alignments: Vec<Alignment>,
    head: Vec<Vec<(String, Style)>>,
    rows: Vec<Vec<Vec<(String, Style)>>>,
    cur_cell: Vec<(String, Style)>,
    cur_row: Vec<Vec<(String, Style)>>,
    in_head: bool,
}

struct Builder<'t> {
    width: usize,
    ascii: bool,
    theme: &'t Theme,
    highlights: Option<&'t Highlights>,
    /// The open code block's info string and text, emitted at its end.
    code_tag: String,
    code_buf: String,
    out: Vec<Logical>,
    cur: Option<Logical>,
    styles: Vec<Style>,
    lists: Vec<Option<u64>>,
    quote: usize,
    code: bool,
    pending_item: Option<String>,
    table: Option<TableState>,
}

impl Builder<'_> {
    fn style(&self) -> Style {
        self.styles
            .iter()
            .fold(Style::new(), |acc, s| acc.patch(*s))
    }

    fn indent(&self) -> String {
        let depth = self.lists.len().saturating_sub(1);
        let mut s = "  ".repeat(depth);
        if self.quote > 0 {
            s = format!("{}{s}", "│ ".repeat(self.quote));
        }
        s
    }

    fn line(&mut self) -> &mut Logical {
        if self.cur.is_none() {
            let base = self.indent();
            let (first, rest) = match self.pending_item.take() {
                Some(bullet) => {
                    let pad = " ".repeat(bullet.width());
                    (format!("{base}{bullet}"), format!("{base}{pad}"))
                }
                None => {
                    let list_pad = if self.lists.is_empty() { "" } else { "  " };
                    (format!("{base}{list_pad}"), format!("{base}{list_pad}"))
                }
            };
            self.cur = Some(Logical {
                first,
                rest,
                ..Logical::default()
            });
        }
        self.cur.get_or_insert_with(Logical::default)
    }

    fn flush(&mut self) {
        if let Some(l) = self.cur.take() {
            self.out.push(l);
        }
        if self.table.is_some() {
            self.render_table();
        }
    }

    fn blank(&mut self) {
        self.flush();
        if self
            .out
            .last()
            .is_some_and(|l| !l.spans.is_empty() || !l.first.trim().is_empty())
        {
            self.out.push(Logical::default());
        }
    }

    fn text(&mut self, t: &str) {
        if self.code {
            self.code_buf.push_str(t);
            return;
        }
        let style = self.style();
        if let Some(table) = &mut self.table {
            table.cur_cell.push((t.to_owned(), style));
            return;
        }
        self.line().spans.push((t.to_owned(), style));
    }

    fn render_table(&mut self) {
        let mut table = match self.table.take() {
            Some(t) => t,
            None => return,
        };
        if !table.cur_cell.is_empty() {
            table.cur_row.push(std::mem::take(&mut table.cur_cell));
        }
        if !table.cur_row.is_empty() {
            let row = std::mem::take(&mut table.cur_row);
            if table.in_head {
                table.head = row;
            } else {
                table.rows.push(row);
            }
        }

        let mut num_cols = table.alignments.len().max(table.head.len());
        for row in &table.rows {
            num_cols = num_cols.max(row.len());
        }
        if num_cols == 0 {
            return;
        }

        let alignments: Vec<Alignment> = (0..num_cols)
            .map(|i| table.alignments.get(i).copied().unwrap_or(Alignment::None))
            .collect();

        let indent = self.indent();
        let indent_w = indent.width();
        let avail_w = self.width.saturating_sub(indent_w);

        let mut max_w = vec![1; num_cols];
        for (c, max) in max_w.iter_mut().enumerate() {
            let head_w = table.head.get(c).map_or(0, |cell| cell_width(cell));
            let mut col_max = head_w;
            for row in &table.rows {
                let cell_w = row.get(c).map_or(0, |cell| cell_width(cell));
                col_max = col_max.max(cell_w);
            }
            *max = col_max.max(1);
        }

        let overhead = 1 + 3 * num_cols;
        let budget = avail_w.saturating_sub(overhead);
        let col_widths = allocate_column_widths(&max_w, budget);

        let (
            top_left,
            top_mid,
            top_right,
            mid_left,
            mid_cross,
            mid_right,
            bot_left,
            bot_mid,
            bot_right,
            v_div,
            h_line,
        ) = if self.ascii {
            ("+", "+", "+", "+", "+", "+", "+", "+", "+", "|", "-")
        } else {
            ("╭", "┬", "╮", "├", "┼", "┤", "╰", "┴", "╯", "│", "─")
        };

        // Top border
        let mut top_spans = vec![(top_left.to_owned(), self.theme.dim)];
        for (c, &w) in col_widths.iter().enumerate() {
            top_spans.push((h_line.repeat(w + 2), self.theme.dim));
            if c + 1 < num_cols {
                top_spans.push((top_mid.to_owned(), self.theme.dim));
            }
        }
        top_spans.push((top_right.to_owned(), self.theme.dim));
        self.out.push(Logical {
            first: indent.clone(),
            rest: indent.clone(),
            nowrap: true,
            spans: top_spans,
        });

        let emit_row = |cells: &[Vec<(String, Style)>], out: &mut Vec<Logical>| {
            let mut col_lines: Vec<Vec<Vec<(String, Style)>>> = Vec::with_capacity(num_cols);
            for (c, &w) in col_widths.iter().enumerate() {
                let cell_spans = cells.get(c).map_or(&[][..], |v| v.as_slice());
                col_lines.push(wrap_cell_spans(cell_spans, w));
            }
            let row_height = col_lines.iter().map(Vec::len).max().unwrap_or(1).max(1);
            for line_idx in 0..row_height {
                let mut row_spans = vec![(v_div.to_owned(), self.theme.dim)];
                for (c, &w) in col_widths.iter().enumerate() {
                    row_spans.push((" ".to_owned(), Style::new()));
                    let line = col_lines[c].get(line_idx);
                    let line_w = line.map_or(0, |spans| cell_width(spans));
                    let pad = w.saturating_sub(line_w);
                    let (left_pad, right_pad) = match alignments[c] {
                        Alignment::Right => (pad, 0),
                        Alignment::Center => (pad / 2, pad - pad / 2),
                        Alignment::Left | Alignment::None => (0, pad),
                    };
                    if left_pad > 0 {
                        row_spans.push((" ".repeat(left_pad), Style::new()));
                    }
                    if let Some(spans) = line {
                        for (text, style) in spans {
                            row_spans.push((text.clone(), *style));
                        }
                    }
                    if right_pad > 0 {
                        row_spans.push((" ".repeat(right_pad), Style::new()));
                    }
                    row_spans.push((" ".to_owned(), Style::new()));
                    row_spans.push((v_div.to_owned(), self.theme.dim));
                }
                out.push(Logical {
                    first: indent.clone(),
                    rest: indent.clone(),
                    nowrap: true,
                    spans: row_spans,
                });
            }
        };

        let has_head = !table.head.is_empty();
        if has_head {
            emit_row(&table.head, &mut self.out);
        }

        if has_head && !table.rows.is_empty() {
            let mut mid_spans = vec![(mid_left.to_owned(), self.theme.dim)];
            for (c, &w) in col_widths.iter().enumerate() {
                mid_spans.push((h_line.repeat(w + 2), self.theme.dim));
                if c + 1 < num_cols {
                    mid_spans.push((mid_cross.to_owned(), self.theme.dim));
                }
            }
            mid_spans.push((mid_right.to_owned(), self.theme.dim));
            self.out.push(Logical {
                first: indent.clone(),
                rest: indent.clone(),
                nowrap: true,
                spans: mid_spans,
            });
        }

        for row in &table.rows {
            emit_row(row, &mut self.out);
        }

        // Bottom border
        let mut bot_spans = vec![(bot_left.to_owned(), self.theme.dim)];
        for (c, &w) in col_widths.iter().enumerate() {
            bot_spans.push((h_line.repeat(w + 2), self.theme.dim));
            if c + 1 < num_cols {
                bot_spans.push((bot_mid.to_owned(), self.theme.dim));
            }
        }
        bot_spans.push((bot_right.to_owned(), self.theme.dim));
        self.out.push(Logical {
            first: indent,
            rest: String::new(),
            nowrap: true,
            spans: bot_spans,
        });
    }

    /// The finished code block, one unwrapped row per line, highlighted when ready.
    fn code_block(&mut self) {
        let code = std::mem::take(&mut self.code_buf);
        let tag = std::mem::take(&mut self.code_tag);
        let plain = self.style();
        let mut lines: Vec<Vec<(String, Style)>> = vec![Vec::new()];
        let runs = self.highlights.and_then(|h| h.get(&tag, &code));
        let pieces: Vec<(&str, Style)> = match runs {
            Some(runs) => runs
                .iter()
                .filter_map(|(r, kind)| {
                    let style = kind.map_or(plain, |k| plain.patch(self.theme.syntax(k)));
                    Some((code.get(r.clone())?, style))
                })
                .collect(),
            None => vec![(code.as_str(), plain)],
        };
        for (text, style) in pieces {
            for (i, part) in text.split('\n').enumerate() {
                if i > 0 {
                    lines.push(Vec::new());
                }
                if !part.is_empty()
                    && let Some(line) = lines.last_mut()
                {
                    line.push((part.to_owned(), style));
                }
            }
        }
        if code.ends_with('\n') {
            lines.pop();
        }
        for spans in lines {
            let bar = format!("{}▏ ", self.indent());
            let line = self.line();
            line.first.clone_from(&bar);
            line.rest.clone_from(&bar);
            line.nowrap = true;
            line.spans = spans;
            self.flush();
        }
    }

    fn event(&mut self, ev: Event<'_>) {
        let t = self.theme;
        match ev {
            Event::Start(tag) => match tag {
                Tag::Paragraph => {}
                Tag::Heading { level, .. } => {
                    self.blank();
                    let mut s = t.accent.add_modifier(Modifier::BOLD);
                    if level >= HeadingLevel::H3 {
                        s = Style::new().add_modifier(Modifier::BOLD);
                    }
                    self.styles.push(s);
                }
                Tag::BlockQuote(_) => {
                    self.flush();
                    self.quote += 1;
                    self.styles.push(t.dim);
                }
                Tag::CodeBlock(kind) => {
                    self.blank();
                    if let CodeBlockKind::Fenced(lang) = kind
                        && !lang.is_empty()
                    {
                        let lang = lang.to_string();
                        self.code_tag.clone_from(&lang);
                        self.line().spans.push((lang, t.dim));
                        self.flush();
                    }
                    self.code = true;
                    self.styles.push(Style::new());
                }
                Tag::List(start) => {
                    if self.lists.is_empty() {
                        self.blank();
                    } else {
                        self.flush();
                    }
                    self.lists.push(start);
                }
                Tag::Item => {
                    self.flush();
                    let bullet = match self.lists.last_mut() {
                        Some(Some(n)) => {
                            let b = format!("{n}. ");
                            *n += 1;
                            b
                        }
                        _ => "• ".to_owned(),
                    };
                    self.pending_item = Some(bullet);
                }
                Tag::Table(alignments) => {
                    self.blank();
                    self.table = Some(TableState {
                        alignments,
                        head: Vec::new(),
                        rows: Vec::new(),
                        cur_cell: Vec::new(),
                        cur_row: Vec::new(),
                        in_head: false,
                    });
                }
                Tag::TableHead => {
                    if let Some(table) = &mut self.table {
                        table.in_head = true;
                        table.cur_row.clear();
                        self.styles.push(Style::new().add_modifier(Modifier::BOLD));
                    }
                }
                Tag::TableRow => {
                    if let Some(table) = &mut self.table {
                        table.cur_row.clear();
                    }
                }
                Tag::TableCell => {
                    if let Some(table) = &mut self.table {
                        table.cur_cell.clear();
                    }
                }
                Tag::Emphasis => self
                    .styles
                    .push(Style::new().add_modifier(Modifier::ITALIC)),
                Tag::Strong => self.styles.push(Style::new().add_modifier(Modifier::BOLD)),
                Tag::Strikethrough => {
                    self.styles
                        .push(Style::new().add_modifier(Modifier::CROSSED_OUT));
                }
                Tag::Link { .. } => self
                    .styles
                    .push(t.accent.add_modifier(Modifier::UNDERLINED)),
                _ => self.styles.push(Style::new()),
            },
            Event::End(end) => match end {
                TagEnd::Paragraph => {
                    if self.lists.is_empty() {
                        self.blank();
                    } else {
                        self.flush();
                    }
                }
                TagEnd::Heading(_) => {
                    self.styles.pop();
                    self.blank();
                }
                TagEnd::BlockQuote(_) => {
                    self.styles.pop();
                    self.flush();
                    self.quote = self.quote.saturating_sub(1);
                    self.blank();
                }
                TagEnd::CodeBlock => {
                    self.code_block();
                    self.code = false;
                    self.styles.pop();
                    self.blank();
                }
                TagEnd::List(_) => {
                    self.flush();
                    self.lists.pop();
                    if self.lists.is_empty() {
                        self.blank();
                    }
                }
                TagEnd::Item => self.flush(),
                TagEnd::TableCell => {
                    if let Some(table) = &mut self.table {
                        table.cur_row.push(std::mem::take(&mut table.cur_cell));
                    }
                }
                TagEnd::TableHead => {
                    if let Some(table) = &mut self.table {
                        table.head = std::mem::take(&mut table.cur_row);
                        table.in_head = false;
                        self.styles.pop();
                    }
                }
                TagEnd::TableRow => {
                    if let Some(table) = &mut self.table {
                        let row = std::mem::take(&mut table.cur_row);
                        if table.in_head {
                            table.head = row;
                        } else {
                            table.rows.push(row);
                        }
                    }
                }
                TagEnd::Table => {
                    self.render_table();
                    self.blank();
                }
                _ => {
                    self.styles.pop();
                }
            },
            Event::Text(s) => self.text(&s),
            Event::Code(s) => {
                let style = self.style().patch(t.warn);
                if let Some(table) = &mut self.table {
                    table.cur_cell.push((s.to_string(), style));
                } else {
                    self.line().spans.push((s.to_string(), style));
                }
            }
            Event::SoftBreak => self.text(" "),
            Event::HardBreak => {
                if self.table.is_some() {
                    self.text(" ");
                } else {
                    self.flush();
                }
            }
            Event::Rule => {
                self.blank();
                self.line().spans.push(("───".to_owned(), t.dim));
                self.blank();
            }
            Event::Html(s) | Event::InlineHtml(s) => self.text(s.trim_end_matches('\n')),
            Event::TaskListMarker(done) => {
                let mark = if done { "[x] " } else { "[ ] " }.to_owned();
                if let Some(table) = &mut self.table {
                    table.cur_cell.push((mark, t.dim));
                } else {
                    self.line().spans.push((mark, t.dim));
                }
            }
            _ => {}
        }
    }
}

/// The fenced code blocks of `text` that are closed, as `(info string, code)`. A block
/// still streaming in is left out.
pub(crate) fn code_blocks(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut open: Option<(String, String)> = None;
    for (ev, range) in Parser::new_ext(text, Options::empty()).into_offset_iter() {
        match ev {
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(tag))) => {
                open = Some((tag.to_string(), String::new()));
            }
            Event::Text(t) => {
                if let Some((_, code)) = &mut open {
                    code.push_str(&t);
                }
            }
            Event::End(TagEnd::CodeBlock) => {
                let src = text.get(range).unwrap_or_default().trim_end();
                let fence = src.lines().next().unwrap_or_default().trim_start();
                let fence = &fence[..fence.len() - fence.trim_start_matches(['`', '~']).len()];
                let closed = src.lines().count() > 1
                    && src
                        .lines()
                        .last()
                        .is_some_and(|l| l.trim().starts_with(fence) && !fence.is_empty());
                if let Some(block) = open.take().filter(|_| closed) {
                    out.push(block);
                }
            }
            _ => {}
        }
    }
    out
}

/// Render markdown into rows of at most `width` columns. `ascii` swaps box-drawing glyphs.
pub(crate) fn render(
    text: &str,
    width: usize,
    theme: &Theme,
    ascii: bool,
    highlights: Option<&Highlights>,
) -> Vec<Vec<Span<'static>>> {
    let mut b = Builder {
        width,
        ascii,
        theme,
        highlights,
        code_tag: String::new(),
        code_buf: String::new(),
        out: Vec::new(),
        cur: None,
        styles: Vec::new(),
        lists: Vec::new(),
        quote: 0,
        code: false,
        pending_item: None,
        table: None,
    };
    let opts = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    for ev in Parser::new_ext(text, opts) {
        b.event(ev);
    }
    b.flush();
    while b
        .out
        .last()
        .is_some_and(|l| l.spans.is_empty() && l.first.trim().is_empty())
    {
        b.out.pop();
    }
    let mut rows = Vec::new();
    for mut l in b.out {
        if ascii {
            let swap = |s: &str| {
                s.chars()
                    .map(|c| match c {
                        '▏' | '│' => '|',
                        '•' => '*',
                        '─' => '-',
                        '┌' | '┐' | '└' | '┘' | '╭' | '╮' | '╰' | '╯' | '┬' | '┴' | '├' | '┤'
                        | '┼' => '+',
                        other => other,
                    })
                    .collect::<String>()
            };
            l.first = swap(&l.first);
            l.rest = swap(&l.rest);
            for (text, _) in &mut l.spans {
                if text.chars().all(|c| {
                    matches!(
                        c,
                        '─' | '┌'
                            | '┐'
                            | '└'
                            | '┘'
                            | '╭'
                            | '╮'
                            | '╰'
                            | '╯'
                            | '┬'
                            | '┴'
                            | '├'
                            | '┤'
                            | '┼'
                    )
                }) {
                    *text = swap(text);
                }
            }
        }
        wrap_logical(&l, width.max(4), theme, &mut rows);
    }
    rows
}

fn trim_end_spans(spans: &mut Vec<(String, Style)>) {
    while let Some((text, _)) = spans.last_mut() {
        let trimmed = text.trim_end();
        if trimmed.is_empty() {
            spans.pop();
        } else {
            if trimmed.len() != text.len() {
                *text = trimmed.to_owned();
            }
            break;
        }
    }
}

fn wrap_cell_spans(spans: &[(String, Style)], width: usize) -> Vec<Vec<(String, Style)>> {
    let width = width.max(1);
    if spans.is_empty() {
        return vec![Vec::new()];
    }
    let mut lines: Vec<Vec<(String, Style)>> = Vec::new();
    let mut cur_line: Vec<(String, Style)> = Vec::new();
    let mut cur_w = 0;

    for (text, style) in spans {
        for word in text.split_inclusive(' ') {
            let w = word.width();
            let visible = w - usize::from(word.ends_with(' '));
            if cur_w + visible > width && !cur_line.is_empty() {
                trim_end_spans(&mut cur_line);
                lines.push(std::mem::take(&mut cur_line));
                cur_w = 0;
            }
            let word = if cur_line.is_empty() {
                word.trim_start()
            } else {
                word
            };
            if word.is_empty() {
                continue;
            }
            if cur_w + word.width() > width && cur_line.is_empty() {
                let mut chunk = String::new();
                for ch in word.chars() {
                    let cw = ch.width().unwrap_or(0);
                    if cur_w + cw > width && !chunk.is_empty() {
                        cur_line.push((std::mem::take(&mut chunk), *style));
                        trim_end_spans(&mut cur_line);
                        lines.push(std::mem::take(&mut cur_line));
                        cur_w = 0;
                    }
                    chunk.push(ch);
                    cur_w += cw;
                }
                if !chunk.is_empty() {
                    cur_w += chunk.width();
                    cur_line.push((chunk, *style));
                }
            } else {
                cur_w += word.width();
                cur_line.push((word.to_owned(), *style));
            }
        }
    }
    trim_end_spans(&mut cur_line);
    if !cur_line.is_empty() || lines.is_empty() {
        lines.push(cur_line);
    }
    lines
}

fn cell_width(spans: &[(String, Style)]) -> usize {
    spans.iter().map(|(s, _)| s.width()).sum()
}

fn allocate_column_widths(max_w: &[usize], budget: usize) -> Vec<usize> {
    let n = max_w.len();
    if n == 0 {
        return Vec::new();
    }
    if budget <= n {
        return vec![1; n];
    }
    let total_max: usize = max_w.iter().sum();
    if total_max <= budget {
        return max_w.to_vec();
    }

    let mut allocated = vec![1; n];
    let mut remaining_budget = budget;
    let mut active = vec![true; n];
    let mut remaining_cols = n;

    loop {
        if remaining_cols == 0 || remaining_budget == 0 {
            break;
        }
        let target_per_col = remaining_budget / remaining_cols;
        let mut satisfied_any = false;
        for i in 0..n {
            if active[i] && max_w[i] <= target_per_col {
                allocated[i] = max_w[i].max(1);
                remaining_budget = remaining_budget.saturating_sub(allocated[i]);
                active[i] = false;
                remaining_cols -= 1;
                satisfied_any = true;
            }
        }
        if !satisfied_any {
            let active_max_sum: usize = active
                .iter()
                .enumerate()
                .filter(|(_, a)| **a)
                .map(|(i, _)| max_w[i])
                .sum();

            let mut used = 0;
            let active_indices: Vec<usize> = (0..n).filter(|&i| active[i]).collect();
            for (idx, &i) in active_indices.iter().enumerate() {
                let w = if idx == active_indices.len() - 1 {
                    remaining_budget.saturating_sub(used)
                } else {
                    (max_w[i] * remaining_budget)
                        .checked_div(active_max_sum)
                        .unwrap_or(0)
                };
                allocated[i] = w.max(1);
                used += allocated[i];
            }
            break;
        }
    }
    allocated
}

fn wrap_logical(l: &Logical, width: usize, theme: &Theme, rows: &mut Vec<Vec<Span<'static>>>) {
    let prefix_style = theme.dim;
    if l.nowrap {
        let total_w = l.first.width() + l.spans.iter().map(|(s, _)| s.width()).sum::<usize>();
        if total_w <= width {
            let mut row = vec![Span::styled(l.first.clone(), prefix_style)];
            for (text, style) in &l.spans {
                row.push(Span::styled(text.clone(), *style));
            }
            rows.push(row);
            return;
        }
        let mut row = vec![Span::styled(l.first.clone(), prefix_style)];
        let mut used = l.first.width();
        let target_w = width.saturating_sub(1);
        for (text, style) in &l.spans {
            let mut s = String::new();
            for ch in text.replace('\t', "    ").chars() {
                let w = ch.width().unwrap_or(0);
                if used + w > target_w {
                    s.push('…');
                    used = width;
                    break;
                }
                s.push(ch);
                used += w;
            }
            if !s.is_empty() {
                row.push(Span::styled(s, *style));
            }
            if used >= width {
                break;
            }
        }
        rows.push(row);
        return;
    }
    let mut row: Vec<Span<'static>> = vec![Span::styled(l.first.clone(), prefix_style)];
    let mut used = l.first.width();
    let mut row_has_text = false;
    for (text, style) in &l.spans {
        for word in text.split_inclusive(' ') {
            let w = word.width();
            let visible = w - usize::from(word.ends_with(' '));
            if used + visible > width && row_has_text {
                rows.push(std::mem::take(&mut row));
                row.push(Span::styled(l.rest.clone(), prefix_style));
                used = l.rest.width();
                row_has_text = false;
            }
            let word = if row_has_text {
                word
            } else {
                word.trim_start()
            };
            if word.is_empty() {
                continue;
            }
            if used + word.width() > width && !row_has_text {
                // A word longer than the row: hard-break it.
                let mut chunk = String::new();
                for ch in word.chars() {
                    let cw = ch.width().unwrap_or(0);
                    if used + cw > width && !chunk.is_empty() {
                        row.push(Span::styled(std::mem::take(&mut chunk), *style));
                        rows.push(std::mem::take(&mut row));
                        row.push(Span::styled(l.rest.clone(), prefix_style));
                        used = l.rest.width();
                    }
                    chunk.push(ch);
                    used += cw;
                }
                row.push(Span::styled(chunk, *style));
            } else {
                row.push(Span::styled(word.to_owned(), *style));
                used += word.width();
            }
            row_has_text = true;
        }
    }
    rows.push(row);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(rows: &[Vec<Span<'static>>]) -> Vec<String> {
        rows.iter()
            .map(|r| {
                r.iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect()
    }

    #[test]
    fn table_rendering_unicode() {
        let theme = Theme::resolve("dark", false);
        let md = "| Name | Role | Speed |\n| :--- | :---: | ---: |\n| Alice | Dev | Fast |\n| Bob | QA | Normal |\n";
        let rows = plain(&render(md, 40, &theme, false, None));
        assert_eq!(
            rows,
            [
                "╭───────┬──────┬────────╮",
                "│ Name  │ Role │  Speed │",
                "├───────┼──────┼────────┤",
                "│ Alice │ Dev  │   Fast │",
                "│ Bob   │  QA  │ Normal │",
                "╰───────┴──────┴────────╯",
            ]
        );
    }

    #[test]
    fn table_wrapping() {
        let theme = Theme::resolve("dark", false);
        let md = "| Key | Description |\n| --- | --- |\n| tool | runs the requested action in sandbox |\n";
        let rows = plain(&render(md, 32, &theme, false, None));
        assert_eq!(
            rows,
            [
                "╭──────┬───────────────────────╮",
                "│ Key  │ Description           │",
                "├──────┼───────────────────────┤",
                "│ tool │ runs the requested    │",
                "│      │ action in sandbox     │",
                "╰──────┴───────────────────────╯",
            ]
        );
    }

    #[test]
    fn table_with_inline_code_and_styles() {
        let theme = Theme::resolve("dark", false);
        let md = "| Function | Status |\n| :--- | :---: |\n| `add(a, b)` | **done** |\n";
        let rendered = render(md, 40, &theme, false, None);
        let rows = plain(&rendered);
        assert_eq!(
            rows,
            [
                "╭───────────┬────────╮",
                "│ Function  │ Status │",
                "├───────────┼────────┤",
                "│ add(a, b) │  done  │",
                "╰───────────┴────────╯",
            ]
        );
        // Verify styling: code cell has warn style, bold cell has BOLD modifier
        let code_span = rendered
            .iter()
            .flatten()
            .find(|s| s.content.starts_with("add(a,"))
            .expect("code span");
        assert_eq!(code_span.style, theme.warn);

        let bold_span = rendered
            .iter()
            .flatten()
            .find(|s| s.content == "done")
            .expect("bold span");
        assert!(bold_span.style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn table_in_blockquote() {
        let theme = Theme::resolve("dark", false);
        let md = "> | A | B |\n> | - | - |\n> | 1 | 2 |\n";
        let rows = plain(&render(md, 30, &theme, false, None));
        assert_eq!(
            rows,
            [
                "│ ╭───┬───╮",
                "│ │ A │ B │",
                "│ ├───┼───┤",
                "│ │ 1 │ 2 │",
                "│ ╰───┴───╯",
            ]
        );
    }

    #[test]
    fn table_streaming_partial() {
        let theme = Theme::resolve("dark", false);
        // Incomplete table ending abruptly during streaming
        let md = "| A | B |\n| - | - |\n| 1 |";
        let rows = plain(&render(md, 30, &theme, false, None));
        assert_eq!(
            rows,
            [
                "╭───┬───╮",
                "│ A │ B │",
                "├───┼───┤",
                "│ 1 │   │",
                "╰───┴───╯",
            ]
        );
    }

    #[test]
    fn table_header_only() {
        let theme = Theme::resolve("dark", false);
        let md = "| Col 1 | Col 2 |\n| --- | --- |\n";
        let rows = plain(&render(md, 30, &theme, false, None));
        assert_eq!(
            rows,
            [
                "╭───────┬───────╮",
                "│ Col 1 │ Col 2 │",
                "╰───────┴───────╯",
            ]
        );
    }

    #[test]
    fn table_rendering_ascii() {
        let theme = Theme::resolve("dark", false);
        let md = "| Name | Role |\n| :--- | ---: |\n| Alice | Dev |\n";
        let rows = plain(&render(md, 40, &theme, true, None));
        assert_eq!(
            rows,
            [
                "+-------+------+",
                "| Name  | Role |",
                "+-------+------+",
                "| Alice |  Dev |",
                "+-------+------+",
            ]
        );
    }

    #[test]
    fn lists_code_and_wrapping() {
        let theme = Theme::resolve("dark", false);
        let md = "# Plan\n\nFix the **bug** in `add`:\n\n- read the file\n- change `-` to `+` and run the tests again afterwards\n\n```rust\nfn add(a: i32, b: i32) -> i32 { a + b }\n```\n";
        let rows = plain(&render(md, 30, &theme, false, None));
        assert_eq!(
            rows,
            [
                "Plan",
                "",
                "Fix the bug in add:",
                "",
                "• read the file",
                "• change - to + and run the",
                "  tests again afterwards",
                "",
                "rust",
                "▏ fn add(a: i32, b: i32) -> i…",
            ]
        );
    }

    #[test]
    fn ordered_and_nested_lists() {
        let theme = Theme::resolve("dark", false);
        let rows = plain(&render(
            "1. one\n2. two\n   - inner\n",
            40,
            &theme,
            true,
            None,
        ));
        assert_eq!(rows, ["1. one", "2. two", "  * inner"]);
    }

    #[test]
    fn only_closed_fences_are_code_blocks() {
        let done = "text\n\n```rust\nfn a() {}\n```\n\n~~~py\nx = 1\n~~~\n";
        assert_eq!(
            code_blocks(done),
            [
                ("rust".to_owned(), "fn a() {}\n".to_owned()),
                ("py".to_owned(), "x = 1\n".to_owned()),
            ]
        );
        assert_eq!(code_blocks("```rust\nfn a() {}\n"), []);
        assert_eq!(code_blocks("```rust\nfn a() {}\n``"), []);
    }

    #[test]
    fn code_is_highlighted_once_ready() {
        use std::sync::Arc;

        use quack_core::grammar::Grammars;
        if !Grammars::compiled_in() {
            return;
        }
        let theme = Theme::resolve("dark", false);
        let md = "```rust\nlet s = \"hi\"; // note\n```\n";
        let mut h = Highlights::new(Arc::new(Grammars::default()));
        let style_of = |rows: &[Vec<Span<'static>>], text: &str| {
            rows.iter()
                .flatten()
                .find(|s| s.content.contains(text))
                .map(|s| s.style)
        };
        let before = render(md, 40, &theme, false, Some(&h));
        assert_eq!(style_of(&before, "let"), Some(Style::new()));
        assert!(h.scan(md));
        let after = render(md, 40, &theme, false, Some(&h));
        assert_eq!(plain(&after), plain(&before));
        assert_eq!(style_of(&after, "let"), Some(theme.syntax("keyword")));
        assert_eq!(style_of(&after, "\"hi\""), Some(theme.syntax("string")));
        assert_eq!(style_of(&after, "// note"), Some(theme.syntax("comment")));
    }
}
