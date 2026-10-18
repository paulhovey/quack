//! The `todo` plan: the model writes it, and every later step shows it again until done.

mod common;

use std::sync::Arc;

use common::{bodies, sse_call, sse_text, turn};
use quack_core::Engine;
use wiremock::matchers::{body_string_contains, method};
use wiremock::{Mock, MockServer};

#[tokio::test]
async fn the_plan_rides_along_with_each_step() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_string_contains("Plan updated"))
        .respond_with(sse_text("working on it"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_string_contains("plan it"))
        .respond_with(sse_call(
            "c1",
            "todo",
            serde_json::json!({"items": [
                {"text": "read the code", "status": "done"},
                {"text": "fix the bug", "status": "in_progress"}
            ]}),
        ))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(sse_text("ok"))
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().expect("tempdir");
    let loaded = common::workspace(tmp.path(), &server.uri(), r#"["tiny"]"#, "");
    // Default `ask` mode: the plan needs no approval.
    let mut engine = Engine::spawn(Arc::new(loaded));
    turn(&mut engine, "plan it").await;
    turn(&mut engine, "next").await;
    let sent = bodies(&server).await;
    let step2: serde_json::Value = serde_json::from_str(&sent[1]).expect("json");
    let last = step2["messages"]
        .as_array()
        .and_then(|m| m.last())
        .expect("last");
    let content = last["content"].as_str().expect("content");
    assert!(
        content.ends_with("[Your plan; update it with the todo tool as you go]\n[x] read the code\n[~] fix the bug"),
        "{content}"
    );
    // The next turn still shows it, but only once: it isn't stored in the history.
    let step3 = &sent[2];
    assert_eq!(step3.matches("[~] fix the bug").count(), 2, "{step3}");
    engine.shutdown().await;
}
