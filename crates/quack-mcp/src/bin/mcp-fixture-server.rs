//! A tiny MCP server over stdio for tests. It speaks the pre-2026 `initialize` handshake
//! and answers `server/discover` with "method not found", like most servers today.
//!
//! Tools: `echo` (read-only), `fail` (an error result), `crash` (exits), `slow` (5 s).

#![allow(clippy::print_stdout)]

use std::io::{BufRead, Write};

use serde_json::{Value, json};

fn main() {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
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
                "capabilities": {"tools": {"listChanged": true}},
                "serverInfo": {"name": "fixture", "version": "1.0.0"}
            })),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": [
                {"name": "echo", "description": "Echo the text back.",
                 "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]},
                 "annotations": {"readOnlyHint": true}},
                {"name": "fail", "description": "Always fails.", "inputSchema": {"type": "object"}},
                {"name": "crash", "description": "Exits the server.", "inputSchema": {"type": "object"}},
                {"name": "slow", "description": "Takes five seconds.", "inputSchema": {"type": "object"}}
            ]})),
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
