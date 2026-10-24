//! `api = "openai-chat"`: `/chat/completions` with SSE streaming.
//! Covers vLLM, SGLang, llama.cpp, LM Studio, TGI, Ollama `/v1` and most hosted services.

use std::collections::{HashMap, HashSet};

use quack_config::provider::{Cost, ReasoningFormat};
use quack_protocol::Usage;
use serde_json::{Map, Value, json};

use crate::error::{ErrorKind, ProviderError, body_message};
use crate::reasoning::{Piece, ThinkSplitter};
use crate::types::{ChatRequest, DiscoveredModel, FinishReason, Message, StreamEvent, merge};

pub(crate) const CHAT_PATH: &str = "chat/completions";
pub(crate) const MODELS_PATH: &str = "models";

/// How a server's `/chat/completions` differs from the common shape.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum Dialect {
    #[default]
    Standard,
    /// Mistral's API: it rejects fields it doesn't know, takes `reasoning_effort` as
    /// `none` or `high`, and wants earlier reasoning sent back as `thinking` parts.
    Mistral,
}

impl Dialect {
    pub(crate) fn of(base_url: &str) -> Self {
        let host = base_url
            .split("://")
            .nth(1)
            .unwrap_or(base_url)
            .split(['/', ':'])
            .next()
            .unwrap_or_default();
        if host.eq_ignore_ascii_case("api.mistral.ai") {
            Self::Mistral
        } else {
            Self::Standard
        }
    }
}

/// How messages are written for one request.
struct Shape<'a> {
    mistral: bool,
    /// Tool-call ids as Mistral's tokenizer takes them: exactly 9 letters or digits.
    short_ids: bool,
    /// Send earlier reasoning back: as `reasoning_content`, or a `thinking` part for
    /// Mistral.
    echo_reasoning: bool,
    /// Tool names by call id, for tool results.
    names: HashMap<&'a str, &'a str>,
}

/// An image as an OpenAI-compatible content part.
fn image_part(img: &quack_protocol::Image) -> Value {
    json!({ "type": "image_url", "image_url": { "url": img.data_url() } })
}

/// Images from tool results ride in a user message after the run of tool messages:
/// many chat-completions servers take image parts only on user content, and a tool
/// message's `content` must stay a string.
fn flush_images(messages: &mut Vec<Value>, pending: &mut Vec<Value>) {
    if !pending.is_empty() {
        messages.push(json!({
            "role": "user",
            "content": Value::Array(std::mem::take(pending)),
        }));
    }
}

/// Serialize a request. Earlier reasoning is sent back, since thinking models expect
/// it across tool calls (DeepSeek rejects a tool request without it; MiniMax, Kimi and GLM
/// lose the thread). Chat templates on local servers drop what they don't use.
/// `preserve_in_history = false` turns it off.
pub(crate) fn build_body(req: &ChatRequest, dialect: Dialect) -> Value {
    let mistral = dialect == Dialect::Mistral;
    let effort = req
        .reasoning_effort
        .as_deref()
        .map(|e| if mistral { mistral_effort(e) } else { e });
    let shape = Shape {
        mistral,
        short_ids: mistral || is_mistral_large(&req.model),
        echo_reasoning: effort != Some("none") && req.preserve_reasoning != Some(false),
        names: req
            .messages
            .iter()
            .flat_map(|m| match m {
                Message::Assistant { tool_calls, .. } => tool_calls.as_slice(),
                _ => &[],
            })
            .map(|c| (c.id.as_str(), c.name.as_str()))
            .collect(),
    };
    let mut messages: Vec<Value> = Vec::with_capacity(req.messages.len() + 1);
    let mut pending: Vec<Value> = Vec::new();
    for m in &req.messages {
        match m {
            Message::Tool { images, .. } if !images.is_empty() => {
                // The text goes back as the tool result; the pixels follow as user parts.
                messages.push(message_json(m, &shape));
                pending.extend(images.iter().map(image_part));
            }
            Message::Tool { .. } => {
                // A run of tool results stays contiguous; the pixels wait for its end.
                messages.push(message_json(m, &shape));
            }
            m => {
                flush_images(&mut messages, &mut pending);
                messages.push(message_json(m, &shape));
            }
        }
    }
    flush_images(&mut messages, &mut pending);
    let mut body = Map::new();
    body.insert("model".into(), json!(req.model));
    body.insert("messages".into(), Value::Array(messages));
    body.insert("stream".into(), json!(true));
    if mistral {
        // Usage comes in the last chunk anyway; a stable key raises prompt-cache hits.
        body.insert("prompt_cache_key".into(), json!(cache_key(&req.messages)));
    } else {
        body.insert("stream_options".into(), json!({ "include_usage": true }));
    }
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
    // Mistral rejects sampling settings it doesn't have.
    let local = |v: Option<Value>| v.filter(|_| !mistral);
    let optional = [
        ("temperature", s.temperature.map(Value::from)),
        ("top_p", s.top_p.map(Value::from)),
        ("top_k", local(s.top_k.map(Value::from))),
        ("min_p", local(s.min_p.map(Value::from))),
        (
            "repetition_penalty",
            local(s.repetition_penalty.map(Value::from)),
        ),
        ("reasoning_effort", effort.map(Value::from)),
    ];
    for (k, v) in optional {
        if let Some(v) = v {
            body.insert(k.into(), v);
        }
    }
    if req.tool_mode == Some(quack_config::provider::ToolCallMode::Grammar) {
        body.insert("response_format".into(), json!({ "type": "json_object" }));
    }
    let mut body = Value::Object(body);
    merge(&mut body, &req.extra_body);
    body
}

/// Mistral takes `none` or `high`, as Mistral's own harness maps them.
fn mistral_effort(effort: &str) -> &'static str {
    match effort {
        "none" | "off" | "minimal" | "low" => "none",
        _ => "high",
    }
}

/// Mistral Large, whose tokenizer takes only 9-character tool-call ids, on any server.
fn is_mistral_large(model: &str) -> bool {
    model.to_ascii_lowercase().contains("mistral-large")
}

/// An id Mistral accepts: kept when it already is one, otherwise derived from it, so the
/// call and its result still match.
fn short_id(id: &str) -> String {
    if id.len() == 9 && id.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return id.to_owned();
    }
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
    let mut n = fnv1a(id.as_bytes());
    (0..9)
        .map(|_| {
            let c = DIGITS[(n % 62) as usize] as char;
            n /= 62;
            c
        })
        .collect()
}

/// The same for every request of a session: the system prompt and the first user message.
fn cache_key(messages: &[Message]) -> String {
    let mut bytes = Vec::new();
    for m in messages.iter().take(2) {
        if let Message::System { content } | Message::User { content, .. } = m {
            bytes.extend_from_slice(content.as_bytes());
            bytes.push(0);
        }
    }
    format!("ch-{:016x}", fnv1a(&bytes))
}

/// FNV-1a: stable across builds, unlike `std`'s hasher.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

fn message_json(m: &Message, shape: &Shape<'_>) -> Value {
    let id = |id: &str| {
        if shape.short_ids {
            short_id(id)
        } else {
            id.to_owned()
        }
    };
    match m {
        Message::System { content } => json!({ "role": "system", "content": content }),
        Message::User { content, images } => {
            if images.is_empty() {
                json!({ "role": "user", "content": content })
            } else {
                let mut parts: Vec<Value> = images.iter().map(image_part).collect();
                if !content.is_empty() {
                    parts.push(json!({ "type": "text", "text": content }));
                }
                json!({ "role": "user", "content": parts })
            }
        }
        Message::Assistant {
            content,
            reasoning,
            tool_calls,
            ..
        } => {
            let mut v = json!({ "role": "assistant", "content": content });
            let echo = reasoning
                .as_deref()
                .filter(|r| shape.echo_reasoning && !r.is_empty());
            if let Some(r) = echo.filter(|_| !shape.mistral) {
                v["reasoning_content"] = json!(r);
            }
            if let Some(r) = echo.filter(|_| shape.mistral) {
                let mut parts = vec![json!({
                    "type": "thinking",
                    "thinking": [{ "type": "text", "text": r }],
                })];
                if !content.is_empty() {
                    parts.push(json!({ "type": "text", "text": content }));
                }
                v["content"] = Value::Array(parts);
            }
            if !tool_calls.is_empty() {
                // Some servers reject an empty string next to tool calls.
                if content.is_empty() && !shape.mistral {
                    v["content"] = Value::Null;
                }
                v["tool_calls"] = tool_calls
                    .iter()
                    .map(|c| {
                        json!({
                            "id": id(&c.id),
                            "type": "function",
                            "function": { "name": c.name, "arguments": c.arguments },
                        })
                    })
                    .collect();
            }
            v
        }
        Message::Tool {
            call_id, content, ..
        } => {
            let mut v = json!({ "role": "tool", "tool_call_id": id(call_id), "content": content });
            if shape.mistral
                && let Some(name) = shape.names.get(call_id.as_str())
            {
                v["name"] = json!(name);
            }
            v
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

    fn text(&mut self, text: &str, out: &mut Vec<StreamEvent>) {
        match &mut self.think {
            Some(t) => out.extend(t.feed(text).into_iter().map(piece_event)),
            None => out.push(StreamEvent::TextDelta(text.to_owned())),
        }
    }

    fn delta(&mut self, d: &Value, out: &mut Vec<StreamEvent>) {
        for key in ["reasoning_content", "reasoning"] {
            if let Some(r) = d.get(key).and_then(Value::as_str).filter(|r| !r.is_empty()) {
                out.push(StreamEvent::ReasoningDelta(r.to_owned()));
            }
        }
        match d.get("content") {
            Some(Value::String(text)) if !text.is_empty() => self.text(text, out),
            // Mistral sends content as parts: `text`, and `thinking` holding text parts.
            Some(Value::Array(parts)) => {
                for part in parts {
                    let text = |v: &Value| v.get("text").and_then(Value::as_str).map(str::to_owned);
                    match part.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            if let Some(t) = text(part).filter(|t| !t.is_empty()) {
                                self.text(&t, out);
                            }
                        }
                        Some("thinking") => {
                            let r: String = match part.get("thinking") {
                                Some(Value::Array(inner)) => {
                                    inner.iter().filter_map(text).collect()
                                }
                                Some(Value::String(t)) => t.clone(),
                                _ => String::new(),
                            };
                            if !r.is_empty() {
                                out.push(StreamEvent::ReasoningDelta(r));
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
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

pub(crate) fn piece_event(p: Piece) -> StreamEvent {
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
        cache_write_tokens: None,
    }
}

/// llama.cpp's server info, at the server root rather than under `/v1`.
pub(crate) const LLAMA_CPP_PROPS_PATH: &str = "props";

/// A `/models` response from llama.cpp's `llama-server` (`owned_by: "llamacpp"`).
pub(crate) fn is_llama_cpp(body: &Value) -> bool {
    body.get("data")
        .and_then(Value::as_array)
        .is_some_and(|d| d.iter().any(|m| m["owned_by"] == "llamacpp"))
}

/// The window llama.cpp actually serves (`-c`, per slot), which `/models` doesn't report:
/// there `meta.n_ctx_train` is the trained window, often far larger.
pub(crate) fn llama_cpp_window(props: &Value) -> Option<u32> {
    props
        .pointer("/default_generation_settings/n_ctx")
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .filter(|&n| n > 0)
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
        // A ChatGPT sign-in's list marks the models it doesn't offer.
        .filter(|m| m["visibility"].as_str().is_none_or(|v| v == "list"))
        .filter_map(|m| {
            let id = m
                .get("id")
                .or_else(|| m.get("slug"))
                .or_else(|| m.get("name"))?
                .as_str()?
                .to_owned();
            Some(DiscoveredModel {
                id,
                context_window: num(
                    m,
                    &[
                        "/context_window",
                        "/max_model_len",
                        "/context_length",
                        "/max_context_length",
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
                cost: m.get("pricing").and_then(pricing),
            })
        })
        .collect()
}

/// Prices from a model's `pricing`, in dollars per million tokens. Two shapes are common:
/// numbers per million (`input`, `output`, `cache_hit`), and OpenRouter's strings per
/// token (`prompt`, `completion`, `input_cache_read`). All zero means not priced.
fn pricing(p: &Value) -> Option<Cost> {
    let per_million = |keys: &[&str]| keys.iter().find_map(|k| p.get(*k)?.as_f64());
    let per_token = |key: &str| {
        let v = p.get(key)?;
        let n = v
            .as_str()
            .map_or_else(|| v.as_f64(), |s| s.trim().parse().ok())?;
        Some(n * 1_000_000.0)
    };
    let cost = match (per_million(&["input"]), per_million(&["output"])) {
        (Some(input), Some(output)) => Cost {
            input,
            output,
            cached_input: per_million(&["cache_hit", "cached_input", "cache_read"]),
            cache_write: None,
        },
        _ => Cost {
            input: per_token("prompt")?,
            output: per_token("completion")?,
            cached_input: per_token("input_cache_read"),
            cache_write: None,
        },
    };
    let valid = |n: f64| n.is_finite() && n >= 0.0;
    (valid(cost.input)
        && valid(cost.output)
        && cost.cached_input.is_none_or(valid)
        && cost.input + cost.output > 0.0)
        .then_some(cost)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ResponseBuilder, ToolCall, ToolSpec};
    use quack_config::provider::Sampling;

    #[test]
    fn llama_cpp_serves_less_than_it_was_trained_on() {
        let models = serde_json::json!({"data":[{"id":"/models/q.gguf","owned_by":"llamacpp","meta":{"n_ctx_train":262144}}]});
        assert!(is_llama_cpp(&models));
        assert_eq!(parse_models(&models)[0].context_window, Some(262_144));
        let props = serde_json::json!({"default_generation_settings":{"n_ctx":16384}});
        assert_eq!(llama_cpp_window(&props), Some(16384));
        assert!(!is_llama_cpp(
            &serde_json::json!({"data":[{"id":"m","owned_by":"vllm"}]})
        ));
    }

    #[test]
    fn prices_come_from_either_pricing_shape() {
        let models = json!({"data":[
            {"id":"hyper","pricing":{"input":0.33,"output":1.31,"cache_create":0,"cache_hit":0.03}},
            {"id":"router","pricing":{"prompt":"0.0000003","completion":"0.0000012","input_cache_read":"0.00000003"}},
            {"id":"no-cache","pricing":{"input":1,"output":2}},
            {"id":"free","pricing":{"prompt":"0","completion":"0"}},
            {"id":"none"},
        ]});
        let cost: Vec<_> = parse_models(&models).into_iter().map(|m| m.cost).collect();
        let near = |a: f64, b: f64| (a - b).abs() < 1e-9;
        let hyper = cost[0].as_ref().expect("hyper priced");
        assert!(near(hyper.input, 0.33) && near(hyper.output, 1.31));
        assert!(near(hyper.cached_input.expect("cache hit"), 0.03));
        let router = cost[1].as_ref().expect("openrouter priced");
        assert!(near(router.input, 0.3) && near(router.output, 1.2));
        assert!(near(router.cached_input.expect("cache read"), 0.03));
        assert_eq!(cost[2].as_ref().and_then(|c| c.cached_input), None);
        assert_eq!(cost[3], None, "all zero is not a price");
        assert_eq!(cost[4], None);
    }

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
                    sealed: None,
                },
                Message::Tool {
                    call_id: "c1".into(),
                    content: "ok".into(),
                    images: Vec::new(),
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
        let b = build_body(&req, Dialect::Standard);
        assert_eq!(b["messages"][1]["content"], Value::Null);
        assert_eq!(
            b["messages"][1]["tool_calls"][0]["function"]["name"],
            "read"
        );
        assert_eq!(b["messages"][1]["reasoning_content"], "hidden");
        assert_eq!(b["messages"][2]["tool_call_id"], "c1");
        assert_eq!(b["tools"][0]["function"]["name"], "read");
        assert_eq!(b["max_tokens"], 64);
        assert_eq!(b["chat_template_kwargs"]["enable_thinking"], false);
        assert!(b.get("temperature").is_none());
    }

    fn img() -> quack_protocol::Image {
        quack_protocol::Image {
            media_type: "image/png".into(),
            data: "eHk=".into(),
            width: Some(2),
            height: Some(1),
        }
    }

    #[test]
    fn user_and_tool_images_take_their_places() {
        let req = ChatRequest {
            model: "m".into(),
            messages: vec![
                Message::User {
                    content: "go".into(),
                    images: vec![img()],
                },
                Message::Assistant {
                    content: String::new(),
                    reasoning: None,
                    tool_calls: vec![
                        ToolCall {
                            id: "c1".into(),
                            name: "view_image".into(),
                            arguments: "{}".into(),
                        },
                        ToolCall {
                            id: "c2".into(),
                            name: "read".into(),
                            arguments: "{}".into(),
                        },
                    ],
                    sealed: None,
                },
                Message::Tool {
                    call_id: "c1".into(),
                    content: "[image: pic.png]".into(),
                    images: vec![img()],
                },
                Message::Tool {
                    call_id: "c2".into(),
                    content: "text".into(),
                    images: Vec::new(),
                },
                Message::User {
                    content: "and?".into(),
                    images: Vec::new(),
                },
            ],
            ..ChatRequest::default()
        };
        let b = build_body(&req, Dialect::Standard);
        let m = b["messages"].as_array().expect("messages");
        assert_eq!(m.len(), 6, "{m:?}");
        // Attached images become parts ahead of the user message's text.
        assert_eq!(
            m[0]["content"],
            json!([
                {"type": "image_url", "image_url": {"url": "data:image/png;base64,eHk="}},
                {"type": "text", "text": "go"},
            ])
        );
        // The captioned result stays a plain tool string...
        assert_eq!(m[2]["role"], "tool");
        assert_eq!(m[2]["content"], "[image: pic.png]");
        // ...nothing is inserted inside the run of tool results...
        assert_eq!(m[3]["role"], "tool");
        // ...and the pixels follow as their own user message.
        assert_eq!(m[4]["role"], "user");
        assert_eq!(
            m[4]["content"],
            json!([{"type": "image_url", "image_url": {"url": "data:image/png;base64,eHk="}}])
        );
        assert_eq!(m[5]["content"], "and?");
    }

    fn mistral_history() -> ChatRequest {
        ChatRequest {
            model: "mistral-large-4".into(),
            messages: vec![
                Message::System {
                    content: "sys".into(),
                },
                Message::User {
                    content: "fix it".into(),
                    images: Vec::new(),
                },
                Message::Assistant {
                    content: String::new(),
                    reasoning: Some("read first".into()),
                    tool_calls: vec![ToolCall {
                        id: "call_0".into(),
                        name: "read".into(),
                        arguments: "{}".into(),
                    }],
                    sealed: None,
                },
                Message::Tool {
                    call_id: "call_0".into(),
                    content: "fn main() {}".into(),
                    images: Vec::new(),
                },
            ],
            sampling: Sampling {
                temperature: Some(0.3),
                top_k: Some(20),
                min_p: Some(0.05),
                ..Sampling::default()
            },
            reasoning_effort: Some("medium".into()),
            ..ChatRequest::default()
        }
    }

    #[test]
    fn mistral_api_shape() {
        assert_eq!(Dialect::of("https://api.mistral.ai/v1"), Dialect::Mistral);
        assert_eq!(
            Dialect::of("https://openrouter.ai/api/v1"),
            Dialect::Standard
        );
        let req = mistral_history();
        let b = build_body(&req, Dialect::Mistral);
        // Only fields Mistral knows.
        assert!(b.get("stream_options").is_none());
        assert!(b.get("top_k").is_none() && b.get("min_p").is_none());
        assert_eq!(b["temperature"], 0.3);
        assert_eq!(b["reasoning_effort"], "high");
        assert_eq!(
            b["prompt_cache_key"],
            build_body(&req, Dialect::Mistral)["prompt_cache_key"]
        );
        // Reasoning goes back as a thinking part; the empty answer stays a string part list.
        let a = &b["messages"][2];
        assert_eq!(
            a["content"],
            json!([{"type": "thinking", "thinking": [{"type": "text", "text": "read first"}]}])
        );
        let id = a["tool_calls"][0]["id"].as_str().expect("id");
        assert_eq!(id.len(), 9);
        assert!(id.bytes().all(|c| c.is_ascii_alphanumeric()), "{id}");
        assert_eq!(b["messages"][3]["tool_call_id"], id);
        assert_eq!(b["messages"][3]["name"], "read");

        // `none` turns thinking off, and with it the echo.
        let mut off = mistral_history();
        off.reasoning_effort = Some("low".into());
        let b = build_body(&off, Dialect::Mistral);
        assert_eq!(b["reasoning_effort"], "none");
        assert_eq!(b["messages"][2]["content"], "");
        let mut kept_out = mistral_history();
        kept_out.preserve_reasoning = Some(false);
        assert_eq!(
            build_body(&kept_out, Dialect::Mistral)["messages"][2]["content"],
            ""
        );
    }

    #[test]
    fn mistral_large_elsewhere_gets_short_ids_only() {
        // For example vLLM serving the open weights: standard fields, Mistral's ids.
        let b = build_body(&mistral_history(), Dialect::Standard);
        assert_eq!(b["stream_options"]["include_usage"], true);
        assert_eq!(b["reasoning_effort"], "medium");
        assert_eq!(b["messages"][2]["content"], Value::Null);
        assert_eq!(
            b["messages"][2]["tool_calls"][0]["id"]
                .as_str()
                .map(str::len),
            Some(9)
        );
        assert!(b["messages"][3].get("name").is_none());
        assert_eq!(short_id("abcDEF123"), "abcDEF123");
        assert_ne!(short_id("call_0"), short_id("call_1"));
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
                cached_prompt_tokens: Some(8),
                cache_write_tokens: None,
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
                sealed: None,
            }
        );
    }

    #[test]
    fn mistral_content_parts() {
        let chunks = [
            r#"{"choices":[{"delta":{"role":"assistant","content":[{"type":"thinking","thinking":[{"type":"text","text":"check "}]}]}}]}"#,
            r#"{"choices":[{"delta":{"content":[{"type":"thinking","thinking":[{"type":"text","text":"the file"}]}]}}]}"#,
            r#"{"choices":[{"delta":{"content":[{"type":"text","text":"Done."}]}}]}"#,
            r#"{"choices":[{"delta":{"content":""},"finish_reason":"stop"}]}"#,
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
        assert_eq!(
            b.into_message(),
            Message::Assistant {
                content: "Done.".into(),
                reasoning: Some("check the file".into()),
                tool_calls: vec![],
                sealed: None,
            }
        );
        let models = parse_models(
            &serde_json::json!({"data": [{"id": "mistral-large-4", "max_context_length": 1048576}]}),
        );
        assert_eq!(models[0].context_window, Some(1_048_576));
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
