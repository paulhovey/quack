//! The model last switched to is the one the next start uses, while it is available.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::workspace;
use quack_core::protocol::{Event, ModelId, Op, ProviderId};
use quack_core::{Engine, EngineHandle};
use wiremock::MockServer;

async fn next(engine: &mut EngineHandle) -> Event {
    tokio::time::timeout(Duration::from_secs(10), engine.events.recv())
        .await
        .expect("event within 10s")
        .expect("channel open")
}

/// The model startup settled on, and the warnings it gave.
async fn started(engine: &mut EngineHandle) -> (String, Vec<String>) {
    let (mut model, mut warnings) = (String::new(), Vec::new());
    loop {
        match next(engine).await {
            Event::SessionConfigured { session } => model = session.model.unwrap_or_default(),
            Event::Warning { message } => warnings.push(message),
            Event::StartupComplete => return (model, warnings),
            _ => {}
        }
    }
}

/// The model of the next `SessionConfigured`.
async fn switched(engine: &mut EngineHandle) -> String {
    loop {
        if let Event::SessionConfigured { session } = next(engine).await {
            return session.model.unwrap_or_default();
        }
    }
}

fn set_default(root: &std::path::Path, model: &str) {
    std::fs::write(
        root.join("user").join("config.toml"),
        format!("[model]\ndefault = \"{model}\"\n"),
    )
    .expect("config");
}

#[tokio::test]
async fn a_switched_model_is_used_at_the_next_start() {
    let server = MockServer::start().await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let models = r#"["small", "big"]"#;
    let start = || {
        let loaded = workspace(tmp.path(), &server.uri(), models, "");
        set_default(tmp.path(), "mock/small");
        // Read again so the default just written is in effect.
        Engine::spawn(Arc::new(loaded.reload().expect("reload")))
    };

    let mut engine = start();
    assert_eq!(started(&mut engine).await.0, "mock/small");
    engine
        .submit(Op::SetModel {
            provider: ProviderId("mock".into()),
            model: ModelId("big".into()),
        })
        .await;
    assert_eq!(switched(&mut engine).await, "mock/big");
    engine.shutdown().await;

    let mut engine = start();
    assert_eq!(started(&mut engine).await.0, "mock/big", "remembered");
    engine.shutdown().await;

    // Its provider is gone: back to the default, with a word about why.
    std::fs::write(
        tmp.path().join("data").join("state.toml"),
        "last_model = \"removed/big\"\n",
    )
    .expect("state");
    let mut engine = start();
    let (model, warnings) = started(&mut engine).await;
    assert_eq!(model, "mock/small");
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("last used model removed/big")),
        "{warnings:?}"
    );
    engine.shutdown().await;
}
