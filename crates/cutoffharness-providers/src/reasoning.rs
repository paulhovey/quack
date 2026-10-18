//! Streaming `<think>…</think>` splitter for servers that leave reasoning inline in `content`.

const OPEN: &str = "<think>";
const CLOSE: &str = "</think>";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Only whitespace so far; the reply may still open with `<think>`.
    Start,
    Thinking,
    /// Just closed; drop the newlines that separate reasoning from the answer.
    AfterThink,
    Content,
}

/// A piece of split output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Piece {
    Text(String),
    Reasoning(String),
}

#[derive(Debug)]
pub struct ThinkSplitter {
    state: State,
    buf: String,
}

impl Default for ThinkSplitter {
    fn default() -> Self {
        Self {
            state: State::Start,
            buf: String::new(),
        }
    }
}

impl ThinkSplitter {
    pub fn feed(&mut self, text: &str) -> Vec<Piece> {
        self.buf.push_str(text);
        let mut out = Vec::new();
        loop {
            match self.state {
                State::Start => {
                    let trimmed = self.buf.trim_start();
                    if let Some(rest) = trimmed.strip_prefix(OPEN) {
                        self.buf = rest.to_owned();
                        self.state = State::Thinking;
                    } else if OPEN.starts_with(trimmed) {
                        return out; // need more input (also covers all-whitespace)
                    } else {
                        self.state = State::Content;
                    }
                }
                State::Thinking => {
                    if let Some(at) = self.buf.find(CLOSE) {
                        let reasoning: String = self.buf.drain(..at).collect();
                        self.buf.drain(..CLOSE.len());
                        push(&mut out, Piece::Reasoning(reasoning));
                        self.state = State::AfterThink;
                    } else {
                        // Hold back a suffix that could be the start of `</think>`.
                        let keep = partial_suffix(&self.buf, CLOSE);
                        let emit: String = self.buf.drain(..self.buf.len() - keep).collect();
                        push(&mut out, Piece::Reasoning(emit));
                        return out;
                    }
                }
                State::AfterThink => {
                    let trimmed = self.buf.trim_start_matches(['\n', '\r']);
                    if trimmed.is_empty() {
                        self.buf.clear();
                        return out;
                    }
                    self.buf = trimmed.to_owned();
                    self.state = State::Content;
                }
                State::Content => {
                    push(&mut out, Piece::Text(std::mem::take(&mut self.buf)));
                    return out;
                }
            }
        }
    }

    /// End of stream: release anything held back.
    pub fn finish(&mut self) -> Vec<Piece> {
        let rest = std::mem::take(&mut self.buf);
        let mut out = Vec::new();
        match self.state {
            State::Thinking => push(&mut out, Piece::Reasoning(rest)),
            State::AfterThink => {}
            State::Start | State::Content => push(&mut out, Piece::Text(rest)),
        }
        out
    }
}

fn push(out: &mut Vec<Piece>, piece: Piece) {
    let empty = match &piece {
        Piece::Text(t) | Piece::Reasoning(t) => t.is_empty(),
    };
    if !empty {
        out.push(piece);
    }
}

/// Length of the longest suffix of `s` that is a proper prefix of `tag`.
fn partial_suffix(s: &str, tag: &str) -> usize {
    (1..tag.len())
        .rev()
        .find(|&n| s.is_char_boundary(s.len().saturating_sub(n)) && s.ends_with(&tag[..n]))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(chunks: &[&str]) -> (String, String) {
        let mut s = ThinkSplitter::default();
        let mut text = String::new();
        let mut reasoning = String::new();
        let mut pieces: Vec<Piece> = chunks.iter().flat_map(|c| s.feed(c)).collect();
        pieces.extend(s.finish());
        for p in pieces {
            match p {
                Piece::Text(t) => text.push_str(&t),
                Piece::Reasoning(r) => reasoning.push_str(&r),
            }
        }
        (text, reasoning)
    }

    #[test]
    fn splits_tags_across_chunks() {
        assert_eq!(
            run(&["\n<thi", "nk>plan it</th", "ink>\n\nAnswer"]),
            ("Answer".into(), "plan it".into())
        );
    }

    #[test]
    fn plain_content_passes_through() {
        assert_eq!(
            run(&["Hello ", "<think> is a tag"]),
            ("Hello <think> is a tag".into(), String::new())
        );
        assert_eq!(run(&["<"]), ("<".into(), String::new()));
    }

    #[test]
    fn unclosed_think_is_reasoning() {
        assert_eq!(
            run(&["<think>still going"]),
            (String::new(), "still going".into())
        );
    }
}
