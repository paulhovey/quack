//! Helpers shared by the engine integration tests.
#![allow(dead_code)]

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use quack_config::{LoadOptions, Loaded, load_with};
use quack_core::EngineHandle;
use quack_core::protocol::{Event, InputItem, Op, TurnOverrides};
use wiremock::{MockServer, ResponseTemplate};

/// A streamed reply with only text.
pub fn sse_text(text: &str) -> ResponseTemplate {
    let chunk = serde_json::json!({"choices":[{"delta":{"content":text},"finish_reason":"stop"}]});
    sse_raw(&[chunk.to_string()])
}

/// A streamed reply that makes one tool call.
pub fn sse_call(id: &str, name: &str, args: serde_json::Value) -> ResponseTemplate {
    let chunk = serde_json::json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":id,"function":{"name":name,"arguments":args.to_string()}}]},"finish_reason":"tool_calls"}]});
    sse_raw(&[chunk.to_string()])
}

pub fn sse_raw(chunks: &[String]) -> ResponseTemplate {
    let body: String = chunks.iter().map(|c| format!("data: {c}\n\n")).collect();
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(body + "data: [DONE]\n\n")
}

/// Isolated directories under `root`, one provider file `mock.toml`, no defaults bundle.
/// The workspace is `root/ws`; the harness starts in `root/ws/<cwd>`.
pub fn workspace(root: &Path, base_url: &str, models: &str, cwd: &str) -> Loaded {
    let user = root.join("user");
    std::fs::create_dir_all(user.join("providers.d")).expect("mkdir");
    std::fs::write(
        user.join("providers.d").join("mock.toml"),
        format!("base_url = \"{base_url}/v1\"\nmodels = {models}\n"),
    )
    .expect("provider");
    let mut vars: HashMap<String, OsString> = HashMap::new();
    vars.insert("QUACK_CONFIG_DIR".into(), user.into_os_string());
    for (k, sub) in [
        ("QUACK_SYSTEM_DIR", "system"),
        ("QUACK_DATA_DIR", "data"),
        ("QUACK_CACHE_DIR", "cache"),
    ] {
        vars.insert(k.into(), root.join(sub).into_os_string());
    }
    vars.insert("QUACK_DEFAULTS_DIR".into(), "none".into());
    let dir: PathBuf = root.join("ws").join(cwd);
    std::fs::create_dir_all(&dir).expect("mkdir");
    load_with(&move |k| vars.get(k).cloned(), &LoadOptions::new(dir)).expect("load")
}

/// Events until the turn (or `/compact`) ends; panics on errors and aborts.
pub async fn until_done(engine: &mut EngineHandle) -> Vec<Event> {
    let mut seen = Vec::new();
    loop {
        let e = tokio::time::timeout(Duration::from_secs(10), engine.events.recv())
            .await
            .expect("event within 10s")
            .expect("channel open");
        match &e {
            Event::Error { error } => panic!("engine error: {}", error.message),
            Event::TurnAborted { reason, .. } => panic!("aborted: {reason}"),
            Event::TurnComplete { .. } => return seen,
            _ => seen.push(e),
        }
    }
}

pub async fn turn(engine: &mut EngineHandle, text: &str) -> Vec<Event> {
    engine
        .submit(Op::UserTurn {
            items: vec![InputItem::text(text)],
            overrides: TurnOverrides::default(),
        })
        .await;
    until_done(engine).await
}

/// Chat request bodies, oldest first.
pub async fn bodies(server: &MockServer) -> Vec<String> {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r.url.path().ends_with("/chat/completions"))
        .map(|r| String::from_utf8_lossy(&r.body).into_owned())
        .collect()
}
