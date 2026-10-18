//! Text tool-call parsers: recover tool calls that a model printed as text instead of
//! sending them as structured `tool_calls` (docs/plan/02-providers.md#fallback-chain).
//!
//! M1 formats: `hermes` (`<tool_call>{"name":…,"arguments":{…}}</tool_call>`) and
//! `qwen3-xml` (`<tool_call><function=name><parameter=k>v</parameter></function></tool_call>`).
//! A missing closing tag at the end of the reply is tolerated. Only names of offered tools
//! are accepted, so prose that merely mentions the syntax is left alone.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Map, Value};

use crate::types::ToolCall;

static HERMES: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?s)<tool_call>\s*(\{.*?\})\s*(?:</tool_call>|\z)")
        .unwrap_or_else(|_| unreachable!())
});
static QWEN_FN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?s)(?:<tool_call>\s*)?<function=([A-Za-z0-9_.\-]+)>(.*?)(?:</function>\s*(?:</tool_call>)?|\z)")
        .unwrap_or_else(|_| unreachable!())
});
static QWEN_PARAM: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?s)<parameter=([A-Za-z0-9_.\-]+)>(.*?)(?:</parameter>|\z)")
        .unwrap_or_else(|_| unreachable!())
});

/// Which formats to try.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextFormat {
    Auto,
    Hermes,
    Qwen3Xml,
    Off,
}

impl TextFormat {
    /// From the provider file's `tool_calling.text_format`. Unknown names fall back to auto.
    pub fn parse(s: &str) -> Self {
        match s {
            "hermes" => Self::Hermes,
            "qwen3-xml" | "qwen3_xml" => Self::Qwen3Xml,
            "none" | "off" => Self::Off,
            _ => Self::Auto,
        }
    }
}

/// Extract text tool calls from `content`. Returns the content with the calls removed and
/// the calls, or `None` when there are none.
pub fn extract(
    content: &str,
    format: TextFormat,
    tools: &[&str],
) -> Option<(String, Vec<ToolCall>)> {
    if format == TextFormat::Off
        || !(content.contains("<tool_call>") || content.contains("<function="))
    {
        return None;
    }
    let try_qwen = matches!(format, TextFormat::Auto | TextFormat::Qwen3Xml);
    let try_hermes = matches!(format, TextFormat::Auto | TextFormat::Hermes);
    if try_qwen && let Some(r) = qwen(content, tools) {
        return Some(r);
    }
    if try_hermes && let Some(r) = hermes(content, tools) {
        return Some(r);
    }
    None
}

fn call(n: usize, name: &str, args: &Value) -> ToolCall {
    ToolCall {
        id: format!("text_call_{n}"),
        name: name.to_owned(),
        arguments: args.to_string(),
    }
}

fn hermes(content: &str, tools: &[&str]) -> Option<(String, Vec<ToolCall>)> {
    let mut calls = Vec::new();
    let mut rest = String::new();
    let mut last = 0;
    for m in HERMES.captures_iter(content) {
        let (Some(whole), Some(json)) = (m.get(0), m.get(1)) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<Value>(json.as_str()) else {
            continue;
        };
        let Some(name) = v
            .get("name")
            .and_then(Value::as_str)
            .filter(|n| tools.contains(n))
        else {
            continue;
        };
        let args = match v.get("arguments").or_else(|| v.get("parameters")) {
            Some(Value::String(s)) => serde_json::from_str(s).unwrap_or(Value::Object(Map::new())),
            Some(a) => a.clone(),
            None => Value::Object(Map::new()),
        };
        rest.push_str(&content[last..whole.start()]);
        last = whole.end();
        calls.push(call(calls.len(), name, &args));
    }
    rest.push_str(&content[last..]);
    (!calls.is_empty()).then(|| (rest.trim().to_owned(), calls))
}

fn qwen(content: &str, tools: &[&str]) -> Option<(String, Vec<ToolCall>)> {
    let mut calls = Vec::new();
    let mut rest = String::new();
    let mut last = 0;
    for m in QWEN_FN.captures_iter(content) {
        let (Some(whole), Some(name), Some(body)) = (m.get(0), m.get(1), m.get(2)) else {
            continue;
        };
        if !tools.contains(&name.as_str()) {
            continue;
        }
        let mut args = Map::new();
        for p in QWEN_PARAM.captures_iter(body.as_str()) {
            let (Some(k), Some(v)) = (p.get(1), p.get(2)) else {
                continue;
            };
            args.insert(k.as_str().to_owned(), param_value(v.as_str()));
        }
        // The opening <tool_call> may precede the match without being part of it.
        let start = content[last..whole.start()]
            .rfind("<tool_call>")
            .filter(|i| {
                content[last + i + "<tool_call>".len()..whole.start()]
                    .trim()
                    .is_empty()
            })
            .map_or(whole.start(), |i| last + i);
        rest.push_str(&content[last..start]);
        last = whole.end();
        calls.push(call(calls.len(), name.as_str(), &Value::Object(args)));
    }
    rest.push_str(&content[last..]);
    (!calls.is_empty()).then(|| (rest.trim().to_owned(), calls))
}

/// Parameter values are raw text with one surrounding newline; JSON literals are decoded.
fn param_value(raw: &str) -> Value {
    let v = raw.strip_prefix('\n').unwrap_or(raw);
    let v = v.strip_suffix('\n').unwrap_or(v);
    let t = v.trim();
    let looks_json = t.starts_with('{')
        || t.starts_with('[')
        || t == "true"
        || t == "false"
        || t == "null"
        || t.parse::<f64>().is_ok();
    if looks_json && let Ok(j) = serde_json::from_str::<Value>(t) {
        return j;
    }
    Value::String(v.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const TOOLS: &[&str] = &["read", "shell", "edit"];

    fn args(c: &ToolCall) -> Value {
        serde_json::from_str(&c.arguments).unwrap_or_default()
    }

    #[test]
    fn hermes_calls_are_extracted() {
        let text = "Let me look.\n<tool_call>\n{\"name\": \"read\", \"arguments\": {\"path\": \"a.rs\"}}\n</tool_call>";
        let (rest, calls) = extract(text, TextFormat::Auto, TOOLS).expect("found");
        assert_eq!(rest, "Let me look.");
        assert_eq!(calls[0].name, "read");
        assert_eq!(args(&calls[0]), json!({"path": "a.rs"}));
    }

    #[test]
    fn qwen_xml_with_multiline_values_and_numbers() {
        let text = "<tool_call>\n<function=edit>\n<parameter=path>\nsrc/a.rs\n</parameter>\n<parameter=old_string>\nfn a() {\n    1\n}\n</parameter>\n<parameter=count>\n2\n</parameter>\n</function>\n</tool_call>";
        let (rest, calls) = extract(text, TextFormat::Auto, TOOLS).expect("found");
        assert_eq!(rest, "");
        assert_eq!(
            args(&calls[0]),
            json!({"path": "src/a.rs", "old_string": "fn a() {\n    1\n}", "count": 2})
        );
    }

    #[test]
    fn unclosed_at_end_and_unknown_tools() {
        let text = "<function=shell>\n<parameter=command>\nls -la\n";
        let (_, calls) = extract(text, TextFormat::Qwen3Xml, TOOLS).expect("found");
        assert_eq!(args(&calls[0]), json!({"command": "ls -la"}));
        assert!(
            extract(
                "<tool_call>{\"name\":\"rm\",\"arguments\":{}}</tool_call>",
                TextFormat::Auto,
                TOOLS
            )
            .is_none()
        );
        assert!(extract("use <tool_call> tags", TextFormat::Off, TOOLS).is_none());
    }
}
