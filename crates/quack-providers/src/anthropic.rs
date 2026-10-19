//! `anthropic-messages`: the Anthropic Messages API, which several open-model hosts also
//! serve (docs/plan/02-providers.md).
//!
//! The history is folded into strictly alternating user and assistant turns: tool results
//! become `tool_result` blocks in a user turn, and a system message after the first becomes
//! user text. The system prompt and the newest message carry `cache_control`, so servers
//! that cache by breakpoint keep the prefix warm. Earlier reasoning is not sent back: it
//! needs signatures this harness does not keep.

use quack_protocol::Usage;
use serde_json::{Value, json};

use crate::error::{ErrorKind, ProviderError, body_message};
use crate::types::{ChatRequest, FinishReason, Message, StreamEvent};

pub(crate) const VERSION: &str = "2023-06-01";
/// `max_tokens` is required; this is used when the model entry sets none.
const DEFAULT_MAX_TOKENS: u32 = 8192;

/// `<base>/messages` when the base already ends in `/v1`, else `<base>/v1/messages`.
pub(crate) fn path(base_url: &str, endpoint: &str) -> String {
    if base_url.trim_end_matches('/').ends_with("/v1") {
        endpoint.to_owned()
    } else {
        format!("v1/{endpoint}")
    }
}

fn arguments(raw: &str) -> Value {
    match serde_json::from_str::<Value>(raw) {
        Ok(v @ Value::Object(_)) => v,
        _ => json!({}),
    }
}

/// Append `block` to the last turn when it has `role`, else start a new turn.
fn push(turns: &mut Vec<Value>, role: &str, block: Value) {
    if let Some(last) = turns.last_mut()
        && last["role"] == role
        && let Some(content) = last["content"].as_array_mut()
    {
        content.push(block);
        return;
    }
    turns.push(json!({ "role": role, "content": [block] }));
}

pub(crate) fn build_body(req: &ChatRequest) -> Value {
    let mut system: Vec<String> = Vec::new();
    let mut turns: Vec<Value> = Vec::new();
    for (i, m) in req.messages.iter().enumerate() {
        match m {
            Message::System { content } if i == 0 => system.push(content.clone()),
            Message::System { content } | Message::User { content } => {
                if !content.is_empty() {
                    push(
                        &mut turns,
                        "user",
                        json!({ "type": "text", "text": content }),
                    );
                }
            }
            Message::Assistant {
                content,
                tool_calls,
                ..
            } => {
                let mut blocks = Vec::new();
                if !content.trim().is_empty() {
                    blocks.push(json!({ "type": "text", "text": content }));
                }
                for c in tool_calls {
                    blocks.push(json!({
                        "type": "tool_use",
                        "id": c.id,
                        "name": c.name,
                        "input": arguments(&c.arguments),
                    }));
                }
                if blocks.is_empty() {
                    blocks.push(json!({ "type": "text", "text": "(no reply)" }));
                }
                for b in blocks {
                    push(&mut turns, "assistant", b);
                }
            }
            Message::Tool { call_id, content } => push(
                &mut turns,
                "user",
                json!({ "type": "tool_result", "tool_use_id": call_id, "content": content }),
            ),
        }
    }
    // The conversation must start with the user.
    if turns.first().is_some_and(|t| t["role"] != "user") {
        turns.insert(
            0,
            json!({ "role": "user", "content": [{ "type": "text", "text": "(start)" }] }),
        );
    }
    // A cache breakpoint on the newest block: everything before it is reused next time.
    if let Some(block) = turns
        .last_mut()
        .and_then(|t| t["content"].as_array_mut())
        .and_then(|c| c.last_mut())
    {
        block["cache_control"] = json!({ "type": "ephemeral" });
    }

    let mut body = json!({
        "model": req.model,
        "max_tokens": req.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
        "messages": turns,
        "stream": true,
    });
    if !system.is_empty() {
        body["system"] = json!([{
            "type": "text",
            "text": system.join("\n\n"),
            "cache_control": { "type": "ephemeral" },
        }]);
    }
    if !req.tools.is_empty() {
        body["tools"] = req
            .tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.parameters,
                })
            })
            .collect();
    }
    let s = &req.sampling;
    if let Some(t) = s.temperature {
        body["temperature"] = json!(t);
    }
    if let Some(p) = s.top_p {
        body["top_p"] = json!(p);
    }
    if let Some(k) = s.top_k {
        body["top_k"] = json!(k);
    }
    for (k, v) in &req.extra_body {
        body[k] = v.clone();
    }
    body
}

/// Turns `data:` payloads into [`StreamEvent`]s.
#[derive(Debug, Default)]
pub(crate) struct EventParser {
    /// Prompt tokens from `message_start`: fresh, cache reads, cache writes.
    input: (u32, u32, u32),
    output: u32,
    finish: Option<FinishReason>,
}

impl EventParser {
    pub(crate) fn parse(&mut self, data: &str) -> Result<Vec<StreamEvent>, ProviderError> {
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            return Ok(Vec::new());
        }
        let v: Value = serde_json::from_str(data).map_err(|e| {
            ProviderError::new(ErrorKind::Protocol, format!("invalid stream event: {e}"))
        })?;
        let num = |v: &Value, p: &str| {
            v.pointer(p)
                .and_then(Value::as_u64)
                .and_then(|n| u32::try_from(n).ok())
        };
        let index = || num(&v, "/index").unwrap_or(0);
        let mut out = Vec::new();
        match v["type"].as_str().unwrap_or_default() {
            "message_start" => {
                let u = &v["message"]["usage"];
                self.input = (
                    num(u, "/input_tokens").unwrap_or(0),
                    num(u, "/cache_read_input_tokens").unwrap_or(0),
                    num(u, "/cache_creation_input_tokens").unwrap_or(0),
                );
                self.output = num(u, "/output_tokens").unwrap_or(0);
            }
            "content_block_start" => {
                let b = &v["content_block"];
                match b["type"].as_str() {
                    Some("tool_use" | "server_tool_use") => {
                        out.push(StreamEvent::ToolCallStart {
                            index: index(),
                            id: b["id"].as_str().map(str::to_owned),
                            name: b["name"].as_str().unwrap_or_default().to_owned(),
                        });
                        // Some servers send the whole input here instead of as deltas.
                        if let Some(input) = b
                            .get("input")
                            .filter(|i| i.as_object().is_some_and(|o| !o.is_empty()))
                        {
                            out.push(StreamEvent::ToolCallArgsDelta {
                                index: index(),
                                fragment: input.to_string(),
                            });
                        }
                    }
                    Some("text") => {
                        if let Some(t) = b["text"].as_str().filter(|t| !t.is_empty()) {
                            out.push(StreamEvent::TextDelta(t.to_owned()));
                        }
                    }
                    Some("thinking") => {
                        if let Some(t) = b["thinking"].as_str().filter(|t| !t.is_empty()) {
                            out.push(StreamEvent::ReasoningDelta(t.to_owned()));
                        }
                    }
                    _ => {}
                }
            }
            "content_block_delta" => {
                let d = &v["delta"];
                match d["type"].as_str() {
                    Some("text_delta") => {
                        if let Some(t) = d["text"].as_str().filter(|t| !t.is_empty()) {
                            out.push(StreamEvent::TextDelta(t.to_owned()));
                        }
                    }
                    Some("thinking_delta") => {
                        if let Some(t) = d["thinking"].as_str().filter(|t| !t.is_empty()) {
                            out.push(StreamEvent::ReasoningDelta(t.to_owned()));
                        }
                    }
                    Some("input_json_delta") => {
                        if let Some(f) = d["partial_json"].as_str().filter(|f| !f.is_empty()) {
                            out.push(StreamEvent::ToolCallArgsDelta {
                                index: index(),
                                fragment: f.to_owned(),
                            });
                        }
                    }
                    _ => {}
                }
            }
            "message_delta" => {
                if let Some(r) = v["delta"]["stop_reason"].as_str() {
                    self.finish = Some(FinishReason::parse(r));
                }
                if let Some(n) = num(&v, "/usage/output_tokens") {
                    self.output = n;
                }
                // Some servers only report input tokens here.
                if let Some(n) = num(&v, "/usage/input_tokens").filter(|n| *n > 0) {
                    self.input.0 = n;
                }
                if let Some(n) = num(&v, "/usage/cache_read_input_tokens") {
                    self.input.1 = n;
                }
            }
            "message_stop" => out.extend(self.end()),
            "error" => {
                let kind = match v["error"]["type"].as_str() {
                    Some("overloaded_error" | "api_error") => ErrorKind::ServerOverloaded,
                    Some("rate_limit_error") => ErrorKind::RateLimited { retry_after: None },
                    _ => ErrorKind::BadRequest,
                };
                return Err(ProviderError::new(kind, body_message(data)));
            }
            _ => {}
        }
        Ok(out)
    }

    fn end(&mut self) -> Vec<StreamEvent> {
        let Some(finish) = self.finish.take() else {
            return Vec::new();
        };
        let (fresh, read, write) = self.input;
        vec![
            StreamEvent::Finish(finish),
            StreamEvent::Usage(Usage {
                prompt_tokens: fresh + read + write,
                completion_tokens: self.output,
                cached_prompt_tokens: Some(read),
            }),
        ]
    }

    /// End of stream: a stream cut before `message_stop` still reports what it had.
    pub(crate) fn finish(&mut self) -> Vec<StreamEvent> {
        self.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ToolCall, ToolSpec};

    #[test]
    fn folds_history_into_alternating_turns() {
        let req = ChatRequest {
            model: "m".into(),
            messages: vec![
                Message::System {
                    content: "sys".into(),
                },
                Message::User {
                    content: "hi".into(),
                },
                Message::Assistant {
                    content: String::new(),
                    reasoning: Some("hmm".into()),
                    tool_calls: vec![
                        ToolCall {
                            id: "a".into(),
                            name: "read".into(),
                            arguments: "{\"path\":\"x\"}".into(),
                        },
                        ToolCall {
                            id: "b".into(),
                            name: "read".into(),
                            arguments: "oops".into(),
                        },
                    ],
                },
                Message::Tool {
                    call_id: "a".into(),
                    content: "A".into(),
                },
                Message::Tool {
                    call_id: "b".into(),
                    content: "B".into(),
                },
                Message::User {
                    content: "and?".into(),
                },
            ],
            tools: vec![ToolSpec {
                name: "read".into(),
                description: "r".into(),
                parameters: json!({"type":"object"}),
            }],
            ..ChatRequest::default()
        };
        let b = build_body(&req);
        assert_eq!(b["system"][0]["text"], "sys");
        assert_eq!(b["max_tokens"], 8192);
        let turns = b["messages"].as_array().expect("turns");
        assert_eq!(turns.len(), 3);
        assert_eq!(turns[1]["content"][0]["input"], json!({"path":"x"}));
        assert_eq!(turns[1]["content"][1]["input"], json!({}));
        let last = turns[2]["content"].as_array().expect("blocks");
        assert_eq!(last.len(), 3);
        assert_eq!(last[0]["type"], "tool_result");
        assert_eq!(last[2]["text"], "and?");
        assert_eq!(last[2]["cache_control"]["type"], "ephemeral");
        assert_eq!(b["tools"][0]["input_schema"]["type"], "object");
        assert!(!b.to_string().contains("hmm"));
    }

    #[test]
    fn parses_a_stream() {
        let mut p = EventParser::default();
        let events: Vec<StreamEvent> = [
            r#"{"type":"message_start","message":{"usage":{"input_tokens":10,"cache_read_input_tokens":90,"output_tokens":1}}}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"plan"}}"#,
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#,
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Hi"}}"#,
            r#"{"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"t1","name":"read","input":{}}}"#,
            r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"path\":"}}"#,
            r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"\"a\"}"}}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":20}}"#,
            r#"{"type":"message_stop"}"#,
        ]
        .iter()
        .flat_map(|d| p.parse(d).expect("parses"))
        .collect();
        let mut b = crate::types::ResponseBuilder::default();
        for e in &events {
            b.push(e);
        }
        assert_eq!(
            b.usage,
            Some(Usage {
                prompt_tokens: 100,
                completion_tokens: 20,
                cached_prompt_tokens: Some(90)
            })
        );
        assert_eq!(b.finish, Some(FinishReason::ToolCalls));
        let Message::Assistant {
            content,
            reasoning,
            tool_calls,
        } = b.into_message()
        else {
            panic!("assistant");
        };
        assert_eq!(
            (content.as_str(), reasoning.as_deref()),
            ("Hi", Some("plan"))
        );
        assert_eq!(tool_calls[0].arguments, "{\"path\":\"a\"}");
        assert_eq!(tool_calls[0].id, "t1");
        assert!(p.finish().is_empty());
    }

    #[test]
    fn stream_errors_are_classified() {
        let mut p = EventParser::default();
        let e = p
            .parse(r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#)
            .expect_err("error");
        assert_eq!(e.kind, ErrorKind::ServerOverloaded);
    }

    #[test]
    fn endpoint_path_follows_the_base() {
        assert_eq!(path("https://api.anthropic.com", "messages"), "v1/messages");
        assert_eq!(
            path("https://host/api/anthropic/v1/", "messages"),
            "messages"
        );
    }
}
