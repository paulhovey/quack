//! MCP servers in a session: tools join the request once the server is ready, calls go
//! through permissions, and large tool sets hide behind `tool_search`.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{bodies, sse_call, sse_text, turn};
use quack_core::protocol::{Event, LoadState, Op, Subsystem};
use quack_core::{Engine, EngineHandle};
use serde_json::{Value, json};
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

/// A stateless MCP server over streamable HTTP that speaks the `initialize` handshake.
struct McpServer;

impl Respond for McpServer {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let Ok(msg) = serde_json::from_slice::<Value>(&req.body) else {
            return ResponseTemplate::new(400);
        };
        let Some(id) = msg.get("id").cloned() else {
            return ResponseTemplate::new(202); // a notification
        };
        let params = msg.get("params").cloned().unwrap_or_default();
        let result = match msg["method"].as_str().unwrap_or_default() {
            "initialize" => json!({
                "protocolVersion": params["protocolVersion"],
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "fx", "version": "1.0.0"}
            }),
            "tools/list" => json!({"tools": [
                {"name": "echo", "description": "Echo the text back.",
                 "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}},
                 "annotations": {"readOnlyHint": true}},
                {"name": "drop_table", "description": "Deletes a table.",
                 "inputSchema": {"type": "object", "properties": {"name": {"type": "string"}}}}
            ]}),
            "tools/call" => json!({"content": [{"type": "text",
                "text": format!("echo: {}", params["arguments"]["text"].as_str().unwrap_or_default())}]}),
            other => {
                return ResponseTemplate::new(200)
                    .set_body_json(json!({"jsonrpc": "2.0", "id": id,
                    "error": {"code": -32601, "message": format!("method not found: {other}")}}));
            }
        };
        ResponseTemplate::new(200)
            .set_body_json(json!({"jsonrpc": "2.0", "id": id, "result": result}))
    }
}

async fn mcp_server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/mcp"))
        .respond_with(McpServer)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(405))
        .mount(&server)
        .await;
    server
}

async fn mcp_ready(engine: &mut EngineHandle) {
    loop {
        let e = tokio::time::timeout(Duration::from_secs(15), engine.events.recv())
            .await
            .expect("MCP ready within 15s")
            .expect("open");
        match e {
            Event::StartupProgress {
                subsystem: Subsystem::Mcp,
                state: LoadState::Ready { .. },
            } => return,
            Event::StartupProgress {
                subsystem: Subsystem::Mcp,
                state: LoadState::NeedsAttention { detail },
            } => panic!("MCP failed: {detail}"),
            _ => {}
        }
    }
}

fn add_server(root: &std::path::Path, uri: &str, extra: &str) {
    let dir = root.join("user/mcp.d");
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(dir.join("fx.toml"), format!("url = \"{uri}/mcp\"\n{extra}")).expect("write");
}

#[tokio::test]
async fn mcp_tools_are_offered_and_called() {
    let mcp = mcp_server().await;
    let llm = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_string_contains("echo: hi"))
        .respond_with(sse_text("The server said hi."))
        .mount(&llm)
        .await;
    Mock::given(method("POST"))
        .respond_with(sse_call("c1", "mcp__fx__echo", json!({"text": "hi"})))
        .up_to_n_times(1)
        .mount(&llm)
        .await;

    let tmp = tempfile::tempdir().expect("tempdir");
    let loaded = common::workspace(tmp.path(), &llm.uri(), r#"["tiny"]"#, "");
    add_server(
        tmp.path(),
        &mcp.uri(),
        "[tools]\nauto_approve = [\"echo\"]\n",
    );
    let mut engine = Engine::spawn(Arc::new(loaded));
    mcp_ready(&mut engine).await;

    let events = turn(&mut engine, "say hi through the server").await;
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::ToolCallEnd { result, .. } if result.ok)),
        "{events:?}"
    );
    let first = bodies(&llm).await.remove(0);
    assert!(first.contains("\"mcp__fx__echo\""), "{first}");
    assert!(first.contains("\"mcp__fx__drop_table\""), "{first}");

    engine.submit(Op::ListMcp).await;
    let servers = loop {
        if let Event::McpServers { servers } = engine.events.recv().await.expect("open") {
            break servers;
        }
    };
    assert_eq!(servers[0].state, "ready");
    assert_eq!(servers[0].tools, ["echo", "drop_table"]);
    assert!(!servers[0].deferred);
    engine.shutdown().await;
}

#[tokio::test]
async fn large_tool_sets_are_found_with_tool_search() {
    let mcp = mcp_server().await;
    let llm = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_string_contains("echo: found"))
        .respond_with(sse_text("done"))
        .mount(&llm)
        .await;
    Mock::given(method("POST"))
        .and(body_string_contains("now available"))
        .respond_with(sse_call("c2", "mcp__fx__echo", json!({"text": "found"})))
        .mount(&llm)
        .await;
    Mock::given(method("POST"))
        .respond_with(sse_call("c1", "tool_search", json!({"query": "echo text"})))
        .up_to_n_times(1)
        .mount(&llm)
        .await;

    let tmp = tempfile::tempdir().expect("tempdir");
    // A 400-token window: the two tool definitions are over a tenth of the budget.
    let loaded = common::workspace(
        tmp.path(),
        &llm.uri(),
        r#"[{ id = "tiny", context_window = 400 }]"#,
        "",
    );
    add_server(tmp.path(), &mcp.uri(), "[tools]\nauto_approve = [\"*\"]\n");
    let mut engine = Engine::spawn(Arc::new(loaded));
    mcp_ready(&mut engine).await;

    turn(&mut engine, "use the echo tool").await;
    // The tiny window also makes the engine compact; leave out the summary requests.
    let sent: Vec<String> = bodies(&llm)
        .await
        .into_iter()
        .filter(|b| !b.contains("You summarize a coding session"))
        .collect();
    assert!(sent[0].contains("\"tool_search\""), "{}", sent[0]);
    assert!(!sent[0].contains("\"mcp__fx__echo\""), "{}", sent[0]);
    assert!(
        sent[1].contains("\"name\":\"mcp__fx__echo\""),
        "{}",
        sent[1]
    );
    assert!(
        !sent[1].contains("\"name\":\"mcp__fx__drop_table\""),
        "{}",
        sent[1]
    );
    assert!(sent[2].contains("echo: found"), "{}", sent[2]);
    engine.shutdown().await;
}
