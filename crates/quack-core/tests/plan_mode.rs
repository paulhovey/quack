//! Plan mode: nothing changes while planning; the request asks for a plan.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{bodies, sse_call, sse_text, turn};
use quack_core::Engine;
use quack_core::protocol::{Event, Op};
use wiremock::matchers::{body_string_contains, method};
use wiremock::{Mock, MockServer};

#[tokio::test]
async fn plan_mode_refuses_edits_and_asks_for_a_plan() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_string_contains("plan mode: no changes"))
        .respond_with(sse_text("1. Write notes.txt"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(sse_call(
            "c1",
            "write",
            serde_json::json!({"path": "notes.txt", "content": "hi"}),
        ))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().expect("tempdir");
    let loaded = common::workspace(tmp.path(), &server.uri(), r#"["tiny"]"#, "");
    let ws = loaded.dirs.workspace_root.clone();
    let mut engine = Engine::spawn(Arc::new(loaded));
    engine
        .submit(Op::SetPermissionMode {
            mode: "plan".into(),
        })
        .await;
    let mode = loop {
        let e = tokio::time::timeout(Duration::from_secs(10), engine.events.recv())
            .await
            .expect("event")
            .expect("open");
        if let Event::PermissionMode { mode } = e
            && mode == "plan"
        {
            break mode;
        }
    };
    assert_eq!(mode, "plan");

    turn(&mut engine, "add notes").await;
    assert!(!ws.join("notes.txt").exists());
    let first = bodies(&server).await.remove(0);
    assert!(first.contains("add notes\\n\\n[Plan mode]"), "{first}");
    engine.shutdown().await;
}
