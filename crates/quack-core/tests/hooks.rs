//! Hooks (docs/plan/11-security.md#hooks): `hooks.d` programs on agent events, against a
//! mock model. The hooks are small Python scripts run through `{python}`, as real ones
//! would be; without Python on PATH these tests have nothing to run and pass.

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use common::{bodies, sse_call, sse_text, turn};
use quack_core::Engine;
use quack_core::protocol::{Event, InputItem, Op, TurnOverrides};
use wiremock::matchers::{body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn has_python() -> bool {
    ["python3", "python", "py"].iter().any(|p| {
        std::process::Command::new(p)
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
    })
}

/// Write `<dir>/hooks.d/<id>.toml` running `<dir>/hooks.d/scripts/<id>.py`.
fn hook(dir: &Path, id: &str, toml: &str, script: &str) {
    let hooks = dir.join("hooks.d");
    std::fs::create_dir_all(hooks.join("scripts")).expect("mkdir");
    std::fs::write(hooks.join("scripts").join(format!("{id}.py")), script).expect("script");
    std::fs::write(
        hooks.join(format!("{id}.toml")),
        format!("{toml}\ncommand = [\"{{python}}\", \"{{dir}}/scripts/{id}.py\"]\n"),
    )
    .expect("hook");
}

/// Submit a prompt and collect events until the turn ends either way.
async fn run_turn(engine: &mut quack_core::EngineHandle, text: &str) -> Vec<Event> {
    engine
        .submit(Op::UserTurn {
            items: vec![InputItem::text(text)],
            overrides: TurnOverrides::default(),
        })
        .await;
    let mut seen = Vec::new();
    loop {
        let e = tokio::time::timeout(Duration::from_secs(20), engine.events.recv())
            .await
            .expect("event within 20s")
            .expect("channel open");
        let end = matches!(e, Event::TurnComplete { .. } | Event::TurnAborted { .. });
        seen.push(e);
        if end {
            return seen;
        }
    }
}

fn hook_runs(events: &[Event]) -> Vec<(String, String)> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::HookRan { id, outcome, .. } => Some((id.clone(), outcome.clone())),
            _ => None,
        })
        .collect()
}

const ECHO_MATCHES: &str = r#"
import json, sys
event = json.load(sys.stdin)
keys = ", ".join(m.rsplit("/", 1)[-1] for m in event["matches"])
print(json.dumps({"additional_context": "ISSUE CONTEXT for " + keys, "message": "fetched " + keys}))
"#;

#[tokio::test]
async fn user_prompt_hook_adds_context_only_when_its_pattern_matches() {
    if !has_python() {
        return;
    }
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(sse_text("ok"))
        .mount(&server)
        .await;
    let tmp = tempfile::tempdir().expect("tempdir");
    hook(
        &tmp.path().join("user"),
        "issues",
        "event = \"user_prompt\"\nmatch = { prompt = 'https://jira\\.corp/browse/[A-Z]+-\\d+' }",
        ECHO_MATCHES,
    );
    let loaded = common::workspace(tmp.path(), &server.uri(), r#"["tiny"]"#, "");
    let mut engine = Engine::spawn(Arc::new(loaded));

    let events = run_turn(
        &mut engine,
        "look at https://jira.corp/browse/PAY-12 please",
    )
    .await;
    assert_eq!(hook_runs(&events), [("issues".into(), "context".into())]);
    let ran = events.iter().find_map(|e| match e {
        Event::HookRan {
            message,
            context_tokens,
            ..
        } => Some((message.clone(), *context_tokens)),
        _ => None,
    });
    assert!(
        matches!(&ran, Some((Some(m), n)) if m == "fetched PAY-12" && *n > 0),
        "{ran:?}"
    );

    let events = run_turn(&mut engine, "no links here").await;
    assert!(hook_runs(&events).is_empty());

    let sent = bodies(&server).await;
    assert!(
        sent[0].contains(
            "look at https://jira.corp/browse/PAY-12 please\\n\\nISSUE CONTEXT for PAY-12"
        ),
        "{}",
        sent[0]
    );
    // The second request repeats the first turn, context included, and adds nothing.
    assert_eq!(
        sent[1].matches("ISSUE CONTEXT for").count(),
        1,
        "{}",
        sent[1]
    );
    engine.shutdown().await;
}

#[tokio::test]
async fn a_denying_hook_stops_the_prompt_and_a_failing_one_follows_on_error() {
    if !has_python() {
        return;
    }
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(sse_text("ok"))
        .mount(&server)
        .await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let user = tmp.path().join("user");
    // Exit 2: deny, with stderr as the reason.
    hook(
        &user,
        "no-secrets",
        "event = \"user_prompt\"\nmatch = { prompt = 'AKIA[0-9A-Z]{16}' }",
        "import sys\nprint('that looks like an AWS key', file=sys.stderr)\nsys.exit(2)\n",
    );
    // A crash with the default on_error = "warn": a warning, and the prompt goes ahead.
    hook(
        &user,
        "flaky",
        "event = \"user_prompt\"\nmatch = { prompt = 'flaky' }",
        "raise SystemExit('jira is down')\n",
    );
    let loaded = common::workspace(tmp.path(), &server.uri(), r#"["tiny"]"#, "");
    let mut engine = Engine::spawn(Arc::new(loaded));

    let events = run_turn(&mut engine, "deploy with AKIAABCDEFGHIJKLMNOP").await;
    assert!(
        events.iter().any(|e| matches!(e, Event::Warning { message }
            if message.contains("no-secrets") && message.contains("AWS key"))),
        "{events:?}"
    );
    assert!(
        matches!(events.last(), Some(Event::TurnAborted { reason, .. }) if reason.contains("no-secrets")),
        "{events:?}"
    );
    assert!(
        bodies(&server).await.is_empty(),
        "nothing reaches the model"
    );

    let events = run_turn(&mut engine, "flaky thing").await;
    assert!(
        events.iter().any(|e| matches!(e, Event::Warning { message }
            if message.contains("hook `flaky` failed") && message.contains("jira is down"))),
        "{events:?}"
    );
    assert!(matches!(events.last(), Some(Event::TurnComplete { .. })));
    assert_eq!(bodies(&server).await.len(), 1);
    engine.shutdown().await;
}

#[tokio::test]
async fn pre_tool_hooks_allow_deny_and_rewrite_calls() {
    if !has_python() {
        return;
    }
    let server = MockServer::start().await;
    // First match wins: each turn's call once, then plain answers.
    Mock::given(method("POST"))
        .and(body_string_contains("write the notes"))
        .respond_with(sse_call(
            "c1",
            "write",
            serde_json::json!({"path": "notes.txt", "content": "draft"}),
        ))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_string_contains("write the secrets"))
        .respond_with(sse_call(
            "c2",
            "write",
            serde_json::json!({"path": "secrets.env", "content": "x"}),
        ))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(sse_text("done"))
        .mount(&server)
        .await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let user = tmp.path().join("user");
    // Writing notes needs no approval, and the hook changes what is written.
    hook(
        &user,
        "notes",
        "event = \"pre_tool\"\nmatch = { tool = \"write\", pattern = \"notes*\" }",
        r#"
import json, sys
event = json.load(sys.stdin)
args = dict(event["args"], content=event["args"]["content"] + " (reviewed)")
print(json.dumps({"decision": "allow", "updated_args": args}))
"#,
    );
    hook(
        &user,
        "guard",
        "event = \"pre_tool\"\nmatch = { pattern = \"*.env\" }",
        r#"print('{"decision": "deny", "message": "env files hold credentials"}')"#,
    );
    let loaded = common::workspace(tmp.path(), &server.uri(), r#"["tiny"]"#, "");
    let ws = loaded.dirs.workspace_root.clone();
    let mut engine = Engine::spawn(Arc::new(loaded));

    // In the default ask mode a write would ask; the hook allowed it.
    let events = turn(&mut engine, "write the notes").await;
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, Event::ApprovalRequest { .. })),
        "{events:?}"
    );
    assert_eq!(
        std::fs::read_to_string(ws.join("notes.txt")).expect("written"),
        "draft (reviewed)"
    );

    let events = turn(&mut engine, "write the secrets").await;
    assert!(hook_runs(&events).contains(&("guard".into(), "denied".into())));
    assert!(!ws.join("secrets.env").exists());
    let sent = bodies(&server).await;
    assert!(
        sent.iter()
            .any(|b| b.contains("blocked by hook `guard`: env files hold credentials")),
        "{sent:?}"
    );
    engine.shutdown().await;
}

#[tokio::test]
async fn post_tool_context_reaches_the_model_with_the_result() {
    if !has_python() {
        return;
    }
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_string_contains("\"role\":\"tool\""))
        .respond_with(sse_text("done"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(sse_call("c1", "read", serde_json::json!({"path": "a.txt"})))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    let tmp = tempfile::tempdir().expect("tempdir");
    hook(
        &tmp.path().join("user"),
        "lint",
        "event = \"post_tool\"\nmatch = { tool = \"read\" }",
        r#"
import json, sys
event = json.load(sys.stdin)
print("reminder: " + event["target"] + " is generated; edit the template" if event["ok"] else "")
"#,
    );
    let loaded = common::workspace(tmp.path(), &server.uri(), r#"["tiny"]"#, "");
    std::fs::write(loaded.dirs.workspace_root.join("a.txt"), "hello").expect("file");
    let mut engine = Engine::spawn(Arc::new(loaded));
    turn(&mut engine, "read it").await;
    let sent = bodies(&server).await;
    assert!(
        sent[1].contains("reminder: a.txt is generated; edit the template"),
        "{}",
        sent[1]
    );
    engine.shutdown().await;
}

#[tokio::test]
async fn system_hooks_cannot_be_turned_off_by_the_user() {
    if !has_python() {
        return;
    }
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(sse_text("ok"))
        .mount(&server)
        .await;
    let tmp = tempfile::tempdir().expect("tempdir");
    hook(
        &tmp.path().join("system"),
        "audit",
        "event = \"user_prompt\"",
        "print('POLICY: follow the corporate guidelines')\n",
    );
    let user = tmp.path().join("user");
    std::fs::create_dir_all(user.join("hooks.d")).expect("mkdir");
    std::fs::write(user.join("hooks.d").join("audit.toml"), "enabled = false\n").expect("file");
    std::fs::write(
        user.join("config.toml"),
        "[hooks]\ndisabled = [\"audit\"]\n",
    )
    .expect("file");
    let loaded = common::workspace(tmp.path(), &server.uri(), r#"["tiny"]"#, "");
    let mut engine = Engine::spawn(Arc::new(loaded));
    turn(&mut engine, "hello").await;
    let sent = bodies(&server).await;
    assert!(
        sent[0].contains("POLICY: follow the corporate guidelines"),
        "{}",
        sent[0]
    );
    engine.shutdown().await;
}

#[tokio::test]
async fn session_start_context_rides_on_the_first_prompt_only() {
    if !has_python() {
        return;
    }
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(sse_text("ok"))
        .mount(&server)
        .await;
    let tmp = tempfile::tempdir().expect("tempdir");
    hook(
        &tmp.path().join("user"),
        "standup",
        "event = \"session_start\"",
        "import json, sys\nprint('SESSION NOTE (' + json.load(sys.stdin)['source'] + ')')\n",
    );
    let loaded = common::workspace(tmp.path(), &server.uri(), r#"["tiny"]"#, "");
    let mut engine = Engine::spawn(Arc::new(loaded));
    turn(&mut engine, "first").await;
    turn(&mut engine, "second").await;
    let sent = bodies(&server).await;
    assert!(
        sent[0].contains("first\\n\\nSESSION NOTE (startup)"),
        "{}",
        sent[0]
    );
    assert_eq!(sent[1].matches("SESSION NOTE").count(), 1, "{}", sent[1]);
    engine.shutdown().await;
}

/// The shipped Jira example, end to end: the prompt links to an issue, `jira.py` fetches
/// it from a mock Jira with the token, and the model gets it as XML.
#[tokio::test]
async fn shipped_jira_hook_fetches_linked_issues() {
    if !has_python() {
        return;
    }
    let jira = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rest/api/2/issue/PAY-1234"))
        .and(header("authorization", "Bearer pat-123"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "key": "PAY-1234",
            "fields": {
                "summary": "Checkout fails on retry",
                "issuetype": {"name": "Bug"},
                "status": {"name": "In Progress"},
                "assignee": {"displayName": "Dana Lee"},
                "labels": ["payments"],
                "description": "Retrying a declined card returns 500 & logs <nil>.",
                "comment": {"comments": [
                    {"author": {"displayName": "Sam"}, "created": "2026-10-01", "body": "Seen in prod."}
                ]},
                "issuelinks": [{
                    "type": {"outward": "blocks"},
                    "outwardIssue": {"key": "PAY-1300", "fields": {"summary": "Release 4.2", "status": {"name": "Open"}}}
                }]
            }
        })))
        .mount(&jira)
        .await;
    Mock::given(method("GET"))
        .and(path("/rest/api/2/issue/PAY-9"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&jira)
        .await;
    let model = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(sse_text("ok"))
        .mount(&model)
        .await;

    let tmp = tempfile::tempdir().expect("tempdir");
    let shipped = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../defaults/hooks.d");
    let hooks = tmp.path().join("user").join("hooks.d");
    std::fs::create_dir_all(hooks.join("jira")).expect("mkdir");
    std::fs::copy(shipped.join("jira/jira.py"), hooks.join("jira/jira.py")).expect("script");
    // Enabled, pointed at the mock, with a token written in (tests never read real env).
    let toml = std::fs::read_to_string(shipped.join("jira.toml"))
        .expect("jira.toml")
        .replace("enabled = false\n", "")
        .replace("https://jira.example.com", &jira.uri())
        .replace("{ env = \"JIRA_TOKEN\" }", "\"pat-123\"");
    std::fs::write(hooks.join("jira.toml"), toml).expect("hook");

    let loaded = common::workspace(tmp.path(), &model.uri(), r#"["tiny"]"#, "");
    let mut engine = Engine::spawn(Arc::new(loaded));
    let prompt = format!(
        "why is {0}/browse/PAY-1234 failing? see also {0}/browse/PAY-9 and \
         https://other.example/browse/OPS-1",
        jira.uri()
    );
    let events = run_turn(&mut engine, &prompt).await;
    assert!(
        events.iter().any(
            |e| matches!(e, Event::HookRan { id, outcome, message: Some(m), .. }
            if id == "jira" && outcome == "context"
                && m == "fetched PAY-1234; could not fetch PAY-9")
        ),
        "{events:?}"
    );
    let sent = bodies(&model).await;
    let body = &sent[0];
    for part in [
        "<jira_issue key=\\\"PAY-1234\\\"",
        "<summary>Checkout fails on retry</summary>",
        "<status>In Progress</status>",
        "<assignee>Dana Lee</assignee>",
        "returns 500 &amp; logs &lt;nil&gt;.",
        "<comment author=\\\"Sam\\\"",
        "relation=\\\"blocks\\\" key=\\\"PAY-1300\\\"",
        "<jira_issue key=\\\"PAY-9\\\" unavailable=\\\"not found, or no permission to see it\\\" />",
    ] {
        assert!(body.contains(part), "missing {part} in {body}");
    }
    assert!(
        !body.contains("OPS-1\\\""),
        "links to another Jira are skipped"
    );
    engine.shutdown().await;
}

/// A cloned repository's `.quack/hooks.d` runs nothing until the project is trusted.
#[tokio::test]
async fn an_untrusted_projects_hooks_do_not_run() {
    if !has_python() {
        return;
    }
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(sse_text("ok"))
        .mount(&server)
        .await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let marker = tmp.path().join("ran");
    hook(
        &tmp.path().join("ws").join(".quack"),
        "evil",
        "event = \"user_prompt\"",
        &format!(
            "open({:?}, 'w').write('x')\nprint('INJECTED')\n",
            marker.display().to_string()
        ),
    );
    let loaded = common::workspace(tmp.path(), &server.uri(), r#"["tiny"]"#, "");
    let mut engine = Engine::spawn(Arc::new(loaded));
    turn(&mut engine, "hello").await;
    assert!(!marker.exists(), "the project's hook ran");
    assert!(!bodies(&server).await[0].contains("INJECTED"));
    engine.shutdown().await;
}
