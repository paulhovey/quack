//! `/fork` goes on in a copy of the session and leaves the original as it was; `/export`
//! writes the transcript.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{sse_text, turn, workspace};
use quack_core::protocol::{Event, ExportFormat, Op, ReplayItem};
use quack_core::{Engine, EngineHandle, Resume};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer};

/// Events until `want` matches one; panics on an error.
async fn until<T>(engine: &mut EngineHandle, want: impl Fn(&Event) -> Option<T>) -> T {
    loop {
        let e = tokio::time::timeout(Duration::from_secs(10), engine.events.recv())
            .await
            .expect("event within 10s")
            .expect("channel open");
        if let Event::Error { error } = &e {
            panic!("engine error: {}", error.message);
        }
        if let Some(t) = want(&e) {
            return t;
        }
    }
}

async fn resumed(root: &std::path::Path, url: &str, id: &str) -> Vec<ReplayItem> {
    let loaded = workspace(root, url, "[\"tiny\"]", "");
    let mut engine = Engine::spawn_with(Arc::new(loaded), Resume::Id(id.to_owned()));
    let items = until(&mut engine, |e| match e {
        Event::SessionResumed { items, .. } => Some(items.clone()),
        _ => None,
    })
    .await;
    engine.shutdown().await;
    items
}

#[tokio::test]
async fn fork_branches_and_export_writes_the_transcript() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(sse_text("Sure."))
        .mount(&server)
        .await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let loaded = workspace(tmp.path(), &server.uri(), "[\"tiny\"]", "");
    let ws = loaded.dirs.workspace_root.clone();
    let mut engine = Engine::spawn(Arc::new(loaded));

    // Nothing to fork before the first message.
    engine.submit(Op::ForkSession).await;
    let warning = until(&mut engine, |e| match e {
        Event::Warning { message } if message.contains("fork") => Some(message.clone()),
        _ => None,
    })
    .await;
    assert!(warning.contains("nothing to fork yet"), "{warning}");

    turn(&mut engine, "first").await;
    engine.submit(Op::ForkSession).await;
    let forked = until(&mut engine, |e| match e {
        Event::SessionConfigured { session } => Some(session.id.0.clone()),
        _ => None,
    })
    .await;
    let notice = until(&mut engine, |e| match e {
        Event::Notice { message } => Some(message.clone()),
        _ => None,
    })
    .await;
    assert!(notice.contains("forked"), "{notice}");

    turn(&mut engine, "second").await;
    engine
        .submit(Op::Export {
            format: ExportFormat::Markdown,
            path: Some("out.md".into()),
        })
        .await;
    until(&mut engine, |e| match e {
        Event::Notice { message } if message.contains("exported") => Some(()),
        _ => None,
    })
    .await;
    engine.shutdown().await;

    let md = std::fs::read_to_string(ws.join("out.md")).expect("exported");
    assert!(md.starts_with("# first (fork)\n"), "{md}");
    assert!(
        md.contains("## You\n\nfirst\n")
            && md.contains("## You\n\nsecond\n")
            && md.contains("## quack\n\nSure.\n"),
        "{md}"
    );

    // The fork holds both turns; the original only the first.
    let (_, sessions) =
        quack_core::list_sessions(&workspace(tmp.path(), &server.uri(), "[\"tiny\"]", ""));
    assert_eq!(sessions.len(), 2, "{sessions:?}");
    let users = |items: &[ReplayItem]| {
        items
            .iter()
            .filter(|i| matches!(i, ReplayItem::User { .. }))
            .count()
    };
    let parent = sessions
        .iter()
        .find(|s| s.id != forked)
        .map(|s| s.id.clone())
        .expect("original listed");
    assert_eq!(users(&resumed(tmp.path(), &server.uri(), &forked).await), 2);
    assert_eq!(users(&resumed(tmp.path(), &server.uri(), &parent).await), 1);
}
