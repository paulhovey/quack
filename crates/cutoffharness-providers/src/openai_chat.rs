//! `api = "openai-chat"`: `/chat/completions` with SSE streaming.
//! Covers vLLM, SGLang, llama.cpp, LM Studio, TGI, Ollama `/v1` and most hosted services.

use std::collections::HashSet;

use cutoffharness_config::provider::ReasoningFormat;
use cutoffharness_protocol::Usage;
use serde_json::{Map, Value, json};

use crate::error::{ErrorKind, ProviderError, body_message};
use crate::reasoning::{Piece, ThinkSplitter};
use crate::types::{ChatRequest, DiscoveredModel, FinishReason, Message, StreamEvent};

pub(crate) const CHAT_PATH: &str = "chat/completions";
pub(crate) const MODELS_PATH: &str = "models";

/// Serialize a request. Reasoning from earlier turns is not sent back.
pub(crate) fn build_body(req: &ChatRequest) -> Value {
    let messages: Vec<Value> = req.messages.iter().map(message_json).collect();
    let mut body = Map::new();
    body.insert("model".into(), json!(req.model));
    body.insert("messages".into(), Value::Array(messages));
    body.insert("stream".into(), json!(true));
    body.insert("stream_options".into(), json!({ "include_usage": true }));
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
    if let Some(n) = req.max_tokens {
        body.insert("max_tokens".into(), json!(n));
    }
    let s = &req.sampling;
    let optional = [
        ("temperature", s.temperature.map(Value::from)),
        ("top_p", s.top_p.map(Value::from)),
        ("top_k", s.top_k.map(Value::from)),
        ("min_p", s.min_p.map(Value::from)),
        ("repetition_penalty", s.repetition_penalty.map(Value::from)),
        (
            "reasoning_effort",
            req.reasoning_effort.clone().map(Value::from),
        ),
    ];
    for (k, v) in optional {
        if let Some(v) = v {
            body.insert(k.into(), v);
        }
    }
    for (k, v) in &req.extra_body {
        body.insert(k.clone(), v.clone());
    }
    Value::Object(body)
}

fn message_json(m: &Message) -> Value {
    match m {
        Message::System { content } => json!({ "role": "system", "content": content }),
        Message::User { content } => json!({ "role": "user", "content": content }),
        Message::Assistant {
            content,
            tool_calls,
            ..
        } => {
            let mut v = json!({ "role": "assistant", "content": content });
            if !tool_calls.is_empty() {
                // Some servers reject an empty string next to tool calls.
                if content.is_empty() {
                    v["content"] = Value::Null;
                }
                v["tool_calls"] = tool_calls
                    .iter()
                    .map(|c| {
                        json!({
                            "id": c.id,
                            "type": "function",
                            "function": { "name": c.name, "arguments": c.arguments },
                        })
                    })
                    .collect();
            }
            v
        }
        Message::Tool { call_id, content } => {
            json!({ "role": "tool", "tool_call_id": call_id, "content": content })
        }
    }
}

/// Turns `data:` payloads into [`StreamEvent`]s.
#[derive(Debug)]
pub(crate) struct ChunkParser {
    think: Option<ThinkSplitter>,
    started: HashSet<u32>,
}

impl ChunkParser {
    pub(crate) fn new(format: ReasoningFormat) -> Self {
        let think = matches!(format, ReasoningFormat::Auto | ReasoningFormat::ThinkTags)
            .then(ThinkSplitter::default);
        Self {
            think,
            started: HashSet::new(),
        }
    }

    /// Parse one payload. `[DONE]` yields nothing.
    pub(crate) fn parse(&mut self, data: &str) -> Result<Vec<StreamEvent>, ProviderError> {
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            return Ok(Vec::new());
        }
        let v: Value = serde_json::from_str(data).map_err(|e| {
            ProviderError::new(ErrorKind::Protocol, format!("invalid stream chunk: {e}"))
        })?;
        if v.get("error").is_some() {
            return Err(ProviderError::new(
                ErrorKind::ServerOverloaded,
                body_message(data),
            ));
        }
        let mut out = Vec::new();
        if let Some(choice) = v.pointer("/choices/0") {
            if let Some(delta) = choice.get("delta") {
                self.delta(delta, &mut out);
            }
            if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
                self.flush(&mut out);
                out.push(StreamEvent::Finish(FinishReason::parse(reason)));
            }
        }
        if let Some(u) = v.get("usage").filter(|u| u.is_object()) {
            out.push(StreamEvent::Usage(parse_usage(u)));
        }
        Ok(out)
    }

    /// End of stream: release text held by the think splitter.
    pub(crate) fn finish(&mut self) -> Vec<StreamEvent> {
        let mut out = Vec::new();
        self.flush(&mut out);
        out
    }

    fn flush(&mut self, out: &mut Vec<StreamEvent>) {
        if let Some(t) = &mut self.think {
            out.extend(t.finish().into_iter().map(piece_event));
        }
    }

    fn delta(&mut self, d: &Value, out: &mut Vec<StreamEvent>) {
        for key in ["reasoning_content", "reasoning"] {
            if let Some(r) = d.get(key).and_then(Value::as_str).filter(|r| !r.is_empty()) {
                out.push(StreamEvent::ReasoningDelta(r.to_owned()));
            }
        }
        if let Some(text) = d
            .get("content")
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty())
        {
            match &mut self.think {
                Some(t) => out.extend(t.feed(text).into_iter().map(piece_event)),
                None => out.push(StreamEvent::TextDelta(text.to_owned())),
            }
        }
        for call in d
            .get("tool_calls")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let index = call
                .get("index")
                .and_then(Value::as_u64)
                .and_then(|i| u32::try_from(i).ok())
                .unwrap_or(0);
            let f = call.get("function");
            let name = f.and_then(|f| f.get("name")).and_then(Value::as_str);
            if !self.started.contains(&index) {
                let Some(name) = name.filter(|n| !n.is_empty()) else {
                    continue; // arguments before a name: nothing to attach them to
                };
                self.started.insert(index);
                out.push(StreamEvent::ToolCallStart {
                    index,
                    id: call
                        .get("id")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned),
                    name: name.to_owned(),
                });
            }
            // Arguments arrive as string fragments; a few servers send an object instead.
            let args = f.and_then(|f| f.get("arguments"));
            let fragment = match args {
                Some(Value::String(s)) => s.clone(),
                Some(Value::Null) | None => String::new(),
                Some(other) => other.to_string(),
            };
            if !fragment.is_empty() {
                out.push(StreamEvent::ToolCallArgsDelta { index, fragment });
            }
        }
    }
}

fn piece_event(p: Piece) -> StreamEvent {
    match p {
        Piece::Text(t) => StreamEvent::TextDelta(t),
        Piece::Reasoning(r) => StreamEvent::ReasoningDelta(r),
    }
}

fn parse_usage(u: &Value) -> Usage {
    let num = |p: &str| {
        u.pointer(p)
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
    };
    Usage {
        prompt_tokens: num("/prompt_tokens").unwrap_or(0),
        completion_tokens: num("/completion_tokens").unwrap_or(0),
        cached_prompt_tokens: num("/prompt_tokens_details/cached_tokens")
            .or_else(|| num("/cached_tokens"))
            .or_else(|| num("/prompt_cache_hit_tokens")),
    }
}

/// Parse a `/models` response. Context sizes go by different names per server.
pub(crate) fn parse_models(body: &Value) -> Vec<DiscoveredModel> {
    let list = body
        .get("data")
        .or_else(|| body.get("models"))
        .unwrap_or(body);
    let num = |m: &Value, keys: &[&str]| {
        keys.iter()
            .find_map(|k| m.pointer(k).and_then(Value::as_u64))
            .and_then(|n| u32::try_from(n).ok())
    };
    list.as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let id = m.get("id").or_else(|| m.get("name"))?.as_str()?.to_owned();
            Some(DiscoveredModel {
                id,
                context_window: num(
                    m,
                    &[
                        "/context_window",
                        "/max_model_len",
                        "/context_length",
                        "/meta/n_ctx_train",
                    ],
                ),
                max_output: num(
                    m,
                    &[
                        "/max_output_tokens",
                        "/max_completion_tokens",
                        "/top_provider/max_completion_tokens",
                    ],
                ),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ResponseBuilder, ToolCall, ToolSpec};

    #[test]
    fn body_shape() {
        let req = ChatRequest {
            model: "m".into(),
            messages: vec![
                Message::System {
                    content: "sys".into(),
                },
                Message::Assistant {
                    content: String::new(),
                    reasoning: Some("hidden".into()),
                    tool_calls: vec![ToolCall {
                        id: "c1".into(),
                        name: "read".into(),
                        arguments: "{}".into(),
                    }],
                },
                Message::Tool {
                    call_id: "c1".into(),
                    content: "ok".into(),
                },
            ],
            tools: vec![ToolSpec {
                name: "read".into(),
                description: "Read a file".into(),
                parameters: json!({"type": "object"}),
            }],
            max_tokens: Some(64),
            extra_body: [(
                "chat_template_kwargs".to_owned(),
                json!({"enable_thinking": false}),
            )]
            .into_iter()
            .collect(),
            ..ChatRequest::default()
        };
        let b = build_body(&req);
        assert_eq!(b["messages"][1]["content"], Value::Null);
        assert_eq!(
            b["messages"][1]["tool_calls"][0]["function"]["name"],
            "read"
        );
        assert!(b["messages"][1].get("reasoning").is_none());
        assert_eq!(b["messages"][2]["tool_call_id"], "c1");
        assert_eq!(b["tools"][0]["function"]["name"], "read");
        assert_eq!(b["max_tokens"], 64);
        assert_eq!(b["chat_template_kwargs"]["enable_thinking"], false);
        assert!(b.get("temperature").is_none());
    }

    #[test]
    fn stream_with_think_tags_tool_call_and_usage() {
        let chunks = [
            r#"{"choices":[{"delta":{"role":"assistant","content":"<think>look"}}]}"#,
            r#"{"choices":[{"delta":{"content":" first</think>\n\nOk."}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_a","type":"function","function":{"name":"read","arguments":""}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"path\":"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"a.rs\"}"}}]}}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
            r#"{"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":5,"prompt_tokens_details":{"cached_tokens":8}}}"#,
            "[DONE]",
        ];
        let mut p = ChunkParser::new(ReasoningFormat::Auto);
        let mut b = ResponseBuilder::default();
        for c in chunks {
            for ev in p.parse(c).expect("chunk") {
                b.push(&ev);
            }
        }
        for ev in p.finish() {
            b.push(&ev);
        }
        assert_eq!(b.finish, Some(FinishReason::ToolCalls));
        assert_eq!(
            b.usage,
            Some(Usage {
                prompt_tokens: 10,
                completion_tokens: 5,
                cached_prompt_tokens: Some(8)
            })
        );
        assert_eq!(
            b.into_message(),
            Message::Assistant {
                content: "Ok.".into(),
                reasoning: Some("look first".into()),
                tool_calls: vec![ToolCall {
                    id: "call_a".into(),
                    name: "read".into(),
                    arguments: r#"{"path":"a.rs"}"#.into()
                }],
            }
        );
    }

    #[test]
    fn mid_stream_error_and_models() {
        let mut p = ChunkParser::new(ReasoningFormat::None);
        let err = p
            .parse(r#"{"error":{"message":"overloaded"}}"#)
            .expect_err("error chunk");
        assert_eq!(err.message, "overloaded");

        let models = parse_models(&json!({"data": [
            {"id": "a", "context_window": 1000, "max_output_tokens": 10},
            {"id": "b", "max_model_len": 32768},
            {"id": "c"}
        ]}));
        assert_eq!(models[0].context_window, Some(1000));
        assert_eq!(models[0].max_output, Some(10));
        assert_eq!(models[1].context_window, Some(32768));
        assert_eq!(models[2].context_window, None);
    }
}
