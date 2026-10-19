//! `quack eval` against a mock model: a passing task, a failing one and a skipped one.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::Path;

use quack_config::LoadOptions;
use quack_exec::eval::{EvalOptions, Report, load_tasks, run_task, table};
use serde_json::json;
use wiremock::matchers::{body_string_contains, method};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn sse(chunk: serde_json::Value) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(format!("data: {chunk}\n\ndata: [DONE]\n\n"))
}

fn task(suite: &Path, id: &str, toml: &str, files: &[(&str, &str)]) {
    let dir = suite.join(id);
    std::fs::create_dir_all(dir.join("repo")).expect("mkdir");
    std::fs::write(dir.join("task.toml"), toml).expect("task");
    for (name, text) in files {
        std::fs::write(dir.join("repo").join(name), text).expect("file");
    }
}

#[tokio::test]
async fn runs_checks_and_reports() {
    let server = MockServer::start().await;
    // The edit task: one edit call, then an answer.
    Mock::given(method("POST"))
        .and(body_string_contains("\"role\":\"tool\""))
        .respond_with(sse(
            json!({"choices":[{"delta":{"content":"Done, it is 7."},"finish_reason":"stop"}]}),
        ))
        .mount(&server)
        .await;
    let args = json!({"path":"a.txt","old_string":"six","new_string":"seven"}).to_string();
    Mock::given(method("POST"))
        .and(body_string_contains("Change six"))
        .respond_with(sse(json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"edit","arguments":args}}]},"finish_reason":"tool_calls"}]})))
        .mount(&server)
        .await;
    // The asking task: a question nobody is there to answer, then the answer.
    let ask = json!({"question": "Which file?", "options": ["a.txt", "b.txt"]}).to_string();
    Mock::given(method("POST"))
        .and(body_string_contains("Ask me first"))
        .respond_with(sse(json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"q1","function":{"name":"ask_user","arguments":ask}}]},"finish_reason":"tool_calls"}]})))
        .mount(&server)
        .await;
    // Anything else: a wrong answer.
    Mock::given(method("POST"))
        .respond_with(sse(
            json!({"choices":[{"delta":{"content":"No idea."},"finish_reason":"stop"}]}),
        ))
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().expect("tempdir");
    let suite = tmp.path().join("tasks");
    task(
        &suite,
        "edit-one",
        "category = \"single-edit\"\nprompt = \"Change six to seven in a.txt.\"\n\
         [[check]]\nfile = \"a.txt\"\nequals = \"seven\"\n[[check]]\nanswer_contains = \"7\"\n\
         [[check]]\ntool_called = \"edit\"\n",
        &[("a.txt", "six\n")],
    );
    task(
        &suite,
        "answer",
        "category = \"navigate\"\nprompt = \"What is the port?\"\n[[check]]\nanswer_contains = \"8080\"\n",
        &[("config.toml", "port = 8080\n")],
    );
    // Unattended: the question is skipped at once and the model goes on.
    task(
        &suite,
        "asks",
        "category = \"navigate\"\nprompt = \"Ask me first.\"\n[[check]]\nanswer_contains = \"7\"\n\
         [[check]]\ntool_called = \"ask_user\"\n",
        &[],
    );
    task(
        &suite,
        "needs-tool",
        "category = \"shell\"\nprompt = \"x\"\nrequires = [\"no-such-program-here\"]\n",
        &[],
    );

    let user = tmp.path().join("user");
    std::fs::create_dir_all(user.join("providers.d")).expect("mkdir");
    std::fs::write(
        user.join("providers.d").join("mock.toml"),
        format!(
            "base_url = \"{}/v1\"\nmodels = [{{ id = \"tiny\", sampling = {{ temperature = 0.3, top_k = 40 }} }}]\n",
            server.uri()
        ),
    )
    .expect("provider");
    let mut env: HashMap<String, OsString> = HashMap::new();
    env.insert("QUACK_CONFIG_DIR".into(), user.into_os_string());
    env.insert(
        "QUACK_SYSTEM_DIR".into(),
        tmp.path().join("system").into_os_string(),
    );
    env.insert(
        "QUACK_CACHE_DIR".into(),
        tmp.path().join("cache").into_os_string(),
    );
    env.insert("QUACK_DEFAULTS_DIR".into(), "none".into());
    let opts = EvalOptions {
        load: LoadOptions::default(),
        repeat: 1,
        keep: false,
        scratch: tmp.path().join("scratch"),
        env,
    };
    let tasks = load_tasks(&suite, &[]).expect("tasks");
    assert_eq!(tasks.len(), 4);
    let mut results = Vec::new();
    for t in &tasks {
        results.push(run_task(t, &opts, 0).await);
    }
    let by = |id: &str| results.iter().find(|r| r.task == id).expect("result");
    let edit = by("edit-one");
    assert!(edit.passed, "{edit:?}");
    assert_eq!((edit.steps, edit.tool_calls, edit.edit_failures), (2, 1, 0));
    assert_eq!(edit.model, "mock/tiny");
    // What the requests carried is recorded with the result.
    assert_eq!(
        edit.sampling,
        [("temperature".to_owned(), 0.3), ("top_k".to_owned(), 40.0)]
    );
    let answer = by("answer");
    assert!(!answer.passed);
    assert_eq!(answer.failed_checks.len(), 1, "{answer:?}");
    assert!(by("needs-tool").skipped.is_some());
    let asks = by("asks");
    assert!(asks.passed, "{asks:?}");
    // Workspaces are cleaned up.
    assert!(std::fs::read_dir(tmp.path().join("scratch")).map_or(true, |mut d| d.next().is_none()));

    let report = Report {
        harness: "test".into(),
        date: "2026-10-18".into(),
        model: "mock/tiny".into(),
        profile: "empty".into(),
        sampling: Report::sampling_of(&edit.sampling),
        results,
    };
    assert_eq!(report.score(), (2, 3));
    let text = table(&report);
    assert!(
        text.contains("sampling: temperature 0.3, top_k 40 · 2/3 passed"),
        "{text}"
    );
    assert!(load_tasks(&suite, &["nope".into()]).is_err());
}
