//! Sub-agents (`task`): they run on a fresh context with their own tools, only their report
//! reaches the parent, plan mode runs only read-only ones, read-only ones run side by side,
//! their approvals reach the client, and a task can tick off a plan item.

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use common::{bodies, sse_call, sse_raw, sse_text};
use quack_core::protocol::{ApprovalDecision, Event, InputItem, Op, TurnOverrides};
use quack_core::{Engine, EngineHandle};
use wiremock::matchers::{body_string_contains, method};
use wiremock::{Mock, MockServer};

/// `explore` (read-only, read and grep only) and `general`, in the user layer.
fn agents(root: &Path) {
    let dir = root.join("user").join("agents.d");
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(
        dir.join("explore.toml"),
        "description = \"Finds things\"\nread_only = true\ntools = [\"read\", \"grep\"]\nprompt = \"SUBAGENT explore\"\n",
    )
    .expect("explore");
    std::fs::write(
        dir.join("general.toml"),
        "description = \"Does work\"\nprompt = \"SUBAGENT general\"\n",
    )
    .expect("general");
}

async fn start(server: &MockServer, root: &Path) -> EngineHandle {
    agents(root);
    let loaded = common::workspace(root, &server.uri(), r#"["tiny"]"#, "");
    Engine::spawn(Arc::new(loaded))
}

async fn submit(engine: &EngineHandle, text: &str) {
    engine
        .submit(Op::UserTurn {
            items: vec![InputItem::text(text)],
            overrides: TurnOverrides::default(),
        })
        .await;
}

/// Events until the turn ends, approving every request on the way.
async fn run_approving(engine: &mut EngineHandle) -> Vec<Event> {
    let mut seen = Vec::new();
    loop {
        let e = tokio::time::timeout(Duration::from_secs(10), engine.events.recv())
            .await
            .expect("event within 10s")
            .expect("channel open");
        match &e {
            Event::ApprovalRequest { call_id, .. } => {
                engine
                    .submit(Op::ApprovalResponse {
                        call_id: call_id.clone(),
                        decision: ApprovalDecision::AllowOnce,
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

fn find_file(dir: &Path, name: &str) -> Option<std::path::PathBuf> {
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        if p.is_dir() {
            if let Some(f) = find_file(&p, name) {
                return Some(f);
            }
        } else if p.file_name().is_some_and(|n| n == name) {
            return Some(p);
        }
    }
    None
}

#[tokio::test]
async fn an_agent_reports_back_and_ticks_off_its_plan_item() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_string_contains("SUBAGENT explore"))
        .respond_with(sse_text("It is in a.rs:3."))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_string_contains("Report from the explore agent"))
        .respond_with(sse_text("All done."))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_string_contains("Plan updated"))
        .respond_with(sse_call(
            "t1",
            "task",
            serde_json::json!({"agent": "explore", "description": "find it", "prompt": "Where is the parser?", "todo": 1}),
        ))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(sse_call(
            "p1",
            "todo",
            serde_json::json!({"items": [{"text": "find the parser", "status": "pending"}]}),
        ))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().expect("tempdir");
    let mut engine = start(&server, tmp.path()).await;
    submit(&engine, "user request text").await;
    let events = run_approving(&mut engine).await;

    let started = events
        .iter()
        .find_map(|e| match e {
            Event::AgentStarted {
                id,
                agent,
                description,
                call_id,
                ..
            } => Some((
                id.clone(),
                agent.clone(),
                description.clone(),
                call_id.0.clone(),
            )),
            _ => None,
        })
        .expect("the agent started");
    assert_eq!(
        started,
        (
            "explore-1".into(),
            "explore".into(),
            "find it".into(),
            "t1".into()
        )
    );
    assert!(events.iter().any(|e| matches!(
        e,
        Event::Agent { id, inner } if id == "explore-1"
            && matches!(**inner, Event::AssistantDelta { .. })
    )));
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::AgentFinished { ok: true, .. }))
    );
    // The sub-agent's own text never shows as the conversation's.
    assert!(!events.iter().any(|e| matches!(
        e,
        Event::AssistantDelta { text } if text.contains("a.rs:3")
    )));
    let plans: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            Event::Plan { checklist } => Some(checklist.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        plans,
        [
            "[ ] find the parser",
            "[~] find the parser",
            "[x] find the parser"
        ],
        "set by todo, in progress while the agent works, then done"
    );

    let bodies = bodies(&server).await;
    assert!(
        bodies[0].contains("explore (read-only): Finds things"),
        "{}",
        bodies[0]
    );
    let sub = bodies
        .iter()
        .find(|b| b.contains("SUBAGENT explore"))
        .expect("sub-agent request");
    assert!(sub.contains("\"name\":\"grep\""), "{sub}");
    for absent in [
        "\"name\":\"task\"",
        "\"name\":\"write\"",
        "\"name\":\"todo\"",
    ] {
        assert!(!sub.contains(absent), "{absent} in {sub}");
    }
    assert!(
        !sub.contains("user request text"),
        "the agent sees only its prompt"
    );
    let last = bodies.last().expect("final request");
    assert!(last.contains("It is in a.rs:3.") && last.contains("Plan item marked done"));
    assert!(!last.contains("SUBAGENT"));

    let log = find_file(&tmp.path().join("data"), "explore-1.jsonl").expect("agent log");
    let text = std::fs::read_to_string(log).expect("read log");
    assert!(text.contains("Where is the parser?") && text.contains("It is in a.rs:3."));
    engine.shutdown().await;
}

#[tokio::test]
async fn plan_mode_runs_only_read_only_agents() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_string_contains("\"role\":\"tool\""))
        .respond_with(sse_text("Planned."))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(sse_call(
            "t1",
            "task",
            serde_json::json!({"agent": "general", "prompt": "change things"}),
        ))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut engine = start(&server, tmp.path()).await;
    engine
        .submit(Op::SetPermissionMode {
            mode: "plan".into(),
        })
        .await;
    submit(&engine, "plan it").await;
    let events = run_approving(&mut engine).await;
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, Event::AgentStarted { .. }))
    );
    let result = events
        .iter()
        .find_map(|e| match e {
            Event::ToolCallEnd { result, .. } => Some(result.clone()),
            _ => None,
        })
        .expect("task result");
    assert!(
        !result.ok && result.summary.contains("only read-only agents"),
        "{result:?}"
    );
    let first = bodies(&server).await.remove(0);
    assert!(
        first.contains("hand searches to read-only agents (explore) with task"),
        "{first}"
    );
    engine.shutdown().await;
}

#[tokio::test]
async fn read_only_agents_started_together_run_side_by_side() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_string_contains("SUBAGENT explore"))
        .respond_with(sse_text("found").set_delay(Duration::from_millis(300)))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_string_contains("\"role\":\"tool\""))
        .respond_with(sse_text("Both done."))
        .mount(&server)
        .await;
    let call = |i: u32, id: &str, prompt: &str| {
        serde_json::json!({"choices":[{"delta":{"tool_calls":[{"index":i,"id":id,"function":{
            "name":"task",
            "arguments": serde_json::json!({"agent":"explore","prompt":prompt}).to_string()
        }}]}}]})
        .to_string()
    };
    let end =
        serde_json::json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}).to_string();
    Mock::given(method("POST"))
        .respond_with(sse_raw(&[
            call(0, "a", "look in src"),
            call(1, "b", "look in tests"),
            end,
        ]))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut engine = start(&server, tmp.path()).await;
    submit(&engine, "explore").await;
    let events = run_approving(&mut engine).await;
    let order: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            Event::AgentStarted { .. } => Some("start"),
            Event::AgentFinished { .. } => Some("end"),
            _ => None,
        })
        .collect();
    assert_eq!(order, ["start", "start", "end", "end"]);
    let last = bodies(&server).await.pop().expect("final request");
    let a = last.find("\"tool_call_id\":\"a\"").expect("result a");
    let b = last.find("\"tool_call_id\":\"b\"").expect("result b");
    assert!(a < b, "results in the order of the calls");
    engine.shutdown().await;
}

#[tokio::test]
async fn a_working_agent_asks_for_approval_through_the_client() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_string_contains("SUBAGENT general"))
        .and(body_string_contains("\"role\":\"tool\""))
        .respond_with(sse_text("Wrote notes.txt."))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_string_contains("SUBAGENT general"))
        .respond_with(sse_call(
            "w1",
            "write",
            serde_json::json!({"path": "notes.txt", "content": "hi"}),
        ))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_string_contains("Report from the general agent"))
        .respond_with(sse_text("Done."))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(sse_call(
            "t1",
            "task",
            serde_json::json!({"agent": "general", "prompt": "write notes.txt"}),
        ))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut engine = start(&server, tmp.path()).await;
    let ws = engine.config().dirs.workspace_root.clone();
    submit(&engine, "write notes").await;
    let events = run_approving(&mut engine).await;
    let asked: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            Event::ApprovalRequest { call_id, .. } => Some(call_id.0.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(asked, ["general-1/w1"]);
    // The agent's second request extends its first, so the server's cache holds.
    let sub: Vec<serde_json::Value> = bodies(&server)
        .await
        .iter()
        .filter(|b| b.contains("SUBAGENT general"))
        .map(|b| serde_json::from_str(b).expect("json"))
        .collect();
    assert_eq!(sub.len(), 2);
    assert_eq!(sub[0]["tools"], sub[1]["tools"]);
    let (first, second) = (
        sub[0]["messages"].as_array().expect("messages"),
        sub[1]["messages"].as_array().expect("messages"),
    );
    assert_eq!(first[..], second[..first.len()]);
    assert_eq!(
        std::fs::read_to_string(ws.join("notes.txt")).expect("written"),
        "hi"
    );
    engine.shutdown().await;
}

async fn stats(engine: &mut EngineHandle) -> quack_core::protocol::StatsReport {
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
async fn an_agents_usage_survives_a_resume() {
    let usage = |p: u32, c: u32| {
        serde_json::json!({"choices":[],"usage":{"prompt_tokens":p,"completion_tokens":c}})
            .to_string()
    };
    let text = |t: &str| {
        serde_json::json!({"choices":[{"delta":{"content":t},"finish_reason":"stop"}]}).to_string()
    };
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_string_contains("SUBAGENT explore"))
        .respond_with(sse_raw(&[text("found"), usage(1000, 100)]))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_string_contains("\"role\":\"tool\""))
        .respond_with(sse_raw(&[text("Done."), usage(300, 10)]))
        .mount(&server)
        .await;
    let call =
        serde_json::json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"t1","function":{
        "name":"task",
        "arguments": serde_json::json!({"agent":"explore","prompt":"look"}).to_string()
    }}]},"finish_reason":"tool_calls"}]})
        .to_string();
    Mock::given(method("POST"))
        .respond_with(sse_raw(&[call, usage(200, 20)]))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    let models = r#"[{ id = "tiny", cost = { input = 1.0, output = 2.0 } }]"#;
    let tmp = tempfile::tempdir().expect("tempdir");
    agents(tmp.path());
    let loaded = common::workspace(tmp.path(), &server.uri(), models, "");
    let mut engine = Engine::spawn(Arc::new(loaded));
    submit(&engine, "explore").await;
    run_approving(&mut engine).await;
    let live = stats(&mut engine).await;
    assert_eq!((live.requests, live.prompt_tokens), (3, 1500));
    engine.shutdown().await;

    let loaded = common::workspace(tmp.path(), &server.uri(), models, "");
    let mut engine = Engine::spawn_with(Arc::new(loaded), quack_core::Resume::Latest);
    loop {
        let e = tokio::time::timeout(Duration::from_secs(10), engine.events.recv())
            .await
            .expect("event within 10s")
            .expect("channel open");
        if matches!(e, Event::SessionResumed { .. }) {
            break;
        }
    }
    let resumed = stats(&mut engine).await;
    assert!(resumed.resumed);
    assert_eq!(
        (
            resumed.requests,
            resumed.prompt_tokens,
            resumed.completion_tokens
        ),
        (live.requests, live.prompt_tokens, live.completion_tokens)
    );
    let (a, b) = (
        live.cost.expect("priced"),
        resumed.cost.expect("priced in the log"),
    );
    assert!((a - b).abs() < 1e-12, "{a} vs {b}");
    engine.shutdown().await;
}
