//! `anthropic-messages`: the Anthropic Messages API, which several open-model hosts also
//! serve (docs/plan/02-providers.md).
//!
//! The history is folded into strictly alternating user and assistant turns: tool results
//! become `tool_result` blocks in a user turn, and a system message after the first becomes
//! user text. The system prompt, the newest message and the end of the user turn before
//! the last reply carry `cache_control`, so servers that cache by breakpoint keep the prefix
//! warm. A reply with thinking is kept as the server sent it ([`crate::Sealed`]): its blocks
//! in order, with the signatures, and sent back unchanged.

use quack_protocol::Usage;
use serde_json::{Value, json};

use crate::error::{ErrorKind, ProviderError, body_message};
use crate::types::{ChatRequest, FinishReason, Message, StreamEvent, merge};

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
                sealed,
                ..
            } => {
                if let Some(blocks) = sealed
                    .as_ref()
                    .and_then(|s| unsealed(content, tool_calls, &s.items))
                {
                    for b in blocks {
                        push(&mut turns, "assistant", b);
                    }
                    continue;
                }
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
    // The previous request put its own on the end of the user turn before the last reply;
    // marking that again reads it back even when the newest step added many blocks.
    let n = turns.len();
    for i in [n.checked_sub(3), n.checked_sub(1)].into_iter().flatten() {
        if turns[i]["role"] != "user" {
            continue;
        }
        if let Some(block) = turns[i]["content"]
            .as_array_mut()
            .and_then(|c| c.last_mut())
        {
            block["cache_control"] = json!({ "type": "ephemeral" });
        }
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
    if let Some(e) = &req.reasoning_effort {
        body["output_config"] = json!({ "effort": e });
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
    merge(&mut body, &req.extra_body);
    body
}

/// The blocks of a sealed reply, with tool calls filled in from the message. `None` when the
/// message no longer matches them (its text or calls were changed), so it is sent rebuilt.
fn unsealed(content: &str, calls: &[crate::ToolCall], items: &[Value]) -> Option<Vec<Value>> {
    let mut text = String::new();
    let mut used = 0;
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        match item["type"].as_str()? {
            "text" => {
                let t = item["text"].as_str()?;
                text.push_str(t);
                if !t.is_empty() {
                    out.push(item.clone());
                }
            }
            "tool_use" => {
                let c = calls.get(usize::try_from(item["ref"].as_u64()?).ok()?)?;
                used += 1;
                out.push(json!({
                    "type": "tool_use",
                    "id": c.id,
                    "name": c.name,
                    "input": arguments(&c.arguments),
                }));
            }
            "thinking" | "redacted_thinking" => out.push(item.clone()),
            _ => return None,
        }
    }
    (text == content && used == calls.len() && !out.is_empty()).then_some(out)
}

/// Turns `data:` payloads into [`StreamEvent`]s.
#[derive(Debug, Default)]
pub(crate) struct EventParser {
    /// Prompt tokens from `message_start`: fresh, cache reads, cache writes.
    input: (u32, u32, u32),
    output: u32,
    finish: Option<FinishReason>,
    /// The reply's blocks in order (tool calls by number), and where each stream index is.
    blocks: Vec<Value>,
    at: std::collections::HashMap<u32, usize>,
    calls: u64,
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
                let block = match b["type"].as_str() {
                    Some("tool_use" | "server_tool_use") => {
                        self.calls += 1;
                        Some(json!({ "type": "tool_use", "ref": self.calls - 1 }))
                    }
                    Some("text") => Some(json!({ "type": "text", "text": b["text"] })),
                    Some("thinking") => Some(json!({
                        "type": "thinking",
                        "thinking": b["thinking"],
                        "signature": b["signature"].as_str().unwrap_or_default(),
                    })),
                    Some("redacted_thinking") => Some(b.clone()),
                    _ => None,
                };
                if let Some(block) = block {
                    self.at.insert(index(), self.blocks.len());
                    self.blocks.push(block);
                }
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
                let block = self.at.get(&index()).and_then(|&i| self.blocks.get_mut(i));
                match d["type"].as_str() {
                    Some("text_delta") => {
                        if let Some(t) = d["text"].as_str().filter(|t| !t.is_empty()) {
                            if let Some(b) = block {
                                append(b, "text", t);
                            }
                            out.push(StreamEvent::TextDelta(t.to_owned()));
                        }
                    }
                    Some("thinking_delta") => {
                        if let Some(t) = d["thinking"].as_str().filter(|t| !t.is_empty()) {
                            if let Some(b) = block {
                                append(b, "thinking", t);
                            }
                            out.push(StreamEvent::ReasoningDelta(t.to_owned()));
                        }
                    }
                    Some("signature_delta") => {
                        if let (Some(b), Some(sig)) = (block, d["signature"].as_str()) {
                            append(b, "signature", sig);
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
                if let Some(n) = num(&v, "/usage/cache_creation_input_tokens") {
                    self.input.2 = n;
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
        let mut out = Vec::new();
        // Only a reply with thinking needs its layout kept.
        let thought = self
            .blocks
            .iter()
            .any(|b| matches!(b["type"].as_str(), Some("thinking" | "redacted_thinking")));
        if thought {
            out.push(StreamEvent::Sealed(std::mem::take(&mut self.blocks)));
        }
        out.push(StreamEvent::Finish(finish));
        out.push(StreamEvent::Usage(Usage {
            prompt_tokens: fresh + read + write,
            completion_tokens: self.output,
            cached_prompt_tokens: Some(read),
            cache_write_tokens: Some(write),
        }));
        out
    }

    /// End of stream: a stream cut before `message_stop` still reports what it had.
    /// A cut stream ends without `message_delta`, so nothing is sealed.
    pub(crate) fn finish(&mut self) -> Vec<StreamEvent> {
        self.end()
    }
}

fn append(block: &mut Value, key: &str, text: &str) {
    match &mut block[key] {
        Value::String(s) => s.push_str(text),
        other => *other = json!(text),
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
                    sealed: None,
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
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"SIG"}}"#,
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#,
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Hi"}}"#,
            r#"{"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"t1","name":"read","input":{}}}"#,
            r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"path\":"}}"#,
            r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"\"a\"}"}}"#,
            r#"{"type":"content_block_start","index":3,"content_block":{"type":"redacted_thinking","data":"XYZ"}}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":20,"cache_creation_input_tokens":5}}"#,
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
                prompt_tokens: 105,
                completion_tokens: 20,
                cached_prompt_tokens: Some(90),
                cache_write_tokens: Some(5),
            })
        );
        assert_eq!(b.finish, Some(FinishReason::ToolCalls));
        let Message::Assistant {
            content,
            reasoning,
            tool_calls,
            sealed,
        } = b.into_message()
        else {
            panic!("assistant");
        };
        let items = sealed.expect("sealed").items;
        assert_eq!(
            items,
            vec![
                json!({"type":"thinking","thinking":"plan","signature":"SIG"}),
                json!({"type":"text","text":"Hi"}),
                json!({"type":"tool_use","ref":0}),
                json!({"type":"redacted_thinking","data":"XYZ"}),
            ]
        );
        assert_eq!(
            (content.as_str(), reasoning.as_deref()),
            ("Hi", Some("plan"))
        );
        assert_eq!(tool_calls[0].arguments, "{\"path\":\"a\"}");
        assert_eq!(tool_calls[0].id, "t1");
        assert!(p.finish().is_empty());
    }

    fn sealed_reply(items: Vec<Value>) -> Message {
        Message::Assistant {
            content: "Hi".into(),
            reasoning: Some("plan".into()),
            tool_calls: vec![ToolCall {
                id: "t1".into(),
                name: "read".into(),
                arguments: "{\"path\":\"a\"}".into(),
            }],
            sealed: Some(crate::Sealed {
                provider: "anthropic".into(),
                model: "m".into(),
                fingerprint: String::new(),
                items,
            }),
        }
    }

    #[test]
    fn sealed_replies_go_back_in_their_order() {
        let items = vec![
            json!({"type":"thinking","thinking":"plan","signature":"SIG"}),
            json!({"type":"text","text":"Hi"}),
            json!({"type":"thinking","thinking":"","signature":"S2"}),
            json!({"type":"tool_use","ref":0}),
        ];
        let req = ChatRequest {
            model: "m".into(),
            messages: vec![
                Message::User {
                    content: "go".into(),
                },
                sealed_reply(items.clone()),
                Message::Tool {
                    call_id: "t1".into(),
                    content: "A".into(),
                },
            ],
            reasoning_effort: Some("high".into()),
            ..ChatRequest::default()
        };
        let b = build_body(&req);
        let reply = &b["messages"][1]["content"];
        assert_eq!(reply[0], items[0]);
        assert_eq!(reply[1], items[1]);
        assert_eq!(reply[2], items[2]);
        assert_eq!(
            reply[3],
            json!({"type":"tool_use","id":"t1","name":"read","input":{"path":"a"}})
        );
        assert_eq!(b["output_config"]["effort"], "high");
        // The user turn before the reply keeps the breakpoint the last request put there.
        assert_eq!(
            b["messages"][0]["content"][0]["cache_control"]["type"],
            "ephemeral"
        );
        assert_eq!(
            b["messages"][2]["content"][0]["cache_control"]["type"],
            "ephemeral"
        );

        // A message whose text changed since is rebuilt, without the thinking.
        let mut changed = sealed_reply(items);
        if let Message::Assistant { content, .. } = &mut changed {
            *content = "Hello".into();
        }
        let req = ChatRequest {
            messages: vec![
                Message::User {
                    content: "go".into(),
                },
                changed,
            ],
            ..req
        };
        let b = build_body(&req);
        assert!(!b.to_string().contains("SIG"));
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
