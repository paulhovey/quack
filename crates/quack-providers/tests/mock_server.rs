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
            images: Vec::new(),
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

#[cfg(feature = "proto-anthropic")]
#[tokio::test]
async fn anthropic_messages_streams_with_its_own_auth() {
    let server = MockServer::start().await;
    let events = [
        r#"{"type":"message_start","message":{"usage":{"input_tokens":5,"cache_read_input_tokens":20}}}"#,
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#,
        r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}"#,
        r#"{"type":"message_stop"}"#,
    ];
    let body: String = events
        .iter()
        .map(|e| format!("event: x\ndata: {e}\n\n"))
        .collect();
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "sekrit"))
        .and(header("anthropic-version", "2023-06-01"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(r#"{"data":[{"id":"glm-5","type":"model"}]}"#),
        )
        .mount(&server)
        .await;
    // The base URL without `/v1`, the way Anthropic-compatible hosts document it.
    let file: ProviderFile = toml::from_str(&format!(
        "base_url = \"{}\"\napi = \"anthropic-messages\"\napi_key = {{ env = \"TEST_KEY\" }}\n",
        server.uri()
    ))
    .expect("provider toml");
    let env = |k: &str| (k == "TEST_KEY").then(|| OsString::from("sekrit"));
    let p = Provider::new("anthropic", file, &NetworkSettings::default(), &env).expect("provider");
    let models = p.list_models().await.expect("models");
    assert_eq!(models[0].id, "glm-5");
    let mut rx = p.stream(request(), ReasoningFormat::Auto, CancellationToken::new());
    let mut b = ResponseBuilder::default();
    while let Some(ev) = rx.recv().await {
        b.push(&ev.expect("event"));
    }
    assert_eq!(
        b.usage.map(|u| (u.prompt_tokens, u.cached_prompt_tokens)),
        Some((25, Some(20)))
    );
    assert!(matches!(b.into_message(), Message::Assistant { content, .. } if content == "Hello"));
}

#[cfg(feature = "proto-openai-responses")]
#[tokio::test]
async fn openai_responses_streams() {
    let server = MockServer::start().await;
    let events = [
        r#"{"type":"response.output_text.delta","output_index":0,"delta":"Hel"}"#,
        r#"{"type":"response.output_text.delta","output_index":0,"delta":"lo"}"#,
        r#"{"type":"response.completed","response":{"usage":{"input_tokens":9,"output_tokens":2}}}"#,
    ];
    let body: String = events.iter().map(|e| format!("data: {e}\n\n")).collect();
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .and(header("authorization", "Bearer sekrit"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;
    let p = provider(&server, "api = \"openai-responses\"\n");
    let mut rx = p.stream(request(), ReasoningFormat::Auto, CancellationToken::new());
    let mut b = ResponseBuilder::default();
    while let Some(ev) = rx.recv().await {
        b.push(&ev.expect("event"));
    }
    assert_eq!(b.usage.map(|u| u.prompt_tokens), Some(9));
    assert!(matches!(b.into_message(), Message::Assistant { content, .. } if content == "Hello"));
}

/// Sealed reasoning goes back only to the provider and setup that made it, and the file's
/// `extra_body` sits under the model's.
#[cfg(feature = "proto-anthropic")]
#[tokio::test]
async fn sealed_replies_go_back_only_where_they_came_from() {
    let server = MockServer::start().await;
    let done = [
        r#"{"type":"message_start","message":{"usage":{"input_tokens":5}}}"#,
        r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}"#,
        r#"{"type":"message_stop"}"#,
    ];
    let body: String = done.iter().map(|e| format!("data: {e}\n\n")).collect();
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;
    let file: ProviderFile = toml::from_str(&format!(
        "base_url = \"{}\"\napi = \"anthropic-messages\"\n\
         extra_body = {{ thinking = {{ type = \"adaptive\", display = \"summarized\" }} }}\n",
        server.uri()
    ))
    .expect("provider toml");
    let p =
        Provider::new("anthropic", file, &NetworkSettings::default(), &|_| None).expect("provider");

    let mut req = request();
    req.extra_body.insert(
        "thinking".into(),
        serde_json::json!({ "display": "omitted" }),
    );
    let fp = quack_providers::fingerprint(&req);
    let reply = |provider: &str, fingerprint: &str, sig: &str| Message::Assistant {
        content: String::new(),
        reasoning: None,
        tool_calls: Vec::new(),
        sealed: Some(quack_providers::Sealed {
            provider: provider.into(),
            model: "other-model".into(),
            fingerprint: fingerprint.into(),
            items: vec![serde_json::json!({"type":"thinking","thinking":"","signature":sig})],
        }),
    };
    req.messages.extend([
        reply("anthropic", &fp, "KEEP"),
        Message::User {
            content: "and".into(),
            images: Vec::new(),
        },
        reply("elsewhere", &fp, "OTHER-PROVIDER"),
        Message::User {
            content: "then".into(),
            images: Vec::new(),
        },
        reply("anthropic", "0000", "OLD-SETUP"),
        Message::User {
            content: "now".into(),
            images: Vec::new(),
        },
    ]);
    let mut rx = p.stream(req, ReasoningFormat::Auto, CancellationToken::new());
    while rx.recv().await.is_some() {}

    let sent = &server.received_requests().await.unwrap_or_default()[0];
    let sent: serde_json::Value = serde_json::from_slice(&sent.body).expect("json");
    let text = sent.to_string();
    assert!(text.contains("KEEP"));
    assert!(!text.contains("OTHER-PROVIDER"));
    assert!(!text.contains("OLD-SETUP"));
    assert_eq!(
        sent["thinking"],
        serde_json::json!({ "type": "adaptive", "display": "omitted" })
    );
}
