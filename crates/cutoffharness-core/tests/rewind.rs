//! `/rewind`: files go back byte for byte (CRLF included), files created since are
//! removed, and the conversation is cut back to before the chosen message.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{bodies, sse_call, sse_text, turn};
use cutoffharness_core::protocol::{Event, Op, RewindPoint};
use cutoffharness_core::{Engine, EngineHandle};
use wiremock::matchers::{body_string_contains, method};
use wiremock::{Mock, MockServer};

async fn next<T>(engine: &mut EngineHandle, pick: impl Fn(Event) -> Option<T>) -> T {
    loop {
        let e = tokio::time::timeout(Duration::from_secs(10), engine.events.recv())
            .await
            .expect("event within 10s")
            .expect("open");
        if let Some(t) = pick(e) {
            return t;
        }
    }
}

#[tokio::test]
async fn rewind_restores_files_and_conversation() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_string_contains("second task"))
        .respond_with(sse_call(
            "c2",
            "write",
            serde_json::json!({"path": "new.txt", "content": "created"}),
        ))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_string_contains("first task"))
        .respond_with(sse_call(
            "c1",
            "edit",
            serde_json::json!({"path": "crlf.txt", "old_string": "two", "new_string": "TWO"}),
        ))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(sse_text("done"))
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().expect("tempdir");
    let loaded = common::workspace(tmp.path(), &server.uri(), r#"["tiny"]"#, "");
    let ws = loaded.dirs.workspace_root.clone();
    let original = b"one\r\ntwo\r\nthree\r\n";
    std::fs::write(ws.join("crlf.txt"), original).expect("write");
    let mut engine = Engine::spawn(Arc::new(loaded));
    engine
        .submit(Op::SetPermissionMode {
            mode: "full-auto".into(),
        })
        .await;
    turn(&mut engine, "first task").await;
    assert_ne!(std::fs::read(ws.join("crlf.txt")).expect("read"), original);
    turn(&mut engine, "second task").await;
    assert!(ws.join("new.txt").exists());

    engine.submit(Op::ListRewindPoints).await;
    let points = next(&mut engine, |e| match e {
        Event::RewindPoints { points } => Some(points),
        _ => None,
    })
    .await;
    assert_eq!(
        points,
        [
            RewindPoint {
                back: 1,
                text: "second task".into(),
                files: vec!["new.txt".into()],
            },
            RewindPoint {
                back: 2,
                text: "first task".into(),
                files: vec!["crlf.txt".into(), "new.txt".into()],
            },
        ]
    );

    engine
        .submit(Op::Rewind {
            back: 2,
            conversation: true,
            files: true,
            force: false,
        })
        .await;
    let (restored, conflicts) = next(&mut engine, |e| match e {
        Event::FilesRestored {
            restored,
            conflicts,
        } => Some((restored, conflicts)),
        _ => None,
    })
    .await;
    assert!(conflicts.is_empty(), "{conflicts:?}");
    assert_eq!(restored.len(), 2, "{restored:?}");
    let text = next(&mut engine, |e| match e {
        Event::Rewound { text, .. } => Some(text),
        _ => None,
    })
    .await;
    assert_eq!(text, "first task");
    assert_eq!(
        std::fs::read(ws.join("crlf.txt")).expect("read"),
        original,
        "byte for byte"
    );
    assert!(!ws.join("new.txt").exists());

    turn(&mut engine, "fresh start").await;
    let last = bodies(&server).await.pop().expect("request");
    assert!(!last.contains("first task"), "{last}");
    assert!(!last.contains("second task"), "{last}");
    engine.shutdown().await;
}

#[tokio::test]
async fn rewind_refuses_when_files_changed_since() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_string_contains("make it"))
        .respond_with(sse_call(
            "c1",
            "write",
            serde_json::json!({"path": "a.txt", "content": "model"}),
        ))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(sse_text("done"))
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
    turn(&mut engine, "make it").await;
    std::fs::write(ws.join("a.txt"), "user").expect("user edit");

    let rewind = |force| Op::Rewind {
        back: 1,
        conversation: true,
        files: true,
        force,
    };
    engine.submit(rewind(false)).await;
    let conflicts = next(&mut engine, |e| match e {
        Event::FilesRestored { conflicts, .. } => Some(conflicts),
        _ => None,
    })
    .await;
    assert_eq!(conflicts, ["a.txt"]);
    assert_eq!(
        std::fs::read_to_string(ws.join("a.txt")).expect("read"),
        "user"
    );

    engine.submit(rewind(true)).await;
    next(&mut engine, |e| {
        matches!(e, Event::Rewound { .. }).then_some(())
    })
    .await;
    assert!(!ws.join("a.txt").exists());
    engine.shutdown().await;
}
