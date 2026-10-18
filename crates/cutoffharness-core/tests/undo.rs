//! `/undo`: edits are put back, and the model is told.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{bodies, sse_call, sse_text, turn};
use cutoffharness_core::Engine;
use cutoffharness_core::protocol::{Event, Op};
use wiremock::matchers::{body_string_contains, method};
use wiremock::{Mock, MockServer};

#[tokio::test]
async fn undo_reverts_the_last_edit_and_tells_the_model() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_string_contains("Created notes.txt"))
        .respond_with(sse_text("written"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_string_contains("first"))
        .respond_with(sse_call(
            "c1",
            "write",
            serde_json::json!({"path": "notes.txt", "content": "hi"}),
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
    let ws = loaded.dirs.workspace_root.clone();
    let mut engine = Engine::spawn(Arc::new(loaded));
    engine
        .submit(Op::SetPermissionMode {
            mode: "full-auto".into(),
        })
        .await;
    turn(&mut engine, "first").await;
    assert_eq!(
        std::fs::read_to_string(ws.join("notes.txt")).expect("written"),
        "hi"
    );

    engine
        .submit(Op::Undo {
            checkpoint: None,
            force: false,
        })
        .await;
    let restored = loop {
        let e = tokio::time::timeout(Duration::from_secs(10), engine.events.recv())
            .await
            .expect("event")
            .expect("open");
        if let Event::FilesRestored {
            restored,
            conflicts,
        } = e
        {
            assert!(conflicts.is_empty());
            break restored;
        }
    };
    assert_eq!(restored, ["notes.txt"]);
    assert!(!ws.join("notes.txt").exists());

    turn(&mut engine, "next").await;
    let last = bodies(&server).await.pop().expect("request");
    assert!(
        last.contains("[The user undid your last change to notes.txt;"),
        "{last}"
    );
    engine.shutdown().await;
}
