//! `api = "ollama"`: Ollama's native `/api/chat` with newline-delimited JSON streaming.
//!
//! The reason to prefer it over Ollama's `/v1`: it takes `num_ctx`, so the window the
//! session plans for is the window Ollama allocates. Through `/v1`, Ollama silently cuts
//! the prompt to its own default, which is far smaller than most sessions need.

use std::collections::HashMap;

use quack_config::provider::ReasoningFormat;
use quack_protocol::Usage;
use serde_json::{Map, Value, json};

use crate::error::{ErrorKind, ProviderError, body_message};
use crate::openai_chat::piece_event;
use crate::reasoning::ThinkSplitter;
use crate::types::{ChatRequest, DiscoveredModel, FinishReason, Message, StreamEvent};

pub(crate) const CHAT_PATH: &str = "api/chat";
pub(crate) const TAGS_PATH: &str = "api/tags";
pub(crate) const SHOW_PATH: &str = "api/show";

/// Window used for discovered models. Ollama allocates memory for the whole window up
/// front, so the trained length (often 128k+) would not fit most machines. A model's
/// `context_window` in the provider file overrides it.
pub(crate) const DISCOVERED_WINDOW_CAP: u32 = 32_768;

/// `http://host:11434/v1` (the OpenAI-compatible form) also works as a base URL.
pub(crate) fn native_base(base_url: &str) -> &str {
    let base = base_url.trim_end_matches('/');
    base.strip_suffix("/v1").unwrap_or(base)
}

pub(crate) fn build_body(req: &ChatRequest) -> Value {
    // Ollama matches tool results by tool name, not call id.
    let names: HashMap<&str, &str> = req
        .messages
        .iter()
        .flat_map(|m| match m {
            Message::Assistant { tool_calls, .. } => tool_calls
                .iter()
                .map(|c| (c.id.as_str(), c.name.as_str()))
                .collect(),
            _ => Vec::new(),
        })
        .collect();
    let messages: Vec<Value> = req
        .messages
        .iter()
        .map(|m| match m {
            Message::System { content } => json!({ "role": "system", "content": content }),
            Message::User { content } => json!({ "role": "user", "content": content }),
            Message::Assistant {
                content,
                reasoning,
                tool_calls,
            } => {
                let mut v = json!({ "role": "assistant", "content": content });
                // Thinking models keep their reasoning across tool calls; the model's
                // template drops what it doesn't use.
                if let Some(r) = reasoning.as_deref().filter(|r| {
                    !r.is_empty()
                        && req.preserve_reasoning != Some(false)
                        && req.reasoning_effort.as_deref() != Some("none")
                }) {
                    v["thinking"] = json!(r);
                }
                if !tool_calls.is_empty() {
                    v["tool_calls"] = tool_calls
                        .iter()
                        .map(|c| {
                            // Arguments go back as an object; keep unparsable text as a string.
                            let args = serde_json::from_str::<Value>(&c.arguments)
                                .unwrap_or_else(|_| Value::String(c.arguments.clone()));
                            json!({ "function": { "name": c.name, "arguments": args } })
                        })
                        .collect();
                }
                v
            }
            Message::Tool { call_id, content } => {
                let mut v = json!({ "role": "tool", "content": content });
                if let Some(name) = names.get(call_id.as_str()) {
                    v["tool_name"] = json!(name);
                }
                v
            }
        })
        .collect();

    let mut body = Map::new();
    body.insert("model".into(), json!(req.model));
    body.insert("messages".into(), Value::Array(messages));
    body.insert("stream".into(), json!(true));
    if !req.tools.is_empty() {
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.parameters,
                    }
                })
            })
            .collect();
        body.insert("tools".into(), Value::Array(tools));
    }
    let s = &req.sampling;
    let mut options = Map::new();
    for (k, v) in [
        ("num_ctx", req.context_window.map(Value::from)),
        ("num_predict", req.max_tokens.map(Value::from)),
        ("temperature", s.temperature.map(Value::from)),
        ("top_p", s.top_p.map(Value::from)),
        ("top_k", s.top_k.map(Value::from)),
        ("min_p", s.min_p.map(Value::from)),
        ("repeat_penalty", s.repetition_penalty.map(Value::from)),
    ] {
        if let Some(v) = v {
            options.insert(k.into(), v);
        }
    }
    if !options.is_empty() {
        body.insert("options".into(), Value::Object(options));
    }
    match req.reasoning_effort.as_deref() {
        None | Some("") => {}
        Some("none" | "off") => {
            body.insert("think".into(), json!(false));
        }
        // gpt-oss takes a level; other thinking models take `true` and ignore levels.
        Some(level @ ("low" | "medium" | "high")) => {
            body.insert("think".into(), json!(level));
        }
        Some(_) => {
            body.insert("think".into(), json!(true));
        }
    }
    for (k, v) in &req.extra_body {
        body.insert(k.clone(), v.clone());
    }
    Value::Object(body)
}

/// Splits a byte stream into lines.
#[derive(Debug, Default)]
pub(crate) struct LineDecoder {
    buf: Vec<u8>,
}

impl LineDecoder {
    pub(crate) fn feed(&mut self, bytes: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        while let Some(i) = self.buf.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=i).collect();
            out.push(String::from_utf8_lossy(&line).into_owned());
        }
        out
    }

    pub(crate) fn finish(&mut self) -> Vec<String> {
        if self.buf.is_empty() {
            return Vec::new();
        }
        vec![String::from_utf8_lossy(&std::mem::take(&mut self.buf)).into_owned()]
    }
}

/// Turns `/api/chat` stream lines into [`StreamEvent`]s.
#[derive(Debug)]
pub(crate) struct LineParser {
    think: Option<ThinkSplitter>,
    calls: u32,
}

impl LineParser {
    pub(crate) fn new(format: ReasoningFormat) -> Self {
        Self {
            think: matches!(format, ReasoningFormat::Auto | ReasoningFormat::ThinkTags)
                .then(ThinkSplitter::default),
            calls: 0,
        }
    }

    pub(crate) fn parse(&mut self, line: &str) -> Result<Vec<StreamEvent>, ProviderError> {
        let line = line.trim();
        if line.is_empty() {
            return Ok(Vec::new());
        }
        let v: Value = serde_json::from_str(line).map_err(|e| {
            ProviderError::new(ErrorKind::Protocol, format!("invalid stream line: {e}"))
        })?;
        if v.get("error").is_some() {
            return Err(ProviderError::new(
                ErrorKind::ServerOverloaded,
                body_message(line),
            ));
        }
        let mut out = Vec::new();
        if let Some(m) = v.get("message") {
            if let Some(r) = m
                .get("thinking")
                .and_then(Value::as_str)
                .filter(|r| !r.is_empty())
            {
                out.push(StreamEvent::ReasoningDelta(r.to_owned()));
            }
            if let Some(text) = m
                .get("content")
                .and_then(Value::as_str)
                .filter(|t| !t.is_empty())
            {
                match &mut self.think {
                    Some(t) => out.extend(t.feed(text).into_iter().map(piece_event)),
                    None => out.push(StreamEvent::TextDelta(text.to_owned())),
                }
            }
            // Ollama sends each call whole, arguments as an object.
            for call in m
                .get("tool_calls")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let f = call.get("function");
                let Some(name) = f
                    .and_then(|f| f.get("name"))
                    .and_then(Value::as_str)
                    .filter(|n| !n.is_empty())
                else {
                    continue;
                };
                let index = self.calls;
                self.calls += 1;
                out.push(StreamEvent::ToolCallStart {
                    index,
                    id: call
                        .get("id")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned),
                    name: name.to_owned(),
                });
                let fragment = match f.and_then(|f| f.get("arguments")) {
                    Some(Value::String(s)) => s.clone(),
                    Some(Value::Null) | None => "{}".to_owned(),
                    Some(other) => other.to_string(),
                };
                out.push(StreamEvent::ToolCallArgsDelta { index, fragment });
            }
        }
        if v.get("done").and_then(Value::as_bool) == Some(true) {
            out.extend(self.finish());
            let num = |k: &str| {
                v.get(k)
                    .and_then(Value::as_u64)
                    .and_then(|n| u32::try_from(n).ok())
            };
            out.push(StreamEvent::Usage(Usage {
                prompt_tokens: num("prompt_eval_count").unwrap_or(0),
                completion_tokens: num("eval_count").unwrap_or(0),
                cached_prompt_tokens: None,
            }));
            let reason = if self.calls > 0 {
                FinishReason::ToolCalls
            } else {
                FinishReason::parse(
                    v.get("done_reason")
                        .and_then(Value::as_str)
                        .unwrap_or("stop"),
                )
            };
            out.push(StreamEvent::Finish(reason));
        }
        Ok(out)
    }

    /// Release text held by the think splitter.
    pub(crate) fn finish(&mut self) -> Vec<StreamEvent> {
        self.think
            .as_mut()
            .map(|t| t.finish().into_iter().map(piece_event).collect())
            .unwrap_or_default()
    }
}

/// Model names from `/api/tags`.
pub(crate) fn parse_tags(body: &Value) -> Vec<String> {
    body.get("models")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|m| m.get("name").or_else(|| m.get("model"))?.as_str())
        .map(str::to_owned)
        .collect()
}

/// The trained context length from `/api/show` (`model_info["<arch>.context_length"]`).
pub(crate) fn parse_show(body: &Value) -> Option<u32> {
    body.get("model_info")?
        .as_object()?
        .iter()
        .find(|(k, _)| k.ends_with(".context_length"))
        .and_then(|(_, v)| v.as_u64())
        .and_then(|n| u32::try_from(n).ok())
}

/// A discovered model with its window capped to what is safe to allocate.
pub(crate) fn discovered(id: String, trained: Option<u32>) -> DiscoveredModel {
    DiscoveredModel {
        id,
        context_window: Some(
            trained.map_or(DISCOVERED_WINDOW_CAP, |t| t.min(DISCOVERED_WINDOW_CAP)),
        ),
        max_output: None,
        cost: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ResponseBuilder, ToolCall, ToolSpec};

    #[test]
    fn body_uses_options_tool_names_and_object_arguments() {
        let req = ChatRequest {
            model: "qwen3:8b".into(),
            messages: vec![
                Message::User {
                    content: "hi".into(),
                },
                Message::Assistant {
                    content: String::new(),
                    reasoning: None,
                    tool_calls: vec![ToolCall {
                        id: "c1".into(),
                        name: "read".into(),
                        arguments: r#"{"path":"a.rs"}"#.into(),
                    }],
                },
                Message::Tool {
                    call_id: "c1".into(),
                    content: "fn a() {}".into(),
                },
            ],
            tools: vec![ToolSpec {
                name: "read".into(),
                description: "Read a file".into(),
                parameters: json!({"type": "object"}),
            }],
            context_window: Some(32_768),
            max_tokens: Some(1024),
            reasoning_effort: Some("none".into()),
            ..ChatRequest::default()
        };
        let b = build_body(&req);
        assert_eq!(b["options"]["num_ctx"], 32_768);
        assert_eq!(b["options"]["num_predict"], 1024);
        assert_eq!(b["think"], false);
        assert_eq!(
            b["messages"][1]["tool_calls"][0]["function"]["arguments"]["path"],
            "a.rs"
        );
        assert_eq!(b["messages"][2]["tool_name"], "read");
        assert_eq!(b["tools"][0]["function"]["name"], "read");
    }

    #[test]
    fn earlier_thinking_goes_back_unless_turned_off() {
        let mut req = ChatRequest {
            model: "qwen3:8b".into(),
            messages: vec![Message::Assistant {
                content: String::new(),
                reasoning: Some("read a.rs first".into()),
                tool_calls: Vec::new(),
            }],
            ..ChatRequest::default()
        };
        assert_eq!(
            build_body(&req)["messages"][0]["thinking"],
            "read a.rs first"
        );
        req.preserve_reasoning = Some(false);
        assert!(build_body(&req)["messages"][0].get("thinking").is_none());
    }

    #[test]
    fn stream_lines_become_events() {
        let mut d = LineDecoder::default();
        let mut p = LineParser::new(ReasoningFormat::Auto);
        let stream = concat!(
            r#"{"message":{"role":"assistant","content":"","thinking":"hmm"},"done":false}"#,
            "\n",
            r#"{"message":{"role":"assistant","content":"Let me read."},"done":false}"#,
            "\n",
            r#"{"message":{"role":"assistant","content":"","tool_calls":[{"function":{"name":"read","arguments":{"path":"a.rs"}}}]},"done":false}"#,
            "\n",
            r#"{"message":{"role":"assistant","content":""},"done":true,"done_reason":"stop","prompt_eval_count":120,"eval_count":9}"#,
        );
        let mut b = ResponseBuilder::default();
        // Split mid-line to exercise the decoder.
        let (a, rest) = stream.split_at(50);
        for line in d
            .feed(a.as_bytes())
            .into_iter()
            .chain(d.feed(rest.as_bytes()))
            .chain(d.finish())
        {
            for ev in p.parse(&line).expect("parse") {
                b.push(&ev);
            }
        }
        assert_eq!(b.finish, Some(FinishReason::ToolCalls));
        assert_eq!(b.usage.map(|u| u.prompt_tokens), Some(120));
        let Message::Assistant {
            content,
            reasoning,
            tool_calls,
        } = b.into_message()
        else {
            unreachable!()
        };
        assert_eq!(content, "Let me read.");
        assert_eq!(reasoning.as_deref(), Some("hmm"));
        assert_eq!(tool_calls[0].name, "read");
        assert_eq!(tool_calls[0].arguments, r#"{"path":"a.rs"}"#);
    }

    #[test]
    fn errors_tags_show_and_base_url() {
        let mut p = LineParser::new(ReasoningFormat::Auto);
        assert!(p.parse(r#"{"error":"model 'x' not found"}"#).is_err());
        assert_eq!(
            parse_tags(&json!({"models":[{"name":"qwen3:8b"},{"name":"llama3.2"}]})),
            vec!["qwen3:8b", "llama3.2"]
        );
        let show =
            json!({"model_info":{"general.architecture":"qwen3","qwen3.context_length":40960}});
        assert_eq!(parse_show(&show), Some(40_960));
        assert_eq!(
            discovered("m".into(), Some(40_960)).context_window,
            Some(32_768)
        );
        assert_eq!(
            discovered("m".into(), Some(8192)).context_window,
            Some(8192)
        );
        assert_eq!(native_base("http://h:11434/v1/"), "http://h:11434");
        assert_eq!(native_base("http://h:11434"), "http://h:11434");
    }
}
