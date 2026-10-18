//! End-to-end against a local mock server: auth header, retry on 503, SSE streaming.

use std::ffi::OsString;

use quack_config::ProviderFile;
use quack_config::provider::ReasoningFormat;
use quack_config::settings::NetworkSettings;
use quack_providers::{ChatRequest, ErrorKind, Message, Provider, ResponseBuilder, StreamEvent};
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SSE: &str = "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\n\
data: {\"choices\":[{\"delta\":{\"content\":\"lo\"},\"finish_reason\":\"stop\"}]}\n\n\
data: {\"choices\":[],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":2}}\n\n\
data: [DONE]\n\n";

fn provider(server: &MockServer, extra: &str) -> Provider {
    let file: ProviderFile = toml::from_str(&format!(
        "base_url = \"{}/v1\"\napi_key = {{ env = \"TEST_KEY\" }}\nmodels = [\"m\"]\n{extra}",
        server.uri()
    ))
    .expect("provider toml");
    let env = |k: &str| (k == "TEST_KEY").then(|| OsString::from("sekrit"));
    Provider::new("mock", file, &NetworkSettings::default(), &env).expect("provider")
}

fn request() -> ChatRequest {
    ChatRequest {
        model: "m".into(),
        messages: vec![Message::User {
            content: "hi".into(),
        }],
        ..ChatRequest::default()
    }
}

#[tokio::test]
async fn streams_after_retrying_a_503() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(503).set_body_string("busy"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer sekrit"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(SSE),
        )
        .mount(&server)
        .await;

    let p = provider(&server, "[http]\nmax_retries = 1\n");
    let mut rx = p.stream(request(), ReasoningFormat::Auto, CancellationToken::new());
    let mut b = ResponseBuilder::default();
    let mut deltas = 0;
    while let Some(ev) = rx.recv().await {
        let ev = ev.expect("no error");
        deltas += usize::from(matches!(ev, StreamEvent::TextDelta(_)));
        b.push(&ev);
    }
    assert_eq!(deltas, 2);
    assert_eq!(b.usage.map(|u| u.prompt_tokens), Some(3));
    assert!(matches!(b.into_message(), Message::Assistant { content, .. } if content == "Hello"));
}

#[tokio::test]
async fn auth_errors_are_not_retried() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(401)
                .set_body_string(r#"{"error":{"message":"invalid api key"}}"#),
        )
        .expect(1)
        .mount(&server)
        .await;
    let p = provider(&server, "");
    let mut rx = p.stream(request(), ReasoningFormat::Auto, CancellationToken::new());
    let err = rx.recv().await.expect("one item").expect_err("auth error");
    assert_eq!(err.kind, ErrorKind::Auth);
    assert_eq!(
        err.to_string(),
        "authentication failed (HTTP 401): invalid api key"
    );
    assert!(rx.recv().await.is_none());
}

#[tokio::test]
async fn discovers_models() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(
                r#"{"object":"list","data":[{"id":"qwen","context_window":131072}]}"#,
            ),
        )
        .mount(&server)
        .await;
    let models = provider(&server, "").list_models().await.expect("models");
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].context_window, Some(131072));
}

#[cfg(feature = "proto-ollama")]
#[tokio::test]
async fn ollama_native_discovers_and_streams() {
    use wiremock::matchers::body_string_contains;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/tags"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(r#"{"models":[{"name":"qwen3:8b"},{"name":"tiny:1b"}]}"#),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/show"))
        .and(body_string_contains("qwen3:8b"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(r#"{"model_info":{"qwen3.context_length":40960}}"#),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/show"))
        .respond_with(ResponseTemplate::new(404).set_body_string(r#"{"error":"not found"}"#))
        .mount(&server)
        .await;
    // num_ctx must reach the server: that is the point of the native adapter.
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .and(body_string_contains(r#""num_ctx":32768"#))
        .respond_with(ResponseTemplate::new(200).set_body_string(concat!(
            "{\"message\":{\"role\":\"assistant\",\"content\":\"Hel\"},\"done\":false}\n",
            "{\"message\":{\"role\":\"assistant\",\"content\":\"lo\"},\"done\":false}\n",
            "{\"message\":{\"role\":\"assistant\",\"content\":\"\"},\"done\":true,\"done_reason\":\"stop\",\"prompt_eval_count\":11,\"eval_count\":2}\n",
        )))
        .mount(&server)
        .await;

    // The OpenAI-style `/v1` base URL works for the native API too.
    let file: ProviderFile = toml::from_str(&format!(
        "base_url = \"{}/v1\"\napi = \"ollama\"\n",
        server.uri()
    ))
    .expect("provider toml");
    let p =
        Provider::new("ollama", file, &NetworkSettings::default(), &|_| None).expect("provider");
    let models = p.list_models().await.expect("models");
    assert_eq!(models.len(), 2);
    assert_eq!(
        models[0].context_window,
        Some(32_768),
        "capped trained length"
    );
    assert_eq!(
        models[1].context_window,
        Some(32_768),
        "default when unknown"
    );

    let mut req = request();
    req.context_window = models[0].context_window;
    let mut rx = p.stream(req, ReasoningFormat::Auto, CancellationToken::new());
    let mut b = ResponseBuilder::default();
    while let Some(ev) = rx.recv().await {
        b.push(&ev.expect("event"));
    }
    assert_eq!(b.usage.map(|u| u.prompt_tokens), Some(11));
    assert!(matches!(b.into_message(), Message::Assistant { content, .. } if content == "Hello"));
}
