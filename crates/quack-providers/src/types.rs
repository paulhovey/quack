//! Provider-neutral request and stream types. Adapters translate these to and from the wire.

use quack_config::provider::Sampling;
use quack_protocol::Usage;
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
    /// Merged verbatim into the request body last.
    pub extra_body: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum Message {
    System {
        content: String,
    },
    User {
        content: String,
    },
    Assistant {
        #[serde(default)]
        content: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tool_calls: Vec<ToolCall>,
    },
    Tool {
        call_id: String,
        content: String,
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
            "length" | "max_tokens" => Self::Length,
            "tool_calls" | "function_call" | "tool_use" => Self::ToolCalls,
            "content_filter" => Self::ContentFilter,
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
        }
    }
}

/// A model reported by the provider's models endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredModel {
    pub id: String,
    pub context_window: Option<u32>,
    pub max_output: Option<u32>,
}
