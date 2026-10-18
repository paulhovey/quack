//! Language servers in a session: an edit's result carries the errors it introduced.

mod common;

use std::path::PathBuf;
use std::sync::Arc;

use common::{bodies, sse_call, sse_text, turn};
use quack_core::Engine;
use quack_core::protocol::Op;
use wiremock::matchers::{body_string_contains, method};
use wiremock::{Mock, MockServer};

/// The fixture language server from quack-lsp, built if needed.
fn fixture_server() -> PathBuf {
    let out = std::process::Command::new(env!("CARGO"))
        .args([
            "build",
            "-p",
            "quack-lsp",
            "--bin",
            "lsp-fixture-server",
            "--message-format=json",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("cargo build");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find_map(|v| v["executable"].as_str().map(PathBuf::from))
        .expect("fixture executable")
}

#[tokio::test]
async fn edit_results_carry_new_diagnostics() {
    edit_reports_diagnostics(false).await;
}

/// The same with the server in the OS sandbox (`sandbox = true` in its file).
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[tokio::test]
async fn sandboxed_server_works() {
    if quack_sandbox::available().is_ok() {
        edit_reports_diagnostics(true).await;
    }
}

async fn edit_reports_diagnostics(sandbox: bool) {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_string_contains("diagnostics (1 new)"))
        .respond_with(sse_text("I see the error"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_string_contains("break it"))
        .respond_with(sse_call(
            "c1",
            "edit",
            serde_json::json!({"path": "a.fx", "old_string": "fn main() {}", "new_string": "fn main() { ERROR }"}),
        ))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().expect("tempdir");
    let user = tmp.path().join("user").join("lsp.d");
    std::fs::create_dir_all(&user).expect("mkdir");
    std::fs::write(
        user.join("fixture.toml"),
        format!(
            "command = {:?}\nextensions = [\"fx\"]\nsandbox = {sandbox}\n[diagnostics]\nsettle_ms = 100\n",
            fixture_server()
        ),
    )
    .expect("lsp file");
    let loaded = common::workspace(tmp.path(), &server.uri(), r#"["tiny"]"#, "");
    std::fs::write(loaded.dirs.workspace_root.join("a.fx"), "fn main() {}\n").expect("write");
    let mut engine = Engine::spawn(Arc::new(loaded));
    engine
        .submit(Op::SetPermissionMode {
            mode: "full-auto".into(),
        })
        .await;
    turn(&mut engine, "break it").await;
    let last = bodies(&server).await.pop().expect("request");
    assert!(
        last.contains("a.fx:1:13 error[F1]: found ERROR"),
        "{last}"
    );
    // The tool is offered, since a server is installed.
    assert!(last.contains("\"name\":\"lsp\""), "{last}");
    engine.shutdown().await;
}
