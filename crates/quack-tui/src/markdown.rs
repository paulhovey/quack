//! Markdown to styled, wrapped rows for assistant messages.
//!
//! Headings, emphasis, inline code, fenced code (not wrapped, truncated instead), lists,
//! block quotes and rules. Syntax highlighting comes with tree-sitter in M2.

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

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

struct Builder<'t> {
    theme: &'t Theme,
    out: Vec<Logical>,
    cur: Option<Logical>,
    styles: Vec<Style>,
    lists: Vec<Option<u64>>,
    quote: usize,
    code: bool,
    pending_item: Option<String>,
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
        let style = self.style();
        if self.code {
            for (i, l) in t.split('\n').enumerate() {
                if i > 0 {
                    self.flush();
                }
                if l.is_empty() && i > 0 && t.ends_with('\n') && i == t.split('\n').count() - 1 {
                    continue;
                }
                let bar = format!("{}▏ ", self.indent());
                let line = self.line();
                line.first.clone_from(&bar);
                line.rest.clone_from(&bar);
                line.nowrap = true;
                line.spans.push((l.to_owned(), style));
            }
            self.flush();
            return;
        }
        self.line().spans.push((t.to_owned(), style));
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
                _ => {
                    self.styles.pop();
                }
            },
            Event::Text(s) => self.text(&s),
            Event::Code(s) => {
                let style = self.style().patch(t.warn);
                self.line().spans.push((s.to_string(), style));
            }
            Event::SoftBreak => self.text(" "),
            Event::HardBreak => self.flush(),
            Event::Rule => {
                self.blank();
                self.line().spans.push(("───".to_owned(), t.dim));
                self.blank();
            }
            Event::Html(s) | Event::InlineHtml(s) => self.text(s.trim_end_matches('\n')),
            Event::TaskListMarker(done) => {
                self.line()
                    .spans
                    .push((if done { "[x] " } else { "[ ] " }.to_owned(), t.dim));
            }
            _ => {}
        }
    }
}

/// Render markdown into rows of at most `width` columns. `ascii` swaps box-drawing glyphs.
pub(crate) fn render(
    text: &str,
    width: usize,
    theme: &Theme,
    ascii: bool,
) -> Vec<Vec<Span<'static>>> {
    let mut b = Builder {
        theme,
        out: Vec::new(),
        cur: None,
        styles: Vec::new(),
        lists: Vec::new(),
        quote: 0,
        code: false,
        pending_item: None,
    };
    let opts = Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
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
                        other => other,
                    })
                    .collect::<String>()
            };
            l.first = swap(&l.first);
            l.rest = swap(&l.rest);
            for (text, _) in &mut l.spans {
                if text.chars().all(|c| c == '─') {
                    *text = swap(text);
                }
            }
        }
        wrap_logical(&l, width.max(4), theme, &mut rows);
    }
    rows
}

fn wrap_logical(l: &Logical, width: usize, theme: &Theme, rows: &mut Vec<Vec<Span<'static>>>) {
    let prefix_style = theme.dim;
    if l.nowrap {
        let mut row = vec![Span::styled(l.first.clone(), prefix_style)];
        let mut used = l.first.width();
        for (text, style) in &l.spans {
            let mut s = String::new();
            for ch in text.replace('\t', "    ").chars() {
                let w = ch.width().unwrap_or(0);
                if used + w + 1 > width {
                    s.push('…');
                    used = width;
                    break;
                }
                s.push(ch);
                used += w;
            }
            row.push(Span::styled(s, *style));
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
    fn lists_code_and_wrapping() {
        let theme = Theme::resolve("dark", false);
        let md = "# Plan\n\nFix the **bug** in `add`:\n\n- read the file\n- change `-` to `+` and run the tests again afterwards\n\n```rust\nfn add(a: i32, b: i32) -> i32 { a + b }\n```\n";
        let rows = plain(&render(md, 30, &theme, false));
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
        let rows = plain(&render("1. one\n2. two\n   - inner\n", 40, &theme, true));
        assert_eq!(rows, ["1. one", "2. two", "  * inner"]);
    }
}
