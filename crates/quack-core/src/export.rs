//! `/export`: the session's transcript as Markdown or JSON (docs/plan/07-sessions-memory-checkpoints.md).
//!
//! Built from the session log the way resume rebuilds the transcript, so it shows what
//! the user saw: messages, tool calls with their results, and where history was
//! compacted. Rewound turns are left out, as they are on resume.

use quack_protocol::{ExportFormat, ReplayItem};
use serde_json::json;

/// What the export starts with.
pub(crate) struct Header<'a> {
    pub id: &'a str,
    pub title: Option<&'a str>,
    pub model: &'a str,
    /// When the session started, as a date.
    pub started: &'a str,
}

pub(crate) fn render(format: ExportFormat, header: &Header<'_>, items: &[ReplayItem]) -> String {
    match format {
        ExportFormat::Markdown => markdown(header, items),
        ExportFormat::Json => {
            let doc = json!({
                "id": header.id,
                "title": header.title,
                "model": header.model,
                "started": header.started,
                "items": items,
            });
            serde_json::to_string_pretty(&doc).unwrap_or_default() + "\n"
        }
    }
}

fn markdown(header: &Header<'_>, items: &[ReplayItem]) -> String {
    let mut out = format!(
        "# {}\n\n_quack session `{}` · {} · {}_\n",
        header.title.unwrap_or("quack session"),
        header.id,
        header.model,
        header.started
    );
    for item in items {
        out.push('\n');
        match item {
            ReplayItem::User { text } => {
                out.push_str("## You\n\n");
                out.push_str(text.trim_end());
                out.push('\n');
            }
            ReplayItem::Assistant { text } => {
                out.push_str("## quack\n\n");
                out.push_str(text.trim_end());
                out.push('\n');
            }
            ReplayItem::Tool {
                summary,
                ok,
                result,
            } => {
                let mark = if *ok { "✓" } else { "✗" };
                let summary = summary.replace('<', "&lt;");
                if result.trim().is_empty() {
                    out.push_str(&format!("> {mark} `{summary}`\n"));
                } else {
                    let fence = fence(result);
                    out.push_str(&format!(
                        "<details><summary>{mark} {summary}</summary>\n\n{fence}\n{}\n{fence}\n\n</details>\n",
                        result.trim_end()
                    ));
                }
            }
            ReplayItem::Compacted => {
                out.push_str("---\n\n_Earlier history was summarized here._\n")
            }
        }
    }
    out
}

/// A backtick fence longer than any run of backticks in `text`.
fn fence(text: &str) -> String {
    let mut longest = 0;
    let mut run = 0;
    for c in text.chars() {
        if c == '`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    "`".repeat(longest.max(2) + 1)
}

/// A file name for the export: the title as a slug, else the session id's start.
pub(crate) fn file_name(header: &Header<'_>, format: ExportFormat) -> String {
    let slug: String = header
        .title
        .unwrap_or_default()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let slug = slug
        .split('-')
        .filter(|w| !w.is_empty())
        .take(6)
        .collect::<Vec<_>>()
        .join("-");
    let short: String = header.id.chars().take(8).collect();
    let stem = if slug.is_empty() {
        format!("quack-{short}")
    } else {
        format!("quack-{slug}-{short}")
    };
    match format {
        ExportFormat::Markdown => format!("{stem}.md"),
        ExportFormat::Json => format!("{stem}.json"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header() -> Header<'static> {
        Header {
            id: "0123456789abcdef",
            title: Some("Fix the parser: tokens!"),
            model: "mock/tiny",
            started: "2026-10-25",
        }
    }

    #[test]
    fn markdown_reads_like_the_transcript() {
        let items = vec![
            ReplayItem::User {
                text: "fix it".into(),
            },
            ReplayItem::Tool {
                summary: "shell cargo test <x>".into(),
                ok: false,
                result: "a ``` fence\nfailed".into(),
            },
            ReplayItem::Tool {
                summary: "edit src/a.rs".into(),
                ok: true,
                result: String::new(),
            },
            ReplayItem::Compacted,
            ReplayItem::Assistant {
                text: "Fixed.\n".into(),
            },
        ];
        let md = render(ExportFormat::Markdown, &header(), &items);
        assert!(md.starts_with("# Fix the parser: tokens!\n\n_quack session `0123456789abcdef` · mock/tiny · 2026-10-25_\n"), "{md}");
        assert!(md.contains("## You\n\nfix it\n"), "{md}");
        assert!(
            md.contains("<details><summary>✗ shell cargo test &lt;x></summary>\n\n````\na ``` fence\nfailed\n````\n"),
            "{md}"
        );
        assert!(md.contains("> ✓ `edit src/a.rs`\n"), "{md}");
        assert!(
            md.contains("_Earlier history was summarized here._"),
            "{md}"
        );
        assert!(md.ends_with("## quack\n\nFixed.\n"), "{md}");
    }

    #[test]
    fn json_keeps_the_items_and_names_are_safe() {
        let items = vec![ReplayItem::User { text: "hi".into() }];
        let doc: serde_json::Value =
            serde_json::from_str(&render(ExportFormat::Json, &header(), &items)).expect("json");
        assert_eq!(doc["items"][0]["kind"], "user");
        assert_eq!(doc["model"], "mock/tiny");
        assert_eq!(
            file_name(&header(), ExportFormat::Markdown),
            "quack-fix-the-parser-tokens-01234567.md"
        );
        let untitled = Header {
            title: None,
            ..header()
        };
        assert_eq!(
            file_name(&untitled, ExportFormat::Json),
            "quack-01234567.json"
        );
    }
}
