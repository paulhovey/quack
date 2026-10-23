//! Multi-line input buffer with a char-indexed cursor and in-memory history.
//! Persisted per-project history, `@` file completion and paste chips land in M1.

use unicode_width::UnicodeWidthChar;

#[derive(Debug, Default)]
pub(crate) struct Input {
    text: String,
    /// Cursor position in chars.
    cursor: usize,
    history: Vec<String>,
    history_pos: Option<usize>,
}

impl Input {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    fn byte_at(&self, char_idx: usize) -> usize {
        self.text
            .char_indices()
            .nth(char_idx)
            .map_or(self.text.len(), |(b, _)| b)
    }

    fn len(&self) -> usize {
        self.text.chars().count()
    }

    pub fn insert_str(&mut self, s: &str) {
        let at = self.byte_at(self.cursor);
        // Normalise pasted CRLF.
        let s = s.replace("\r\n", "\n").replace('\r', "\n");
        self.text.insert_str(at, &s);
        self.cursor += s.chars().count();
        self.history_pos = None;
    }

    pub fn insert(&mut self, c: char) {
        let at = self.byte_at(self.cursor);
        self.text.insert(at, c);
        self.cursor += 1;
        self.history_pos = None;
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            let at = self.byte_at(self.cursor);
            self.text.remove(at);
        }
    }

    pub fn delete(&mut self) {
        if self.cursor < self.len() {
            let at = self.byte_at(self.cursor);
            self.text.remove(at);
        }
    }

    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.len());
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.len();
    }

    pub fn set(&mut self, text: &str) {
        self.text = text.to_owned();
        self.cursor = self.len();
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.history_pos = None;
    }

    /// Take the text for submission and record it in history.
    pub fn submit(&mut self) -> String {
        let text = std::mem::take(&mut self.text);
        self.cursor = 0;
        self.history_pos = None;
        if !text.trim().is_empty() && self.history.last() != Some(&text) {
            self.history.push(text.clone());
        }
        text
    }

    pub fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let pos = self
            .history_pos
            .map_or(self.history.len() - 1, |p| p.saturating_sub(1));
        self.history_pos = Some(pos);
        self.text = self.history[pos].clone();
        self.cursor = self.len();
    }

    pub fn history_next(&mut self) {
        let Some(pos) = self.history_pos else {
            return;
        };
        if pos + 1 < self.history.len() {
            self.history_pos = Some(pos + 1);
            self.text = self.history[pos + 1].clone();
        } else {
            self.history_pos = None;
            self.text.clear();
        }
        self.cursor = self.len();
    }

    /// The text soft-wrapped to `width` columns, and the cursor's row and column there.
    pub fn layout(&self, width: usize) -> Layout {
        let width = width.max(1);
        let mut rows = Vec::new();
        let mut spans = Vec::new();
        let mut cursor = None;
        let mut start = 0;
        for line in self.text.split('\n') {
            let chars: Vec<char> = line.chars().collect();
            let end = start + chars.len();
            let wrapped = wrap_line(&chars, width);
            let last = wrapped.len() - 1;
            for (n, (a, b)) in wrapped.into_iter().enumerate() {
                // A cursor on a row boundary sits at the start of the next row.
                let here = self.cursor >= start + a
                    && (self.cursor < start + b || (b == chars.len() && self.cursor == end));
                if cursor.is_none() && here {
                    let col = chars[a..self.cursor - start]
                        .iter()
                        .map(|c| c.width().unwrap_or(0))
                        .sum();
                    cursor = Some((rows.len(), col));
                }
                rows.push(chars[a..b].iter().collect());
                // A wrapped row's end is the next row's start, so the cursor stops short of it.
                spans.push((start + a, start + b - usize::from(n < last)));
            }
            start = end + 1;
        }
        let (mut row, mut col) = cursor.unwrap_or((rows.len().saturating_sub(1), 0));
        // A full last row leaves no room for the cursor: it moves to a row of its own.
        if col >= width {
            row += 1;
            col = 0;
            if row == rows.len() {
                rows.push(String::new());
                spans.push((self.cursor, self.cursor));
            }
        }
        Layout {
            rows,
            spans,
            cursor: (row, col),
        }
    }

    /// Move the cursor to display column `col` of `row` in `layout`, or as near as it gets.
    pub fn place(&mut self, layout: &Layout, row: usize, col: usize) {
        if let Some(at) = self.index(layout, row, col) {
            self.cursor = at;
        }
    }

    /// The char at display column `col` of `row` in `layout`, or the nearest place on the row.
    fn index(&self, layout: &Layout, row: usize, col: usize) -> Option<usize> {
        let &(start, max) = layout
            .spans
            .get(row.min(layout.spans.len().checked_sub(1)?))?;
        let mut at = start;
        let mut w = 0;
        for c in self.text.chars().skip(start).take(max - start) {
            w += c.width().unwrap_or(0);
            if w > col {
                break;
            }
            at += 1;
        }
        Some(at)
    }

    /// The text from cell `from` through cell `to` (both `(row, column)` in `layout`), with
    /// its real line breaks rather than the wrapping.
    pub fn slice(&self, layout: &Layout, from: (usize, usize), to: (usize, usize)) -> String {
        let (Some(a), Some(b)) = (
            self.index(layout, from.0, from.1),
            self.index(layout, to.0, to.1 + 1),
        ) else {
            return String::new();
        };
        self.text
            .chars()
            .skip(a)
            .take(b.saturating_sub(a))
            .collect()
    }

    /// Move the cursor up a row at `width` columns. False when it is already on the top row.
    pub fn up(&mut self, width: usize) -> bool {
        let layout = self.layout(width);
        let (row, col) = layout.cursor;
        if row == 0 {
            return false;
        }
        self.place(&layout, row - 1, col);
        true
    }

    /// Move the cursor down a row at `width` columns. False when it is already on the bottom row.
    pub fn down(&mut self, width: usize) -> bool {
        let layout = self.layout(width);
        let (row, col) = layout.cursor;
        if row + 1 >= layout.rows.len() {
            return false;
        }
        self.place(&layout, row + 1, col);
        true
    }
}

/// The text of the input as it is drawn.
#[derive(Debug)]
pub(crate) struct Layout {
    pub rows: Vec<String>,
    /// Each row's first char and the furthest the cursor can go on it.
    spans: Vec<(usize, usize)>,
    /// Row and display column of the cursor.
    pub cursor: (usize, usize),
}

/// Char ranges of one line's rows at `width` columns, breaking after spaces where possible.
/// Ranges are contiguous, so a cursor position always falls in one of them.
fn wrap_line(chars: &[char], width: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut row_w = 0;
    // Where the row could break: just after its last space.
    let mut brk = None;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let cw = c.width().unwrap_or(0);
        if row_w + cw > width && i > start {
            if c == ' ' {
                // A space past the edge hangs off the row instead of starting the next one.
                out.push((start, i + 1));
                start = i + 1;
                row_w = 0;
                brk = None;
                i += 1;
                continue;
            }
            let at = brk.unwrap_or(i);
            out.push((start, at));
            start = at;
            row_w = chars[at..i].iter().map(|c| c.width().unwrap_or(0)).sum();
            brk = None;
        }
        row_w += cw;
        if c == ' ' {
            brk = Some(i + 1);
        }
        i += 1;
    }
    out.push((start, chars.len()));
    out
}

#[cfg(test)]
mod tests {
    use super::Input;

    #[test]
    fn editing_and_history() {
        let mut i = Input::default();
        i.insert_str("héllo");
        i.left();
        i.backspace();
        assert_eq!(i.text(), "hélo");
        i.insert('\n');
        assert_eq!(i.layout(10).cursor, (1, 0));
        assert_eq!(i.submit(), "hél\no");
        i.insert_str("second");
        i.submit();
        i.history_prev();
        assert_eq!(i.text(), "second");
        i.history_prev();
        assert_eq!(i.text(), "hél\no");
        i.history_next();
        i.history_next();
        assert!(i.is_empty());
    }
    #[test]
    fn layout_wraps_at_spaces_and_follows_the_cursor() {
        let mut i = Input::default();
        i.insert_str("hello brave new world");
        let l = i.layout(10);
        assert_eq!(l.rows, ["hello ", "brave new ", "world"]);
        assert_eq!(l.cursor, (2, 5));
        i.home();
        assert_eq!(i.layout(10).cursor, (0, 0));
        // The cursor just after a wrapped space starts the next row.
        for _ in 0..6 {
            i.right();
        }
        assert_eq!(i.layout(10).cursor, (1, 0));

        // A word longer than the row is hard-broken, and a full row pushes the cursor down.
        i.set("abcdef");
        let l = i.layout(3);
        assert_eq!(l.rows, ["abc", "def", ""]);
        assert_eq!(l.cursor, (2, 0));

        i.set("ab\ncd");
        assert_eq!(i.layout(5).rows, ["ab", "cd"]);
        assert_eq!(i.layout(5).cursor, (1, 2));
    }

    #[test]
    fn up_down_and_place_move_between_rows() {
        let mut i = Input::default();
        // Rows at 10 columns: "hello ", "brave new ", "world".
        i.insert_str("hello brave new world");
        assert!(i.up(10));
        assert_eq!(i.layout(10).cursor, (1, 5));
        assert!(i.up(10));
        assert_eq!(i.layout(10).cursor, (0, 5));
        assert!(!i.up(10));
        assert!(i.down(10));
        assert!(i.down(10));
        assert!(!i.down(10));
        assert_eq!(i.layout(10).cursor, (2, 5));

        // Past the end of a wrapped row stops before the next row's first char.
        let layout = i.layout(10);
        i.place(&layout, 0, 40);
        assert_eq!(i.layout(10).cursor, (0, 5));
        i.place(&layout, 2, 2);
        assert_eq!(i.layout(10).cursor, (2, 2));
        i.place(&layout, 9, 0);
        assert_eq!(i.layout(10).cursor, (2, 0));

        // Wide chars: a click on the second half of one lands before it.
        i.set("a日b");
        let layout = i.layout(10);
        i.place(&layout, 0, 2);
        assert_eq!(i.layout(10).cursor, (0, 1));
    }

    #[test]
    fn slice_follows_cells_and_keeps_real_line_breaks() {
        let mut i = Input::default();
        // Rows at 10 columns: "hello ", "brave new ", "world", "two".
        i.insert_str("hello brave new world\ntwo");
        let l = i.layout(10);
        assert_eq!(i.slice(&l, (0, 0), (0, 4)), "hello");
        assert_eq!(i.slice(&l, (1, 0), (1, 4)), "brave");
        assert_eq!(i.slice(&l, (0, 4), (1, 4)), "o brave");
        // Past a row's end stops at it; wrapping adds no break, a newline stays one.
        assert_eq!(i.slice(&l, (1, 6), (3, 40)), "new world\ntwo");
        i.set("a日b");
        assert_eq!(i.slice(&i.layout(10), (0, 2), (0, 2)), "日");
    }
}
