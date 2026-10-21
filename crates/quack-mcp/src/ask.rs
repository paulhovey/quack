//! Requests a server sends quack (docs/plan/09-mcp.md#client-capabilities): elicitation
//! (a form for the user to fill in) and sampling (a reply from the session's model).
//!
//! The manager hands them to whoever called [`McpManager::set_asker`](crate::McpManager)
//! (the engine) and waits for the answer. Without an asker, servers are told quack can't
//! do either, and forms that arrive anyway are declined.

use serde_json::{Map, Value};
use tokio::sync::oneshot;

/// A request from a server, answered on `reply`. Dropping `reply` declines a form and
/// refuses a sampling request.
#[derive(Debug)]
pub enum ServerAsk {
    /// Fill in a form (`elicitation/create`, form mode).
    Elicit {
        server: String,
        form: Form,
        reply: oneshot::Sender<Elicited>,
    },
    /// Write a reply with the session's model (`sampling/createMessage`).
    Sample {
        server: String,
        request: SampleRequest,
        reply: oneshot::Sender<Result<Sampled, String>>,
    },
}

/// What a form asks for.
#[derive(Debug, Clone, PartialEq)]
pub struct Form {
    /// Why the server asks.
    pub message: String,
    pub fields: Vec<Field>,
}

/// One field of a [`Form`]; the spec allows only flat, primitive-typed fields.
#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    /// The property name in the answer.
    pub name: String,
    /// `title`, else the name.
    pub label: String,
    pub description: String,
    pub required: bool,
    pub kind: FieldKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FieldKind {
    Text,
    Number {
        integer: bool,
    },
    Boolean,
    /// One of these values, each with a label to show.
    Choice(Vec<(String, String)>),
    /// Any number of these values.
    Choices(Vec<(String, String)>),
}

impl Field {
    /// Turn the user's text into the value this field takes.
    pub fn value(&self, text: &str) -> Result<Value, String> {
        let text = text.trim();
        let pick = |options: &[(String, String)], t: &str| {
            options
                .iter()
                .find(|(v, l)| v.eq_ignore_ascii_case(t) || l.eq_ignore_ascii_case(t))
                .map(|(v, _)| Value::String(v.clone()))
                .ok_or_else(|| {
                    let names: Vec<&str> = options.iter().map(|(_, l)| l.as_str()).collect();
                    format!("`{t}` is not one of: {}", names.join(", "))
                })
        };
        match &self.kind {
            FieldKind::Text => Ok(Value::String(text.to_owned())),
            FieldKind::Number { integer: true } => text
                .parse::<i64>()
                .map(Value::from)
                .map_err(|_| format!("`{text}` is not a whole number")),
            FieldKind::Number { integer: false } => text
                .parse::<f64>()
                .ok()
                .and_then(|n| serde_json::Number::from_f64(n).map(Value::Number))
                .ok_or_else(|| format!("`{text}` is not a number")),
            FieldKind::Boolean => match text.to_ascii_lowercase().as_str() {
                "yes" | "y" | "true" => Ok(Value::Bool(true)),
                "no" | "n" | "false" => Ok(Value::Bool(false)),
                _ => Err(format!("`{text}` is not yes or no")),
            },
            FieldKind::Choice(options) => pick(options, text),
            FieldKind::Choices(options) => text
                .split(',')
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(|t| pick(options, t))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::Array),
        }
    }
}

impl Form {
    /// Read a `requestedSchema`. Properties of a type the spec doesn't allow become text.
    pub fn from_schema(message: String, schema: &Value) -> Self {
        let required: Vec<&str> = schema["required"]
            .as_array()
            .map(|r| r.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let fields = schema["properties"]
            .as_object()
            .map(|props| {
                props
                    .iter()
                    .map(|(name, p)| Field {
                        name: name.clone(),
                        label: p["title"].as_str().unwrap_or(name).to_owned(),
                        description: p["description"].as_str().unwrap_or_default().to_owned(),
                        required: required.contains(&name.as_str()),
                        kind: kind(p),
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self { message, fields }
    }
}

fn kind(p: &Value) -> FieldKind {
    match p["type"].as_str().unwrap_or_default() {
        "boolean" => FieldKind::Boolean,
        "integer" => FieldKind::Number { integer: true },
        "number" => FieldKind::Number { integer: false },
        "array" => FieldKind::Choices(options(&p["items"])),
        _ => match options(p) {
            o if o.is_empty() => FieldKind::Text,
            o => FieldKind::Choice(o),
        },
    }
}

/// `enum` (with the older `enumNames` labels) or titled `oneOf`/`anyOf` values.
fn options(p: &Value) -> Vec<(String, String)> {
    if let Some(values) = p["enum"].as_array() {
        let names = p["enumNames"].as_array();
        return values
            .iter()
            .enumerate()
            .filter_map(|(i, v)| {
                let v = v.as_str()?.to_owned();
                let label = names
                    .and_then(|n| n.get(i))
                    .and_then(Value::as_str)
                    .map_or_else(|| v.clone(), str::to_owned);
                Some((v, label))
            })
            .collect();
    }
    p["oneOf"]
        .as_array()
        .or_else(|| p["anyOf"].as_array())
        .map(|alts| {
            alts.iter()
                .filter_map(|a| {
                    let v = a["const"].as_str()?.to_owned();
                    let label = a["title"].as_str().map_or_else(|| v.clone(), str::to_owned);
                    Some((v, label))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The user's answer to a form.
#[derive(Debug, Clone, PartialEq)]
pub enum Elicited {
    /// The filled-in fields, by property name.
    Accept(Map<String, Value>),
    /// The user said no; the server carries on without it.
    Decline,
    /// The user dismissed it; the server stops what it was doing.
    Cancel,
}

/// A sampling request, reduced to what quack sends the model: text only.
#[derive(Debug, Clone, PartialEq)]
pub struct SampleRequest {
    pub system: Option<String>,
    pub messages: Vec<SampleMessage>,
    pub max_tokens: u32,
    pub temperature: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SampleMessage {
    pub assistant: bool,
    pub text: String,
}

impl SampleRequest {
    /// Read `sampling/createMessage` params. Images, audio and tool use are refused:
    /// quack sends the model text only.
    pub fn from_params(params: &Value) -> Result<Self, String> {
        let mut messages = Vec::new();
        for m in params["messages"].as_array().into_iter().flatten() {
            let blocks = match &m["content"] {
                Value::Array(a) => a.clone(),
                one => vec![one.clone()],
            };
            let mut text = Vec::new();
            for b in blocks {
                match b["type"].as_str() {
                    Some("text") => text.push(b["text"].as_str().unwrap_or_default().to_owned()),
                    other => {
                        return Err(format!(
                            "quack sends the model text only, not `{}` content",
                            other.unwrap_or("unknown")
                        ));
                    }
                }
            }
            messages.push(SampleMessage {
                assistant: m["role"] == "assistant",
                text: text.join("\n\n"),
            });
        }
        if messages.is_empty() {
            return Err("the request has no messages".into());
        }
        if params["tools"].as_array().is_some_and(|t| !t.is_empty()) {
            return Err("quack does not offer tools to sampling requests".into());
        }
        Ok(Self {
            system: params["systemPrompt"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_owned),
            messages,
            max_tokens: params["maxTokens"]
                .as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .unwrap_or(1_024),
            temperature: params["temperature"].as_f64(),
        })
    }
}

/// The model's reply to a sampling request.
#[derive(Debug, Clone, PartialEq)]
pub struct Sampled {
    /// `provider/model`.
    pub model: String,
    pub text: String,
    /// It stopped at `max_tokens`.
    pub truncated: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn forms_read_every_field_kind() {
        let form = Form::from_schema(
            "Where to?".into(),
            &json!({"type": "object", "required": ["org"], "properties": {
                "org": {"type": "string", "title": "Organisation"},
                "count": {"type": "integer"},
                "ratio": {"type": "number"},
                "force": {"type": "boolean"},
                "env": {"type": "string", "enum": ["dev", "prod"], "enumNames": ["Dev", "Production"]},
                "size": {"type": "string", "oneOf": [{"const": "s", "title": "Small"}]},
                "tags": {"type": "array", "items": {"enum": ["a", "b"]}}
            }}),
        );
        let f = |n: &str| form.fields.iter().find(|f| f.name == n).expect("field");
        assert_eq!(f("org").label, "Organisation");
        assert!(f("org").required && !f("count").required);
        assert_eq!(f("count").value("3"), Ok(json!(3)));
        assert!(f("count").value("3.5").is_err());
        assert_eq!(f("ratio").value("0.5"), Ok(json!(0.5)));
        assert_eq!(f("force").value("Yes"), Ok(json!(true)));
        assert_eq!(f("env").value("production"), Ok(json!("prod")));
        assert!(f("env").value("staging").is_err());
        assert_eq!(f("size").value("Small"), Ok(json!("s")));
        assert_eq!(f("tags").value("a, b"), Ok(json!(["a", "b"])));
    }

    #[test]
    fn sampling_takes_text_only() {
        let r = SampleRequest::from_params(&json!({
            "messages": [
                {"role": "user", "content": {"type": "text", "text": "hi"}},
                {"role": "assistant", "content": [{"type": "text", "text": "hello"}]}
            ],
            "systemPrompt": "Be brief.", "maxTokens": 50
        }))
        .expect("text is fine");
        assert_eq!(r.system.as_deref(), Some("Be brief."));
        assert_eq!(r.max_tokens, 50);
        assert!(r.messages[1].assistant);
        let image = json!({"messages": [{"role": "user", "content":
            {"type": "image", "data": "", "mimeType": "image/png"}}], "maxTokens": 5});
        assert!(SampleRequest::from_params(&image).is_err());
    }
}
