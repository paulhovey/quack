//! A tiny MCP server over stdio for tests. It speaks the pre-2026 `initialize` handshake
//! and answers `server/discover` with "method not found", like most servers today.
//!
//! Tools: `echo` (read-only), `fail` (an error result), `crash` (exits), `slow` (5 s),
//! `deploy` (asks the user where, through a form) and `summarize` (asks the client's
//! model, through sampling); both send their request mid-call, the pre-2026 way.
//! Prompts: `review` (`file` required, `focus` optional). Resources: `mem://notes`, and
//! the template `mem://files/{name}`.

#![allow(clippy::print_stdout)]

use std::io::{BufRead, Write};

use serde_json::{Value, json};

/// Send the client a request and wait for its answer: the `result`, or the error.
fn ask(
    lines: &mut impl Iterator<Item = std::io::Result<String>>,
    out: &mut impl Write,
    method: &str,
    params: Value,
) -> Result<Value, Value> {
    let msg = json!({"jsonrpc": "2.0", "id": "fixture-ask", "method": method, "params": params});
    let _ = writeln!(out, "{msg}");
    let _ = out.flush();
    for line in lines.by_ref() {
        let Ok(reply) = serde_json::from_str::<Value>(&line.unwrap_or_default()) else {
            continue;
        };
        if reply["id"] == "fixture-ask" {
            return match reply.get("result") {
                Some(r) => Ok(r.clone()),
                None => Err(reply["error"].clone()),
            };
        }
    }
    Err(json!("stdin closed"))
}

fn main() {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    let mut lines = stdin.lock().lines();
    while let Some(line) = lines.next() {
        let Ok(line) = line else { return };
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(id) = msg.get("id").cloned() else {
            continue; // a notification
        };
        let method = msg
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let params = msg.get("params").cloned().unwrap_or_default();
        let reply = match method {
            "initialize" => Ok(json!({
                "protocolVersion": params["protocolVersion"],
                "capabilities": {"tools": {"listChanged": true}, "prompts": {}, "resources": {}},
                "serverInfo": {"name": "fixture", "version": "1.0.0"}
            })),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": [
                {"name": "echo", "description": "Echo the text back.",
                 "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]},
                 "annotations": {"readOnlyHint": true}},
                {"name": "fail", "description": "Always fails.", "inputSchema": {"type": "object"}},
                {"name": "crash", "description": "Exits the server.", "inputSchema": {"type": "object"}},
                {"name": "slow", "description": "Takes five seconds.", "inputSchema": {"type": "object"}},
                {"name": "deploy", "description": "Deploys, asking where.", "inputSchema": {"type": "object"}},
                {"name": "summarize", "description": "Summarizes with the client's model.",
                 "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}}}
            ]})),
            "prompts/list" => Ok(json!({"prompts": [
                {"name": "review", "description": "Review a file.", "arguments": [
                    {"name": "file", "description": "The file to review", "required": true},
                    {"name": "focus", "description": "What to look at"}
                ]}
            ]})),
            "prompts/get" => {
                let args = &params["arguments"];
                let focus = args["focus"].as_str().unwrap_or("everything");
                match args["file"].as_str() {
                    Some(file) => Ok(json!({"messages": [{"role": "user", "content": {
                        "type": "text", "text": format!("Review {file}, looking at {focus}.")}}]})),
                    None => Err("missing argument: file".to_owned()),
                }
            }
            "resources/list" => Ok(json!({"resources": [
                {"uri": "mem://notes", "name": "notes", "description": "Team notes",
                 "mimeType": "text/plain"}
            ]})),
            "resources/templates/list" => Ok(json!({"resourceTemplates": [
                {"uriTemplate": "mem://files/{name}", "name": "files"}
            ]})),
            "resources/read" => match params["uri"].as_str().unwrap_or_default() {
                "mem://notes" => Ok(json!({"contents": [
                    {"uri": "mem://notes", "mimeType": "text/plain", "text": "ship on friday"}]})),
                uri if uri.starts_with("mem://files/") => Ok(json!({"contents": [
                    {"uri": uri, "text": format!("contents of {}", &uri[12..])}]})),
                uri => Err(format!("no resource {uri}")),
            },
            "tools/call" => {
                let args = &params["arguments"];
                match params["name"].as_str().unwrap_or_default() {
                    "echo" => Ok(json!({"content": [{"type": "text",
                        "text": format!("echo: {}", args["text"].as_str().unwrap_or_default())}]})),
                    "fail" => Ok(
                        json!({"content": [{"type": "text", "text": "it broke"}], "isError": true}),
                    ),
                    "crash" => std::process::exit(3),
                    "slow" => {
                        std::thread::sleep(std::time::Duration::from_secs(5));
                        Ok(json!({"content": [{"type": "text", "text": "done"}]}))
                    }
                    "deploy" => {
                        let form = json!({"message": "Deploy where?", "requestedSchema": {
                            "type": "object", "required": ["env"], "properties": {
                                "env": {"type": "string", "enum": ["dev", "prod"]}}}});
                        let text = match ask(&mut lines, &mut out, "elicitation/create", form) {
                            Ok(r) if r["action"] == "accept" => {
                                format!(
                                    "deployed to {}",
                                    r["content"]["env"].as_str().unwrap_or("?")
                                )
                            }
                            Ok(r) => {
                                format!("not deployed: {}", r["action"].as_str().unwrap_or("?"))
                            }
                            Err(e) => format!("not deployed: {e}"),
                        };
                        Ok(json!({"content": [{"type": "text", "text": text}]}))
                    }
                    "summarize" => {
                        let request = json!({"maxTokens": 50, "messages": [{"role": "user",
                            "content": {"type": "text", "text": format!("Summarize: {}",
                                args["text"].as_str().unwrap_or_default())}}]});
                        Ok(
                            match ask(&mut lines, &mut out, "sampling/createMessage", request) {
                                Ok(r) => json!({"content": [{"type": "text", "text": format!(
                                "{} said: {}", r["model"].as_str().unwrap_or("?"),
                                r["content"]["text"].as_str().unwrap_or_default())}]}),
                                Err(e) => json!({"content": [{"type": "text",
                                "text": format!("sampling failed: {}", e["message"])}],
                                "isError": true}),
                            },
                        )
                    }
                    other => Err(format!("unknown tool {other}")),
                }
            }
            other => Err(format!("method not found: {other}")),
        };
        let msg = match reply {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err(message) => json!({"jsonrpc": "2.0", "id": id,
                "error": {"code": -32601, "message": message}}),
        };
        let _ = writeln!(out, "{msg}");
        let _ = out.flush();
    }
}
