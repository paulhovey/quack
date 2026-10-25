//! `quack acp` end to end: an editor's JSON-RPC over a pipe, the real engine, and a mock
//! model server.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use quack_acp::{SessionStart, Spawn};
use quack_config::{LoadOptions, load_with};
use quack_core::Engine;
use serde_json::{Value, json};
use tokio::io::{
    AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, Lines, ReadHalf, WriteHalf,
};
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn sse(chunks: &[&str]) -> ResponseTemplate {
    let body: String = chunks.iter().map(|c| format!("data: {c}\n\n")).collect();
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(body + "data: [DONE]\n\n")
}

/// Isolated config, data and cache dirs under `root` with one `mock` provider; sessions
/// start wherever the editor says.
fn spawner(root: &Path, base_url: &str) -> Spawn {
    let user = root.join("user");
    std::fs::create_dir_all(user.join("providers.d")).expect("mkdir");
    std::fs::write(
        user.join("providers.d").join("mock.toml"),
        format!("base_url = \"{base_url}/v1\"\nmodels = [\"tiny\"]\n"),
    )
    .expect("provider");
    let mut vars: HashMap<String, OsString> = HashMap::new();
    vars.insert("QUACK_CONFIG_DIR".into(), user.into_os_string());
    for (k, sub) in [
        ("QUACK_SYSTEM_DIR", "system"),
        ("QUACK_DATA_DIR", "data"),
        ("QUACK_CACHE_DIR", "cache"),
    ] {
        vars.insert(k.into(), root.join(sub).into_os_string());
    }
    vars.insert("QUACK_DEFAULTS_DIR".into(), "none".into());
    Arc::new(move |start: SessionStart| {
        let mut opts = LoadOptions::new(start.cwd);
        opts.mcp = start.mcp;
        let loaded = load_with(&|k| vars.get(k).cloned(), &opts).map_err(|e| e.to_string())?;
        Ok(Engine::spawn_with(Arc::new(loaded), start.resume))
    })
}

fn workspace(root: &Path) -> PathBuf {
    let ws = root.join("ws");
    std::fs::create_dir_all(ws.join(".git")).expect("mkdir");
    ws
}

/// The editor's end of the connection.
struct Editor {
    w: WriteHalf<DuplexStream>,
    lines: Lines<BufReader<ReadHalf<DuplexStream>>>,
    next_id: i64,
    server: tokio::task::JoinHandle<std::io::Result<()>>,
}

impl Editor {
    fn connect(spawn: Spawn) -> Self {
        let (ours, theirs) = tokio::io::duplex(1 << 20);
        let (r, w) = tokio::io::split(theirs);
        let server = tokio::spawn(quack_acp::serve(r, w, spawn));
        let (r, w) = tokio::io::split(ours);
        Self {
            w,
            lines: BufReader::new(r).lines(),
            next_id: 1,
            server,
        }
    }

    async fn send(&mut self, msg: Value) {
        let line = msg.to_string() + "\n";
        self.w.write_all(line.as_bytes()).await.expect("write");
    }

    async fn request(&mut self, method: &str, params: Value) -> i64 {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await;
        id
    }

    async fn next(&mut self) -> Value {
        let line = tokio::time::timeout(Duration::from_secs(10), self.lines.next_line())
            .await
            .expect("a message within 10s")
            .expect("read")
            .expect("server still connected");
        serde_json::from_str(&line).expect("JSON")
    }

    /// Messages until the response to `id`, then that response. Permission requests are
    /// answered with `choose`.
    async fn until(&mut self, id: i64, choose: &str) -> (Vec<Value>, Value) {
        let mut seen = Vec::new();
        loop {
            let msg = self.next().await;
            if msg.get("method").is_none() && msg["id"] == id {
                return (seen, msg);
            }
            if msg["method"] == "session/request_permission" {
                let answer = json!({"outcome": {"outcome": "selected", "optionId": choose}});
                self.send(json!({"jsonrpc": "2.0", "id": msg["id"], "result": answer}))
                    .await;
            }
            seen.push(msg);
        }
    }

    async fn call(&mut self, method: &str, params: Value) -> Value {
        let id = self.request(method, params).await;
        let (_, reply) = self.until(id, "deny").await;
        reply
    }

    async fn close(self) {
        drop(self.w);
        drop(self.lines);
        let ended = tokio::time::timeout(Duration::from_secs(10), self.server)
            .await
            .expect("server ends when the editor hangs up");
        ended.expect("join").expect("clean end");
    }
}

/// The text of the `session/update`s of one kind, joined.
fn updates<'a>(msgs: &'a [Value], kind: &str) -> Vec<&'a Value> {
    msgs.iter()
        .filter(|m| {
            m["method"] == "session/update" && m["params"]["update"]["sessionUpdate"] == kind
        })
        .map(|m| &m["params"]["update"])
        .collect()
}

fn text_of(msgs: &[Value], kind: &str) -> String {
    updates(msgs, kind)
        .iter()
        .filter_map(|u| u["content"]["text"].as_str())
        .collect()
}

#[tokio::test]
async fn a_prompt_streams_the_answer_and_ends_the_turn() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(sse(&[
            r#"{"choices":[{"delta":{"content":"Hello "}}]}"#,
            r#"{"choices":[{"delta":{"content":"there."},"finish_reason":"stop"}]}"#,
        ]))
        .mount(&server)
        .await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let ws = workspace(tmp.path());
    let mut editor = Editor::connect(spawner(tmp.path(), &server.uri()));

    let init = editor
        .call(
            "initialize",
            json!({"protocolVersion": 1, "clientCapabilities": {}}),
        )
        .await;
    assert_eq!(init["result"]["protocolVersion"], 1, "{init}");
    assert_eq!(init["result"]["agentCapabilities"]["loadSession"], true);
    assert_eq!(init["result"]["agentInfo"]["name"], "quack");

    let new = editor
        .call("session/new", json!({"cwd": ws, "mcpServers": []}))
        .await;
    let session = new["result"]["sessionId"]
        .as_str()
        .expect("session id")
        .to_owned();
    assert_eq!(new["result"]["modes"]["currentModeId"], "ask", "{new}");

    let id = editor
        .request(
            "session/prompt",
            json!({"sessionId": session, "prompt": [{"type": "text", "text": "hi"}]}),
        )
        .await;
    let (seen, reply) = editor.until(id, "deny").await;
    assert_eq!(reply["result"]["stopReason"], "end_turn", "{reply}");
    assert_eq!(text_of(&seen, "agent_message_chunk"), "Hello there.");
    assert!(
        seen.iter().all(|m| m["params"]["sessionId"] == session),
        "every update names the session"
    );
    let requests = server.received_requests().await.expect("requests");
    let chat = requests
        .iter()
        .find(|r| r.url.path().ends_with("/chat/completions"))
        .expect("a chat request");
    assert!(
        String::from_utf8_lossy(&chat.body).contains("\"hi\""),
        "the prompt reached the model"
    );

    let unknown = editor.call("session/fork", json!({})).await;
    assert_eq!(unknown["error"]["code"], -32601, "{unknown}");
    let missing = editor
        .call("session/prompt", json!({"sessionId": "nope", "prompt": []}))
        .await;
    assert_eq!(missing["error"]["code"], -32602, "{missing}");
    editor.close().await;
}

#[tokio::test]
async fn approvals_go_to_the_editor_and_load_replays_the_session() {
    let server = MockServer::start().await;
    // Step 2: the tool result is in the request; the model answers.
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_string_contains("\"role\":\"tool\""))
        .respond_with(sse(&[
            r#"{"choices":[{"delta":{"content":"Done."},"finish_reason":"stop"}]}"#,
        ]))
        .mount(&server)
        .await;
    // Step 1: the model writes a file, which asks in `ask` mode.
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(sse(&[
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"write","arguments":"{\"path\":\"notes.txt\",\"content\":\"hi\\n\"}"}}]},"finish_reason":"tool_calls"}]}"#,
        ]))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let ws = workspace(tmp.path());
    let spawn = spawner(tmp.path(), &server.uri());
    let mut editor = Editor::connect(spawn.clone());
    editor
        .call("initialize", json!({"protocolVersion": 1}))
        .await;
    let new = editor
        .call("session/new", json!({"cwd": ws, "mcpServers": []}))
        .await;
    let session = new["result"]["sessionId"]
        .as_str()
        .expect("session id")
        .to_owned();

    let id = editor
        .request(
            "session/prompt",
            json!({"sessionId": session, "prompt": [{"type": "text", "text": "write notes"}]}),
        )
        .await;
    let (seen, reply) = editor.until(id, "allow").await;
    assert_eq!(reply["result"]["stopReason"], "end_turn", "{reply}");
    let asked: Vec<_> = seen
        .iter()
        .filter(|m| m["method"] == "session/request_permission")
        .collect();
    assert_eq!(asked.len(), 1, "one approval");
    let ask = &asked[0]["params"];
    assert_eq!(ask["sessionId"], session);
    assert!(
        ask["toolCall"]["content"][0]["content"]["text"]
            .as_str()
            .is_some_and(|t| t.contains("+hi")),
        "the diff is shown: {ask}"
    );
    let ids: Vec<_> = ask["options"]
        .as_array()
        .map(|o| o.iter().map(|o| o["optionId"].clone()).collect())
        .unwrap_or_default();
    assert_eq!(
        ids,
        ["allow", "allow-session", "allow-always", "deny"],
        "{ask}"
    );
    assert!(
        ask["options"][2]["name"]
            .as_str()
            .is_some_and(|n| n.contains("`edit:notes.txt`")),
        "always names its rule: {ask}"
    );
    let calls = updates(&seen, "tool_call");
    assert_eq!(calls.len(), 1, "the call is announced once");
    assert_eq!(calls[0]["kind"], "edit");
    let call_id = calls[0]["toolCallId"].clone();
    assert_eq!(ask["toolCall"]["toolCallId"], call_id);
    let done = updates(&seen, "tool_call_update");
    assert!(
        done.iter()
            .any(|u| u["toolCallId"] == call_id && u["status"] == "completed"),
        "{done:?}"
    );
    assert_eq!(text_of(&seen, "agent_message_chunk"), "Done.");
    assert_eq!(
        std::fs::read_to_string(ws.join("notes.txt")).expect("written"),
        "hi\n"
    );
    editor.close().await;

    // A new connection loads the saved session: the transcript is replayed first.
    let mut editor = Editor::connect(spawn);
    editor
        .call("initialize", json!({"protocolVersion": 1}))
        .await;
    let id = editor
        .request(
            "session/load",
            json!({"sessionId": session, "cwd": ws, "mcpServers": []}),
        )
        .await;
    let (seen, reply) = editor.until(id, "deny").await;
    assert!(reply.get("result").is_some(), "{reply}");
    assert_eq!(text_of(&seen, "user_message_chunk"), "write notes");
    assert_eq!(text_of(&seen, "agent_message_chunk"), "Done.");
    assert_eq!(updates(&seen, "tool_call").len(), 1, "{seen:?}");

    let gone = editor
        .call(
            "session/load",
            json!({"sessionId": "no-such-session", "cwd": ws, "mcpServers": []}),
        )
        .await;
    assert!(gone.get("error").is_some(), "{gone}");
    editor.close().await;
}

#[tokio::test]
async fn cancel_stops_the_turn_with_cancelled() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            sse(&[r#"{"choices":[{"delta":{"content":"late"},"finish_reason":"stop"}]}"#])
                .set_delay(Duration::from_secs(30)),
        )
        .mount(&server)
        .await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let ws = workspace(tmp.path());
    let mut editor = Editor::connect(spawner(tmp.path(), &server.uri()));
    editor
        .call("initialize", json!({"protocolVersion": 1}))
        .await;
    let new = editor
        .call("session/new", json!({"cwd": ws, "mcpServers": []}))
        .await;
    let session = new["result"]["sessionId"]
        .as_str()
        .expect("session id")
        .to_owned();
    let id = editor
        .request(
            "session/prompt",
            json!({"sessionId": session, "prompt": [{"type": "text", "text": "slow"}]}),
        )
        .await;
    // Cancel once the model has the request.
    for _ in 0..100 {
        let asked = server.received_requests().await.unwrap_or_default();
        if asked
            .iter()
            .any(|r| r.url.path().ends_with("/chat/completions"))
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    editor
        .send(
            json!({"jsonrpc": "2.0", "method": "session/cancel", "params": {"sessionId": session}}),
        )
        .await;
    let (_, reply) = editor.until(id, "deny").await;
    assert_eq!(reply["result"]["stopReason"], "cancelled", "{reply}");
    editor.close().await;
}
