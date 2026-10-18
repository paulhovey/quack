//! Turning a `tools/call` result into text for the model (docs/plan/09-mcp.md#results).
//!
//! Works on the result's JSON so it reads every content shape the spec revisions use.

use serde_json::Value;

/// Text for the model and whether the server reported an error.
pub(crate) fn render(result: &Value, max_chars: usize) -> (bool, String) {
    let is_error = result
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut parts: Vec<String> = Vec::new();
    for c in result
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let field = |k: &str| c.get(k).and_then(Value::as_str).unwrap_or_default();
        match field("type") {
            "text" => parts.push(field("text").to_owned()),
            "image" | "audio" => parts.push(format!(
                "[{} ({}, {} bytes) not shown: this model cannot see it]",
                field("type"),
                field("mimeType"),
                field("data").len() * 3 / 4
            )),
            "resource" => {
                let r = c.get("resource").cloned().unwrap_or_default();
                let uri = r.get("uri").and_then(Value::as_str).unwrap_or_default();
                match r.get("text").and_then(Value::as_str) {
                    Some(text) => parts.push(format!("[resource {uri}]\n{text}")),
                    None => parts.push(format!("[binary resource {uri} not shown]")),
                }
            }
            "resource_link" => parts.push(format!("[link: {} {}]", field("uri"), field("name"))),
            other => parts.push(format!("[{other} content not shown]")),
        }
    }
    if parts.is_empty()
        && let Some(s) = result.get("structuredContent").filter(|v| !v.is_null())
    {
        parts.push(s.to_string());
    }
    let text = if parts.is_empty() {
        "(no output)".to_owned()
    } else {
        parts.join("\n")
    };
    (is_error, cut(&text, max_chars))
}

/// Keep the start and the end of long output.
pub(crate) fn cut(text: &str, max_chars: usize) -> String {
    let total = text.chars().count();
    if total <= max_chars {
        return text.to_owned();
    }
    let head = max_chars * 3 / 5;
    let tail = max_chars - head;
    let start: String = text.chars().take(head).collect();
    let end: String = text.chars().skip(total - tail).collect();
    format!(
        "{start}\n[… {} characters cut …]\n{end}",
        total - head - tail
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_each_content_kind() {
        let r = serde_json::json!({
            "content": [
                {"type": "text", "text": "hello"},
                {"type": "image", "data": "AAAA", "mimeType": "image/png"},
                {"type": "resource", "resource": {"uri": "file:///a", "text": "body"}},
                {"type": "resource_link", "uri": "file:///b", "name": "b"}
            ],
            "isError": true
        });
        let (err, text) = render(&r, 10_000);
        assert!(err);
        assert_eq!(
            text,
            "hello\n[image (image/png, 3 bytes) not shown: this model cannot see it]\n[resource file:///a]\nbody\n[link: file:///b b]"
        );
        let (_, text) = render(
            &serde_json::json!({"content": [], "structuredContent": {"n": 1}}),
            100,
        );
        assert_eq!(text, "{\"n\":1}");
    }

    #[test]
    fn long_output_keeps_both_ends() {
        let text: String = (0..100).map(|i| format!("{i:03}")).collect();
        let short = cut(&text, 50);
        assert!(short.starts_with("000001"));
        assert!(short.ends_with("099"));
        assert!(short.contains("250 characters cut"));
    }
}
