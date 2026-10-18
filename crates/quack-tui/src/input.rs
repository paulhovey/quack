//! Multi-line input buffer with a char-indexed cursor and in-memory history.
//! Persisted per-project history, `@` file completion and paste chips land in M1.

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

    /// Line index and column (in chars) of the cursor.
    pub fn cursor_line_col(&self) -> (usize, usize) {
        let before: String = self.text.chars().take(self.cursor).collect();
        let line = before.matches('\n').count();
        let col = before.rsplit('\n').next().map_or(0, |l| l.chars().count());
        (line, col)
    }

    pub fn line_count(&self) -> usize {
        self.text.matches('\n').count() + 1
    }
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
        assert_eq!(i.cursor_line_col(), (1, 0));
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
}
