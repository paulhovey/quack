//! The Anthropic Messages API keeps a reply's signed thinking only while the history before
//! it is unchanged. Each request must extend the one before it: thinking blocks go back
//! with their signatures, the plan reminder stays where it was first sent, and a resumed
//! session sends the same history.

mod common;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use common::{turn, workspace};
use quack_core::{Engine, Resume};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

/// Answers with the next reply in order.
struct Script(Mutex<VecDeque<ResponseTemplate>>);

impl Respond for Script {
    fn respond(&self, _: &Request) -> ResponseTemplate {
        self.0
            .lock()
            .expect("lock")
            .pop_front()
            .unwrap_or_else(|| ResponseTemplate::new(500))
    }
}

/// A streamed Messages reply made of `blocks`.
fn reply(blocks: &[Value], stop: &str) -> ResponseTemplate {
    let mut events = vec![
        json!({"type":"message_start","message":{"usage":{"input_tokens":10,"output_tokens":1}}}),
    ];
    for (i, b) in blocks.iter().enumerate() {
        match b["type"].as_str() {
            Some("thinking") => {
                events.push(json!({"type":"content_block_start","index":i,"content_block":{"type":"thinking","thinking":""}}));
                events.push(json!({"type":"content_block_delta","index":i,"delta":{"type":"thinking_delta","thinking":b["thinking"]}}));
                events.push(json!({"type":"content_block_delta","index":i,"delta":{"type":"signature_delta","signature":b["signature"]}}));
            }
            Some("text") => {
                events.push(json!({"type":"content_block_start","index":i,"content_block":{"type":"text","text":""}}));
                events.push(json!({"type":"content_block_delta","index":i,"delta":{"type":"text_delta","text":b["text"]}}));
            }
            _ => {
                events.push(json!({"type":"content_block_start","index":i,"content_block":{"type":"tool_use","id":b["id"],"name":b["name"],"input":{}}}));
                events.push(json!({"type":"content_block_delta","index":i,"delta":{"type":"input_json_delta","partial_json":b["input"].to_string()}}));
            }
        }
        events.push(json!({"type":"content_block_stop","index":i}));
    }
    events.push(
        json!({"type":"message_delta","delta":{"stop_reason":stop},"usage":{"output_tokens":5}}),
    );
    events.push(json!({"type":"message_stop"}));
    let body: String = events
        .iter()
        .map(|e| format!("event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap_or("")))
        .collect();
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(body)
}

/// The request's messages without cache breakpoints, which move by design.
fn messages(body: &Value) -> Vec<Value> {
    fn strip(v: &mut Value) {
        match v {
            Value::Object(o) => {
                o.remove("cache_control");
                o.values_mut().for_each(strip);
            }
            Value::Array(a) => a.iter_mut().for_each(strip),
            _ => {}
        }
    }
    let mut m = body["messages"].clone();
    strip(&mut m);
    m.as_array().cloned().unwrap_or_default()
}

fn extends(before: &[Value], after: &[Value]) -> bool {
    after.len() > before.len() && after[..before.len()] == *before
}

#[tokio::test]
async fn signed_thinking_and_the_plan_stay_in_an_append_only_history() {
    // `cargo test -p quack-core` alone builds without the adapter.
    if !quack_providers::supports("anthropic-messages") {
        return;
    }
    let server = MockServer::start().await;
    let plan =
        json!({"items":[{"text":"read","status":"in_progress"},{"text":"fix","status":"pending"}]});
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(Script(Mutex::new(VecDeque::from([
            reply(
                &[
                    json!({"type":"thinking","thinking":"plan it","signature":"S1"}),
                    json!({"type":"tool_use","id":"t1","name":"todo","input":plan}),
                ],
                "tool_use",
            ),
            reply(
                &[
                    json!({"type":"thinking","thinking":"","signature":"S2"}),
                    json!({"type":"text","text":"Done."}),
                ],
                "end_turn",
            ),
            reply(&[json!({"type":"text","text":"Again."})], "end_turn"),
            reply(&[json!({"type":"text","text":"Third."})], "end_turn"),
        ]))))
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    let spawn = |resume| {
        let loaded = workspace(
            root,
            &server.uri(),
            "[\"claude\"]\napi = \"anthropic-messages\"",
            "",
        );
        Engine::spawn_with(Arc::new(loaded), resume)
    };
    let mut live = spawn(Resume::New);
    turn(&mut live, "go").await;
    turn(&mut live, "again").await;
    live.shutdown().await;
    let mut resumed = spawn(Resume::Latest);
    turn(&mut resumed, "third").await;
    resumed.shutdown().await;

    let bodies: Vec<Value> = server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|r| r.url.path() == "/v1/messages")
        .map(|r| serde_json::from_slice(&r.body).expect("json"))
        .collect();
    assert_eq!(bodies.len(), 4);
    let m: Vec<Vec<Value>> = bodies.iter().map(messages).collect();

    // The first reply goes back as it came: thinking with its signature, then the call.
    assert_eq!(
        m[1][1]["content"][0],
        json!({"type":"thinking","thinking":"plan it","signature":"S1"})
    );
    assert_eq!(m[1][1]["content"][1]["type"], "tool_use");
    // The plan rides on the tool result, and is still there in later requests.
    let result = m[1][2]["content"][0]["content"].as_str().expect("result");
    assert!(result.contains("[Your plan"), "{result}");
    for (i, pair) in m.windows(2).enumerate() {
        assert!(
            extends(&pair[0], &pair[1]),
            "request {} does not extend request {i}:\n{:#}\n{:#}",
            i + 1,
            json!(pair[0]),
            json!(pair[1])
        );
    }
    assert_eq!(
        m[2][3]["content"][0],
        json!({"type":"thinking","thinking":"","signature":"S2"})
    );
}
