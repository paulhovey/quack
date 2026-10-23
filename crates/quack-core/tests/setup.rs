//! First-run setup: detecting a server, adding it as a provider, and using it.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{sse_text, turn};
use quack_core::protocol::{Event, Op};
use quack_core::setup::{Candidate, detect_in};
use quack_core::{Engine, EngineHandle};
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
        format!("# Added by setup.\nbase_url = \"{base_url}\"\n")
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

#[tokio::test]
async fn a_server_added_by_url_lists_its_models_and_a_service_waits_for_its_key() {
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
    let providers_d = tmp.path().join("user/providers.d");
    std::fs::remove_file(providers_d.join("mock.toml")).expect("rm");
    // A hosted service whose key is read from a variable nobody sets.
    std::fs::write(
        providers_d.join("example.toml"),
        "name = \"Example\"\ngroup = \"Hosted\"\nbase_url = \"https://api.example.com/v1\"\n\
         api_key = { env = \"QUACK_TEST_UNSET_KEY\" }\n",
    )
    .expect("write");
    let mut engine = Engine::spawn(Arc::new(loaded));
    next_matching(&mut engine, |e| matches!(e, Event::StartupComplete)).await;

    engine.submit(Op::ListServices).await;
    let Event::Services { services } =
        next_matching(&mut engine, |e| matches!(e, Event::Services { .. })).await
    else {
        unreachable!()
    };
    assert_eq!(services.len(), 1, "{services:?}");
    assert_eq!(services[0].id, "example");
    assert_eq!(services[0].env.as_deref(), Some("QUACK_TEST_UNSET_KEY"));
    assert!(!services[0].ready);

    // A server with no key: the file names only its URL, and its models follow.
    let base_url = format!("{}/v1", server.uri());
    engine
        .submit(Op::AddServer {
            base_url: base_url.clone(),
            key: None,
        })
        .await;
    let Event::ProviderAdded { id, path } =
        next_matching(&mut engine, |e| matches!(e, Event::ProviderAdded { .. })).await
    else {
        unreachable!()
    };
    let port = server.address().port();
    assert_eq!(id, format!("server-{port}"));
    assert!(path.is_some());
    let text = std::fs::read_to_string(providers_d.join(format!("{id}.toml"))).expect("file");
    assert_eq!(
        text,
        format!("# Added by setup.\nbase_url = \"{base_url}\"\n")
    );
    let Event::ModelCatalog { providers, .. } =
        next_matching(&mut engine, |e| matches!(e, Event::ModelCatalog { .. })).await
    else {
        unreachable!()
    };
    assert_eq!(providers.len(), 1, "only the new provider: {providers:?}");
    assert!(providers[0].available, "{providers:?}");
    assert_eq!(providers[0].models[0].id, "tiny");

    // Picking the model starts the session on it.
    engine
        .submit(Op::SetModel {
            provider: quack_core::protocol::ProviderId(id.clone()),
            model: quack_core::protocol::ModelId("tiny".into()),
        })
        .await;
    next_matching(&mut engine, |e| {
        matches!(e, Event::SessionConfigured { .. })
    })
    .await;
    turn(&mut engine, "hi").await;

    // A bad URL and an unknown service are refused before anything is stored.
    for op in [
        Op::AddServer {
            base_url: "gpu-box:8000".into(),
            key: None,
        },
        Op::SetProviderKey {
            id: "nope".into(),
            key: quack_core::protocol::SecretText("k".into()),
        },
    ] {
        engine.submit(op).await;
        let e = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Ok(Event::Error { error }) = engine.events.recv().await {
                    return error.message;
                }
            }
        })
        .await
        .expect("an error");
        assert!(e.starts_with("cannot add the provider"), "{e}");
    }
    engine.shutdown().await;
}

#[test]
fn a_new_key_replaces_a_stored_one_or_moves_the_key_to_the_store() {
    use quack_core::setup::set_provider_key_with;
    let tmp = tempfile::tempdir().expect("tempdir");
    let loaded = common::workspace(tmp.path(), "http://127.0.0.1:9/v1", "[]", "");
    let providers_d = tmp.path().join("user/providers.d");
    let stored = "base_url = \"https://api.example.com/v1\"\n\
                  api_key = { keyring = \"example/me\" }\n";
    std::fs::write(providers_d.join("stored.toml"), stored).expect("write");
    std::fs::write(
        providers_d.join("from-env.toml"),
        "base_url = \"https://api.example.org/v1\"\n\
         api_key = { env = \"QUACK_TEST_UNSET_KEY\" }\n",
    )
    .expect("write");
    let mut saved: Vec<(String, String)> = Vec::new();
    let mut store = |name: &str, key: &str| {
        saved.push((name.to_owned(), key.to_owned()));
        Ok(())
    };

    // Already in the store: replaced under its own name, and the file is left alone.
    let path = set_provider_key_with(&loaded, "stored", "new-key", &mut store).expect("stored");
    assert_eq!(path, None);
    assert_eq!(
        std::fs::read_to_string(providers_d.join("stored.toml")).expect("file"),
        stored
    );

    // Read from elsewhere: stored under the provider's id, and the file points at it.
    let path = set_provider_key_with(&loaded, "from-env", "k2", &mut store).expect("moved");
    assert_eq!(path, Some(providers_d.join("from-env.toml")));
    let text = std::fs::read_to_string(providers_d.join("from-env.toml")).expect("file");
    assert!(
        text.contains("api_key = { keyring = \"from-env\" }"),
        "{text}"
    );

    // Nothing is stored for an unknown provider or an empty key.
    for (id, key) in [("nope", "k"), ("stored", "")] {
        set_provider_key_with(&loaded, id, key, &mut store).expect_err("refused");
    }
    assert_eq!(
        saved,
        [
            ("example/me".to_owned(), "new-key".to_owned()),
            ("from-env".to_owned(), "k2".to_owned())
        ]
    );
}
