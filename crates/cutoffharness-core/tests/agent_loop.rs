//! A full turn against a mock OpenAI-compatible server: tool call → approval → result → answer.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use cutoffharness_config::{LoadOptions, Loaded, load_with};
use cutoffharness_core::protocol::{
    ApprovalDecision, ApprovalRequest, Event, InputItem, ModelId, Op, ProviderId, ReplayItem,
    TurnOverrides,
};
use cutoffharness_core::{Engine, Resume};
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn sse(chunks: &[&str]) -> ResponseTemplate {
    let body: String = chunks
        .iter()
        .map(|c| format!("data: {c}\n\n"))
        .collect::<String>()
        + "data: [DONE]\n\n";
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(body)
}

fn workspace(root: &Path, base_url: &str) -> Loaded {
    workspace_with(root, base_url, "[\"tiny\"]")
}

fn workspace_with(root: &Path, base_url: &str, models: &str) -> Loaded {
    let user = root.join("user");
    std::fs::create_dir_all(user.join("providers.d")).expect("mkdir");
    // The short provider form: no api, no model list beyond one id.
    std::fs::write(
        user.join("providers.d").join("mock.toml"),
        format!("base_url = \"{base_url}/v1\"\nmodels = {models}\n"),
    )
    .expect("provider");
    let mut vars: HashMap<String, OsString> = HashMap::new();
    vars.insert("CUTOFFHARNESS_CONFIG_DIR".into(), user.into_os_string());
    for (k, sub) in [
        ("CUTOFFHARNESS_SYSTEM_DIR", "system"),
        ("CUTOFFHARNESS_DATA_DIR", "data"),
        ("CUTOFFHARNESS_CACHE_DIR", "cache"),
    ] {
        vars.insert(k.into(), root.join(sub).into_os_string());
    }
    vars.insert("CUTOFFHARNESS_DEFAULTS_DIR".into(), "none".into());
    let ws = root.join("ws");
    std::fs::create_dir_all(&ws).expect("mkdir");
    load_with(&move |k| vars.get(k).cloned(), &LoadOptions::new(ws)).expect("load")
}

async fn next(rx: &mut tokio::sync::broadcast::Receiver<Event>) -> Event {
    tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("event within 10s")
        .expect("channel open")
}

#[tokio::test]
async fn tool_call_with_approval_then_answer() {
    let server = MockServer::start().await;
    // Step 2: the request carries the tool result; the model answers.
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_string_contains("Created notes.txt"))
        .respond_with(sse(&[
            r#"{"choices":[{"delta":{"content":"Done."},"finish_reason":"stop"}]}"#,
            r#"{"choices":[],"usage":{"prompt_tokens":50,"completion_tokens":2}}"#,
        ]))
        .mount(&server)
        .await;
    // Step 1: the model asks to write a file.
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(sse(&[
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"write","arguments":"{\"path\":\"notes.txt\","}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"content\":\"hi\\n\"}"}}]},"finish_reason":"tool_calls"}]}"#,
        ]))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().expect("tempdir");
    let loaded = workspace(tmp.path(), &server.uri());
    let ws = loaded.dirs.workspace_root.clone();
    let mut engine = Engine::spawn(Arc::new(loaded));
    engine
        .submit(Op::UserTurn {
            items: vec![InputItem::text("write notes")],
            overrides: TurnOverrides::default(),
        })
        .await;

    let mut text = String::new();
    let mut ended = None;
    loop {
        match next(&mut engine.events).await {
            Event::ApprovalRequest { call_id, request } => {
                assert!(
                    matches!(&request, ApprovalRequest::Edit { path, diff } if path == "notes.txt" && diff.contains("+hi")),
                    "{request:?}"
                );
                engine
                    .submit(Op::ApprovalResponse {
                        call_id,
                        decision: ApprovalDecision::AllowOnce,
                    })
                    .await;
            }
            Event::ToolCallEnd { result, .. } => ended = Some(result),
            Event::AssistantDelta { text: t } => text.push_str(&t),
            Event::Error { error } => panic!("engine error: {}", error.message),
            Event::TurnComplete { usage, .. } => {
                assert_eq!(usage.completion_tokens, 2);
                break;
            }
            Event::TurnAborted { reason, .. } => panic!("aborted: {reason}"),
            _ => {}
        }
    }
    assert_eq!(text, "Done.");
    assert!(ended.is_some_and(|r| r.ok));
    assert_eq!(
        std::fs::read_to_string(ws.join("notes.txt")).expect("written"),
        "hi\n"
    );
    engine.shutdown().await;

    // Resume in a fresh engine: the transcript comes back, and the next request carries
    // the old history (the step-2 mock only matches when the tool result is present).
    let loaded = workspace(tmp.path(), &server.uri());
    let mut engine = Engine::spawn_with(Arc::new(loaded), Resume::Latest);
    let items = loop {
        if let Event::SessionResumed { items, .. } = next(&mut engine.events).await {
            break items;
        }
    };
    assert_eq!(
        items.first(),
        Some(&ReplayItem::User {
            text: "write notes".into()
        })
    );
    assert!(
        items
            .iter()
            .any(|i| matches!(i, ReplayItem::Tool { ok: true, .. }))
    );
    engine
        .submit(Op::UserTurn {
            items: vec![InputItem::text("thanks")],
            overrides: TurnOverrides::default(),
        })
        .await;
    loop {
        match next(&mut engine.events).await {
            Event::TurnComplete { .. } => break,
            Event::Error { error } => panic!("engine error: {}", error.message),
            Event::TurnAborted { reason, .. } => panic!("aborted: {reason}"),
            _ => {}
        }
    }
    engine.shutdown().await;
}

#[tokio::test]
async fn tool_call_printed_as_text_is_promoted() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_string_contains("secret-value"))
        .respond_with(sse(&[
            r#"{"choices":[{"delta":{"content":"It says secret-value."},"finish_reason":"stop"}]}"#,
        ]))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(sse(&[
            r#"{"choices":[{"delta":{"content":"Reading.\n<tool_call>\n<function=read>\n<parameter=path>\nkey.txt\n</parameter>\n</function>\n</tool_call>"},"finish_reason":"stop"}]}"#,
        ]))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let loaded = workspace(tmp.path(), &server.uri());
    std::fs::write(loaded.dirs.workspace_root.join("key.txt"), "secret-value\n").expect("write");
    let mut engine = Engine::spawn(Arc::new(loaded));
    engine
        .submit(Op::UserTurn {
            items: vec![InputItem::text("what does key.txt say?")],
            overrides: TurnOverrides::default(),
        })
        .await;
    let mut rewritten = None;
    let mut tool_ok = false;
    loop {
        match next(&mut engine.events).await {
            Event::AssistantRewrite { text } => rewritten = Some(text),
            Event::ToolCallEnd { result, .. } => tool_ok = result.ok,
            Event::TurnComplete { .. } => break,
            Event::Error { error } => panic!("engine error: {}", error.message),
            Event::TurnAborted { reason, .. } => panic!("aborted: {reason}"),
            _ => {}
        }
    }
    assert_eq!(rewritten.as_deref(), Some("Reading."));
    assert!(tool_ok);
    engine.shutdown().await;
}

#[tokio::test]
async fn switching_models_keeps_history() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_string_contains(r#""model":"big""#))
        .and(body_string_contains("first question"))
        .respond_with(sse(&[
            r#"{"choices":[{"delta":{"content":"from big"},"finish_reason":"stop"}]}"#,
        ]))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_string_contains(r#""model":"small""#))
        .respond_with(sse(&[
            r#"{"choices":[{"delta":{"content":"from small"},"finish_reason":"stop"}]}"#,
        ]))
        .mount(&server)
        .await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let loaded = workspace_with(tmp.path(), &server.uri(), r#"["small", "big"]"#);
    let mut engine = Engine::spawn(Arc::new(loaded));
    let turn = |text: &str| Op::UserTurn {
        items: vec![InputItem::text(text)],
        overrides: TurnOverrides::default(),
    };
    async fn answer(rx: &mut tokio::sync::broadcast::Receiver<Event>) -> String {
        let mut text = String::new();
        loop {
            match next(rx).await {
                Event::AssistantDelta { text: t } => text.push_str(&t),
                Event::TurnComplete { .. } => return text,
                Event::Error { error } => panic!("engine error: {}", error.message),
                Event::TurnAborted { reason, .. } => panic!("aborted: {reason}"),
                _ => {}
            }
        }
    }
    engine.submit(turn("first question")).await;
    assert_eq!(answer(&mut engine.events).await, "from small");

    engine
        .submit(Op::SetModel {
            provider: ProviderId("mock".into()),
            model: ModelId("big".into()),
        })
        .await;
    loop {
        if let Event::SessionConfigured { session } = next(&mut engine.events).await
            && session.model.as_deref() == Some("mock/big")
        {
            break;
        }
    }
    // The big-model mock only answers when the earlier question is still in the history.
    engine.submit(turn("second")).await;
    assert_eq!(answer(&mut engine.events).await, "from big");
    engine.shutdown().await;
}
