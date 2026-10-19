//! `ask_user`: the model's questions reach the client, and the answers reach the model.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{bodies, sse_call, sse_text};
use quack_core::protocol::{Answer, Event, InputItem, Op, TurnOverrides};
use quack_core::{Engine, EngineHandle};
use wiremock::matchers::{body_string_contains, method};
use wiremock::{Mock, MockServer};

/// A model that asks two questions, then answers once it has the tool result.
async fn asking_server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_string_contains("\"role\":\"tool\""))
        .respond_with(sse_text("Planned."))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(sse_call(
            "q1",
            "ask_user",
            serde_json::json!({"questions": [
                {
                    "header": "Storage",
                    "question": "Where should tokens live?",
                    "options": [
                        {"label": "OS keyring (recommended)", "description": "safest"},
                        {"label": "A dotfile"}
                    ]
                },
                {"question": "Which database?"}
            ]}),
        ))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    server
}

async fn submit(engine: &EngineHandle, text: &str) {
    engine
        .submit(Op::UserTurn {
            items: vec![InputItem::text(text)],
            overrides: TurnOverrides::default(),
        })
        .await;
}

async fn next(engine: &mut EngineHandle) -> Event {
    tokio::time::timeout(Duration::from_secs(10), engine.events.recv())
        .await
        .expect("event within 10s")
        .expect("channel open")
}

/// Answer the questions with `answer` and run the turn to its end. Returns the events.
async fn answer_with(
    engine: &mut EngineHandle,
    answers: Vec<Answer>,
    stop_asking: bool,
) -> Vec<Event> {
    let mut answers = Some(answers);
    let mut seen = Vec::new();
    loop {
        let e = next(engine).await;
        match &e {
            Event::Questions { call_id, .. } => {
                engine
                    .submit(Op::AnswerQuestions {
                        call_id: call_id.clone(),
                        answers: answers.take().expect("asked once"),
                        stop_asking,
                    })
                    .await;
            }
            Event::Error { error } => panic!("engine error: {}", error.message),
            Event::TurnAborted { reason, .. } => panic!("aborted: {reason}"),
            Event::TurnComplete { .. } => return seen,
            _ => {}
        }
        seen.push(e);
    }
}

#[tokio::test]
async fn answers_reach_the_model_in_plan_mode_without_approval() {
    let server = asking_server().await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let loaded = common::workspace(tmp.path(), &server.uri(), r#"["tiny"]"#, "");
    let mut engine = Engine::spawn(Arc::new(loaded));
    engine
        .submit(Op::SetPermissionMode {
            mode: "plan".into(),
        })
        .await;
    submit(&engine, "plan the login").await;
    let seen = answer_with(
        &mut engine,
        vec![
            Answer::Picked { index: 0 },
            Answer::Text {
                text: "sqlite".into(),
            },
        ],
        false,
    )
    .await;
    assert!(
        !seen
            .iter()
            .any(|e| matches!(e, Event::ApprovalRequest { .. })),
        "asking needs no approval, even in plan mode"
    );
    let questions = seen
        .iter()
        .find_map(|e| match e {
            Event::Questions { questions, .. } => Some(questions.clone()),
            _ => None,
        })
        .expect("questions");
    assert_eq!(questions.len(), 2);
    assert_eq!(questions[0].header, "Storage");
    assert_eq!(questions[0].options[0].label, "OS keyring");
    assert_eq!(questions[0].recommended, Some(0));

    let sent = bodies(&server).await;
    assert!(
        sent[0].contains("ask the user with ask_user") && sent[0].contains("todo tool"),
        "plan mode asks for questions and a todo plan: {}",
        sent[0]
    );
    assert!(
        sent[1].contains("A: OS keyring (your recommendation)"),
        "{}",
        sent[1]
    );
    assert!(
        sent[1].contains("Q: Which database?\\nA: sqlite"),
        "{}",
        sent[1]
    );
    engine.shutdown().await;
}

#[tokio::test]
async fn stop_asking_tells_the_model_to_decide() {
    let server = asking_server().await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let loaded = common::workspace(tmp.path(), &server.uri(), r#"["tiny"]"#, "");
    let mut engine = Engine::spawn(Arc::new(loaded));
    submit(&engine, "add login").await;
    answer_with(&mut engine, Vec::new(), true).await;
    let sent = bodies(&server).await;
    assert!(
        sent[1].contains("A: (skipped) decide yourself"),
        "{}",
        sent[1]
    );
    assert!(sent[1].contains("wants no more questions"), "{}", sent[1]);
    engine.shutdown().await;
}

#[tokio::test]
async fn interrupting_while_asked_ends_the_turn() {
    let server = asking_server().await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let loaded = common::workspace(tmp.path(), &server.uri(), r#"["tiny"]"#, "");
    let mut engine = Engine::spawn(Arc::new(loaded));
    submit(&engine, "add login").await;
    loop {
        match next(&mut engine).await {
            Event::Questions { .. } => engine.submit(Op::Interrupt).await,
            Event::TurnAborted { .. } | Event::TurnComplete { .. } => break,
            Event::Error { error } => panic!("engine error: {}", error.message),
            _ => continue,
        };
    }
    // The model was not called again with an answer.
    assert_eq!(bodies(&server).await.len(), 1);
    engine.shutdown().await;
}
