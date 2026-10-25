//! MCP servers in a session: tools join the request once the server is ready, calls go
//! through permissions, large tool sets hide behind `tool_search`, prompts run as slash
//! commands, resources are read with `read_mcp_resource`, and servers' forms and sampling
//! requests reach the user and the model.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{bodies, sse_call, sse_text, turn};
use quack_core::protocol::{
    Answer, ApprovalDecision, ApprovalRequest, Event, InputItem, LoadState, Op, Subsystem,
};
use quack_core::{Engine, EngineHandle};
use serde_json::{Value, json};
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

/// A stateless MCP server over streamable HTTP that speaks the `initialize` handshake.
/// With `extras`, it also offers a prompt and resources; with `asks`, the tools `deploy`
/// (asks the user through a form) and `summarize` (asks the client's model). Being
/// stateless, it asks the 2026-07-28 way: an `input_required` result, then the retry.
struct McpServer {
    extras: bool,
    asks: bool,
}

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
                "capabilities": if self.extras {
                    json!({"tools": {}, "prompts": {}, "resources": {}})
                } else {
                    json!({"tools": {}})
                },
                "serverInfo": {"name": "fx", "version": "1.0.0"}
            }),
            "tools/list" => {
                let mut tools = vec![
                    json!({"name": "echo", "description": "Echo the text back.",
                     "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}},
                     "annotations": {"readOnlyHint": true}}),
                    json!({"name": "drop_table", "description": "Deletes a table.",
                     "inputSchema": {"type": "object", "properties": {"name": {"type": "string"}}}}),
                ];
                if self.asks {
                    for name in ["deploy", "summarize"] {
                        tools.push(json!({"name": name, "description": name,
                            "inputSchema": {"type": "object"}}));
                    }
                }
                json!({ "tools": tools })
            }
            "prompts/list" => {
                json!({"prompts": [{"name": "review", "description": "Review a file.",
                "arguments": [{"name": "file", "required": true}]}]})
            }
            "prompts/get" => json!({"messages": [{"role": "user", "content": {"type": "text",
                "text": format!("Review {} carefully.", params["arguments"]["file"].as_str().unwrap_or_default())}}]}),
            "resources/list" => json!({"resources": [{"uri": "mem://notes", "name": "notes"}]}),
            "resources/read" => {
                json!({"contents": [{"uri": params["uri"], "text": "ship on friday"}]})
            }
            "tools/call" => {
                let answers = &params["inputResponses"];
                let text = match params["name"].as_str().unwrap_or_default() {
                    "deploy" if answers.is_null() => json!({"elicitation/create": {"mode": "form",
                        "message": "Deploy where?", "requestedSchema": {"type": "object",
                        "required": ["env"], "properties": {"env": {"type": "string",
                        "enum": ["dev", "prod"]}}}}}),
                    "summarize" if answers.is_null() => json!({"sampling/createMessage": {
                        "maxTokens": 40, "messages": [{"role": "user", "content":
                        {"type": "text", "text": "Summarize: the release notes"}}]}}),
                    "deploy" => json!(format!(
                        "deployed to {}",
                        answers["ask"]["content"]["env"].as_str().unwrap_or("?")
                    )),
                    "summarize" => json!(format!(
                        "summary: {}",
                        answers["ask"]["content"]["text"].as_str().unwrap_or("?")
                    )),
                    _ => json!(format!(
                        "echo: {}",
                        params["arguments"]["text"].as_str().unwrap_or_default()
                    )),
                };
                match text {
                    Value::Object(ask) => {
                        let (method, params) = ask.into_iter().next().expect("one request");
                        json!({"resultType": "input_required", "requestState": "s1",
                            "inputRequests": {"ask": {"method": method, "params": params}}})
                    }
                    text => json!({"content": [{"type": "text", "text": text}]}),
                }
            }
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
    serve(McpServer {
        extras: false,
        asks: false,
    })
    .await
}

async fn serve(responder: McpServer) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/mcp"))
        .respond_with(responder)
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

#[tokio::test]
async fn prompts_run_as_commands_and_resources_are_read() {
    let mcp = serve(McpServer {
        extras: true,
        asks: false,
    })
    .await;
    let llm = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_string_contains("ship on friday"))
        .respond_with(sse_text("It ships on Friday."))
        .mount(&llm)
        .await;
    Mock::given(method("POST"))
        .respond_with(sse_call(
            "c1",
            "read_mcp_resource",
            json!({"server": "fx", "uri": "mem://notes"}),
        ))
        .up_to_n_times(1)
        .mount(&llm)
        .await;

    let tmp = tempfile::tempdir().expect("tempdir");
    let loaded = common::workspace(tmp.path(), &llm.uri(), r#"["tiny"]"#, "");
    add_server(tmp.path(), &mcp.uri(), "");
    let mut engine = Engine::spawn(Arc::new(loaded));
    mcp_ready(&mut engine).await;
    let prompts = loop {
        let e = tokio::time::timeout(Duration::from_secs(5), engine.events.recv())
            .await
            .expect("prompts within 5s")
            .expect("open");
        if let Event::McpPrompts { prompts } = e
            && !prompts.is_empty()
        {
            break prompts;
        }
    };
    assert_eq!(prompts[0].name, "fx:review");
    assert_eq!(prompts[0].args, "<file>");

    // A missing argument stops the turn before anything reaches the model.
    engine
        .submit(Op::UserTurn {
            items: vec![InputItem::text("/fx:review")],
            overrides: Default::default(),
        })
        .await;
    let reason = loop {
        if let Event::TurnAborted { reason, .. } = engine.events.recv().await.expect("open") {
            break reason;
        }
    };
    assert!(reason.contains("missing file"), "{reason}");
    assert!(bodies(&llm).await.is_empty());

    let events = turn(&mut engine, "/fx:review src/a.rs").await;
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::ToolCallEnd { result, .. } if result.ok)),
        "{events:?}"
    );
    let sent = bodies(&llm).await;
    assert!(
        sent[0].contains("Review src/a.rs carefully."),
        "{}",
        sent[0]
    );
    assert!(!sent[0].contains("/fx:review"), "{}", sent[0]);
    assert!(sent[0].contains("\"list_mcp_resources\""), "{}", sent[0]);
    assert!(sent[0].contains("\"read_mcp_resource\""), "{}", sent[0]);
    assert!(sent[1].contains("ship on friday"), "{}", sent[1]);

    engine.submit(Op::ListMcp).await;
    let servers = loop {
        if let Event::McpServers { servers } = engine.events.recv().await.expect("open") {
            break servers;
        }
    };
    assert_eq!(servers[0].prompts, ["review"]);
    assert_eq!(servers[0].resources, ["mem://notes"]);
    engine.shutdown().await;
}

#[tokio::test]
async fn servers_ask_the_user_and_the_model() {
    let mcp = serve(McpServer {
        extras: false,
        asks: true,
    })
    .await;
    let llm = MockServer::start().await;
    // Most specific first: wiremock answers with the first mock that matches.
    Mock::given(method("POST"))
        .and(body_string_contains("summary: Short notes."))
        .respond_with(sse_text("Summarized."))
        .mount(&llm)
        .await;
    Mock::given(method("POST"))
        .and(body_string_contains("Summarize: the release notes"))
        .respond_with(sse_text("Short notes."))
        .mount(&llm)
        .await;
    Mock::given(method("POST"))
        .and(body_string_contains("summarize it"))
        .respond_with(sse_call("c2", "mcp__fx__summarize", json!({})))
        .mount(&llm)
        .await;
    Mock::given(method("POST"))
        .and(body_string_contains("deployed to"))
        .respond_with(sse_text("Deployed."))
        .mount(&llm)
        .await;
    Mock::given(method("POST"))
        .respond_with(sse_call("c1", "mcp__fx__deploy", json!({})))
        .up_to_n_times(1)
        .mount(&llm)
        .await;

    let tmp = tempfile::tempdir().expect("tempdir");
    let loaded = common::workspace(tmp.path(), &llm.uri(), r#"["tiny"]"#, "");
    add_server(tmp.path(), &mcp.uri(), "[tools]\nauto_approve = [\"*\"]\n");
    let mut engine = Engine::spawn(Arc::new(loaded));
    mcp_ready(&mut engine).await;

    // The form arrives as questions; the user picks `prod`.
    engine
        .submit(Op::UserTurn {
            items: vec![InputItem::text("deploy please")],
            overrides: Default::default(),
        })
        .await;
    let (call_id, questions) = loop {
        if let Event::Questions { call_id, questions } = engine.events.recv().await.expect("open") {
            break (call_id, questions);
        }
    };
    assert_eq!(questions[0].header, "fx");
    assert!(questions[0].question.starts_with("Deploy where?"));
    engine
        .submit(Op::AnswerQuestions {
            call_id,
            answers: vec![Answer::Picked { index: 1 }],
            stop_asking: false,
        })
        .await;
    common::until_done(&mut engine).await;
    let sent = bodies(&llm).await;
    assert!(sent[1].contains("deployed to prod"), "{}", sent[1]);

    // Sampling asks for approval as `mcp:fx/sampling`, then goes to the model, toolless.
    engine
        .submit(Op::UserTurn {
            items: vec![InputItem::text("summarize it")],
            overrides: Default::default(),
        })
        .await;
    let (call_id, request) = loop {
        if let Event::ApprovalRequest {
            call_id, request, ..
        } = engine.events.recv().await.expect("open")
        {
            break (call_id, request);
        }
    };
    let ApprovalRequest::Tool { name, arguments } = request else {
        panic!("{request:?}")
    };
    assert_eq!(name, "mcp:fx/sampling");
    assert!(
        arguments.contains("Summarize: the release notes"),
        "{arguments}"
    );
    engine
        .submit(Op::ApprovalResponse {
            call_id,
            decision: ApprovalDecision::AllowOnce,
        })
        .await;
    common::until_done(&mut engine).await;
    let sent = bodies(&llm).await;
    let sample = sent
        .iter()
        .find(|b| b.contains("Summarize: the release notes"))
        .expect("the sampling request");
    assert!(!sample.contains("\"tools\""), "{sample}");
    assert!(sent.last().expect("sent").contains("summary: Short notes."));
    engine.shutdown().await;
}
