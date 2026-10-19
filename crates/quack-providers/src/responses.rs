//! `openai-responses`: the OpenAI Responses API (`POST /v1/responses`), also served by
//! some gateways and by vLLM for gpt-oss (docs/plan/02-providers.md).
//!
//! Requests are stateless (`store: false`): the whole history goes in `input` every time,
//! the same way the chat adapter sends it, so prefix caching works the same.

use std::collections::HashSet;

use quack_protocol::Usage;
use serde_json::{Value, json};

use crate::error::{ErrorKind, ProviderError, body_message};
use crate::types::{ChatRequest, FinishReason, Message, StreamEvent};

pub(crate) const RESPONSES_PATH: &str = "responses";

pub(crate) fn build_body(req: &ChatRequest) -> Value {
    let mut instructions = None;
    let mut input = Vec::new();
    for (i, m) in req.messages.iter().enumerate() {
        match m {
            Message::System { content } if i == 0 => instructions = Some(content.clone()),
            Message::System { content } => {
                input.push(json!({ "role": "developer", "content": content }));
            }
            Message::User { content } => input.push(json!({ "role": "user", "content": content })),
            Message::Assistant {
                content,
                tool_calls,
                ..
            } => {
                if !content.is_empty() {
                    input.push(json!({ "role": "assistant", "content": content }));
                }
                for c in tool_calls {
                    input.push(json!({
                        "type": "function_call",
                        "call_id": c.id,
                        "name": c.name,
                        "arguments": c.arguments,
                    }));
                }
            }
            Message::Tool { call_id, content } => input.push(json!({
                "type": "function_call_output",
                "call_id": call_id,
                "output": content,
            })),
        }
    }
    let mut body = json!({
        "model": req.model,
        "input": input,
        "stream": true,
        "store": false,
    });
    if let Some(i) = instructions {
        body["instructions"] = json!(i);
    }
    if !req.tools.is_empty() {
        body["tools"] = req
            .tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.parameters,
                })
            })
            .collect();
    }
    if let Some(n) = req.max_tokens {
        body["max_output_tokens"] = json!(n);
    }
    if let Some(e) = &req.reasoning_effort {
        body["reasoning"] = json!({ "effort": e });
    }
    let s = &req.sampling;
    if let Some(t) = s.temperature {
        body["temperature"] = json!(t);
    }
    if let Some(p) = s.top_p {
        body["top_p"] = json!(p);
    }
    for (k, v) in &req.extra_body {
        body[k] = v.clone();
    }
    body
}

/// Turns `data:` payloads into [`StreamEvent`]s.
#[derive(Debug, Default)]
pub(crate) struct EventParser {
    /// Output indexes of function calls, and those whose arguments came as deltas.
    calls: HashSet<u32>,
    streamed: HashSet<u32>,
}

fn usage(u: &Value) -> Usage {
    let num = |p: &str| {
        u.pointer(p)
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
    };
    Usage {
        prompt_tokens: num("/input_tokens").unwrap_or(0),
        completion_tokens: num("/output_tokens").unwrap_or(0),
        cached_prompt_tokens: num("/input_tokens_details/cached_tokens"),
    }
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
        let index = v["output_index"]
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .unwrap_or(0);
        let text = |k: &str| v[k].as_str().filter(|t| !t.is_empty()).map(str::to_owned);
        let mut out = Vec::new();
        match v["type"].as_str().unwrap_or_default() {
            "response.output_text.delta" | "response.refusal.delta" => {
                out.extend(text("delta").map(StreamEvent::TextDelta));
            }
            "response.reasoning_text.delta" | "response.reasoning_summary_text.delta" => {
                out.extend(text("delta").map(StreamEvent::ReasoningDelta));
            }
            "response.output_item.added" => {
                let item = &v["item"];
                if item["type"] == "function_call" {
                    self.calls.insert(index);
                    out.push(StreamEvent::ToolCallStart {
                        index,
                        id: item["call_id"].as_str().map(str::to_owned),
                        name: item["name"].as_str().unwrap_or_default().to_owned(),
                    });
                }
            }
            "response.function_call_arguments.delta" => {
                if let Some(f) = text("delta") {
                    self.streamed.insert(index);
                    out.push(StreamEvent::ToolCallArgsDelta { index, fragment: f });
                }
            }
            // Servers that don't stream arguments send them whole at the end.
            "response.function_call_arguments.done" => {
                if !self.streamed.contains(&index)
                    && let Some(f) = text("arguments")
                {
                    self.streamed.insert(index);
                    out.push(StreamEvent::ToolCallArgsDelta { index, fragment: f });
                }
            }
            kind @ ("response.completed" | "response.incomplete") => {
                let r = &v["response"];
                let finish = if kind == "response.incomplete" {
                    match r["incomplete_details"]["reason"].as_str() {
                        Some("max_output_tokens") => FinishReason::Length,
                        Some("content_filter") => FinishReason::ContentFilter,
                        Some(other) => FinishReason::Other(other.to_owned()),
                        None => FinishReason::Other("incomplete".into()),
                    }
                } else if self.calls.is_empty() {
                    FinishReason::Stop
                } else {
                    FinishReason::ToolCalls
                };
                out.push(StreamEvent::Finish(finish));
                if r["usage"].is_object() {
                    out.push(StreamEvent::Usage(usage(&r["usage"])));
                }
            }
            "response.failed" | "error" => {
                let msg = body_message(data);
                let code = v
                    .pointer("/response/error/code")
                    .or_else(|| v.get("code"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let kind = if code.contains("context") || msg.contains("context") {
                    ErrorKind::ContextLengthExceeded
                } else if code.contains("rate_limit") {
                    ErrorKind::RateLimited { retry_after: None }
                } else {
                    ErrorKind::ServerOverloaded
                };
                return Err(ProviderError::new(kind, msg));
            }
            _ => {}
        }
        Ok(out)
    }

    pub(crate) fn finish(&mut self) -> Vec<StreamEvent> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ResponseBuilder, ToolCall, ToolSpec};

    #[test]
    fn history_becomes_input_items() {
        let req = ChatRequest {
            model: "gpt-oss".into(),
            messages: vec![
                Message::System {
                    content: "sys".into(),
                },
                Message::User {
                    content: "hi".into(),
                },
                Message::Assistant {
                    content: "Looking.".into(),
                    reasoning: None,
                    tool_calls: vec![ToolCall {
                        id: "c1".into(),
                        name: "shell".into(),
                        arguments: "{\"command\":[\"ls\"]}".into(),
                    }],
                },
                Message::Tool {
                    call_id: "c1".into(),
                    content: "a b".into(),
                },
            ],
            tools: vec![ToolSpec {
                name: "shell".into(),
                description: "d".into(),
                parameters: json!({"type":"object"}),
            }],
            reasoning_effort: Some("high".into()),
            ..ChatRequest::default()
        };
        let b = build_body(&req);
        assert_eq!(b["instructions"], "sys");
        assert_eq!(b["store"], false);
        assert_eq!(b["reasoning"]["effort"], "high");
        let input = b["input"].as_array().expect("input");
        assert_eq!(input.len(), 4);
        assert_eq!(input[2]["type"], "function_call");
        assert_eq!(input[3]["type"], "function_call_output");
        assert_eq!(input[3]["output"], "a b");
        assert_eq!(b["tools"][0]["type"], "function");
        assert_eq!(b["tools"][0]["name"], "shell");
    }

    #[test]
    fn parses_a_stream() {
        let mut p = EventParser::default();
        let mut b = ResponseBuilder::default();
        for d in [
            r#"{"type":"response.created","response":{}}"#,
            r#"{"type":"response.reasoning_text.delta","output_index":0,"delta":"think"}"#,
            r#"{"type":"response.output_text.delta","output_index":1,"delta":"Hi"}"#,
            r#"{"type":"response.output_item.added","output_index":2,"item":{"type":"function_call","call_id":"c9","name":"shell","arguments":""}}"#,
            r#"{"type":"response.function_call_arguments.delta","output_index":2,"delta":"{\"command\":"}"#,
            r#"{"type":"response.function_call_arguments.delta","output_index":2,"delta":"[\"ls\"]}"}"#,
            r#"{"type":"response.function_call_arguments.done","output_index":2,"arguments":"{\"command\":[\"ls\"]}"}"#,
            r#"{"type":"response.output_item.added","output_index":3,"item":{"type":"function_call","call_id":"c10","name":"shell"}}"#,
            r#"{"type":"response.function_call_arguments.done","output_index":3,"arguments":"{}"}"#,
            r#"{"type":"response.completed","response":{"usage":{"input_tokens":50,"output_tokens":7,"input_tokens_details":{"cached_tokens":40}}}}"#,
        ] {
            for e in p.parse(d).expect("parses") {
                b.push(&e);
            }
        }
        assert_eq!(b.finish, Some(FinishReason::ToolCalls));
        assert_eq!(
            b.usage,
            Some(Usage {
                prompt_tokens: 50,
                completion_tokens: 7,
                cached_prompt_tokens: Some(40)
            })
        );
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
            ("Hi", Some("think"))
        );
        assert_eq!(tool_calls.len(), 2);
        assert_eq!(tool_calls[0].arguments, "{\"command\":[\"ls\"]}");
        assert_eq!(
            (tool_calls[1].id.as_str(), tool_calls[1].arguments.as_str()),
            ("c10", "{}")
        );
    }

    #[test]
    fn incomplete_and_failed() {
        let mut p = EventParser::default();
        let ev = p
            .parse(r#"{"type":"response.incomplete","response":{"incomplete_details":{"reason":"max_output_tokens"}}}"#)
            .expect("parses");
        assert_eq!(ev, [StreamEvent::Finish(FinishReason::Length)]);
        let e = p
            .parse(r#"{"type":"response.failed","response":{"error":{"code":"context_length_exceeded","message":"too long"}}}"#)
            .expect_err("failed");
        assert_eq!(e.kind, ErrorKind::ContextLengthExceeded);
    }
}
