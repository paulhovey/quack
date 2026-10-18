//! Compaction against a mock server: at the threshold, on overflow, and on `/compact`.

mod common;

use std::sync::Arc;

use common::{bodies, sse_text as sse, turn, until_done};
use cutoffharness_core::protocol::{Event, Op, ReplayItem};
use cutoffharness_core::{Engine, Resume};
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SUMMARIZER: &str = "You summarize a coding session";

fn workspace(root: &std::path::Path, base_url: &str, models: &str) -> cutoffharness_config::Loaded {
    common::workspace(root, base_url, models, "")
}

fn compacted(events: &[Event]) -> bool {
    events.iter().any(|e| matches!(e, Event::Compacted { .. }))
}

#[tokio::test]
async fn compacts_at_the_threshold_and_resumes_compacted() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_string_contains(SUMMARIZER))
        .respond_with(sse("SUMMARY-TEXT"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(sse("ok"))
        .mount(&server)
        .await;

    // Budget 17,000 tokens, threshold 13,600. Each prompt is about 4,900 tokens, so the
    // third request goes over and the first two turns get summarized.
    let tmp = tempfile::tempdir().expect("tempdir");
    let loaded = Arc::new(workspace(
        tmp.path(),
        &server.uri(),
        r#"[{ id = "tiny", context_window = 20000 }]"#,
    ));
    let big = "x".repeat(17_000);
    let mut engine = Engine::spawn(loaded.clone());
    assert!(!compacted(
        &turn(&mut engine, &format!("FIRST {big}")).await
    ));
    assert!(!compacted(
        &turn(&mut engine, &format!("SECOND {big}")).await
    ));
    let third = turn(&mut engine, &format!("THIRD {big}")).await;
    assert!(compacted(&third), "{third:?}");

    let all = bodies(&server).await;
    let summarize = &all[all.len() - 2];
    // The summarizer gets about 60% of the budget, so the oldest turn is left out.
    assert!(summarize.contains(SUMMARIZER) && summarize.contains("SECOND"));
    assert!(summarize.contains("older messages left out"));
    assert!(
        !summarize.contains("\"tools\""),
        "the summary request has no tools"
    );
    let last = all.last().expect("request");
    assert!(last.contains("[compaction summary]") && last.contains("SUMMARY-TEXT"));
    assert!(last.contains("THIRD") && !last.contains("FIRST") && !last.contains("SECOND"));
    engine.shutdown().await;

    // A resumed session continues from the compacted history.
    let mut engine = Engine::spawn_with(loaded, Resume::Latest);
    let resumed = loop {
        let e = engine.events.recv().await.expect("event");
        if let Event::SessionResumed { items, .. } = e {
            break items;
        }
    };
    assert!(resumed.contains(&ReplayItem::Compacted));
    turn(&mut engine, "FOURTH").await;
    let last = bodies(&server).await.pop().expect("request");
    assert!(last.contains("SUMMARY-TEXT") && last.contains("THIRD") && last.contains("FOURTH"));
    assert!(!last.contains("FIRST"));
    engine.shutdown().await;
}

#[tokio::test]
async fn an_overflow_compacts_and_retries_once() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(sse("first answer"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_string_contains(SUMMARIZER))
        .respond_with(sse("SUMMARY-TEXT"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_string_contains("[compaction summary]"))
        .respond_with(sse("second answer"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(400).set_body_string(
            r#"{"error":{"message":"This model's maximum context length is 32768 tokens"}}"#,
        ))
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().expect("tempdir");
    let loaded = workspace(tmp.path(), &server.uri(), r#"["tiny"]"#);
    let mut engine = Engine::spawn(Arc::new(loaded));
    turn(&mut engine, "first question").await;
    let events = turn(&mut engine, "second question").await;
    assert!(compacted(&events));
    let text: String = events
        .iter()
        .filter_map(|e| match e {
            Event::AssistantDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "second answer");
    engine.shutdown().await;
}

#[tokio::test]
async fn compact_command_keeps_only_the_last_turn() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_string_contains(SUMMARIZER))
        .respond_with(sse("SUMMARY-TEXT"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(sse("ok"))
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().expect("tempdir");
    let loaded = workspace(tmp.path(), &server.uri(), r#"["tiny"]"#);
    let mut engine = Engine::spawn(Arc::new(loaded));

    // Nothing before the current turn: nothing to compact.
    turn(&mut engine, "ONE").await;
    engine.submit(Op::Compact { focus: None }).await;
    let reason = loop {
        match engine.events.recv().await.expect("event") {
            Event::TurnAborted { reason, .. } => break reason,
            Event::TurnComplete { .. } => panic!("compacted a single turn"),
            _ => {}
        }
    };
    assert!(reason.contains("nothing to compact"), "{reason}");

    turn(&mut engine, "TWO").await;
    engine
        .submit(Op::Compact {
            focus: Some("the parser".into()),
        })
        .await;
    assert!(compacted(&until_done(&mut engine).await));
    let summarize = bodies(&server).await.pop().expect("request");
    assert!(summarize.contains("Pay particular attention to: the parser"));
    assert!(summarize.contains("ONE") && !summarize.contains("TWO"));

    turn(&mut engine, "THREE").await;
    let last = bodies(&server).await.pop().expect("request");
    assert!(last.contains("SUMMARY-TEXT") && last.contains("TWO") && last.contains("THREE"));
    assert!(!last.contains("\"ONE\""));
    engine.shutdown().await;
}
