//! Text file handling shared by the file tools: encoding checks, BOM and line endings, diffs.

use std::path::Path;

/// Files above this size are not loaded whole by the edit tools.
pub(crate) const MAX_EDIT_BYTES: u64 = 10 * 1024 * 1024;

/// A text file split into its content (with `\n` newlines) and the format to restore on write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TextFile {
    pub text: String,
    pub bom: bool,
    pub crlf: bool,
}

impl TextFile {
    pub fn load(path: &Path) -> Result<Self, String> {
        let meta = std::fs::metadata(path).map_err(|e| io_msg(path, &e))?;
        if meta.is_dir() {
            return Err(format!("{} is a directory", path.display()));
        }
        if meta.len() > MAX_EDIT_BYTES {
            return Err(format!(
                "{} is {} bytes; too large to load whole",
                path.display(),
                meta.len()
            ));
        }
        let bytes = std::fs::read(path).map_err(|e| io_msg(path, &e))?;
        Self::from_bytes(&bytes)
            .ok_or_else(|| format!("{} is not a UTF-8 text file", path.display()))
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if looks_binary(bytes) {
            return None;
        }
        let s = std::str::from_utf8(bytes).ok()?;
        let (bom, s) = match s.strip_prefix('\u{feff}') {
            Some(rest) => (true, rest),
            None => (false, s),
        };
        let crlf = s.contains("\r\n");
        Some(Self {
            text: if crlf {
                s.replace("\r\n", "\n")
            } else {
                s.to_owned()
            },
            bom,
            crlf,
        })
    }

    /// Bytes to write for `text`, restoring the original BOM and newline style.
    pub fn encode(&self, text: &str) -> String {
        let mut out = String::with_capacity(text.len() + 8);
        if self.bom {
            out.push('\u{feff}');
        }
        if self.crlf {
            out.push_str(&text.replace("\r\n", "\n").replace('\n', "\r\n"));
        } else {
            out.push_str(text);
        }
        out
    }
}

pub(crate) fn looks_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8192).any(|&b| b == 0)
}

pub(crate) fn io_msg(path: &Path, e: &std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::NotFound => format!("{} does not exist", path.display()),
        std::io::ErrorKind::PermissionDenied => format!("permission denied: {}", path.display()),
        _ => format!("{}: {e}", path.display()),
    }
}

/// Unified diff with three lines of context.
pub(crate) fn unified_diff(name: &str, before: &str, after: &str) -> String {
    similar::TextDiff::from_lines(before, after)
        .unified_diff()
        .context_radius(3)
        .header(&format!("a/{name}"), &format!("b/{name}"))
        .to_string()
}

/// `(+added, -removed)` line counts.
pub(crate) fn diff_stats(before: &str, after: &str) -> (usize, usize) {
    let diff = similar::TextDiff::from_lines(before, after);
    let mut add = 0;
    let mut del = 0;
    for change in diff.iter_all_changes() {
        match change.tag() {
            similar::ChangeTag::Insert => add += 1,
            similar::ChangeTag::Delete => del += 1,
            similar::ChangeTag::Equal => {}
        }
    }
    (add, del)
}

/// Keep the head and tail of long output, marking what was cut.
pub(crate) fn trim_middle(s: &str, head: usize, tail: usize) -> String {
    if s.len() <= head + tail {
        return s.to_owned();
    }
    let mut h = head;
    while !s.is_char_boundary(h) {
        h -= 1;
    }
    let mut t = s.len() - tail;
    while !s.is_char_boundary(t) {
        t += 1;
    }
    let omitted = s[h..t].lines().count();
    format!("{}\n… [{omitted} lines omitted] …\n{}", &s[..h], &s[t..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crlf_and_bom_round_trip() {
        let f = TextFile::from_bytes("\u{feff}a\r\nb\r\n".as_bytes()).expect("text");
        assert!(f.bom && f.crlf);
        assert_eq!(f.text, "a\nb\n");
        assert_eq!(f.encode("a\nc\n"), "\u{feff}a\r\nc\r\n");
        assert!(TextFile::from_bytes(b"\x00\x01").is_none());
    }
}
