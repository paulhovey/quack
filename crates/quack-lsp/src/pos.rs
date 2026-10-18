//! File URIs and positions. Positions shown to the model are 1-based lines and
//! characters; LSP counts from 0 in UTF-16 code units unless UTF-8 was negotiated.

use std::path::{Path, PathBuf};

pub fn path_to_uri(path: &Path) -> String {
    url::Url::from_file_path(path)
        .map_or_else(|()| format!("file://{}", path.display()), |u| u.to_string())
}

/// The path of a `file:` URI. Handles `file:///c%3A/x` (a percent-encoded drive colon)
/// as well as `file:///C:/x`.
pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let u = url::Url::parse(uri).ok()?;
    if u.scheme() != "file" {
        return None;
    }
    u.to_file_path().ok()
}

/// A path as a map key. Servers echo back the URIs they are sent, except that some
/// lowercase the drive letter (`file:///c%3A/…`), so the drive letter is uppercased.
pub fn key(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    let mut chars = s.chars();
    match (chars.next(), chars.next()) {
        (Some(d), Some(':')) if cfg!(windows) && d.is_ascii_alphabetic() => {
            PathBuf::from(format!("{}{}", d.to_ascii_uppercase(), &s[1..]))
        }
        _ => path.to_path_buf(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Utf8,
    Utf16,
}

impl Encoding {
    pub fn from_server(name: Option<&str>) -> Self {
        match name {
            Some("utf-8") => Self::Utf8,
            _ => Self::Utf16,
        }
    }

    /// LSP column of the `chars`-th character of `line`.
    pub fn to_lsp(self, line: &str, chars: usize) -> u32 {
        let units: usize = line
            .chars()
            .take(chars)
            .map(|c| match self {
                Self::Utf8 => c.len_utf8(),
                Self::Utf16 => c.len_utf16(),
            })
            .sum();
        u32::try_from(units).unwrap_or(u32::MAX)
    }

    /// Character index in `line` of an LSP column.
    pub fn from_lsp(self, line: &str, column: u32) -> usize {
        let mut units = 0usize;
        for (i, c) in line.chars().enumerate() {
            if units >= column as usize {
                return i;
            }
            units += match self {
                Self::Utf8 => c.len_utf8(),
                Self::Utf16 => c.len_utf16(),
            };
        }
        line.chars().count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns() {
        let line = "let é = \"😀x\";";
        // `x` is the 11th character: UTF-16 counts the emoji as 2, UTF-8 as 4 (é as 2).
        assert_eq!(Encoding::Utf16.to_lsp(line, 10), 11);
        assert_eq!(Encoding::Utf8.to_lsp(line, 10), 14);
        assert_eq!(Encoding::Utf16.from_lsp(line, 11), 10);
        assert_eq!(Encoding::Utf8.from_lsp(line, 14), 10);
        assert_eq!(Encoding::Utf16.from_lsp(line, 999), line.chars().count());
    }

    #[cfg(unix)]
    #[test]
    fn uris() {
        let p = Path::new("/work/my repo/a.rs");
        let uri = path_to_uri(p);
        assert_eq!(uri, "file:///work/my%20repo/a.rs");
        assert_eq!(uri_to_path(&uri).as_deref(), Some(p));
        assert_eq!(uri_to_path("untitled:x"), None);
    }

    #[cfg(windows)]
    #[test]
    fn windows_uris() {
        let p = Path::new(r"C:\Users\me\my repo\a.rs");
        let uri = path_to_uri(p);
        assert_eq!(uri, "file:///C:/Users/me/my%20repo/a.rs");
        assert_eq!(key(&uri_to_path(&uri).expect("path")), key(p));
        let encoded = uri_to_path("file:///c%3A/Users/me/my%20repo/a.rs").expect("path");
        assert_eq!(key(&encoded), key(p));
    }
}
