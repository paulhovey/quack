use std::path::{Path, PathBuf};

use miette::{Diagnostic, NamedSource, SourceSpan};

/// A problem in one config file, rendered by miette with the offending span.
#[derive(Debug, Clone, thiserror::Error, Diagnostic)]
#[error("{message}")]
pub struct FileDiagnostic {
    pub message: String,
    pub path: PathBuf,
    #[source_code]
    src: NamedSource<String>,
    #[label("here")]
    span: Option<SourceSpan>,
    #[help]
    pub help: Option<String>,
}

impl FileDiagnostic {
    pub fn new(
        path: &Path,
        text: &str,
        message: impl Into<String>,
        span: Option<std::ops::Range<usize>>,
    ) -> Self {
        Self {
            message: message.into(),
            path: path.to_path_buf(),
            src: NamedSource::new(path.display().to_string(), text.to_owned()),
            span: span.map(Into::into),
            help: None,
        }
    }

    pub fn from_toml(path: &Path, text: &str, err: &toml::de::Error) -> Self {
        Self::new(path, text, err.message().trim(), err.span())
    }

    pub fn io(path: &Path, err: &std::io::Error) -> Self {
        Self::new(path, "", format!("cannot read file: {err}"), None)
    }

    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }

    /// `path:line:col: message`, for the startup panel and logs.
    pub fn one_line(&self) -> String {
        let pos = self.span.map(|s| {
            let text = self.src.inner();
            let before = &text[..s.offset().min(text.len())];
            let line = before.matches('\n').count() + 1;
            let col = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
            format!(":{line}:{col}")
        });
        format!(
            "{}{}: {}",
            self.path.display(),
            pos.unwrap_or_default(),
            self.message
        )
    }
}

/// Read a UTF-8 config file, stripping a BOM (Notepad adds one).
pub(crate) fn read_text(path: &Path) -> std::io::Result<String> {
    let text = std::fs::read_to_string(path)?;
    Ok(match text.strip_prefix('\u{feff}') {
        Some(rest) => rest.to_owned(),
        None => text,
    })
}
