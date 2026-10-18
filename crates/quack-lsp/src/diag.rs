//! Diagnostics: parsing, the before/after delta, and the text the model sees.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    Error = 1,
    Warning = 2,
    Info = 3,
    Hint = 4,
}

impl Severity {
    pub fn parse(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "error" => Self::Error,
            "info" | "information" => Self::Info,
            "hint" => Self::Hint,
            _ => Self::Warning,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Info => "info",
            Self::Hint => "hint",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// 0-based, as the server sent it.
    pub line: u32,
    pub column: u32,
    pub severity: Severity,
    pub code: Option<String>,
    pub message: String,
}

impl Diagnostic {
    pub fn from_lsp(v: &Value) -> Option<Self> {
        let start = &v["range"]["start"];
        let code = match &v["code"] {
            Value::String(s) => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        };
        Some(Self {
            line: u32::try_from(start["line"].as_u64()?).ok()?,
            column: u32::try_from(start["character"].as_u64().unwrap_or(0)).unwrap_or(0),
            severity: match v["severity"].as_u64() {
                Some(1) => Severity::Error,
                Some(3) => Severity::Info,
                Some(4) => Severity::Hint,
                // Missing severity: the client decides; treat it as a warning.
                _ => Severity::Warning,
            },
            code,
            message: v["message"].as_str()?.to_owned(),
        })
    }

    /// What stays the same when lines move: an edit above a warning doesn't make it new.
    fn identity(&self) -> (Severity, Option<&str>, &str) {
        (self.severity, self.code.as_deref(), self.message.as_str())
    }

    /// `src/a.rs:88:17 error[E0308]: mismatched types` (first line of the message).
    pub fn render(&self, shown: &str) -> String {
        let code = self
            .code
            .as_deref()
            .map(|c| format!("[{c}]"))
            .unwrap_or_default();
        let first = self.message.lines().next().unwrap_or_default().trim();
        let first: String = if first.chars().count() > 240 {
            format!("{}…", first.chars().take(240).collect::<String>())
        } else {
            first.to_owned()
        };
        format!(
            "{shown}:{}:{} {}{code}: {first}",
            self.line + 1,
            self.column + 1,
            self.severity.label()
        )
    }
}

pub type Snapshot = HashMap<PathBuf, Vec<Diagnostic>>;

/// Diagnostics in `after` that were not in `before` (as a multiset, ignoring position).
pub fn new_since(before: &[Diagnostic], after: &[Diagnostic]) -> Vec<Diagnostic> {
    let mut left: HashMap<(Severity, Option<&str>, &str), usize> = HashMap::new();
    for d in before {
        *left.entry(d.identity()).or_default() += 1;
    }
    after
        .iter()
        .filter(|d| match left.get_mut(&d.identity()) {
            Some(n) if *n > 0 => {
                *n -= 1;
                false
            }
            _ => true,
        })
        .cloned()
        .collect()
}

/// Settings for one report.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub min_severity: Severity,
    pub max_items: usize,
}

/// The block appended to an edit's result: new problems in the edited file at or above
/// the threshold, then new errors in other files. `None` when there is nothing new.
pub fn report(
    edited: &Path,
    before: &Snapshot,
    after: &Snapshot,
    limits: Limits,
    display: &(dyn Fn(&Path) -> String + Sync),
) -> Option<String> {
    let none = Vec::new();
    let mut lines = Vec::new();
    let mut count = 0usize;
    let mut fixed = 0usize;
    let mut push = |path: &Path, found: Vec<Diagnostic>, lines: &mut Vec<String>| {
        let shown = display(path);
        count += found.len();
        lines.extend(
            found
                .iter()
                .take(limits.max_items)
                .map(|d| format!("  {}", d.render(&shown))),
        );
        if found.len() > limits.max_items {
            lines.push(format!(
                "  … and {} more in {shown}",
                found.len() - limits.max_items
            ));
        }
    };

    let key = crate::pos::key(edited);
    let was = before.get(&key).unwrap_or(&none);
    let now = after.get(&key).unwrap_or(&none);
    let mut here = new_since(was, now);
    here.retain(|d| d.severity <= limits.min_severity);
    here.sort_by_key(|d| (d.severity, d.line, d.column));
    fixed += new_since(now, was)
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .count();
    push(edited, here, &mut lines);

    let mut others: Vec<(&PathBuf, Vec<Diagnostic>)> = after
        .iter()
        .filter(|(p, _)| **p != key)
        .map(|(p, now)| {
            let was = before.get(p).unwrap_or(&none);
            let mut errs = new_since(was, now);
            errs.retain(|d| d.severity == Severity::Error);
            errs.sort_by_key(|d| (d.line, d.column));
            (p, errs)
        })
        .filter(|(_, e)| !e.is_empty())
        .collect();
    others.sort_by(|a, b| a.0.cmp(b.0));
    for (p, errs) in others.into_iter().take(5) {
        push(p, errs, &mut lines);
    }

    if count == 0 {
        return (fixed > 0).then(|| {
            format!(
                "diagnostics: {fixed} error{} fixed, none new",
                if fixed == 1 { "" } else { "s" }
            )
        });
    }
    Some(format!("diagnostics ({count} new):\n{}", lines.join("\n")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(line: u32, severity: Severity, message: &str) -> Diagnostic {
        Diagnostic {
            line,
            column: 4,
            severity,
            code: None,
            message: message.into(),
        }
    }

    #[test]
    fn delta_ignores_moved_lines() {
        let before = vec![d(3, Severity::Warning, "unused x")];
        let after = vec![
            d(5, Severity::Warning, "unused x"),
            d(9, Severity::Error, "mismatched types"),
        ];
        let new = new_since(&before, &after);
        assert_eq!(new, vec![d(9, Severity::Error, "mismatched types")]);
    }

    #[test]
    fn report_lists_new_problems_and_other_files_errors() {
        let edited = PathBuf::from("/w/a.rs");
        let other = PathBuf::from("/w/b.rs");
        let before: Snapshot = [(edited.clone(), vec![d(1, Severity::Warning, "old")])].into();
        let after: Snapshot = [
            (
                edited.clone(),
                vec![
                    d(1, Severity::Warning, "old"),
                    d(2, Severity::Hint, "style"),
                    Diagnostic {
                        code: Some("E0308".into()),
                        ..d(7, Severity::Error, "mismatched types\nexpected u32")
                    },
                ],
            ),
            (
                other.clone(),
                vec![d(0, Severity::Error, "unresolved import")],
            ),
        ]
        .into();
        let limits = Limits {
            min_severity: Severity::Warning,
            max_items: 20,
        };
        let display = |p: &Path| p.strip_prefix("/w").unwrap_or(p).display().to_string();
        let text = report(&edited, &before, &after, limits, &display).expect("report");
        assert_eq!(
            text,
            "diagnostics (2 new):\n  a.rs:8:5 error[E0308]: mismatched types\n  b.rs:1:5 error: unresolved import"
        );
        assert_eq!(report(&edited, &after, &after, limits, &display), None);
        let fixed = report(&edited, &after, &before, limits, &display).expect("fixed");
        assert_eq!(fixed, "diagnostics: 1 error fixed, none new");
    }
}
