//! Prefix stability (docs/plan/13-testing-and-evals.md#invariant-tests-must-never-regress):
//! for every shipped profile, the system prompt and tool block are byte-identical across
//! the requests of a session, and each request extends the one before it, so server KV
//! caches stay warm. Only the extras that ride on the last message (the plan) may differ,
//! and only compaction (an epoch boundary) may rewrite the history.

use std::collections::{HashMap, VecDeque};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use quack_config::{LoadOptions, Loaded, load_with};
use quack_core::protocol::{ApprovalDecision, Event, InputItem, Op, TurnOverrides};
use quack_core::{Engine, EngineHandle};
use serde_json::{Value, json};
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const SUMMARIZER: &str = "You summarize a coding session";

fn sse(chunks: &[Value]) -> ResponseTemplate {
    let body: String = chunks.iter().map(|c| format!("data: {c}\n\n")).collect();
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(body + "data: [DONE]\n\n")
}

fn text(t: &str) -> ResponseTemplate {
    sse(&[json!({"choices":[{"delta":{"content":t},"finish_reason":"stop"}]})])
}

/// The name a profile gives an internal tool.
fn named(profile: &str, internal: &str) -> String {
    let path = defaults_bundle()
        .join("profiles.d")
        .join(profile)
        .join("profile.toml");
    let file: quack_config::profile::ProfileFile =
        toml::from_str(&std::fs::read_to_string(path).expect("profile")).expect("profile toml");
    file.tools
        .iter()
        .find(|t| t.maps_to.as_deref().unwrap_or(&t.name) == internal)
        .map_or_else(|| internal.to_owned(), |t| t.name.clone())
}

fn call(id: &str, name: &str, args: Value) -> ResponseTemplate {
    sse(&[
        json!({"choices":[{"delta":{"content":"On it."}}]}),
        json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":id,"function":{"name":name,"arguments":args.to_string()}}]},"finish_reason":"tool_calls"}]}),
    ])
}

/// Replies in order, then "ok" for anything after.
struct Script(Mutex<VecDeque<ResponseTemplate>>);

impl Respond for Script {
    fn respond(&self, _: &Request) -> ResponseTemplate {
        self.0
            .lock()
            .ok()
            .and_then(|mut q| q.pop_front())
            .unwrap_or_else(|| text("ok"))
    }
}

fn defaults_bundle() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../defaults")
}

fn profiles() -> Vec<String> {
    let mut ids: Vec<String> = std::fs::read_dir(defaults_bundle().join("profiles.d"))
        .expect("profiles.d")
        .flatten()
        .filter(|e| e.path().join("profile.toml").is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    ids.sort();
    ids
}

fn configure(root: &Path, base_url: &str, profile: &str, config: &str) -> Loaded {
    let user = root.join("user");
    std::fs::create_dir_all(user.join("providers.d")).expect("mkdir");
    std::fs::write(
        user.join("providers.d").join("mock.toml"),
        format!(
            "base_url = \"{base_url}/v1\"\nmodels = [{{ id = \"tiny\", profile = \"{profile}\" }}]\n"
        ),
    )
    .expect("provider");
    std::fs::write(user.join("config.toml"), config).expect("config");
    let mut vars: HashMap<String, OsString> = HashMap::new();
    vars.insert("QUACK_CONFIG_DIR".into(), user.into_os_string());
    for (k, sub) in [
        ("QUACK_SYSTEM_DIR", "system"),
        ("QUACK_DATA_DIR", "data"),
        ("QUACK_CACHE_DIR", "cache"),
    ] {
        vars.insert(k.into(), root.join(sub).into_os_string());
    }
    vars.insert(
        "QUACK_DEFAULTS_DIR".into(),
        defaults_bundle().into_os_string(),
    );
    let ws = root.join("ws");
    std::fs::create_dir_all(&ws).expect("mkdir");
    load_with(&move |k| vars.get(k).cloned(), &LoadOptions::new(ws)).expect("load")
}

async fn next(engine: &mut EngineHandle) -> Event {
    tokio::time::timeout(Duration::from_secs(10), engine.events.recv())
        .await
        .expect("event within 10s")
        .expect("channel open")
}

/// Wait for the end of a turn (or `/compact`), approving whatever is asked.
async fn finish(engine: &mut EngineHandle) {
    loop {
        match next(engine).await {
            Event::ApprovalRequest { call_id, .. } => {
                engine
                    .submit(Op::ApprovalResponse {
                        call_id,
                        decision: ApprovalDecision::AllowOnce,
                    })
                    .await;
            }
            Event::TurnComplete { .. } => return,
            Event::TurnAborted { reason, .. } => panic!("aborted: {reason}"),
            Event::Error { error } => panic!("engine error: {}", error.message),
            _ => {}
        }
    }
}

async fn turn(engine: &mut EngineHandle, text: &str) {
    engine
        .submit(Op::UserTurn {
            items: vec![InputItem::text(text)],
            overrides: TurnOverrides::default(),
        })
        .await;
    finish(engine).await;
}

async fn mode(engine: &mut EngineHandle, mode: &str) {
    engine
        .submit(Op::SetPermissionMode { mode: mode.into() })
        .await;
    loop {
        if let Event::PermissionMode { .. } = next(engine).await {
            return;
        }
    }
}

/// A message without what may ride on the last message of a request: the plan reminder
/// appended to its text.
fn base(m: &Value, last: &Value) -> bool {
    if m == last {
        return true;
    }
    let (Some(a), Some(b)) = (m["content"].as_str(), last["content"].as_str()) else {
        return false;
    };
    let mut m = m.clone();
    let mut l = last.clone();
    m["content"] = Value::Null;
    l["content"] = Value::Null;
    m == l && b.starts_with(a)
}

/// Every request after the first extends the one before it: the same system prompt,
/// tools and parameters, and the earlier messages unchanged. The one before's last
/// message may have carried an extra (the plan) that is gone once it is not the last.
/// Returns whether the last message carried such an extra.
fn assert_extends(profile: &str, i: usize, before: &Value, after: &Value) -> bool {
    let strip = |b: &Value| {
        let mut b = b.clone();
        b["messages"] = Value::Null;
        b
    };
    assert_eq!(
        strip(before),
        strip(after),
        "{profile}: request {i} changed the tools or parameters"
    );
    let a = before["messages"].as_array().expect("messages");
    let b = after["messages"].as_array().expect("messages");
    assert!(b.len() > a.len(), "{profile}: request {i} dropped messages");
    let (last, earlier) = a.split_last().expect("a message");
    for (k, m) in earlier.iter().enumerate() {
        assert_eq!(
            m, &b[k],
            "{profile}: request {i} changed message {k}, before the newest"
        );
    }
    assert!(
        base(&b[earlier.len()], last),
        "{profile}: request {i} changed the newest message beyond its reminder:\n{last:#}\n{:#}",
        b[earlier.len()]
    );
    &b[earlier.len()] != last
}

async fn session(profile: &str, config: &str) {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_string_contains(SUMMARIZER))
        .respond_with(text("SUMMARY-TEXT"))
        .mount(&server)
        .await;
    let call = |id: &str, internal: &str, args: Value| call(id, &named(profile, internal), args);
    let script = [
        // Turn 1: write, read, a plan, a memory note.
        call(
            "c1",
            "write",
            json!({"path":"notes.txt","content":"one\ntwo\n"}),
        ),
        call("c2", "read", json!({"path":"notes.txt"})),
        call(
            "c3",
            "todo",
            json!({"items":[{"text":"edit notes","status":"in_progress"}]}),
        ),
        call(
            "c4",
            "memory",
            json!({"action":"create","scope":"project","name":"notes","description":"The notes file","type":"project","content":"notes.txt holds two lines."}),
        ),
        text("Written."),
        // Turn 2, in plan mode: a read-only look around.
        call("c5", "list_dir", json!({"path":"."})),
        text("Here is the plan."),
        // Turn 3: edit and a command.
        call(
            "c6",
            "edit",
            json!({"path":"notes.txt","old_string":"two","new_string":"three"}),
        ),
        call("c7", "shell", json!({"command":"echo done"})),
        text("Edited."),
        // Turn 4: a plain answer.
        text("Fine."),
    ];
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(Script(Mutex::new(script.into())))
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().expect("tempdir");
    let loaded = configure(tmp.path(), &server.uri(), profile, config);
    let mut engine = Engine::spawn(Arc::new(loaded));
    turn(&mut engine, "write notes").await;
    mode(&mut engine, "plan").await;
    turn(&mut engine, "plan the edit").await;
    mode(&mut engine, "full-auto").await;
    turn(&mut engine, "do the edit").await;
    turn(&mut engine, "anything else?").await;
    // An epoch boundary: the history is rewritten, the system prompt and tools are not.
    engine.submit(Op::Compact { focus: None }).await;
    finish(&mut engine).await;
    turn(&mut engine, "after compaction").await;
    engine.shutdown().await;

    let requests = server.received_requests().await.unwrap_or_default();
    let bodies: Vec<Value> = requests
        .iter()
        .filter(|r| r.url.path().ends_with("/chat/completions"))
        .filter(|r| !String::from_utf8_lossy(&r.body).contains(SUMMARIZER))
        .map(|r| serde_json::from_slice(&r.body).expect("json body"))
        .collect();
    assert_eq!(bodies.len(), 12, "{profile}: {} requests", bodies.len());
    let (live, compacted) = bodies.split_at(11);
    let mut extras = 0;
    for (i, pair) in live.windows(2).enumerate() {
        extras += usize::from(assert_extends(profile, i + 1, &pair[0], &pair[1]));
    }
    // The plan rode on some requests' last message and was gone from the next.
    assert!(extras > 0, "{profile}: no reminder was exercised");
    let system = |b: &Value| b["messages"][0].clone();
    let tools = |b: &Value| b["tools"].clone();
    assert_eq!(system(&live[0])["role"], "system", "{profile}");
    assert_eq!(
        system(&live[0]),
        system(&compacted[0]),
        "{profile}: compaction changed the system prompt"
    );
    assert_eq!(
        tools(&live[0]),
        tools(&compacted[0]),
        "{profile}: compaction changed the tools"
    );
    assert!(
        compacted[0].to_string().contains("SUMMARY-TEXT"),
        "{profile}: not compacted"
    );
}

#[tokio::test]
async fn every_profile_keeps_its_prefix_across_turns() {
    for p in profiles() {
        session(&p, "[permissions]\nmode = \"full-auto\"\n").await;
    }
}

#[tokio::test]
async fn hashline_edits_keep_the_prefix_too() {
    for p in profiles() {
        session(
            &p,
            "[permissions]\nmode = \"full-auto\"\n[tools]\nedit_format = \"hashline\"\n",
        )
        .await;
    }
}
