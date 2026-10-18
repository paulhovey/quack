//! First-run setup: detecting a server, adding it as a provider, and using it.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{sse_text, turn};
use cutoffharness_core::protocol::{Event, Op};
use cutoffharness_core::setup::{Candidate, detect_in};
use cutoffharness_core::{Engine, EngineHandle};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn next_matching(engine: &mut EngineHandle, want: impl Fn(&Event) -> bool) -> Event {
    loop {
        let e = tokio::time::timeout(Duration::from_secs(10), engine.events.recv())
            .await
            .expect("event within 10s")
            .expect("channel open");
        if let Event::Error { error } = &e {
            panic!("engine error: {}", error.message);
        }
        if want(&e) {
            return e;
        }
    }
}

#[tokio::test]
async fn detected_server_becomes_a_provider_and_the_default_model() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": [{"id": "tiny"}]})),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(sse_text("hello"))
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().expect("tempdir");
    let loaded = common::workspace(tmp.path(), &server.uri(), "[]", "");
    // Start with no providers at all.
    let providers_d = tmp.path().join("user/providers.d");
    std::fs::remove_file(providers_d.join("mock.toml")).expect("rm");
    let config = Arc::new(loaded);

    let base_url = format!("{}/v1", server.uri());
    let candidates = vec![
        Candidate {
            id: "llama-cpp".into(),
            name: "llama.cpp".into(),
            base_url: base_url.clone(),
            api: None,
            note: String::new(),
        },
        // Nothing listens here.
        Candidate {
            id: "vllm".into(),
            name: "vLLM".into(),
            base_url: "http://127.0.0.1:9/v1".into(),
            api: None,
            note: String::new(),
        },
    ];
    let found = detect_in(config.clone(), candidates.clone()).await;
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].id, "llama-cpp");
    assert_eq!(found[0].models[0].id, "tiny");
    assert_eq!(found[0].configured, None);

    let mut engine = Engine::spawn(config.clone());
    next_matching(&mut engine, |e| matches!(e, Event::StartupComplete)).await;
    engine
        .submit(Op::AddProvider {
            id: "llama-cpp".into(),
            base_url: base_url.clone(),
            api: None,
            model: "tiny".into(),
        })
        .await;
    let added = next_matching(&mut engine, |e| matches!(e, Event::ProviderAdded { .. })).await;
    assert!(
        matches!(&added, Event::ProviderAdded { id, path: Some(_) } if id == "llama-cpp"),
        "{added:?}"
    );
    let text = std::fs::read_to_string(providers_d.join("llama-cpp.toml")).expect("file");
    assert_eq!(
        text,
        format!(
            "# llama.cpp on this machine, found by setup.\n\
             # Start llama-server with `--jinja` so tool calls work.\n\
             base_url = \"{base_url}\"\n"
        )
    );
    let configured = next_matching(&mut engine, |e| {
        matches!(e, Event::SessionConfigured { .. })
    })
    .await;
    let Event::SessionConfigured { session } = configured else {
        unreachable!()
    };
    assert_eq!(session.model.as_deref(), Some("llama-cpp/tiny"));
    let user_config = std::fs::read_to_string(tmp.path().join("user/config.toml")).expect("config");
    assert!(
        user_config.contains("default = \"llama-cpp/tiny\""),
        "{user_config}"
    );

    turn(&mut engine, "hi").await;

    // Adding the same server again reuses the provider.
    engine
        .submit(Op::AddProvider {
            id: "llama-cpp".into(),
            base_url: base_url.clone(),
            api: None,
            model: "tiny".into(),
        })
        .await;
    let again = next_matching(&mut engine, |e| matches!(e, Event::ProviderAdded { .. })).await;
    assert!(
        matches!(&again, Event::ProviderAdded { id, path: None } if id == "llama-cpp"),
        "{again:?}"
    );
    engine.shutdown().await;

    let found = detect_in(config, candidates).await;
    assert_eq!(found[0].configured.as_deref(), Some("llama-cpp"));
}
