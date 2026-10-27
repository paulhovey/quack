//! The core that works without the defaults bundle: the `generic` profile and the
//! `explore` and `general` sub-agents are compiled in, and files on disk still replace or
//! turn them off.

mod common;

use std::sync::Arc;

use common::{bodies, sse_text, turn, workspace};
use quack_core::Engine;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer};

async fn server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(sse_text("Hi."))
        .mount(&server)
        .await;
    server
}

fn messages(body: &str) -> Vec<serde_json::Value> {
    let v: serde_json::Value = serde_json::from_str(body).expect("json body");
    v["messages"].as_array().cloned().unwrap_or_default()
}

#[tokio::test]
async fn without_the_bundle_the_generic_prompt_and_sub_agents_are_there() {
    let server = server().await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut engine = Engine::spawn(Arc::new(workspace(
        tmp.path(),
        &server.uri(),
        "[\"tiny\"]",
        "",
    )));
    turn(&mut engine, "hello").await;
    let body = bodies(&server).await.pop().expect("a request");
    let msgs = messages(&body);
    assert_eq!(msgs[0]["role"], "system");
    let system = msgs[0]["content"].as_str().unwrap_or_default();
    assert!(system.contains("You are a coding agent"), "{system}");
    let user = msgs[1]["content"].as_str().unwrap_or_default();
    assert!(user.starts_with("<context>"), "{user}");
    assert!(user.ends_with("hello"), "{user}");
    let v: serde_json::Value = serde_json::from_str(&body).expect("json");
    let task = v["tools"]
        .as_array()
        .and_then(|t| t.iter().find(|t| t["function"]["name"] == "task"))
        .map(ToString::to_string)
        .expect("the task tool is offered");
    assert!(
        task.contains("explore") && task.contains("general"),
        "{task}"
    );
    engine.shutdown().await;
}

#[tokio::test]
async fn files_on_disk_turn_the_built_ins_off() {
    let server = server().await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let loaded = workspace(tmp.path(), &server.uri(), "[\"tiny\"]", "");
    let user = loaded.dirs.user_config.clone();
    let generic = user.join("profiles.d").join("generic");
    std::fs::create_dir_all(&generic).expect("mkdir");
    std::fs::write(generic.join("profile.toml"), "enabled = false\n").expect("write");
    std::fs::create_dir_all(user.join("agents.d")).expect("mkdir");
    std::fs::write(
        user.join("agents.d").join("explore.toml"),
        "enabled = false\n",
    )
    .expect("write");
    // Reload so the files written after `workspace` loaded are seen.
    let loaded = workspace(tmp.path(), &server.uri(), "[\"tiny\"]", "");
    let mut engine = Engine::spawn(Arc::new(loaded));
    turn(&mut engine, "hello").await;
    let body = bodies(&server).await.pop().expect("a request");
    assert!(
        messages(&body).iter().all(|m| m["role"] != "system"),
        "no system prompt once generic is off: {body}"
    );
    assert!(!body.contains("explore"), "{body}");
    assert!(body.contains("general"), "{body}");
    engine.shutdown().await;
}
