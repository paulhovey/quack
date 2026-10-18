use std::fmt;
use std::time::Duration;

/// A normalized provider error (docs/plan/02-providers.md#errors).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub struct ProviderError {
    pub kind: ErrorKind,
    pub status: Option<u16>,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    Config,
    Auth,
    RateLimited { retry_after: Option<Duration> },
    ContextLengthExceeded,
    ServerOverloaded,
    BadRequest,
    Network,
    Timeout,
    Protocol,
    Cancelled,
}

impl ErrorKind {
    fn label(self) -> &'static str {
        match self {
            Self::Config => "configuration error",
            Self::Auth => "authentication failed",
            Self::RateLimited { .. } => "rate limited",
            Self::ContextLengthExceeded => "context length exceeded",
            Self::ServerOverloaded => "server error",
            Self::BadRequest => "request rejected",
            Self::Network => "network error",
            Self::Timeout => "timed out",
            Self::Protocol => "unexpected response",
            Self::Cancelled => "cancelled",
        }
    }
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.kind.label())?;
        if let Some(s) = self.status {
            write!(f, " (HTTP {s})")?;
        }
        if !self.message.is_empty() {
            write!(f, ": {}", self.message)?;
        }
        Ok(())
    }
}

impl ProviderError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            status: None,
            message: message.into(),
        }
    }

    pub fn retryable(&self) -> bool {
        matches!(
            self.kind,
            ErrorKind::RateLimited { .. }
                | ErrorKind::ServerOverloaded
                | ErrorKind::Network
                | ErrorKind::Timeout
        )
    }

    pub(crate) fn from_reqwest(e: &reqwest::Error) -> Self {
        let kind = if e.is_timeout() {
            ErrorKind::Timeout
        } else if e.is_connect() || e.is_request() || e.is_body() {
            ErrorKind::Network
        } else if e.is_decode() {
            ErrorKind::Protocol
        } else {
            ErrorKind::Network
        };
        // The innermost source is the useful part ("Connection refused", "dns error: …").
        let mut inner = e.to_string();
        let mut source = std::error::Error::source(e);
        while let Some(s) = source {
            inner = s.to_string();
            source = s.source();
        }
        let inner = match inner.find(" (os error") {
            Some(i) => inner[..i].to_owned(),
            None => inner,
        };
        let host = e.url().and_then(|u| {
            let host = u.host_str()?;
            Some(match u.port() {
                Some(p) => format!("{host}:{p}"),
                None => host.to_owned(),
            })
        });
        let message = match (kind, host) {
            (ErrorKind::Network, Some(h)) if e.is_connect() => {
                format!("cannot connect to {h}: {}", inner.to_lowercase())
            }
            (_, Some(h)) => format!("{h}: {inner}"),
            (_, None) => inner,
        };
        Self::new(kind, message)
    }

    /// Classify a non-success HTTP response.
    pub(crate) fn from_status(status: u16, retry_after: Option<Duration>, body: &str) -> Self {
        let message = body_message(body);
        let lower = message.to_lowercase();
        let kind = match status {
            401 | 403 => ErrorKind::Auth,
            429 => ErrorKind::RateLimited { retry_after },
            400 | 413 | 422 if is_context_overflow(&lower) => ErrorKind::ContextLengthExceeded,
            500 | 502 | 503 | 504 | 529 => ErrorKind::ServerOverloaded,
            _ => ErrorKind::BadRequest,
        };
        Self {
            kind,
            status: Some(status),
            message,
        }
    }
}

/// Servers phrase context overflow differently and usually return it as a plain 400.
fn is_context_overflow(lower: &str) -> bool {
    [
        "context length",
        "context_length",
        "maximum context",
        "context window",
        "too many tokens",
        "prompt is too long",
        "input is too long",
        "exceeds the model",
        "reduce the length",
    ]
    .iter()
    .any(|p| lower.contains(p))
}

/// The `error.message` of a JSON error body, else the body itself, shortened.
pub(crate) fn body_message(body: &str) -> String {
    let parsed = serde_json::from_str::<serde_json::Value>(body).ok();
    let msg = parsed
        .as_ref()
        .and_then(|v| {
            v.pointer("/error/message")
                .or_else(|| v.get("error").filter(|e| e.is_string()))
                .or_else(|| v.get("message"))
                .or_else(|| v.get("detail"))
        })
        .and_then(|m| m.as_str())
        .unwrap_or(body)
        .trim();
    shorten(msg, 300)
}

fn shorten(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let cut: String = s.chars().take(max).collect();
    format!("{cut}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_context_overflow_and_auth() {
        let e = ProviderError::from_status(
            400,
            None,
            r#"{"error":{"message":"This model's maximum context length is 32768 tokens"}}"#,
        );
        assert_eq!(e.kind, ErrorKind::ContextLengthExceeded);
        assert_eq!(
            e.message,
            "This model's maximum context length is 32768 tokens"
        );
        assert_eq!(
            ProviderError::from_status(401, None, "nope").kind,
            ErrorKind::Auth
        );
        assert!(ProviderError::from_status(503, None, "").retryable());
        assert!(!ProviderError::from_status(400, None, "bad").retryable());
    }
}
