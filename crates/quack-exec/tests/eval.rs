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
    // The edit task: one edit call, then an answer, each reporting its prompt size.
    Mock::given(method("POST"))
        .and(body_string_contains("\"role\":\"tool\""))
        .respond_with(sse(json!({
            "choices":[{"delta":{"content":"Done, it is 7."},"finish_reason":"stop"}],
            "usage":{"prompt_tokens":150,"completion_tokens":5}
        })))
        .mount(&server)
        .await;
    let args = json!({"path":"a.txt","old_string":"six","new_string":"seven"}).to_string();
    Mock::given(method("POST"))
        .and(body_string_contains("Change six"))
        .respond_with(sse(json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"edit","arguments":args}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":120,"completion_tokens":9}})))
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
        planner: None,
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
    // How full the context got: the larger of the two requests.
    assert_eq!(
        (edit.prompt_tokens, edit.peak_prompt, edit.pruned),
        (270, 150, 0)
    );
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
        suite: "tasks".into(),
        model: "mock/tiny".into(),
        profile: "empty".into(),
        planner: None,
        window: None,
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

#[tokio::test]
async fn a_planner_plans_and_the_model_carries_it_out() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_string_contains(r#""model":"big""#))
        .respond_with(sse(
            json!({"choices":[{"delta":{"content":"1. Change six to seven in a.txt."},"finish_reason":"stop"}]}),
        ))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_string_contains("[Plan handoff]"))
        .and(body_string_contains("\"role\":\"tool\""))
        .respond_with(sse(
            json!({"choices":[{"delta":{"content":"Done, it is 7."},"finish_reason":"stop"}]}),
        ))
        .mount(&server)
        .await;
    let args = json!({"path":"a.txt","old_string":"six","new_string":"seven"}).to_string();
    Mock::given(method("POST"))
        .and(body_string_contains("[Plan handoff]"))
        .respond_with(sse(json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"edit","arguments":args}}]},"finish_reason":"tool_calls"}]})))
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().expect("tempdir");
    let suite = tmp.path().join("tasks");
    task(
        &suite,
        "edit-one",
        "category = \"single-edit\"\nprompt = \"Change six to seven in a.txt.\"\n\
         [[check]]\nfile = \"a.txt\"\nequals = \"seven\"\n",
        &[("a.txt", "six\n")],
    );
    let user = tmp.path().join("user");
    std::fs::create_dir_all(user.join("providers.d")).expect("mkdir");
    std::fs::write(
        user.join("providers.d").join("mock.toml"),
        format!(
            "base_url = \"{}/v1\"\nmodels = [\"tiny\", \"big\"]\n",
            server.uri()
        ),
    )
    .expect("provider");
    let mut env: HashMap<String, OsString> = HashMap::new();
    env.insert("QUACK_CONFIG_DIR".into(), user.into_os_string());
    for (k, sub) in [("QUACK_SYSTEM_DIR", "system"), ("QUACK_CACHE_DIR", "cache")] {
        env.insert(k.into(), tmp.path().join(sub).into_os_string());
    }
    env.insert("QUACK_DEFAULTS_DIR".into(), "none".into());
    let mut load = LoadOptions::default();
    load.overrides.push("model.default=\"mock/tiny\"".into());
    let opts = EvalOptions {
        load,
        repeat: 1,
        keep: false,
        planner: Some("mock/big".into()),
        scratch: tmp.path().join("scratch"),
        env,
    };
    let tasks = load_tasks(&suite, &[]).expect("tasks");
    let r = run_task(&tasks[0], &opts, 0).await;
    assert!(r.passed, "{r:?}");
    assert_eq!(
        (r.steps, r.tool_calls),
        (3, 1),
        "one planning step, two to carry it out"
    );
    assert_eq!(
        r.model, "mock/tiny",
        "the result names the model that did the work"
    );
}

#[tokio::test]
async fn a_shared_base_hidden_checks_and_failure_labels() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_string_contains("\"role\":\"tool\""))
        .respond_with(sse(
            json!({"choices":[{"delta":{"content":"Fixed."},"finish_reason":"stop"}]}),
        ))
        .mount(&server)
        .await;
    // The model edits the overlay's file, but the bug is in the base's.
    let args = json!({"path":"app.txt","old_string":"v1","new_string":"v2"}).to_string();
    Mock::given(method("POST"))
        .respond_with(sse(json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"edit","arguments":args}}]},"finish_reason":"tool_calls"}]})))
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().expect("tempdir");
    let suite = tmp.path().join("hard");
    let base = suite.join("_projects").join("shop");
    std::fs::create_dir_all(&base).expect("mkdir");
    std::fs::write(base.join("lib.txt"), "bug\n").expect("base");
    std::fs::write(base.join("app.txt"), "v0\n").expect("base");
    task(
        &suite,
        "fix-lib",
        "category = \"fix-test\"\nprompt = \"Fix the bug.\"\nbase = \"../_projects/shop\"\n\
         [[check]]\nfile = \"lib.txt\"\nequals = \"fixed\"\n\
         [[check]]\nfile = \"revealed.txt\"\nequals = \"yes\"\n",
        // The overlay replaces one of the base's files.
        &[("app.txt", "v1\n")],
    );
    let dir = suite.join("fix-lib");
    std::fs::create_dir_all(dir.join("hidden")).expect("mkdir");
    std::fs::write(dir.join("hidden").join("revealed.txt"), "yes\n").expect("hidden");
    std::fs::create_dir_all(dir.join("solution")).expect("mkdir");
    std::fs::write(dir.join("solution").join("lib.txt"), "fixed\n").expect("solution");

    let user = tmp.path().join("user");
    std::fs::create_dir_all(user.join("providers.d")).expect("mkdir");
    std::fs::write(
        user.join("providers.d").join("mock.toml"),
        format!("base_url = \"{}/v1\"\nmodels = [\"tiny\"]\n", server.uri()),
    )
    .expect("provider");
    let mut env: HashMap<String, OsString> = HashMap::new();
    env.insert("QUACK_CONFIG_DIR".into(), user.into_os_string());
    for (k, sub) in [("QUACK_SYSTEM_DIR", "system"), ("QUACK_CACHE_DIR", "cache")] {
        env.insert(k.into(), tmp.path().join(sub).into_os_string());
    }
    env.insert("QUACK_DEFAULTS_DIR".into(), "none".into());
    let opts = EvalOptions {
        load: LoadOptions::default(),
        repeat: 1,
        keep: false,
        planner: None,
        scratch: tmp.path().join("scratch"),
        env,
    };
    let tasks = load_tasks(&suite, &[]).expect("tasks");
    assert_eq!(tasks.len(), 1, "the projects folder is not a task");
    assert_eq!(tasks[0].solution_paths(), ["lib.txt"]);
    let r = run_task(&tasks[0], &opts, 0).await;
    assert!(!r.passed, "{r:?}");
    assert_eq!(
        r.changed,
        ["app.txt"],
        "hidden files are not the model's changes"
    );
    assert_eq!(
        r.failed_checks,
        ["lib.txt is not \"fixed\""],
        "the hidden file is there for the checks"
    );
    assert_eq!(r.failure.map(|f| f.label()), Some("wrong-files"));
    let json = serde_json::to_value(&r).expect("json");
    assert_eq!(json["failure"], "wrong-files");
}
