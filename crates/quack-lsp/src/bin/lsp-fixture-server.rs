//! A minimal language server for tests, over stdio. For `.fx` files:
//!
//! - each line containing `ERROR` gets an error diagnostic, and `WARN` a warning;
//! - `BREAK_OTHER` in a file gives `other.fx` next to it an error (a cross-file break);
//! - `fn <name>` lines are definitions and symbols; hover shows `hover: <word>`;
//! - rename replaces the word everywhere in the open documents.
#![allow(clippy::print_stdout, clippy::print_stderr, clippy::unwrap_used)]

use std::collections::HashMap;
use std::io::{BufRead, Write};

use serde_json::{Value, json};

fn main() {
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let mut docs: HashMap<String, String> = HashMap::new();
    eprintln!("fixture server started");
    while let Some(msg) = read(&mut input) {
        let method = msg["method"].as_str().unwrap_or_default();
        let params = &msg["params"];
        let id = msg.get("id").cloned();
        let result = match method {
            "initialize" => Some(json!({ "capabilities": {
                "textDocumentSync": 1,
                "definitionProvider": true,
                "referencesProvider": true,
                "hoverProvider": true,
                "documentSymbolProvider": true,
                "workspaceSymbolProvider": true,
                "renameProvider": true,
            }})),
            "textDocument/didOpen" => {
                let uri = params["textDocument"]["uri"].as_str().unwrap().to_owned();
                docs.insert(uri.clone(), params["textDocument"]["text"].as_str().unwrap().to_owned());
                publish(&uri, &docs);
                None
            }
            "textDocument/didChange" => {
                let uri = params["textDocument"]["uri"].as_str().unwrap().to_owned();
                let text = params["contentChanges"][0]["text"].as_str().unwrap().to_owned();
                docs.insert(uri.clone(), text);
                publish(&uri, &docs);
                None
            }
            "textDocument/didSave" => {
                let uri = params["textDocument"]["uri"].as_str().unwrap().to_owned();
                if docs.contains_key(&uri) {
                    publish(&uri, &docs);
                }
                None
            }
            "textDocument/definition" => Some(word_at(params, &docs).map_or(Value::Null, |w| {
                let found = docs.iter().find_map(|(uri, text)| {
                    text.lines().enumerate().find_map(|(i, l)| {
                        l.find(&format!("fn {w}")).map(|c| location(uri, i, c + 3))
                    })
                });
                found.unwrap_or(Value::Null)
            })),
            "textDocument/references" => Some(word_at(params, &docs).map_or(json!([]), |w| {
                let mut out = Vec::new();
                for (uri, text) in &docs {
                    for (i, l) in text.lines().enumerate() {
                        if let Some(c) = l.find(&w) {
                            out.push(location(uri, i, c));
                        }
                    }
                }
                Value::Array(out)
            })),
            "textDocument/hover" => Some(word_at(params, &docs).map_or(Value::Null, |w| {
                json!({ "contents": { "kind": "markdown", "value": format!("hover: {w}\r\n") } })
            })),
            "textDocument/documentSymbol" => {
                let uri = params["textDocument"]["uri"].as_str().unwrap_or_default();
                Some(Value::Array(functions(uri, &docs).into_iter().map(|(name, line, _)| {
                    json!({ "name": name, "kind": 12,
                        "range": range(line, 0), "selectionRange": range(line, 3) })
                }).collect()))
            }
            "workspace/symbol" => {
                let q = params["query"].as_str().unwrap_or_default().to_owned();
                let mut out = Vec::new();
                for uri in docs.keys() {
                    for (name, line, uri) in functions(uri, &docs) {
                        if name.contains(&q) {
                            out.push(json!({ "name": name, "kind": 12, "location": location(&uri, line, 3) }));
                        }
                    }
                }
                Some(Value::Array(out))
            }
            "textDocument/rename" => Some(word_at(params, &docs).map_or(Value::Null, |w| {
                let new_name = params["newName"].as_str().unwrap_or_default();
                let mut changes = serde_json::Map::new();
                for (uri, text) in &docs {
                    let mut edits = Vec::new();
                    for (i, l) in text.lines().enumerate() {
                        let mut from = 0;
                        while let Some(c) = l[from..].find(&w) {
                            let start = from + c;
                            edits.push(json!({
                                "range": { "start": { "line": i, "character": start },
                                           "end": { "line": i, "character": start + w.len() } },
                                "newText": new_name,
                            }));
                            from = start + w.len();
                        }
                    }
                    if !edits.is_empty() {
                        changes.insert(uri.clone(), Value::Array(edits));
                    }
                }
                json!({ "changes": changes })
            })),
            "shutdown" => Some(Value::Null),
            "exit" => std::process::exit(0),
            _ => None,
        };
        if let Some(id) = id {
            send(&json!({ "jsonrpc": "2.0", "id": id, "result": result.unwrap_or(Value::Null) }));
        }
    }
}

fn read(r: &mut impl BufRead) -> Option<Value> {
    let mut len = 0usize;
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim();
        if line.is_empty() {
            break;
        }
        if let Some(v) = line.strip_prefix("Content-Length:") {
            len = v.trim().parse().ok()?;
        }
    }
    let mut body = vec![0; len];
    r.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

fn send(v: &Value) {
    let body = v.to_string();
    let mut out = std::io::stdout().lock();
    write!(out, "Content-Length: {}\r\n\r\n{body}", body.len()).unwrap();
    out.flush().unwrap();
}

fn range(line: usize, col: usize) -> Value {
    json!({ "start": { "line": line, "character": col }, "end": { "line": line, "character": col + 1 } })
}

fn location(uri: &str, line: usize, col: usize) -> Value {
    json!({ "uri": uri, "range": range(line, col) })
}

fn functions(uri: &str, docs: &HashMap<String, String>) -> Vec<(String, usize, String)> {
    let Some(text) = docs.get(uri) else {
        return Vec::new();
    };
    text.lines()
        .enumerate()
        .filter_map(|(i, l)| {
            let rest = l.trim_start().strip_prefix("fn ")?;
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            Some((name, i, uri.to_owned()))
        })
        .collect()
}

fn word_at(params: &Value, docs: &HashMap<String, String>) -> Option<String> {
    let text = docs.get(params["textDocument"]["uri"].as_str()?)?;
    let line = text
        .lines()
        .nth(usize::try_from(params["position"]["line"].as_u64()?).ok()?)?;
    let col = usize::try_from(params["position"]["character"].as_u64()?).ok()?;
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    let chars: Vec<char> = line.chars().collect();
    let mut start = col.min(chars.len());
    while start > 0 && ident(chars[start - 1]) {
        start -= 1;
    }
    let mut end = col.min(chars.len());
    while end < chars.len() && ident(chars[end]) {
        end += 1;
    }
    (end > start).then(|| chars[start..end].iter().collect())
}

fn publish(uri: &str, docs: &HashMap<String, String>) {
    let text = &docs[uri];
    let mut diags = Vec::new();
    for (i, l) in text.lines().enumerate() {
        if let Some(c) = l.find("ERROR") {
            diags.push(json!({ "range": range(i, c), "severity": 1, "code": "F1", "message": "found ERROR" }));
        }
        if let Some(c) = l.find("WARN") {
            diags.push(json!({ "range": range(i, c), "severity": 2, "message": "found WARN" }));
        }
    }
    send(
        &json!({ "jsonrpc": "2.0", "method": "textDocument/publishDiagnostics",
        "params": { "uri": uri, "diagnostics": diags } }),
    );
    let other = uri
        .rsplit_once('/')
        .map(|(dir, _)| format!("{dir}/other.fx"))
        .unwrap();
    if other != uri {
        let broken = text.contains("BREAK_OTHER");
        let diags = if broken {
            json!([{ "range": range(0, 0), "severity": 1, "message": "other is broken" }])
        } else {
            json!([])
        };
        send(
            &json!({ "jsonrpc": "2.0", "method": "textDocument/publishDiagnostics",
            "params": { "uri": other, "diagnostics": diags } }),
        );
    }
}
