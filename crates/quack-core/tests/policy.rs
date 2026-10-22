//! System policy locks hold for the running session, not only at load: `/permissions`
//! and `/yolo` cannot leave a locked mode.

mod common;

use std::sync::Arc;
use std::time::Duration;

use quack_core::protocol::{Event, Op};
use quack_core::{Engine, EngineHandle};
use wiremock::MockServer;

/// The next `PermissionMode` event, and whether a lock warning came before it.
async fn next_mode(engine: &mut EngineHandle) -> (String, bool) {
    let mut warned = false;
    loop {
        let e = tokio::time::timeout(Duration::from_secs(10), engine.events.recv())
            .await
            .expect("event")
            .expect("open");
        match e {
            Event::Warning { message } if message.contains("locked") => warned = true,
            Event::PermissionMode { mode } => return (mode, warned),
            _ => {}
        }
    }
}

#[tokio::test]
async fn a_locked_permission_mode_cannot_be_changed_at_runtime() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let server = MockServer::start().await;
    std::fs::create_dir_all(tmp.path().join("system")).expect("mkdir");
    std::fs::write(
        tmp.path().join("system").join("config.toml"),
        "[permissions]\nmode = \"ask\"\n[policy]\nlocked = [\"permissions\"]\n",
    )
    .expect("config");
    let loaded = common::workspace(tmp.path(), &server.uri(), r#"["tiny"]"#, "");
    let mut engine = Engine::spawn(Arc::new(loaded));

    for mode in ["yolo", "full-auto", "read-only"] {
        engine
            .submit(Op::SetPermissionMode { mode: mode.into() })
            .await;
        assert_eq!(next_mode(&mut engine).await, ("ask".into(), true), "{mode}");
    }

    // Plan mode only narrows what runs, so it stays available.
    engine
        .submit(Op::SetPermissionMode {
            mode: "plan".into(),
        })
        .await;
    assert_eq!(next_mode(&mut engine).await, ("plan".into(), false));
    engine.shutdown().await;
}
