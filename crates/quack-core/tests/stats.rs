//! `/stats`: usage, cost and tool counts add up over a turn; tokens and cost survive resume.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{sse_raw, turn, workspace};
use quack_core::protocol::{Event, Op, StatsReport};
use quack_core::{Engine, EngineHandle, Resume};
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer};

async fn stats(engine: &mut EngineHandle) -> StatsReport {
    engine.submit(Op::GetStats).await;
    loop {
        let e = tokio::time::timeout(Duration::from_secs(10), engine.events.recv())
            .await
            .expect("event within 10s")
            .expect("channel open");
        if let Event::Stats { report } = e {
            return report;
        }
    }
}

#[tokio::test]
async fn a_turn_is_counted_priced_and_kept_on_resume() {
    let server = MockServer::start().await;
    // Step 2: after the tool result, the answer.
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_string_contains("\"role\":\"tool\""))
        .respond_with(sse_raw(&[
            r#"{"choices":[{"delta":{"content":"Two files."},"finish_reason":"stop"}]}"#.into(),
            r#"{"choices":[],"usage":{"prompt_tokens":3000,"completion_tokens":500,"prompt_tokens_details":{"cached_tokens":2000}}}"#.into(),
        ]))
        .mount(&server)
        .await;
    // Step 1: list the workspace.
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(sse_raw(&[
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"ls","arguments":"{}"}}]},"finish_reason":"tool_calls"}]}"#.into(),
            r#"{"choices":[],"usage":{"prompt_tokens":1000,"completion_tokens":100}}"#.into(),
        ]))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().expect("tempdir");
    let models = r#"[{ id = "tiny", cost = { input = 1.0, output = 10.0, cached_input = 0.5 } }]"#;
    let mut engine = Engine::spawn(Arc::new(workspace(tmp.path(), &server.uri(), models, "")));
    turn(&mut engine, "what is here?").await;

    let r = stats(&mut engine).await;
    assert_eq!(r.model, "mock/tiny");
    assert_eq!((r.turns, r.requests, r.unreported), (1, 2, 0));
    assert_eq!(r.prompt_tokens, 4000);
    assert_eq!(r.completion_tokens, 600);
    assert_eq!((r.cached_tokens, r.cache_reported), (2000, 1));
    // 1000 + 1000 fresh at $1, 2000 cached at $0.50, 600 out at $10, per million.
    let want = (2000.0 + 1000.0 + 6000.0) / 1e6;
    assert!(
        (r.cost.expect("priced") - want).abs() < 1e-12,
        "{:?}",
        r.cost
    );
    assert_eq!(r.tools.len(), 1, "{:?}", r.tools);
    assert_eq!((r.tools[0].name.as_str(), r.tools[0].calls), ("ls", 1));
    assert!(r.ttft_ms.is_some());
    assert!(!r.resumed);
    let sent: Vec<u32> = r.history.iter().map(|q| q.prompt).collect();
    assert_eq!(sent, [1000, 3000]);
    assert_eq!(r.history[1].cached, 2000);
    assert!(r.history.iter().all(|q| q.cost.is_some()));
    engine.shutdown().await;

    // Resumed: tokens come back from the log; timings and tools start over.
    let loaded = workspace(tmp.path(), &server.uri(), models, "");
    let mut engine = Engine::spawn_with(Arc::new(loaded), Resume::Latest);
    loop {
        let e = tokio::time::timeout(Duration::from_secs(10), engine.events.recv())
            .await
            .expect("event within 10s")
            .expect("channel open");
        if matches!(e, Event::SessionResumed { .. }) {
            break;
        }
    }
    let r = stats(&mut engine).await;
    assert!(r.resumed);
    assert_eq!((r.turns, r.requests), (1, 2));
    assert_eq!(r.prompt_tokens, 4000);
    assert!(r.tools.is_empty());
    assert_eq!(r.history.len(), 2, "the charts come back from the log");
    assert!(r.history.iter().all(|q| q.ttft_ms.is_none()));
    assert!((r.cost.expect("priced in the log") - want).abs() < 1e-12);
    assert_eq!(r.unpriced, 0);
    engine.shutdown().await;
}
