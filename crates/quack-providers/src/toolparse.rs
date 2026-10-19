//! Text tool-call parsers: recover tool calls that a model printed as text instead of
//! sending them as structured `tool_calls` (docs/plan/02-providers.md#fallback-chain).
//!
//! Formats:
//! - `hermes`: `<tool_call>{"name":…,"arguments":{…}}</tool_call>`
//! - `qwen3-xml`: `<tool_call><function=name><parameter=k>v</parameter></function></tool_call>`
//! - `glm`: `<tool_call>name<arg_key>k</arg_key><arg_value>v</arg_value></tool_call>`
//! - `deepseek`: `<｜tool▁call▁begin｜>name<｜tool▁sep｜>{…}<｜tool▁call▁end｜>` (and the older
//!   `function<｜tool▁sep｜>name` + a fenced JSON block)
//! - `kimi`: `<|tool_call_begin|>functions.name:0<|tool_call_argument_begin|>{…}<|tool_call_end|>`
//! - `json-block`: a fenced JSON object with `name` and `arguments`. Only when chosen: in
//!   `auto` it would take JSON the model only meant to show.
//!
//! A missing closing tag at the end of the reply is tolerated. Only names of offered tools
//! are accepted, so prose that merely mentions the syntax is left alone. The formats are
//! written from each family's published chat templates; recorded fixtures should confirm
//! them (docs/plan/13-testing-and-evals.md#recording-fixtures).

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

static GLM: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?s)<tool_call>\s*([A-Za-z0-9_.\-]+)\s*((?:<arg_key>.*?</arg_key>\s*<arg_value>.*?(?:</arg_value>|\z)\s*)*)(?:</tool_call>|\z)")
        .unwrap_or_else(|_| unreachable!())
});
static GLM_ARG: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?s)<arg_key>(.*?)</arg_key>\s*<arg_value>(.*?)(?:</arg_value>|\z)")
        .unwrap_or_else(|_| unreachable!())
});
static DEEPSEEK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?s)<｜tool▁call▁begin｜>(.*?)<｜tool▁sep｜>(.*?)(?:<｜tool▁call▁end｜>|\z)")
        .unwrap_or_else(|_| unreachable!())
});
static KIMI: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?s)<\|tool_call_begin\|>\s*(?:functions\.)?([A-Za-z0-9_\-]+(?:\.[A-Za-z0-9_\-]+)*?)(?::\d+)?\s*<\|tool_call_argument_begin\|>(.*?)(?:<\|tool_call_end\|>|\z)")
        .unwrap_or_else(|_| unreachable!())
});
static JSON_BLOCK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?s)```(?:json)?[ \t]*\n(\{.*?\})\s*```").unwrap_or_else(|_| unreachable!())
});
static FENCED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?s)```(?:json)?\s*(.*?)\s*```").unwrap_or_else(|_| unreachable!())
});

/// Section markers around DeepSeek and Kimi calls, removed with them.
const SECTIONS: &[&str] = &[
    "<｜tool▁calls▁begin｜>",
    "<｜tool▁calls▁end｜>",
    "<|tool_calls_section_begin|>",
    "<|tool_calls_section_end|>",
];

/// Which formats to try.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextFormat {
    Auto,
    Hermes,
    Qwen3Xml,
    Glm,
    Deepseek,
    Kimi,
    JsonBlock,
    Off,
}

impl TextFormat {
    /// From the provider file's `tool_calling.text_format`. Unknown names fall back to auto.
    pub fn parse(s: &str) -> Self {
        match s {
            "hermes" => Self::Hermes,
            "qwen3-xml" | "qwen3_xml" => Self::Qwen3Xml,
            "glm" => Self::Glm,
            "deepseek" => Self::Deepseek,
            "kimi" => Self::Kimi,
            "json-block" | "json_block" => Self::JsonBlock,
            "none" | "off" => Self::Off,
            _ => Self::Auto,
        }
    }
}

/// Openings of a text tool call, in any supported format.
const MARKERS: &[&str] = &[
    "<tool_call>",
    "<function=",
    "<｜tool▁calls▁begin｜>",
    "<｜tool▁call▁begin｜>",
    "<|tool_calls_section_begin|>",
    "<|tool_call_begin|>",
];

/// Holds back streamed text from the first tool-call marker on, so a call printed as text
/// is never shown raw. Whatever it hid comes back through the step's final content.
#[derive(Debug, Default)]
pub struct TextGate {
    pending: String,
    hiding: bool,
    /// Pass everything through (`text_format = "none"`).
    off: bool,
}

impl TextGate {
    pub fn new(format: TextFormat) -> Self {
        Self {
            off: format == TextFormat::Off,
            ..Self::default()
        }
    }

    /// Text that is safe to show now.
    pub fn feed(&mut self, delta: &str) -> String {
        if self.off {
            return delta.to_owned();
        }
        if self.hiding {
            return String::new();
        }
        self.pending.push_str(delta);
        if let Some(i) = MARKERS.iter().filter_map(|m| self.pending.find(m)).min() {
            self.hiding = true;
            let shown = self.pending[..i].to_owned();
            self.pending.clear();
            return shown;
        }
        // Keep back a tail that could be the start of a marker. Prefixes end on the
        // marker's character boundaries, so the cut is on one of the text's too.
        let keep = MARKERS
            .iter()
            .map(|m| {
                (1..m.len())
                    .rev()
                    .filter(|k| m.is_char_boundary(*k))
                    .find(|k| self.pending.ends_with(&m[..*k]))
                    .unwrap_or(0)
            })
            .max()
            .unwrap_or(0);
        let cut = self.pending.len() - keep;
        self.pending.drain(..cut).collect()
    }

    /// The held-back tail at the end of the stream, unless it is part of a call.
    pub fn finish(&mut self) -> String {
        if self.off || self.hiding {
            return String::new();
        }
        std::mem::take(&mut self.pending)
    }

    /// Whether some text was withheld because it looked like a tool call.
    pub fn hid(&self) -> bool {
        self.hiding
    }
}

/// Extract text tool calls from `content`. Returns the content with the calls removed and
/// the calls, or `None` when there are none.
pub fn extract(
    content: &str,
    format: TextFormat,
    tools: &[&str],
) -> Option<(String, Vec<ToolCall>)> {
    use TextFormat as F;
    if format == F::Off {
        return None;
    }
    if format == F::JsonBlock {
        return json_block(content, tools);
    }
    if !MARKERS.iter().any(|m| content.contains(m)) {
        return None;
    }
    let tries: [(TextFormat, Parser); 5] = [
        (F::Qwen3Xml, qwen),
        (F::Hermes, hermes),
        (F::Glm, glm),
        (F::Deepseek, deepseek),
        (F::Kimi, kimi),
    ];
    tries
        .iter()
        .filter(|(f, _)| format == F::Auto || format == *f)
        .find_map(|(_, parse)| parse(content, tools))
}

type Parser = fn(&str, &[&str]) -> Option<(String, Vec<ToolCall>)>;

/// Replace each match of `re` that `read` turns into an offered call; the rest stays text.
fn by_regex(
    content: &str,
    re: &Regex,
    tools: &[&str],
    read: impl Fn(&regex::Captures<'_>) -> Option<(String, Value)>,
) -> Option<(String, Vec<ToolCall>)> {
    let mut calls = Vec::new();
    let mut rest = String::new();
    let mut last = 0;
    for m in re.captures_iter(content) {
        let Some(whole) = m.get(0) else { continue };
        let Some((name, args)) = read(&m).filter(|(n, _)| tools.contains(&n.as_str())) else {
            continue;
        };
        rest.push_str(&content[last..whole.start()]);
        last = whole.end();
        calls.push(call(calls.len(), &name, &args));
    }
    if calls.is_empty() {
        return None;
    }
    rest.push_str(&content[last..]);
    for s in SECTIONS {
        rest = rest.replace(s, "");
    }
    Some((rest.trim().to_owned(), calls))
}

/// A JSON object from a call's argument text: plain, or in a fence.
fn json_args(raw: &str) -> Value {
    let t = raw.trim();
    let t = FENCED
        .captures(t)
        .and_then(|c| c.get(1))
        .map_or(t, |m| m.as_str());
    match serde_json::from_str::<Value>(t) {
        Ok(v @ Value::Object(_)) => v,
        _ => Value::Object(Map::new()),
    }
}

fn glm(content: &str, tools: &[&str]) -> Option<(String, Vec<ToolCall>)> {
    by_regex(content, &GLM, tools, |m| {
        let name = m.get(1)?.as_str().to_owned();
        let mut args = Map::new();
        for a in GLM_ARG.captures_iter(m.get(2).map_or("", |b| b.as_str())) {
            let (Some(k), Some(v)) = (a.get(1), a.get(2)) else {
                continue;
            };
            args.insert(k.as_str().trim().to_owned(), param_value(v.as_str()));
        }
        Some((name, Value::Object(args)))
    })
}

fn deepseek(content: &str, tools: &[&str]) -> Option<(String, Vec<ToolCall>)> {
    by_regex(content, &DEEPSEEK, tools, |m| {
        let head = m.get(1)?.as_str().trim();
        let body = m.get(2)?.as_str();
        if head == "function" {
            // V3: `function<｜tool▁sep｜>name` then a fenced JSON block.
            let (name, rest) = body.split_once('\n').unwrap_or((body, ""));
            return Some((name.trim().to_owned(), json_args(rest)));
        }
        Some((head.to_owned(), json_args(body)))
    })
}

fn kimi(content: &str, tools: &[&str]) -> Option<(String, Vec<ToolCall>)> {
    by_regex(content, &KIMI, tools, |m| {
        Some((m.get(1)?.as_str().to_owned(), json_args(m.get(2)?.as_str())))
    })
}

fn json_block(content: &str, tools: &[&str]) -> Option<(String, Vec<ToolCall>)> {
    by_regex(content, &JSON_BLOCK, tools, |m| {
        let v: Value = serde_json::from_str(m.get(1)?.as_str()).ok()?;
        let name = v
            .get("name")
            .or_else(|| v.get("tool"))?
            .as_str()?
            .to_owned();
        let args = match v.get("arguments").or_else(|| v.get("parameters")) {
            Some(Value::String(s)) => json_args(s),
            Some(a @ Value::Object(_)) => a.clone(),
            _ => Value::Object(Map::new()),
        };
        Some((name, args))
    })
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
    fn gate_hides_calls_split_across_deltas() {
        let mut g = TextGate::new(TextFormat::Auto);
        let mut shown = String::new();
        for d in [
            "Let me look.\n<to",
            "ol_call>\n{\"name\"",
            ": \"read\"}</tool_call>",
        ] {
            shown.push_str(&g.feed(d));
        }
        shown.push_str(&g.finish());
        assert_eq!(shown, "Let me look.\n");
        assert!(g.hid());

        let mut g = TextGate::new(TextFormat::Auto);
        let shown = g.feed("a < b and <fun") + &g.feed("ny stuff>") + &g.finish();
        assert_eq!(shown, "a < b and <funny stuff>");
        assert!(!g.hid());

        let mut g = TextGate::new(TextFormat::Off);
        assert_eq!(g.feed("<tool_call>"), "<tool_call>");
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

    #[test]
    fn glm_calls() {
        let text = "Checking.\n<tool_call>read\n<arg_key>path</arg_key>\n<arg_value>src/a.rs</arg_value>\n<arg_key>limit</arg_key>\n<arg_value>20</arg_value>\n</tool_call>";
        let (rest, calls) = extract(text, TextFormat::Auto, TOOLS).expect("found");
        assert_eq!(rest, "Checking.");
        assert_eq!(args(&calls[0]), json!({"path": "src/a.rs", "limit": 20}));
        // No arguments, and unclosed at the end.
        let (_, calls) = extract("<tool_call>shell", TextFormat::Glm, TOOLS).expect("found");
        assert_eq!(
            (calls[0].name.as_str(), args(&calls[0])),
            ("shell", json!({}))
        );
    }

    #[test]
    fn deepseek_calls_both_generations() {
        let v31 = "Sure.<｜tool▁calls▁begin｜><｜tool▁call▁begin｜>read<｜tool▁sep｜>{\"path\": \"a.rs\"}<｜tool▁call▁end｜><｜tool▁call▁begin｜>shell<｜tool▁sep｜>{\"command\": \"ls\"}<｜tool▁call▁end｜><｜tool▁calls▁end｜>";
        let (rest, calls) = extract(v31, TextFormat::Auto, TOOLS).expect("found");
        assert_eq!(rest, "Sure.");
        assert_eq!(calls.len(), 2);
        assert_eq!(args(&calls[1]), json!({"command": "ls"}));
        let v3 = "<｜tool▁calls▁begin｜><｜tool▁call▁begin｜>function<｜tool▁sep｜>read\n```json\n{\"path\": \"b.rs\"}\n```<｜tool▁call▁end｜><｜tool▁calls▁end｜>";
        let (rest, calls) = extract(v3, TextFormat::Deepseek, TOOLS).expect("found");
        assert_eq!(rest, "");
        assert_eq!(args(&calls[0]), json!({"path": "b.rs"}));
    }

    #[test]
    fn kimi_calls() {
        let text = "<|tool_calls_section_begin|><|tool_call_begin|>functions.edit:0<|tool_call_argument_begin|>{\"path\": \"a\", \"old_string\": \"x\", \"new_string\": \"y\"}<|tool_call_end|><|tool_calls_section_end|>";
        let (rest, calls) = extract(text, TextFormat::Auto, TOOLS).expect("found");
        assert_eq!(rest, "");
        assert_eq!(calls[0].name, "edit");
        assert_eq!(args(&calls[0])["new_string"], "y");
    }

    #[test]
    fn json_blocks_only_when_chosen() {
        let text = "I'll read it.\n```json\n{\"name\": \"read\", \"arguments\": {\"path\": \"a.rs\"}}\n```";
        assert!(extract(text, TextFormat::Auto, TOOLS).is_none());
        let (rest, calls) = extract(text, TextFormat::JsonBlock, TOOLS).expect("found");
        assert_eq!(rest, "I'll read it.");
        assert_eq!(args(&calls[0]), json!({"path": "a.rs"}));
    }

    #[test]
    fn gate_holds_back_multibyte_markers() {
        let mut g = TextGate::new(TextFormat::Auto);
        let shown = g.feed("Done<｜tool▁ca") + &g.feed("lls▁begin｜>…") + &g.finish();
        assert_eq!(shown, "Done");
        let mut g = TextGate::new(TextFormat::Auto);
        let shown = g.feed("a <｜b") + &g.finish();
        assert_eq!(shown, "a <｜b");
    }
}
