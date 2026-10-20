//! `[model] planner`: plan mode runs on the planner; accepting the plan hands the session's
//! model one brief in place of the planner's messages, so its cached prefix still holds.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{sse_call, sse_text, turn, until_done};
use quack_core::protocol::{Answer, Event, Op};
use quack_core::{Engine, Resume};
use serde_json::Value;
use wiremock::matchers::{body_string_contains, method};
use wiremock::{Mock, MockServer};

async fn chat_bodies(server: &MockServer) -> Vec<Value> {
    common::bodies(server)
        .await
        .iter()
        .map(|b| serde_json::from_str(b).expect("json body"))
        .collect()
}

fn messages(body: &Value) -> Vec<Value> {
    body["messages"].as_array().cloned().unwrap_or_default()
}

#[tokio::test]
async fn the_planner_plans_and_the_session_model_gets_a_brief() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_string_contains(r#""model":"big""#))
        .and(body_string_contains(r#""role":"tool""#))
        .respond_with(sse_text("1. Retry fetch three times."))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_string_contains(r#""model":"big""#))
        .respond_with(sse_call(
            "q1",
            "ask_user",
            serde_json::json!({"questions": [{"question": "How many retries?", "options": ["3", "5"]}]}),
        ))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_string_contains(r#""model":"small""#))
        .respond_with(sse_text("done"))
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(tmp.path().join("user")).expect("mkdir");
    std::fs::write(
        tmp.path().join("user").join("config.toml"),
        "[model]\ndefault = \"mock/small\"\nplanner = \"mock/big\"\n",
    )
    .expect("config");
    let spawn = |resume| {
        let loaded = common::workspace(tmp.path(), &server.uri(), r#"["small", "big"]"#, "");
        Engine::spawn_with(Arc::new(loaded), resume)
    };
    let mut engine = spawn(Resume::New);
    turn(&mut engine, "hello").await;

    engine
        .submit(Op::SetPermissionMode {
            mode: "plan".into(),
        })
        .await;
    engine
        .submit(Op::UserTurn {
            items: vec![quack_core::protocol::InputItem::text("add retries")],
            overrides: Default::default(),
        })
        .await;
    loop {
        let e = tokio::time::timeout(Duration::from_secs(10), engine.events.recv())
            .await
            .expect("event")
            .expect("open");
        match e {
            Event::Questions { call_id, .. } => {
                let answers = vec![Answer::Text {
                    text: "three".into(),
                }];
                engine
                    .submit(Op::AnswerQuestions {
                        call_id,
                        answers,
                        stop_asking: false,
                    })
                    .await;
            }
            Event::TurnComplete { .. } => break,
            Event::Error { error } => panic!("engine error: {}", error.message),
            Event::TurnAborted { reason, .. } => panic!("aborted: {reason}"),
            _ => {}
        }
    }
    engine
        .submit(Op::AcceptPlan {
            mode: "auto-edit".into(),
        })
        .await;
    until_done(&mut engine).await;
    engine.shutdown().await;

    let bodies = chat_bodies(&server).await;
    let (big, small): (Vec<&Value>, Vec<&Value>) = bodies.iter().partition(|b| b["model"] == "big");
    assert_eq!((big.len(), small.len()), (2, 2), "{bodies:#?}");
    assert!(
        messages(big[0]).iter().any(|m| m["content"] == "done"),
        "the planner sees the conversation so far"
    );

    // The session model's request extends its earlier one with its reply and one brief.
    let before = messages(small[0]);
    let after = messages(small[1]);
    assert_eq!(after.len(), before.len() + 2, "{after:#?}");
    assert_eq!(
        after[..before.len()],
        before[..],
        "the cached prefix is untouched"
    );
    assert_eq!(after[before.len()]["content"], "done");
    let brief = after[before.len() + 1]["content"].as_str().expect("text");
    assert!(brief.starts_with("[Plan handoff] mock/big"), "{brief}");
    assert!(brief.contains("Request:\nadd retries"), "{brief}");
    assert!(brief.contains("Q: How many retries?\nA: three"), "{brief}");
    assert!(
        brief.contains("Plan:\n1. Retry fetch three times."),
        "{brief}"
    );
    assert!(!brief.contains("[Plan mode]"), "{brief}");

    // A resume rebuilds the same history: the next request extends the last one.
    let mut resumed = spawn(Resume::Latest);
    turn(&mut resumed, "next").await;
    resumed.shutdown().await;
    let bodies = chat_bodies(&server).await;
    let last = messages(bodies.last().expect("a request"));
    assert_eq!(last[..after.len()], after[..], "{last:#?}");
}

#[tokio::test]
async fn leaving_plan_mode_goes_back_to_the_session_model() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(sse_text("ok"))
        .mount(&server)
        .await;
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(tmp.path().join("user")).expect("mkdir");
    std::fs::write(
        tmp.path().join("user").join("config.toml"),
        "[model]\ndefault = \"mock/small\"\nplanner = \"mock/big\"\n",
    )
    .expect("config");
    let loaded = common::workspace(tmp.path(), &server.uri(), r#"["small", "big"]"#, "");
    let mut engine = Engine::spawn(Arc::new(loaded));
    let mode = |m: &str| Op::SetPermissionMode { mode: m.into() };
    engine.submit(mode("plan")).await;
    turn(&mut engine, "plan it").await;
    engine.submit(mode("ask")).await;
    turn(&mut engine, "just talk").await;
    engine.shutdown().await;

    let models: Vec<Value> = chat_bodies(&server)
        .await
        .iter()
        .map(|b| b["model"].clone())
        .collect();
    assert_eq!(models, ["big", "small"]);
}
