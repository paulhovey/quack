//! Pruning against a mock server: an old whole-file read keeps only its outline.

mod common;

use std::sync::Arc;

use common::{bodies, sse_call, sse_text, turn};
use quack_core::Engine;
use wiremock::matchers::{body_string_contains, method};
use wiremock::{Mock, MockServer};

#[tokio::test]
async fn an_old_read_is_replaced_by_its_outline() {
    if !quack_tools::outline::supported(std::path::Path::new("a.rs")) {
        return; // built without grammars
    }
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_string_contains("read the file"))
        .and(body_string_contains("\"role\":\"tool\""))
        .respond_with(sse_text("Read it."))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_string_contains("read the file"))
        .respond_with(sse_call(
            "c1",
            "read",
            serde_json::json!({"path": "big.rs"}),
        ))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(sse_text("ok"))
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(tmp.path().join("user")).expect("mkdir");
    // Prune before every request; reads return whole files.
    std::fs::write(
        tmp.path().join("user").join("config.toml"),
        "[context]\ncache_strategy = \"aggressive\"\n[tools]\nread_outline_tokens = 0\n",
    )
    .expect("config");
    let loaded = common::workspace(
        tmp.path(),
        &server.uri(),
        r#"[{ id = "tiny", context_window = 200000 }]"#,
        "",
    );
    let mut src = String::new();
    for i in 0..60 {
        src.push_str(&format!("pub fn f{i}() -> u32 {{\n"));
        for k in 0..5 {
            src.push_str(&format!("    let v{k} = {i} * {k};\n"));
        }
        src.push_str("    0\n}\n\n");
    }
    std::fs::write(loaded.dirs.workspace_root.join("big.rs"), &src).expect("write");
    let mut engine = Engine::spawn(Arc::new(loaded));
    turn(&mut engine, "read the file").await;
    let first = bodies(&server).await.pop().expect("request");
    assert!(first.contains("let v4 = 50 * 4"), "the whole file was read");
    for n in 0..5 {
        turn(&mut engine, &format!("question {n}")).await;
    }
    let last = bodies(&server).await.pop().expect("request");
    assert!(last.contains("only its outline is kept"), "{last}");
    assert!(last.contains("pub fn f50() -> u32 {"), "{last}");
    assert!(!last.contains("let v4 = 50 * 4"), "{last}");
    engine.shutdown().await;
}
