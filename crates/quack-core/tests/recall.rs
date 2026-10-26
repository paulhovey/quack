//! `recall` against a mock server: a long command output is trimmed for the model, the
//! whole is kept under a handle, and `recall` pages through it, also after a resume.

mod common;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use common::{bodies, sse_call, sse_text, turn};
use quack_core::protocol::Event;
use quack_core::{Engine, Resume};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

/// Answers each request with the next reply in order.
struct Script(Mutex<VecDeque<ResponseTemplate>>);

impl Respond for Script {
    fn respond(&self, _: &Request) -> ResponseTemplate {
        self.0
            .lock()
            .ok()
            .and_then(|mut q| q.pop_front())
            .unwrap_or_else(|| sse_text("ok"))
    }
}

/// A session whose model replies with `script`, with `config` in the user layer, and
/// `lines` lines in the workspace's `big.txt`.
async fn session(
    config: &str,
    script: Vec<ResponseTemplate>,
    lines: usize,
) -> (MockServer, tempfile::TempDir, quack_config::Loaded) {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(Script(Mutex::new(script.into())))
        .mount(&server)
        .await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let user = tmp.path().join("user");
    std::fs::create_dir_all(&user).expect("mkdir");
    std::fs::write(
        user.join("config.toml"),
        format!("[permissions]\nmode = \"full-auto\"\n{config}"),
    )
    .expect("config");
    let loaded = common::workspace(
        tmp.path(),
        &server.uri(),
        r#"[{ id = "tiny", context_window = 200000 }]"#,
        "",
    );
    let big: String = (1..=lines)
        .map(|n| format!("line {n:04} {}\n", "x".repeat(30)))
        .collect();
    std::fs::write(tmp.path().join("ws").join("big.txt"), big).expect("big");
    (server, tmp, loaded)
}

/// Shows `big.txt`, recalls line 500, and after a resume the last two lines.
fn trimmed_script() -> Vec<ResponseTemplate> {
    vec![
        sse_call("c1", "shell", json!({ "command": "cat big.txt" })),
        sse_call(
            "c2",
            "recall",
            json!({ "id": "o1", "pattern": "line 0500" }),
        ),
        sse_text("done"),
        // After the resume.
        sse_call("c3", "recall", json!({ "id": "o1", "offset": 999 })),
        sse_text("done again"),
    ]
}

fn tools(body: &str) -> Vec<String> {
    let v: serde_json::Value = serde_json::from_str(body).expect("json");
    v["tools"]
        .as_array()
        .map(|t| {
            t.iter()
                .filter_map(|t| t["function"]["name"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn a_trimmed_output_can_be_recalled_after_a_resume() {
    let (server, _tmp, loaded) =
        session("[context]\nrecall = true\n", trimmed_script(), 1000).await;
    let loaded = Arc::new(loaded);
    let mut engine = Engine::spawn(loaded.clone());
    turn(&mut engine, "look at big.txt").await;
    engine.shutdown().await;

    let all = bodies(&server).await;
    assert!(tools(&all[0]).contains(&"recall".to_owned()));
    // The model saw the head and tail, and where the rest is.
    let shown = &all[1];
    assert!(shown.contains("lines omitted") && !shown.contains("line 0500"));
    assert!(shown.contains(r#"is kept: `recall` with id \"o1\" pages through it]"#));
    // The recalled line comes back with its number.
    let recalled = &all[2];
    assert!(recalled.contains("lines match /line 0500/"));
    assert!(recalled.contains(r"   500\tline 0500 xxx"));

    // A resumed session still has the output.
    let mut engine = Engine::spawn_with(loaded, Resume::Latest);
    loop {
        let e = engine.events.recv().await.expect("event");
        if matches!(e, Event::SessionResumed { .. }) {
            break;
        }
    }
    turn(&mut engine, "the last lines?").await;
    engine.shutdown().await;
    let last = bodies(&server).await.pop().expect("request");
    assert!(
        last.contains("lines 999-1000 of 100"),
        "{:.300}",
        &last[last.len() - 300..]
    );
    assert!(last.contains(r"  1000\tline 1000 xxx"));
}

#[tokio::test]
async fn without_recall_nothing_is_offered_or_kept() {
    let (server, _tmp, loaded) = session("", trimmed_script(), 1000).await;
    let mut engine = Engine::spawn(Arc::new(loaded));
    turn(&mut engine, "look at big.txt").await;
    engine.shutdown().await;
    let all = bodies(&server).await;
    assert!(!tools(&all[0]).contains(&"recall".to_owned()));
    assert!(all[1].contains("lines omitted") && !all[1].contains("recall"));
    // The model calls it anyway: an unknown tool, not a crash.
    assert!(all[2].contains("recall"));
}

#[tokio::test]
async fn an_output_pruning_shortens_can_be_recalled() {
    // `aggressive` prunes before every request; four more calls push the first result out
    // of the protected tail, and pruning shortens it to its head and tail.
    let mut script = vec![sse_call("c1", "shell", json!({ "command": "cat big.txt" }))];
    for n in 2..=5 {
        script.push(sse_call(
            &format!("c{n}"),
            "shell",
            json!({ "command": format!("echo step{n}") }),
        ));
    }
    script.push(sse_call(
        "c6",
        "recall",
        json!({ "id": "o1", "pattern": "line 0150" }),
    ));
    script.push(sse_text("done"));
    let config = "[context]\nrecall = true\ncache_strategy = \"aggressive\"\n";
    let (server, _tmp, loaded) = session(config, script, 300).await;
    let mut engine = Engine::spawn(Arc::new(loaded));
    let events = turn(&mut engine, "look at big.txt").await;
    engine.shutdown().await;
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::ContextPruned { .. }))
    );
    let all = bodies(&server).await;
    // Shown whole at first: 12 kB is under the shell's trim limit, so nothing was kept.
    assert!(all[1].contains("line 0150") && !all[1].contains("pages through it"));
    let pruned = &all[5];
    assert!(
        pruned.contains(r#"lines omitted; `recall` with id \"o1\" returns all of it]"#),
        "{pruned:.400}"
    );
    assert!(!pruned.contains("line 0150"));
    assert!(all[6].contains("lines match /line 0150/") && all[6].contains(r"\tline 0150 xxx"));
}
