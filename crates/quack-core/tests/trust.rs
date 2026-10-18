//! Project trust: an untrusted repo's `.quack/` runs nothing, and trusting it
//! reloads the session around it without losing the conversation.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{bodies, sse_text, turn};
use quack_core::protocol::{Event, Op};
use quack_core::{Engine, EngineHandle};
use wiremock::matchers::method;
use wiremock::{Mock, MockServer};

/// Events up to and including the first that `stop` accepts.
async fn until(engine: &mut EngineHandle, stop: impl Fn(&Event) -> bool) -> Vec<Event> {
    let mut seen = Vec::new();
    loop {
        let e = tokio::time::timeout(Duration::from_secs(15), engine.events.recv())
            .await
            .expect("event within 15s")
            .expect("open");
        let done = stop(&e);
        seen.push(e);
        if done {
            return seen;
        }
    }
}

#[cfg(unix)]
#[tokio::test]
async fn untrusted_project_runs_nothing_until_trusted() {
    let llm = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(sse_text("ok"))
        .mount(&llm)
        .await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let marker = tmp.path().join("ran");
    let project = tmp.path().join("ws/.quack");
    std::fs::create_dir_all(project.join("mcp.d")).expect("mkdir");
    std::fs::write(
        project.join("mcp.d/evil.toml"),
        format!(
            "command = \"sh\"\nargs = [\"-c\", \"touch '{}'; sleep 30\"]\n",
            marker.display()
        ),
    )
    .expect("write");
    let loaded = common::workspace(tmp.path(), &llm.uri(), r#"["tiny"]"#, "");
    let mut engine = Engine::spawn(Arc::new(loaded));

    let startup = until(&mut engine, |e| matches!(e, Event::StartupComplete)).await;
    let prompt = startup.iter().find_map(|e| match e {
        Event::ProjectTrust {
            summary, changed, ..
        } => Some((summary.clone(), *changed)),
        _ => None,
    });
    assert_eq!(prompt, Some(("1 MCP server".to_owned(), false)));
    turn(&mut engine, "first question").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!marker.exists(), "an untrusted mcp.d must not start");

    engine
        .submit(Op::TrustProject {
            decision: "trusted".into(),
        })
        .await;
    until(&mut engine, |e| matches!(e, Event::ConfigReloaded)).await;
    let reloaded = until(&mut engine, |e| matches!(e, Event::StartupComplete)).await;
    assert!(
        !reloaded
            .iter()
            .any(|e| matches!(e, Event::ProjectTrust { .. })),
        "trusted now: {reloaded:?}"
    );
    for _ in 0..50 {
        if marker.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(marker.exists(), "a trusted mcp.d starts after the reload");

    // The conversation carried over.
    turn(&mut engine, "second question").await;
    let last = bodies(&llm).await.pop().expect("a request");
    assert!(last.contains("first question"), "{last}");

    // Editing the server's command asks again.
    std::fs::write(project.join("mcp.d/evil.toml"), "command = \"other\"\n").expect("write");
    engine.submit(Op::ReloadConfig).await;
    let again = until(&mut engine, |e| matches!(e, Event::StartupComplete)).await;
    assert!(
        again
            .iter()
            .any(|e| matches!(e, Event::ProjectTrust { changed: true, .. })),
        "{again:?}"
    );
    engine.shutdown().await;
}
