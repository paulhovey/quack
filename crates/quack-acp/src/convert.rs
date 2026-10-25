//! Conversions between ACP messages and the engine's protocol.

use std::path::PathBuf;

use quack_config::McpServerFile;
use quack_protocol::{InputItem, ReplayItem};
use serde_json::{Value, json};

use crate::wire::{ContentBlock, McpServer};

/// The ACP kind of a tool, from its name. Profiles name tools differently (`shell`,
/// `Bash`, `run_shell_command`), so this matches on parts of the name.
pub(crate) fn tool_kind(name: &str) -> &'static str {
    let n = name.to_ascii_lowercase();
    let has = |parts: &[&str]| parts.iter().any(|p| n.contains(p));
    if has(&["shell", "bash", "exec", "command", "powershell"]) {
        "execute"
    } else if has(&["edit", "write", "patch", "replace", "block"]) {
        "edit"
    } else if n == "ls" || has(&["grep", "glob", "search", "find", "list"]) {
        "search"
    } else if has(&["read", "view", "index", "outline", "cat"]) {
        "read"
    } else if has(&["fetch", "web"]) {
        "fetch"
    } else if has(&["todo", "think", "plan"]) {
        "think"
    } else {
        "other"
    }
}

/// The engine's `[x]` / `[~]` / `[ ]` checklist as an ACP `plan` update.
pub(crate) fn plan(checklist: &str) -> Value {
    let entries: Vec<Value> = checklist
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let (status, rest) = if let Some(r) = line.strip_prefix("[x]") {
                ("completed", r)
            } else if let Some(r) = line.strip_prefix("[~]") {
                ("in_progress", r)
            } else {
                ("pending", line.strip_prefix("[ ]")?)
            };
            Some(json!({"content": rest.trim(), "priority": "medium", "status": status}))
        })
        .collect();
    json!({"sessionUpdate": "plan", "entries": entries})
}

/// A prompt's content blocks as turn input. Files the user mentioned become their path
/// (the model reads them itself); embedded file contents are quoted with their path.
pub(crate) fn prompt_items(blocks: Vec<ContentBlock>) -> Vec<InputItem> {
    blocks
        .into_iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(InputItem::text(text)),
            ContentBlock::ResourceLink { uri } => Some(InputItem::text(display_uri(&uri))),
            ContentBlock::Resource { resource } => resource.text.map(|text| {
                InputItem::text(format!(
                    "{}:\n{}",
                    display_uri(&resource.uri),
                    fenced(&text, "")
                ))
            }),
            ContentBlock::Other => None,
        })
        .collect()
}

/// A `file://` URI as a local path; anything else as it is.
fn display_uri(uri: &str) -> String {
    file_path(uri).map_or_else(|| uri.to_owned(), |p| p.display().to_string())
}

/// The local path of a `file://` URI, percent-decoded. On Windows the URI's
/// `/C:/...` path loses its leading slash.
pub(crate) fn file_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    // `file://host/path`: only an empty host or `localhost` is this machine.
    let path = match rest.strip_prefix("localhost") {
        Some(p) if p.starts_with('/') => p,
        _ if rest.starts_with('/') => rest,
        _ => return None,
    };
    let decoded = percent_decode(path)?;
    let b = decoded.as_bytes();
    let drive = b.len() >= 3 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b':';
    Some(PathBuf::from(if drive && cfg!(windows) {
        &decoded[1..]
    } else {
        decoded.as_str()
    }))
}

fn percent_decode(s: &str) -> Option<String> {
    let mut out = Vec::with_capacity(s.len());
    let mut bytes = s.bytes();
    while let Some(b) = bytes.next() {
        if b == b'%' {
            let hex = [bytes.next()?, bytes.next()?];
            let hex = std::str::from_utf8(&hex).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
        } else {
            out.push(b);
        }
    }
    String::from_utf8(out).ok()
}

/// `text` in a Markdown code block, with a fence longer than any backtick run inside.
pub(crate) fn fenced(text: &str, lang: &str) -> String {
    let mut longest = 0;
    let mut run = 0;
    for c in text.chars() {
        run = if c == '`' { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    let fence = "`".repeat(longest.max(2) + 1);
    let nl = if text.ends_with('\n') { "" } else { "\n" };
    format!("{fence}{lang}\n{text}{nl}{fence}")
}

/// A tool's output or diff as tool call content.
pub(crate) fn tool_content(detail: &str) -> Value {
    let diff =
        detail.starts_with("--- ") || detail.starts_with("@@") || detail.starts_with("diff ");
    text_content(&fenced(detail, if diff { "diff" } else { "" }))
}

/// Text as tool call content.
pub(crate) fn text_content(text: &str) -> Value {
    json!({"type": "content", "content": {"type": "text", "text": text}})
}

/// A message, thought or user chunk update.
pub(crate) fn chunk(kind: &str, text: &str) -> Value {
    json!({"sessionUpdate": kind, "content": {"type": "text", "text": text}})
}

/// A `tool_call` update announcing a call.
pub(crate) fn tool_call(id: &str, title: &str, kind: &str, status: &str) -> Value {
    json!({
        "sessionUpdate": "tool_call",
        "toolCallId": id,
        "title": title,
        "kind": kind,
        "status": status,
    })
}

/// An editor's MCP server as an `mcp.d` entry, or why it cannot be used.
pub(crate) fn mcp_server(server: McpServer) -> Result<(String, McpServerFile), String> {
    let pairs = |list: Vec<crate::wire::NameValue>| -> serde_json::Map<String, Value> {
        list.into_iter()
            .map(|v| (v.name, Value::String(v.value)))
            .collect()
    };
    let value = match (server.kind.as_deref(), server.command, server.url) {
        (None | Some("stdio"), Some(command), _) => json!({
            "command": command.display().to_string(),
            "args": server.args,
            "env": pairs(server.env),
            "description": "from the editor",
        }),
        (Some("http"), _, Some(url)) => json!({
            "url": url,
            "headers": pairs(server.headers),
            "description": "from the editor",
        }),
        (Some("sse"), _, _) => {
            return Err(format!(
                "MCP server `{}` uses SSE, which quack does not speak; use its streamable HTTP endpoint",
                server.name
            ));
        }
        _ => {
            return Err(format!(
                "MCP server `{}` names no command or URL",
                server.name
            ));
        }
    };
    let name = server.name;
    let file = serde_json::from_value(value).map_err(|e| format!("MCP server `{name}`: {e}"))?;
    Ok((mcp_id(&name), file))
}

/// An editor's server name as an id: what a file stem could be.
fn mcp_id(name: &str) -> String {
    let id: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if id.is_empty() { "editor".into() } else { id }
}

/// A resumed session's transcript as the updates that show it again.
pub(crate) fn replay(items: Vec<ReplayItem>) -> Vec<Value> {
    items
        .into_iter()
        .enumerate()
        .map(|(i, item)| match item {
            ReplayItem::User { text } => chunk("user_message_chunk", &text),
            ReplayItem::Assistant { text } => chunk("agent_message_chunk", &text),
            ReplayItem::Tool {
                summary,
                ok,
                result,
            } => {
                let status = if ok { "completed" } else { "failed" };
                let name = summary.split_whitespace().next().unwrap_or_default();
                let mut call = tool_call(&format!("replay-{i}"), &summary, tool_kind(name), status);
                if !result.is_empty() {
                    call["content"] = json!([tool_content(&result)]);
                }
                call
            }
            ReplayItem::Compacted => {
                chunk("agent_thought_chunk", "(earlier conversation summarized)")
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_names_of_every_profile_get_a_kind() {
        let cases = [
            ("shell", "execute"),
            ("Bash", "execute"),
            ("run_shell_command", "execute"),
            ("edit_hashline", "edit"),
            ("apply_patch", "edit"),
            ("Write", "edit"),
            ("grep", "search"),
            ("ls", "search"),
            ("read", "read"),
            ("view_image", "read"),
            ("todo", "think"),
            ("task", "other"),
            ("load_skill", "other"),
        ];
        for (name, kind) in cases {
            assert_eq!(tool_kind(name), kind, "{name}");
        }
    }

    #[test]
    fn checklist_becomes_plan_entries() {
        let p = plan("[x] read the code\n[~] fix the bug\n[ ] run the tests\nnot an item");
        assert_eq!(
            p["entries"],
            json!([
                {"content": "read the code", "priority": "medium", "status": "completed"},
                {"content": "fix the bug", "priority": "medium", "status": "in_progress"},
                {"content": "run the tests", "priority": "medium", "status": "pending"},
            ])
        );
    }

    #[test]
    fn file_uris_become_paths() {
        assert_eq!(
            file_path("file:///home/me/my%20project/a.rs"),
            Some(PathBuf::from("/home/me/my project/a.rs"))
        );
        assert_eq!(
            file_path("file://localhost/tmp/x"),
            Some(PathBuf::from("/tmp/x"))
        );
        assert_eq!(file_path("file://server/share/x"), None);
        assert_eq!(file_path("https://example.com/x"), None);
        let drive = file_path("file:///C:/src/a%2Bb.rs").expect("path");
        if cfg!(windows) {
            assert_eq!(drive, PathBuf::from("C:/src/a+b.rs"));
        } else {
            assert_eq!(drive, PathBuf::from("/C:/src/a+b.rs"));
        }
    }

    #[test]
    fn fences_outgrow_backticks_inside() {
        assert_eq!(fenced("a", ""), "```\na\n```");
        assert_eq!(fenced("x ```` y\n", "diff"), "`````diff\nx ```` y\n`````");
    }

    fn server(v: Value) -> McpServer {
        serde_json::from_value(v).expect("server")
    }

    #[test]
    fn editor_mcp_servers_become_server_files() {
        let (id, file) = mcp_server(server(json!({
            "name": "my files",
            "command": "/usr/bin/mcp-files",
            "args": ["--root", "."],
            "env": [{"name": "TOKEN", "value": "t"}],
        })))
        .expect("stdio");
        assert_eq!(id, "my-files");
        assert_eq!(file.command, "/usr/bin/mcp-files");
        assert_eq!(file.args, ["--root", "."]);
        assert_eq!(file.transport(), Ok("stdio"));

        let (_, file) = mcp_server(server(json!({
            "type": "http",
            "name": "docs",
            "url": "https://mcp.example.com/mcp",
            "headers": [{"name": "Authorization", "value": "Bearer x"}],
        })))
        .expect("http");
        assert_eq!(file.transport(), Ok("http"));
        assert_eq!(file.headers.len(), 1);

        let sse =
            server(json!({"type": "sse", "name": "old", "url": "https://x/sse", "headers": []}));
        assert!(mcp_server(sse).is_err());
    }

    #[test]
    fn embedded_files_are_quoted_with_their_path() {
        let blocks: Vec<ContentBlock> = serde_json::from_value(json!([
            {"type": "text", "text": "explain"},
            {"type": "resource_link", "name": "a.rs", "uri": "file:///w/a.rs"},
            {"type": "resource", "resource": {"uri": "file:///w/b.rs", "text": "fn b() {}"}},
            {"type": "image", "data": "AAAA", "mimeType": "image/png"},
        ]))
        .expect("blocks");
        let texts: Vec<_> = prompt_items(blocks)
            .into_iter()
            .map(|i| match i {
                InputItem::Text { text } => text,
                other => format!("{other:?}"),
            })
            .collect();
        assert_eq!(texts.len(), 3, "the image is left out: {texts:?}");
        assert_eq!(texts[0], "explain");
        assert!(texts[1].ends_with("a.rs"), "{}", texts[1]);
        assert!(
            texts[2].contains("b.rs:\n```\nfn b() {}\n```"),
            "{}",
            texts[2]
        );
    }
}
