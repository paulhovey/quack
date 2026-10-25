//! "Always allow" saves the call's rule to config, and later sessions allow it unasked.

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use common::{sse_call, sse_text, workspace};
use quack_config::{Loaded, ProjectStatus, TrustDecision};
use quack_core::protocol::{ApprovalDecision, Event, InputItem, Op, TurnOverrides, WriteScope};
use quack_core::{Engine, EngineHandle};
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer};

async fn mock() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_string_contains("\"role\":\"tool\""))
        .respond_with(sse_text("Done."))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(sse_call(
            "c1",
            "write",
            serde_json::json!({"path": "notes.txt", "content": "hi\n"}),
        ))
        .mount(&server)
        .await;
    server
}

/// Run one turn, answering any approval with `answer`; returns the rules offered.
async fn run(loaded: Loaded, answer: Option<ApprovalDecision>) -> Option<Vec<String>> {
    let mut engine: EngineHandle = Engine::spawn(Arc::new(loaded));
    engine
        .submit(Op::UserTurn {
            items: vec![InputItem::text("write notes")],
            overrides: TurnOverrides::default(),
        })
        .await;
    let mut offered = None;
    loop {
        let e = tokio::time::timeout(Duration::from_secs(10), engine.events.recv())
            .await
            .expect("event within 10s")
            .expect("channel open");
        match e {
            Event::ApprovalRequest {
                call_id, always, ..
            } => {
                offered = Some(always);
                let decision = answer.clone().expect("no approval expected");
                engine
                    .submit(Op::ApprovalResponse { call_id, decision })
                    .await;
            }
            Event::Error { error } => panic!("engine error: {}", error.message),
            Event::TurnAborted { reason, .. } => panic!("aborted: {reason}"),
            Event::TurnComplete { .. } => break,
            _ => {}
        }
    }
    engine.shutdown().await;
    offered
}

fn always() -> Option<ApprovalDecision> {
    Some(ApprovalDecision::AlwaysAllow {
        scope: WriteScope::Project,
    })
}

#[tokio::test]
async fn without_a_project_config_the_rule_goes_to_the_user_layer() {
    let server = mock().await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let offered = run(
        workspace(tmp.path(), &server.uri(), "[\"tiny\"]", ""),
        always(),
    )
    .await;
    assert_eq!(offered, Some(vec!["edit:notes.txt".to_owned()]));
    let user = std::fs::read_to_string(tmp.path().join("user").join("config.toml"))
        .expect("user config written");
    assert!(
        user.contains("allow = { append = [\"edit:notes.txt\"] }"),
        "{user}"
    );
    assert!(!tmp.path().join("ws").join(".quack").exists());

    // The next session writes the file without asking.
    std::fs::remove_file(tmp.path().join("ws").join("notes.txt")).expect("rm");
    let offered = run(workspace(tmp.path(), &server.uri(), "[\"tiny\"]", ""), None).await;
    assert_eq!(offered, None);
    assert!(tmp.path().join("ws").join("notes.txt").exists());
}

#[tokio::test]
async fn a_trusted_project_keeps_the_rule_and_its_trust() {
    let server = mock().await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let project = tmp.path().join("ws").join(".quack");
    std::fs::create_dir_all(&project).expect("mkdir");
    std::fs::write(
        project.join("config.toml"),
        "# team settings\n[ui]\nascii = true\n",
    )
    .expect("write");
    let loaded = workspace(tmp.path(), &server.uri(), "[\"tiny\"]", "");
    loaded.trust_project(TrustDecision::Trusted).expect("trust");
    let loaded = workspace(tmp.path(), &server.uri(), "[\"tiny\"]", "");
    assert_eq!(loaded.project, ProjectStatus::Trusted);

    run(loaded, always()).await;
    let text = std::fs::read_to_string(project.join("config.toml")).expect("read");
    assert!(
        text.starts_with("# team settings\n[ui]\nascii = true\n"),
        "{text}"
    );
    assert!(text.contains("\"edit:notes.txt\""), "{text}");
    assert!(!Path::new(&tmp.path().join("user").join("config.toml")).exists());

    // Still trusted after the edit, and the rule applies.
    std::fs::remove_file(tmp.path().join("ws").join("notes.txt")).expect("rm");
    let loaded = workspace(tmp.path(), &server.uri(), "[\"tiny\"]", "");
    assert_eq!(loaded.project, ProjectStatus::Trusted);
    assert_eq!(run(loaded, None).await, None);
    assert!(tmp.path().join("ws").join("notes.txt").exists());
}
