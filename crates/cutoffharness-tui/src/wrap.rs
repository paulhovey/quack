use unicode_width::UnicodeWidthChar;

/// Wrap `text` to `width` columns, breaking at spaces where possible.
pub(crate) fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    for line in text.split('\n') {
        let mut row = String::new();
        let mut row_w = 0;
        for word in line.split_inclusive(' ') {
            let w: usize = word.chars().map(|c| c.width().unwrap_or(0)).sum();
            // A trailing space may hang past the edge; it is trimmed when the row ends.
            let visible = w - usize::from(word.ends_with(' '));
            if row_w + visible > width && !row.is_empty() {
                out.push(std::mem::take(&mut row).trim_end().to_owned());
                row_w = 0;
            }
            if w > width {
                // Hard-break a word longer than the line.
                for ch in word.chars() {
                    let cw = ch.width().unwrap_or(0);
                    if row_w + cw > width {
                        out.push(std::mem::take(&mut row));
                        row_w = 0;
                    }
                    row.push(ch);
                    row_w += cw;
                }
            } else {
                row.push_str(word);
                row_w += w;
            }
        }
        out.push(row.trim_end().to_owned());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::wrap;

    #[test]
    fn wraps_words_and_long_tokens() {
        assert_eq!(
            wrap("hello brave new world", 11),
            ["hello brave", "new world"]
        );
        assert_eq!(wrap("abcdefgh", 3), ["abc", "def", "gh"]);
        assert_eq!(wrap("a\n\nb", 5), ["a", "", "b"]);
    }
}
