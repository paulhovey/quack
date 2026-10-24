//! Provider-neutral request and stream types. Adapters translate these to and from the wire.

use quack_config::provider::{Cost, Sampling};
use quack_protocol::{Image, Usage};
use serde::{Deserialize, Serialize};

/// One request to a model. The profile layer shapes it before an adapter serializes it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
    /// The window the session plans for. Servers that must be told (Ollama) get it.
    pub context_window: Option<u32>,
    pub max_tokens: Option<u32>,
    pub sampling: Sampling,
    pub reasoning_effort: Option<String>,
    /// Send earlier reasoning back with each assistant message, where the server takes it
    /// (Mistral). `None`: the server's convention.
    pub preserve_reasoning: Option<bool>,
    /// Merged into the request body last; objects merge key by key.
    pub extra_body: serde_json::Map<String, serde_json::Value>,
    /// A stable id for the conversation, for servers that route by it to keep their prompt
    /// cache warm (OpenAI's `prompt_cache_key`).
    pub cache_key: Option<String>,
    /// Tool calling mode (e.g. grammar / guided decoding).
    pub tool_mode: Option<quack_config::provider::ToolCallMode>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum Message {
    System {
        content: String,
    },
    User {
        content: String,
        /// Images sent with the message (pasted, attached, or from a tool the profile
        /// turned into input); sent by adapters that support vision.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<Image>,
    },
    Assistant {
        #[serde(default)]
        content: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tool_calls: Vec<ToolCall>,
        /// The reply as the server produced it, with the reasoning it asks to get back
        /// unchanged (Anthropic's signed thinking, OpenAI's encrypted reasoning).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sealed: Option<Sealed>,
    },
    Tool {
        call_id: String,
        content: String,
        /// Images the tool returned (`view_image`); sent where the protocol takes them,
        /// next to or after the text.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<Image>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// Raw JSON text, exactly as the model produced it.
    pub arguments: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema of the arguments object.
    pub parameters: serde_json::Value,
}

/// A reply's layout and opaque reasoning, replayed to the API that produced it. `items` is
/// the adapter's own format; text and tool calls stay in the message, and items refer to
/// tool calls by position.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sealed {
    /// The provider id and model that produced it.
    pub provider: String,
    pub model: String,
    /// [`fingerprint`] of the request that produced it. Signed reasoning is tied to the
    /// system prompt and tools it was made under, so it is not sent once they change.
    pub fingerprint: String,
    pub items: Vec<serde_json::Value>,
}

/// A short stable hash of a request's system prompt and tools (FNV-1a, hex).
pub fn fingerprint(req: &ChatRequest) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |bytes: &[u8]| {
        for b in bytes {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    };
    if let Some(Message::System { content }) = req.messages.first() {
        feed(content.as_bytes());
    }
    for t in &req.tools {
        feed(&[0]);
        feed(t.name.as_bytes());
        feed(t.description.as_bytes());
        feed(t.parameters.to_string().as_bytes());
    }
    format!("{h:016x}")
}

/// Remove sealed replies from a history. Callers do this after editing earlier messages
/// (pruning, compaction): signed reasoning after the edit would no longer verify.
pub fn unseal(history: &mut [Message]) {
    for m in history {
        if let Message::Assistant { sealed, .. } = m {
            *sealed = None;
        }
    }
}

/// Merge `from` into `into`: objects key by key, everything else replaced.
pub(crate) fn merge(
    into: &mut serde_json::Value,
    from: &serde_json::Map<String, serde_json::Value>,
) {
    let Some(into) = into.as_object_mut() else {
        return;
    };
    for (k, v) in from {
        if let (Some(a @ serde_json::Value::Object(_)), serde_json::Value::Object(b)) =
            (into.get_mut(k), v)
        {
            merge(a, b);
            continue;
        }
        into.insert(k.clone(), v.clone());
    }
}

/// A normalized streaming event.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    TextDelta(String),
    ReasoningDelta(String),
    ToolCallStart {
        index: u32,
        id: Option<String>,
        name: String,
    },
    ToolCallArgsDelta {
        index: u32,
        fragment: String,
    },
    Usage(Usage),
    Finish(FinishReason),
    /// The reply's layout and opaque reasoning, sent before `Finish`.
    Sealed(Vec<serde_json::Value>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FinishReason {
    Stop,
    Length,
    ToolCalls,
    ContentFilter,
    Other(String),
}

impl FinishReason {
    pub fn parse(s: &str) -> Self {
        match s {
            "stop" | "eos" | "end_turn" => Self::Stop,
            "length" | "max_tokens" | "model_context_window_exceeded" => Self::Length,
            "tool_calls" | "function_call" | "tool_use" => Self::ToolCalls,
            "content_filter" | "refusal" => Self::ContentFilter,
            other => Self::Other(other.to_owned()),
        }
    }
}

/// Folds stream events into the final assistant message.
#[derive(Debug, Default)]
pub struct ResponseBuilder {
    content: String,
    reasoning: String,
    calls: Vec<(u32, ToolCall)>,
    sealed: Vec<serde_json::Value>,
    pub usage: Option<Usage>,
    pub finish: Option<FinishReason>,
}

impl ResponseBuilder {
    pub fn push(&mut self, ev: &StreamEvent) {
        match ev {
            StreamEvent::TextDelta(t) => self.content.push_str(t),
            StreamEvent::ReasoningDelta(t) => self.reasoning.push_str(t),
            StreamEvent::ToolCallStart { index, id, name } => {
                let n = self.calls.len();
                self.calls.push((
                    *index,
                    ToolCall {
                        id: id.clone().unwrap_or_else(|| format!("call_{n}")),
                        name: name.clone(),
                        arguments: String::new(),
                    },
                ));
            }
            StreamEvent::ToolCallArgsDelta { index, fragment } => {
                if let Some((_, c)) = self.calls.iter_mut().rev().find(|(i, _)| i == index) {
                    c.arguments.push_str(fragment);
                }
            }
            StreamEvent::Usage(u) => self.usage = Some(*u),
            StreamEvent::Finish(f) => self.finish = Some(f.clone()),
            StreamEvent::Sealed(items) => self.sealed.clone_from(items),
        }
    }

    pub fn has_tool_calls(&self) -> bool {
        !self.calls.is_empty()
    }

    pub fn into_message(self) -> Message {
        Message::Assistant {
            content: self.content,
            reasoning: Some(self.reasoning).filter(|r| !r.is_empty()),
            tool_calls: self.calls.into_iter().map(|(_, c)| c).collect(),
            sealed: (!self.sealed.is_empty()).then(|| Sealed {
                provider: String::new(),
                model: String::new(),
                fingerprint: String::new(),
                items: self.sealed,
            }),
        }
    }
}

/// A model reported by the provider's models endpoint.
#[derive(Debug, Clone, PartialEq)]
pub struct DiscoveredModel {
    pub id: String,
    pub context_window: Option<u32>,
    pub max_output: Option<u32>,
    /// Prices, when the endpoint lists them.
    pub cost: Option<Cost>,
}
