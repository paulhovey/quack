//! `openai-responses`: the OpenAI Responses API (`POST /v1/responses`), also served by
//! some gateways and by vLLM for gpt-oss (docs/plan/02-providers.md).
//!
//! Requests are stateless (`store: false`): the whole history goes in `input` every time,
//! the same way the chat adapter sends it, so prefix caching works the same. Reasoning
//! comes back encrypted; a reply with reasoning or a `phase` is kept as the server sent it
//! ([`crate::Sealed`]) and goes back in its order, so the model keeps its earlier thinking.
//!
//! [`Route::ChatgptPlan`] is the route a ChatGPT sign-in uses: no sampling or output cap,
//! and function tools inside a namespace.

use std::collections::HashSet;

use quack_protocol::Usage;
use serde_json::{Value, json};

use crate::error::{ErrorKind, ProviderError, body_message};
use crate::types::{ChatRequest, FinishReason, Message, StreamEvent, merge};

pub(crate) const RESPONSES_PATH: &str = "responses";

/// The namespace the tools go in on the ChatGPT plan route.
const NAMESPACE: &str = "workspace";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Route {
    Api,
    /// Requests billed to a ChatGPT plan (Sign in with ChatGPT).
    ChatgptPlan,
}

/// The input items of a sealed reply, with tool calls filled in from the message. `None`
/// when the message no longer matches them, so it is sent rebuilt.
fn unsealed(
    content: &str,
    calls: &[crate::ToolCall],
    items: &[Value],
    route: Route,
) -> Option<Vec<Value>> {
    let mut text = String::new();
    let mut used = 0;
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        match item["type"].as_str()? {
            "reasoning" => out.push(item.clone()),
            "message" => {
                let t = item["text"].as_str()?;
                text.push_str(t);
                let mut m = json!({ "role": "assistant", "content": t });
                if let Some(phase) = item.get("phase") {
                    m["phase"] = phase.clone();
                }
                out.push(m);
            }
            "function_call" => {
                let c = calls.get(usize::try_from(item["ref"].as_u64()?).ok()?)?;
                used += 1;
                out.push(call_item(c, route));
            }
            _ => return None,
        }
    }
    (text == content && used == calls.len()).then_some(out)
}

fn call_item(c: &crate::ToolCall, route: Route) -> Value {
    let mut item = json!({
        "type": "function_call",
        "call_id": c.id,
        "name": c.name,
        "arguments": c.arguments,
    });
    if route == Route::ChatgptPlan {
        item["namespace"] = json!(NAMESPACE);
    }
    item
}

pub(crate) fn build_body(req: &ChatRequest, route: Route) -> Value {
    let mut instructions = None;
    let mut input = Vec::new();
    for (i, m) in req.messages.iter().enumerate() {
        match m {
            Message::System { content } if i == 0 => instructions = Some(content.clone()),
            Message::System { content } => {
                input.push(json!({ "role": "developer", "content": content }));
            }
            Message::User { content, images } => {
                if images.is_empty() {
                    input.push(json!({ "role": "user", "content": content }))
                } else {
                    let mut parts: Vec<Value> = images
                        .iter()
                        .map(|i| json!({ "type": "input_image", "image_url": i.data_url() }))
                        .collect();
                    if !content.is_empty() {
                        parts.push(json!({ "type": "input_text", "text": content }));
                    }
                    input.push(json!({ "role": "user", "content": parts }))
                }
            }
            Message::Assistant {
                content,
                tool_calls,
                sealed,
                ..
            } => {
                if let Some(items) = sealed
                    .as_ref()
                    .and_then(|s| unsealed(content, tool_calls, &s.items, route))
                {
                    input.extend(items);
                    continue;
                }
                if !content.is_empty() {
                    input.push(json!({ "role": "assistant", "content": content }));
                }
                input.extend(tool_calls.iter().map(|c| call_item(c, route)));
            }
            Message::Tool {
                call_id,
                content,
                images,
            } => {
                let mut item = json!({
                    "type": "function_call_output",
                    "call_id": call_id,
                    "output": content,
                });
                if !images.is_empty() {
                    // The output takes text and image content items.
                    let mut parts = vec![json!({ "type": "input_text", "text": content })];
                    parts.extend(
                        images
                            .iter()
                            .map(|i| json!({ "type": "input_image", "image_url": i.data_url() })),
                    );
                    item["output"] = Value::Array(parts);
                }
                if route == Route::ChatgptPlan {
                    item["namespace"] = json!(NAMESPACE);
                }
                input.push(item);
            }
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
        let tools: Vec<Value> = req
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
        body["tools"] = match route {
            Route::Api => json!(tools),
            Route::ChatgptPlan => json!([{
                "type": "namespace",
                "name": NAMESPACE,
                "description": "Tools for working in the user's workspace.",
                "tools": tools,
            }]),
        };
    }
    if let Some(e) = &req.reasoning_effort {
        body["reasoning"] = json!({ "effort": e });
    }
    if let Some(k) = &req.cache_key {
        body["prompt_cache_key"] = json!(k);
    }
    // The plan route takes no output cap or sampling settings.
    if route == Route::Api {
        if let Some(n) = req.max_tokens {
            body["max_output_tokens"] = json!(n);
        }
        let s = &req.sampling;
        if let Some(t) = s.temperature {
            body["temperature"] = json!(t);
        }
        if let Some(p) = s.top_p {
            body["top_p"] = json!(p);
        }
    }
    merge(&mut body, &req.extra_body);
    body
}

/// Turns `data:` payloads into [`StreamEvent`]s.
#[derive(Debug, Default)]
pub(crate) struct EventParser {
    /// Output indexes of function calls, and those whose arguments came as deltas.
    calls: HashSet<u32>,
    streamed: HashSet<u32>,
    /// The reply's items in output order (function calls by number), and whether any
    /// needs keeping: reasoning, or a message `phase`.
    items: std::collections::BTreeMap<u32, Value>,
    numbers: std::collections::HashMap<u32, u64>,
    keep: bool,
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
        cache_write_tokens: num("/input_tokens_details/cache_write_tokens"),
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
                    self.numbers.insert(index, self.numbers.len() as u64);
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
            "response.output_item.done" => {
                let item = &v["item"];
                let kept = match item["type"].as_str() {
                    Some("reasoning") => {
                        self.keep = true;
                        // Ids refer to stored items; this adapter stores nothing.
                        let mut r = item.clone();
                        if let Some(o) = r.as_object_mut() {
                            o.remove("id");
                            o.remove("status");
                        }
                        Some(r)
                    }
                    Some("message") => {
                        let text: String = item["content"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|c| c["text"].as_str())
                            .collect();
                        let mut m = json!({ "type": "message", "text": text });
                        if let Some(phase) = item.get("phase").filter(|p| p.is_string()) {
                            self.keep = true;
                            m["phase"] = phase.clone();
                        }
                        Some(m)
                    }
                    Some("function_call") => self
                        .numbers
                        .get(&index)
                        .map(|n| json!({ "type": "function_call", "ref": n })),
                    _ => None,
                };
                if let Some(k) = kept {
                    self.items.insert(index, k);
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
                if self.keep {
                    self.keep = false;
                    let items = std::mem::take(&mut self.items).into_values().collect();
                    out.push(StreamEvent::Sealed(items));
                }
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
                } else if code == "subscription_sharing_usage_limit_exceeded" {
                    // The plan's own limit: retrying soon will not help.
                    return Err(ProviderError::new(
                        ErrorKind::BadRequest,
                        format!(
                            "{msg} · the ChatGPT plan's usage limit is reached; see Usage in \
                             ChatGPT settings, or use an API key"
                        ),
                    ));
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
                    images: Vec::new(),
                },
                Message::Assistant {
                    content: "Looking.".into(),
                    reasoning: None,
                    tool_calls: vec![ToolCall {
                        id: "c1".into(),
                        name: "shell".into(),
                        arguments: "{\"command\":[\"ls\"]}".into(),
                    }],
                    sealed: None,
                },
                Message::Tool {
                    call_id: "c1".into(),
                    content: "a b".into(),
                    images: Vec::new(),
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
        let b = build_body(&req, Route::Api);
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
    fn images_ride_user_content_and_call_outputs() {
        let img = || quack_protocol::Image {
            media_type: "image/png".into(),
            data: "eHk=".into(),
            width: Some(2),
            height: Some(1),
        };
        let req = ChatRequest {
            model: "gpt-oss".into(),
            messages: vec![
                Message::User {
                    content: "look".into(),
                    images: vec![img()],
                },
                Message::Assistant {
                    content: String::new(),
                    reasoning: None,
                    tool_calls: vec![ToolCall {
                        id: "c1".into(),
                        name: "view_image".into(),
                        arguments: "{}".into(),
                    }],
                    sealed: None,
                },
                Message::Tool {
                    call_id: "c1".into(),
                    content: "cap".into(),
                    images: vec![img()],
                },
            ],
            ..ChatRequest::default()
        };
        let b = build_body(&req, Route::Api);
        let input = b["input"].as_array().expect("input");
        assert_eq!(input.len(), 3, "{input:?}");
        // User images become content parts ahead of the text.
        assert_eq!(
            input[0]["content"],
            json!([
                {"type": "input_image", "image_url": "data:image/png;base64,eHk="},
                {"type": "input_text", "text": "look"},
            ])
        );
        // Tool-result images ride inside the function_call_output, next to the text.
        assert_eq!(
            input[2]["output"],
            json!([
                {"type": "input_text", "text": "cap"},
                {"type": "input_image", "image_url": "data:image/png;base64,eHk="},
            ])
        );
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
                cached_prompt_tokens: Some(40),
                cache_write_tokens: None,
            })
        );
        let Message::Assistant {
            content,
            reasoning,
            tool_calls,
            ..
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
    fn reasoning_and_phase_are_sealed_and_replayed() {
        let mut p = EventParser::default();
        let mut b = ResponseBuilder::default();
        for d in [
            r#"{"type":"response.output_item.done","output_index":0,"item":{"type":"reasoning","id":"rs_1","status":"completed","summary":[{"type":"summary_text","text":"s"}],"encrypted_content":"ENC"}}"#,
            r#"{"type":"response.output_text.delta","output_index":1,"delta":"Look"}"#,
            r#"{"type":"response.output_item.done","output_index":1,"item":{"type":"message","role":"assistant","phase":"commentary","content":[{"type":"output_text","text":"Look"}]}}"#,
            r#"{"type":"response.output_item.added","output_index":2,"item":{"type":"function_call","call_id":"c1","name":"read","namespace":"workspace"}}"#,
            r#"{"type":"response.function_call_arguments.done","output_index":2,"arguments":"{}"}"#,
            r#"{"type":"response.output_item.done","output_index":2,"item":{"type":"function_call","call_id":"c1","name":"read","arguments":"{}"}}"#,
            r#"{"type":"response.completed","response":{"usage":{"input_tokens":50,"output_tokens":7,"input_tokens_details":{"cached_tokens":30,"cache_write_tokens":15}}}}"#,
        ] {
            for e in p.parse(d).expect("parses") {
                b.push(&e);
            }
        }
        assert_eq!(b.usage.and_then(|u| u.cache_write_tokens), Some(15));
        let msg = b.into_message();
        let Message::Assistant { sealed, .. } = &msg else {
            panic!("assistant");
        };
        let items = &sealed.as_ref().expect("sealed").items;
        assert_eq!(
            items[0],
            json!({"type":"reasoning","summary":[{"type":"summary_text","text":"s"}],"encrypted_content":"ENC"})
        );
        assert_eq!(
            items[1],
            json!({"type":"message","text":"Look","phase":"commentary"})
        );
        assert_eq!(items[2], json!({"type":"function_call","ref":0}));

        let req = ChatRequest {
            model: "m".into(),
            messages: vec![
                Message::User {
                    content: "go".into(),
                    images: Vec::new(),
                },
                msg,
                Message::Tool {
                    call_id: "c1".into(),
                    content: "x".into(),
                    images: Vec::new(),
                },
            ],
            tools: vec![ToolSpec {
                name: "read".into(),
                description: "d".into(),
                parameters: json!({"type":"object"}),
            }],
            max_tokens: Some(100),
            cache_key: Some("session-1".into()),
            sampling: quack_config::provider::Sampling {
                temperature: Some(0.5),
                ..Default::default()
            },
            ..ChatRequest::default()
        };
        let api = build_body(&req, Route::Api);
        let input = api["input"].as_array().expect("input");
        assert_eq!(input[1]["encrypted_content"], "ENC");
        assert_eq!(
            input[2],
            json!({"role":"assistant","content":"Look","phase":"commentary"})
        );
        assert_eq!(input[3]["call_id"], "c1");
        assert!(input[3].get("namespace").is_none());
        assert_eq!(api["prompt_cache_key"], "session-1");
        assert_eq!(api["max_output_tokens"], 100);
        assert_eq!(api["tools"][0]["name"], "read");

        // The ChatGPT plan route: tools in a namespace, no cap or sampling.
        let plan = build_body(&req, Route::ChatgptPlan);
        assert_eq!(plan["tools"][0]["type"], "namespace");
        assert_eq!(plan["tools"][0]["tools"][0]["name"], "read");
        assert_eq!(plan["input"][3]["namespace"], NAMESPACE);
        assert_eq!(plan["input"][4]["namespace"], NAMESPACE);
        assert!(plan.get("max_output_tokens").is_none());
        assert!(plan.get("temperature").is_none());
    }

    #[test]
    fn plan_usage_limit_is_not_retried() {
        let mut p = EventParser::default();
        let e = p
            .parse(r#"{"type":"response.failed","response":{"error":{"code":"subscription_sharing_usage_limit_exceeded","message":"limit"}}}"#)
            .expect_err("failed");
        assert_eq!(e.kind, ErrorKind::BadRequest);
        assert!(e.message.contains("usage limit"));
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
