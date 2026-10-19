//! `[tools] edit_format`: the model gets the chosen edit tool only, multi-file calls change
//! every file, and `/undo` puts all of them back.

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use common::{bodies, sse_call, sse_text, turn};
use quack_core::protocol::{Event, Op};
use quack_core::{Engine, EngineHandle};
use serde_json::{Value, json};
use wiremock::matchers::{body_string_contains, method};
use wiremock::{Mock, MockServer};

async fn engine(root: &Path, server: &MockServer, format: &str) -> EngineHandle {
    std::fs::create_dir_all(root.join("user")).expect("mkdir");
    std::fs::write(
        root.join("user").join("config.toml"),
        format!("[tools]\nedit_format = \"{format}\"\n[permissions]\nmode = \"full-auto\"\n"),
    )
    .expect("config");
    let loaded = common::workspace(root, &server.uri(), r#"["tiny"]"#, "");
    Engine::spawn(Arc::new(loaded))
}

async fn reply_with_call(server: &MockServer, name: &str, args: Value) {
    Mock::given(method("POST"))
        .and(body_string_contains("\"role\":\"tool\""))
        .respond_with(sse_text("done"))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .respond_with(sse_call("c1", name, args))
        .up_to_n_times(1)
        .mount(server)
        .await;
}

fn tool_names(body: &str) -> Vec<String> {
    let v: Value = serde_json::from_str(body).expect("json");
    v["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .filter_map(|t| t["function"]["name"].as_str().map(str::to_owned))
        .collect()
}

#[tokio::test]
async fn apply_patch_changes_several_files_and_undo_restores_them() {
    let server = MockServer::start().await;
    let patch = "*** Begin Patch\n*** Update File: a.txt\n one\n-two\n+TWO\n three\n\
                 *** Delete File: b.txt\n*** Add File: sub/c.txt\n+new\n*** End Patch";
    reply_with_call(&server, "apply_patch", json!({ "patch": patch })).await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let ws = tmp.path().join("ws");
    let mut engine = engine(tmp.path(), &server, "apply_patch").await;
    std::fs::write(ws.join("a.txt"), "one\r\ntwo\r\nthree\r\n").expect("a");
    std::fs::write(ws.join("b.txt"), "bye\n").expect("b");

    turn(&mut engine, "change them").await;
    let first = bodies(&server).await.remove(0);
    let names = tool_names(&first);
    assert!(names.contains(&"apply_patch".to_owned()), "{names:?}");
    assert!(!names.contains(&"edit".to_owned()), "{names:?}");
    // Line endings are kept.
    assert_eq!(
        std::fs::read_to_string(ws.join("a.txt")).expect("a"),
        "one\r\nTWO\r\nthree\r\n"
    );
    assert!(!ws.join("b.txt").exists());
    assert_eq!(
        std::fs::read_to_string(ws.join("sub").join("c.txt")).expect("c"),
        "new\n"
    );
    let result = bodies(&server).await.pop().expect("second request");
    assert!(result.contains("Edited a.txt"), "{result}");

    engine
        .submit(Op::Undo {
            checkpoint: None,
            force: false,
        })
        .await;
    let mut restored = loop {
        let e = tokio::time::timeout(Duration::from_secs(10), engine.events.recv())
            .await
            .expect("event")
            .expect("open");
        if let Event::FilesRestored {
            restored,
            conflicts,
        } = e
        {
            assert!(conflicts.is_empty(), "{conflicts:?}");
            break restored;
        }
    };
    restored.sort();
    assert_eq!(restored, ["a.txt", "b.txt", "sub/c.txt"]);
    assert_eq!(
        std::fs::read_to_string(ws.join("a.txt")).expect("a"),
        "one\r\ntwo\r\nthree\r\n"
    );
    assert_eq!(
        std::fs::read_to_string(ws.join("b.txt")).expect("b"),
        "bye\n"
    );
    assert!(!ws.join("sub").join("c.txt").exists());
    engine.shutdown().await;
}

#[tokio::test]
async fn edit_blocks_edit_two_files_in_one_call() {
    let server = MockServer::start().await;
    let blocks = "a.txt\n<<<<<<< SEARCH\ntwo\n=======\n2\n>>>>>>> REPLACE\n\n\
                  b.txt\n<<<<<<< SEARCH\nbye\n=======\nhello\n>>>>>>> REPLACE\n";
    reply_with_call(&server, "edit_blocks", json!({ "blocks": blocks })).await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let ws = tmp.path().join("ws");
    let mut engine = engine(tmp.path(), &server, "search_replace_blocks").await;
    std::fs::write(ws.join("a.txt"), "one\ntwo\n").expect("a");
    std::fs::write(ws.join("b.txt"), "bye\n").expect("b");

    turn(&mut engine, "change them").await;
    let names = tool_names(&bodies(&server).await.remove(0));
    assert!(names.contains(&"edit_blocks".to_owned()), "{names:?}");
    assert!(!names.contains(&"edit".to_owned()), "{names:?}");
    assert_eq!(
        std::fs::read_to_string(ws.join("a.txt")).expect("a"),
        "one\n2\n"
    );
    assert_eq!(
        std::fs::read_to_string(ws.join("b.txt")).expect("b"),
        "hello\n"
    );
    engine.shutdown().await;
}

#[tokio::test]
async fn whole_file_offers_only_write() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(sse_text("ok"))
        .mount(&server)
        .await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut engine = engine(tmp.path(), &server, "whole_file").await;
    turn(&mut engine, "hi").await;
    let body = bodies(&server).await.remove(0);
    let names = tool_names(&body);
    assert!(names.contains(&"write".to_owned()), "{names:?}");
    for edit in ["edit", "edit_hashline", "apply_patch", "edit_blocks"] {
        assert!(!names.contains(&edit.to_owned()), "{names:?}");
    }
    assert!(body.contains("complete new content"), "{body}");
    engine.shutdown().await;
}
