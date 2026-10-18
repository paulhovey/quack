//! Incremental server-sent-events decoder. Yields the `data` payload of each event.

#[derive(Debug, Default)]
pub struct SseDecoder {
    buf: Vec<u8>,
    data: Vec<String>,
}

impl SseDecoder {
    /// Feed raw bytes; returns the payloads of every event completed by them.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        while let Some(nl) = self.buf.iter().position(|&b| b == b'\n') {
            let mut line: Vec<u8> = self.buf.drain(..=nl).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            self.line(&String::from_utf8_lossy(&line), &mut out);
        }
        out
    }

    /// Flush an event left unterminated at end of stream.
    pub fn finish(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        if !self.buf.is_empty() {
            let rest = String::from_utf8_lossy(&std::mem::take(&mut self.buf)).into_owned();
            self.line(rest.trim_end_matches('\r'), &mut out);
        }
        self.line("", &mut out);
        out
    }

    fn line(&mut self, line: &str, out: &mut Vec<String>) {
        if line.is_empty() {
            if !self.data.is_empty() {
                out.push(std::mem::take(&mut self.data).join("\n"));
            }
            return;
        }
        if let Some(rest) = line.strip_prefix("data:") {
            self.data
                .push(rest.strip_prefix(' ').unwrap_or(rest).to_owned());
        }
        // `event:`, `id:`, `retry:` and `:` comments are not used by the chat protocols.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_across_chunks_and_line_endings() {
        let mut d = SseDecoder::default();
        assert!(d.feed(b": keep-alive\n\nda").is_empty());
        assert_eq!(d.feed(b"ta: {\"a\":1}\r\n\r\ndata: x\n"), vec!["{\"a\":1}"]);
        assert_eq!(d.feed(b"data: y\n\n"), vec!["x\ny"]);
        assert!(d.feed(b"data: [DONE]").is_empty());
        assert_eq!(d.finish(), vec!["[DONE]"]);
    }

    #[test]
    fn multibyte_split_is_preserved() {
        let mut d = SseDecoder::default();
        let s = "data: héllo\n\n".as_bytes();
        let (a, b) = s.split_at(8);
        assert!(d.feed(a).is_empty());
        assert_eq!(d.feed(b), vec!["héllo"]);
    }
}
