//! The manager against the fixture server over stdio.

use std::sync::Arc;
use std::time::Duration;

use quack_config::McpServerFile;
use quack_mcp::{McpManager, ServerSpec, ServerState};
use tokio_util::sync::CancellationToken;

fn fixture(extra: &str) -> McpServerFile {
    let exe = env!("CARGO_BIN_EXE_mcp-fixture-server").replace('\\', "\\\\");
    toml::from_str(&format!("command = \"{exe}\"\n{extra}")).expect("server file")
}

async fn ready(m: &Arc<McpManager>, id: &str) {
    let mut rx = m.subscribe();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let s = m.status().into_iter().find(|s| s.id == id).expect("server");
        match s.state {
            ServerState::Ready => return,
            ServerState::Failed(e) if !e.contains("restarting") => panic!("{id} failed: {e}"),
            _ => {}
        }
        tokio::time::timeout_at(deadline, rx.changed())
            .await
            .expect("ready within 20s")
            .expect("open");
    }
}

#[tokio::test]
async fn lists_and_calls_tools_and_restarts_after_a_crash() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let m = McpManager::new(
        vec![ServerSpec {
            id: "fx".into(),
            file: fixture("[tools]\ndeny = [\"slow\"]\nauto_approve = [\"echo\"]\n"),
        }],
        tmp.path().to_path_buf(),
        Default::default(),
    );
    m.start();
    ready(&m, "fx").await;

    let tools = m.tools();
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["echo", "fail", "crash"]);
    assert!(tools[0].read_only && tools[0].auto_approve);
    assert!(!tools[1].read_only && !tools[1].auto_approve);

    let cancel = CancellationToken::new();
    let out = m
        .call("fx", "echo", serde_json::json!({"text": "hi"}), &cancel)
        .await;
    assert!(out.ok, "{out:?}");
    assert_eq!(out.text, "echo: hi");
    let out = m.call("fx", "fail", serde_json::json!({}), &cancel).await;
    assert!(!out.ok);
    assert_eq!(out.text, "it broke");

    // The server dies mid-call; the call fails and the server comes back.
    let out = m.call("fx", "crash", serde_json::json!({}), &cancel).await;
    assert!(!out.ok, "{out:?}");
    let mut rx = m.subscribe();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let s = m.status().remove(0);
            if matches!(&s.state, ServerState::Failed(e) if e.contains("restarting")) {
                break;
            }
            rx.changed().await.expect("open");
        }
    })
    .await
    .expect("noticed the exit");
    ready(&m, "fx").await;
    let out = m
        .call("fx", "echo", serde_json::json!({"text": "again"}), &cancel)
        .await;
    assert_eq!(out.text, "echo: again");
    m.shutdown().await;
    assert_eq!(m.status()[0].state, ServerState::Stopped);
}

#[tokio::test]
async fn bad_servers_fail_without_blocking_others() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let missing: McpServerFile =
        toml::from_str("command = \"no-such-mcp-server-xyz\"").expect("file");
    let m = McpManager::new(
        vec![
            ServerSpec {
                id: "missing".into(),
                file: missing,
            },
            ServerSpec {
                id: "fx".into(),
                file: fixture(""),
            },
        ],
        tmp.path().to_path_buf(),
        Default::default(),
    );
    m.start();
    ready(&m, "fx").await;
    let status = m.status();
    assert!(
        matches!(&status[0].state, ServerState::Failed(e) if e.contains("no-such-mcp-server-xyz")),
        "{:?}",
        status[0].state
    );
    assert_eq!(m.tools().len(), 4);
    let out = m
        .call(
            "missing",
            "x",
            serde_json::json!({}),
            &CancellationToken::new(),
        )
        .await;
    assert!(out.text.contains("not running"), "{out:?}");
    m.shutdown().await;
}
