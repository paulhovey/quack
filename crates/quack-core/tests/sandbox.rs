//! The OS sandbox in a session: a `full-auto` command can't write outside the
//! workspace, and running outside the sandbox asks even in `full-auto`.
#![cfg(any(target_os = "linux", target_os = "macos"))]

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{bodies, sse_call, sse_text, turn};
use quack_core::Engine;
use quack_core::protocol::{
    ApprovalDecision, ApprovalRequest, Event, InputItem, Op, TurnOverrides,
};
use wiremock::matchers::{body_string_contains, method};
use wiremock::{Mock, MockServer};

#[tokio::test]
async fn full_auto_stays_in_the_sandbox() {
    if quack_sandbox::available().is_err() {
        return;
    }
    // Not under the temp folder (writable in the sandbox) or a git repository (it would
    // become the workspace root).
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .expect("HOME");
    let tmp = tempfile::Builder::new()
        .prefix(".quack-sandbox-test")
        .tempdir_in(home)
        .expect("tempdir");
    let outside = tmp.path().join("outside.txt");
    let write_outside = format!("echo leaked > '{}'", outside.display());

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_string_contains("This command ran in the sandbox"))
        .respond_with(sse_text("blocked, as expected"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_string_contains("the user declined"))
        .respond_with(sse_text("ok, I won't"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_string_contains("try outside"))
        .respond_with(sse_call(
            "c2",
            "shell",
            serde_json::json!({"command": write_outside, "sandbox": false}),
        ))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_string_contains("write it"))
        .respond_with(sse_call(
            "c1",
            "shell",
            serde_json::json!({"command": format!("echo ok > inside.txt && {write_outside}")}),
        ))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    std::fs::create_dir_all(tmp.path().join("user")).expect("mkdir");
    std::fs::write(
        tmp.path().join("user").join("config.toml"),
        "[sandbox]\nmode = \"workspace-write\"\n",
    )
    .expect("config");
    let loaded = common::workspace(tmp.path(), &server.uri(), r#"["tiny"]"#, "");
    let ws = loaded.dirs.workspace_root.clone();
    let mut engine = Engine::spawn(Arc::new(loaded));
    engine
        .submit(Op::SetPermissionMode {
            mode: "full-auto".into(),
        })
        .await;

    turn(&mut engine, "write it").await;
    assert!(ws.join("inside.txt").exists());
    assert!(!outside.exists(), "the sandbox let a write out");
    let last = bodies(&server).await.pop().expect("request");
    assert!(last.contains("This command ran in the sandbox"), "{last}");

    // Leaving the sandbox asks, even in full-auto; declining keeps the file unwritten.
    engine
        .submit(Op::UserTurn {
            items: vec![InputItem::text("try outside")],
            overrides: TurnOverrides::default(),
        })
        .await;
    loop {
        let e = tokio::time::timeout(Duration::from_secs(10), engine.events.recv())
            .await
            .expect("event")
            .expect("open");
        match e {
            Event::ApprovalRequest { call_id, request } => {
                assert!(
                    matches!(
                        request,
                        ApprovalRequest::Shell {
                            outside_sandbox: true,
                            ..
                        }
                    ),
                    "{request:?}"
                );
                engine
                    .submit(Op::ApprovalResponse {
                        call_id,
                        decision: ApprovalDecision::Deny,
                    })
                    .await;
            }
            Event::TurnComplete { .. } => break,
            Event::TurnAborted { reason, .. } => panic!("aborted: {reason}"),
            _ => {}
        }
    }
    assert!(!outside.exists());
    engine.shutdown().await;
}
