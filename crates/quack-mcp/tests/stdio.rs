//! The manager against the fixture server over stdio.

use std::sync::Arc;
use std::time::Duration;

use quack_config::McpServerFile;
use quack_mcp::{Elicited, FieldKind, McpManager, Sampled, ServerAsk, ServerSpec, ServerState};
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
    assert_eq!(names, ["echo", "fail", "crash", "deploy", "summarize"]);
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
async fn lists_prompts_and_resources_and_reads_them() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let m = McpManager::new(
        vec![ServerSpec {
            id: "fx".into(),
            file: fixture(""),
        }],
        tmp.path().to_path_buf(),
        Default::default(),
    );
    m.start();
    ready(&m, "fx").await;

    let prompts = m.prompts();
    assert_eq!(prompts.len(), 1);
    assert_eq!(prompts[0].name, "review");
    let args: Vec<(&str, bool)> = prompts[0]
        .arguments
        .iter()
        .map(|a| (a.name.as_str(), a.required))
        .collect();
    assert_eq!(args, [("file", true), ("focus", false)]);
    let mut args = serde_json::Map::new();
    args.insert("file".into(), "src/a.rs".into());
    let text = m.get_prompt("fx", "review", args).await.expect("prompt");
    assert_eq!(text, "Review src/a.rs, looking at everything.");
    let err = m
        .get_prompt("fx", "review", serde_json::Map::new())
        .await
        .expect_err("missing argument");
    assert!(err.contains("file"), "{err}");

    let resources = m.resources();
    let uris: Vec<(&str, bool)> = resources
        .iter()
        .map(|r| (r.uri.as_str(), r.template))
        .collect();
    assert_eq!(uris, [("mem://notes", false), ("mem://files/{name}", true)]);
    let status = m.status().remove(0);
    assert_eq!(status.prompts, ["review"]);
    assert_eq!(status.resources.len(), 2);

    let out = m.read_resource("fx", "mem://notes").await;
    assert!(out.ok, "{out:?}");
    assert_eq!(out.text, "[resource mem://notes]\nship on friday");
    let out = m.read_resource("fx", "mem://files/plan").await;
    assert!(out.text.ends_with("contents of plan"), "{out:?}");
    let out = m.read_resource("fx", "mem://nope").await;
    assert!(!out.ok, "{out:?}");
    m.shutdown().await;
}

#[tokio::test]
async fn forms_and_sampling_requests_reach_the_asker() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let spec = || ServerSpec {
        id: "fx".into(),
        // A short timeout: time spent on the form must not count toward it.
        file: fixture("call_timeout_secs = 1\n"),
    };
    let m = McpManager::new(vec![spec()], tmp.path().to_path_buf(), Default::default());
    let (tx, mut asks) = tokio::sync::mpsc::channel(4);
    m.set_asker(tx);
    m.start();
    ready(&m, "fx").await;
    let answer = tokio::spawn(async move {
        let mut seen = Vec::new();
        while let Some(ask) = asks.recv().await {
            match ask {
                ServerAsk::Elicit {
                    server,
                    form,
                    reply,
                } => {
                    assert_eq!(server, "fx");
                    assert_eq!(form.message, "Deploy where?");
                    assert!(matches!(form.fields[0].kind, FieldKind::Choice(_)));
                    tokio::time::sleep(Duration::from_millis(1500)).await;
                    let env = form.fields[0].value("prod").expect("an option");
                    let content = [("env".to_owned(), env)].into_iter().collect();
                    let _ = reply.send(Elicited::Accept(content));
                    seen.push("form");
                }
                ServerAsk::Sample { request, reply, .. } => {
                    assert_eq!(request.messages[0].text, "Summarize: a long text");
                    assert_eq!(request.max_tokens, 50);
                    let _ = reply.send(Ok(Sampled {
                        model: "mock/tiny".into(),
                        text: "short".into(),
                        truncated: false,
                    }));
                    seen.push("sample");
                }
            }
        }
        seen
    });

    let cancel = CancellationToken::new();
    let out = m.call("fx", "deploy", serde_json::json!({}), &cancel).await;
    assert!(out.ok, "{out:?}");
    assert_eq!(out.text, "deployed to prod");
    let out = m
        .call(
            "fx",
            "summarize",
            serde_json::json!({"text": "a long text"}),
            &cancel,
        )
        .await;
    assert_eq!(out.text, "mock/tiny said: short");
    m.shutdown().await;
    drop(m);
    assert_eq!(answer.await.expect("asker"), ["form", "sample"]);

    // Without an asker, servers are told quack can't sample, and forms are declined.
    let m = McpManager::new(vec![spec()], tmp.path().to_path_buf(), Default::default());
    m.start();
    ready(&m, "fx").await;
    let out = m.call("fx", "deploy", serde_json::json!({}), &cancel).await;
    assert_eq!(out.text, "not deployed: decline");
    let out = m
        .call("fx", "summarize", serde_json::json!({}), &cancel)
        .await;
    assert!(
        !out.ok && out.text.starts_with("sampling failed"),
        "{out:?}"
    );
    m.shutdown().await;
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
    assert_eq!(m.tools().len(), 6);
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

/// A server with `sandbox = true` runs under the sandbox policy, and won't start
/// without one.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[tokio::test]
async fn sandboxed_server() {
    if quack_sandbox::available().is_err() {
        return;
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    let spec = || ServerSpec {
        id: "fx".into(),
        file: fixture("sandbox = true\n"),
    };
    let policy = quack_sandbox::Policy {
        writable: vec![tmp.path().to_path_buf()],
        network: false,
    };
    let m = McpManager::with_sandbox(
        vec![spec()],
        tmp.path().to_path_buf(),
        Default::default(),
        Some(Arc::new(policy)),
    );
    m.start();
    ready(&m, "fx").await;
    let out = m
        .call(
            "fx",
            "echo",
            serde_json::json!({"text": "boxed"}),
            &CancellationToken::new(),
        )
        .await;
    assert_eq!(out.text, "echo: boxed");
    m.shutdown().await;

    let m = McpManager::new(vec![spec()], tmp.path().to_path_buf(), Default::default());
    m.start();
    let mut rx = m.subscribe();
    let failed = loop {
        if let ServerState::Failed(e) = &m.status()[0].state {
            break e.clone();
        }
        tokio::time::timeout(Duration::from_secs(10), rx.changed())
            .await
            .expect("state change")
            .expect("open");
    };
    assert!(failed.contains("sandbox"), "{failed}");
    m.shutdown().await;
}
