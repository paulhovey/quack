//! Mouse text selection in the transcript and the input box. A drag selects, and letting
//! go copies the text through the terminal (OSC 52).

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use unicode_width::UnicodeWidthChar;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pane {
    Transcript,
    Input,
}

/// A drag between two cells, both included, as `(row, display column)` in the pane's
/// content: transcript rows count from its first row, so the selection stays on its text
/// while the view scrolls; input rows are the layout's, and columns leave out the prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Selection {
    pub pane: Pane,
    pub anchor: (usize, usize),
    pub head: (usize, usize),
    /// The mouse has moved since the button went down; a plain click selects nothing.
    pub dragged: bool,
}

impl Selection {
    pub fn new(pane: Pane, at: (usize, usize)) -> Self {
        Self {
            pane,
            anchor: at,
            head: at,
            dragged: false,
        }
    }

    /// First and last cell in reading order.
    pub fn bounds(&self) -> ((usize, usize), (usize, usize)) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }

    /// The selected columns of `row`, as `[from, to)`.
    pub fn columns(&self, row: usize) -> Option<(usize, usize)> {
        if !self.dragged {
            return None;
        }
        let ((r0, c0), (r1, c1)) = self.bounds();
        if row < r0 || row > r1 {
            return None;
        }
        let from = if row == r0 { c0 } else { 0 };
        let to = if row == r1 { c1 + 1 } else { usize::MAX };
        Some((from, to))
    }
}

/// How selected cells look: reversed, which reads in every theme and without colour.
pub(crate) fn style() -> Style {
    Style::new().add_modifier(Modifier::REVERSED)
}

/// Mark the selected cells of `area`, whose top row is content row `first`. `width` gives
/// each row's text width, so a row is marked only as far as its text goes.
pub(crate) fn paint(
    buf: &mut Buffer,
    area: Rect,
    first: usize,
    sel: &Selection,
    width: impl Fn(usize) -> usize,
) {
    for y in 0..area.height {
        let row = first + usize::from(y);
        let Some((from, to)) = sel.columns(row) else {
            continue;
        };
        let to = to.min(width(row)).min(usize::from(area.width));
        if from >= to {
            continue;
        }
        let x = area.x + u16::try_from(from).unwrap_or(u16::MAX);
        let w = u16::try_from(to - from).unwrap_or(u16::MAX);
        buf.set_style(Rect::new(x, area.y + y, w, 1), style());
    }
}

/// The chars of `line` that touch display columns `[from, to)`.
pub(crate) fn cut(line: &str, from: usize, to: usize) -> String {
    let mut col = 0;
    let mut out = String::new();
    for c in line.chars() {
        let w = c.width().unwrap_or(0);
        if col + w.max(1) > from && col < to {
            out.push(c);
        }
        col += w;
        if col >= to {
            break;
        }
    }
    out
}

/// Selected transcript rows, each with the width of its lead (`gutter`), as text. Leads
/// and trailing blanks go, and so does the indent every row shares, keeping code's own.
pub(crate) fn transcript_text(rows: &[(String, usize)], sel: &Selection) -> String {
    let ((r0, _), (r1, _)) = sel.bounds();
    let lead = |s: &str| s.len() - s.trim_start_matches(' ').len();
    // `(row past its lead, first selected column there, its selected text)`.
    let picked: Vec<(&str, usize, &str)> = (r0..=r1.min(rows.len().saturating_sub(1)))
        .filter_map(|r| {
            let (from, to) = sel.columns(r)?;
            let (row, gutter) = (&rows[r].0, rows[r].1);
            let row = &row[row.len() - cut(row, gutter, usize::MAX).len()..];
            let (from, to) = (from.saturating_sub(gutter), to.saturating_sub(gutter));
            let start = row.len() - cut(row, from, usize::MAX).len();
            let end = start + cut(row, from, to).len();
            Some((row, from, row[start..end].trim_end()))
        })
        .collect();
    // Measured on whole rows, so a selection that starts mid-line still finds the gutter.
    let indent = picked
        .iter()
        .filter(|(row, ..)| !row.trim().is_empty())
        .map(|(row, ..)| lead(row))
        .min()
        .unwrap_or(0);
    picked
        .iter()
        .map(|(_, from, text)| &text[lead(text).min(indent.saturating_sub(*from))..])
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drag(anchor: (usize, usize), head: (usize, usize)) -> Selection {
        Selection {
            dragged: true,
            head,
            ..Selection::new(Pane::Transcript, anchor)
        }
    }

    #[test]
    fn cut_keeps_wide_chars_that_touch_the_range() {
        assert_eq!(cut("hello world", 6, 11), "world");
        assert_eq!(cut("a日b", 2, 3), "日");
        assert_eq!(cut("a日b", 0, 2), "a日");
        assert_eq!(cut("ab", 5, 9), "");
    }

    #[test]
    fn transcript_text_drops_leads_and_the_shared_indent() {
        let rows: Vec<(String, usize)> = ["   fn main() {", "       run();", "   }", "", " › next"]
            .map(|r| (r.to_owned(), 0))
            .into();
        // Backwards drag from the closing brace to the start.
        assert_eq!(
            transcript_text(&rows, &drag((2, 3), (0, 0))),
            "fn main() {\n    run();\n}"
        );
        // Starting mid-line still finds the gutter on the whole row.
        assert_eq!(
            transcript_text(&rows, &drag((0, 6), (1, 20))),
            "main() {\n    run();"
        );
        // Only the indent every row shares goes.
        assert_eq!(
            transcript_text(&rows, &drag((2, 0), (4, 9))),
            "  }\n\n› next"
        );
        // A lead is never copied, even when the drag starts on it.
        let rows: Vec<(String, usize)> = vec![
            (" _(•)> Run this:".into(), 7),
            ("         cargo test".into(), 7),
        ];
        assert_eq!(
            transcript_text(&rows, &drag((0, 0), (1, 40))),
            "Run this:\n  cargo test"
        );
        // A plain click selects nothing.
        assert_eq!(
            transcript_text(&rows, &Selection::new(Pane::Transcript, (0, 0))),
            ""
        );
    }
}
