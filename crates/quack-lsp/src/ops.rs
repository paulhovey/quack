//! The queries behind the `lsp` tool, with results as text for the model. Positions are
//! 1-based lines and characters; a symbol name can stand in for the column, since models
//! often get columns wrong.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};

use crate::client::Client;
use crate::diag::{Diagnostic, Severity};
use crate::manager::{LspManager, TOOL_WAIT};
use crate::pos;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_LOCATIONS: usize = 100;
const MAX_SYMBOLS: usize = 60;
const MAX_HOVER: usize = 3_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    Definition,
    References,
    Hover,
    Symbols,
    Diagnostics,
}

impl Operation {
    pub const NAMES: [&'static str; 5] = [
        "definition",
        "references",
        "hover",
        "symbols",
        "diagnostics",
    ];

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "definition" | "goto_definition" | "go_to_definition" | "def" => Self::Definition,
            "references" | "find_references" | "refs" => Self::References,
            "hover" | "type" | "info" => Self::Hover,
            "symbols" | "symbol" | "workspace_symbols" | "document_symbols" | "outline" => {
                Self::Symbols
            }
            "diagnostics" | "errors" | "problems" => Self::Diagnostics,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct Query {
    pub path: Option<PathBuf>,
    /// 1-based.
    pub line: Option<u32>,
    /// 1-based, in characters.
    pub column: Option<u32>,
    /// An identifier to find on `line` (or in the file), standing in for the column.
    pub symbol: Option<String>,
}

type Display<'a> = &'a (dyn Fn(&Path) -> String + Sync);

/// Run one operation. `Err` is a message for the model.
pub async fn run(
    mgr: &Arc<LspManager>,
    op: Operation,
    q: Query,
    display: Display<'_>,
) -> Result<String, String> {
    match op {
        Operation::Definition | Operation::References | Operation::Hover => {
            let path = q.path.clone().ok_or("`path` is required")?;
            let client = client(mgr, &path).await?;
            let text = client.sync(&path).await?;
            let (line, character) = position(&text, &q)?;
            let line_text = text.lines().nth(line).unwrap_or_default();
            let params = json!({
                "textDocument": { "uri": pos::path_to_uri(&path) },
                "position": {
                    "line": line,
                    "character": client.encoding().to_lsp(line_text, character),
                },
            });
            match op {
                Operation::Definition => {
                    need(&client, "definitionProvider", "definitions")?;
                    let r = client
                        .request("textDocument/definition", params, REQUEST_TIMEOUT)
                        .await?;
                    Ok(locations(&client, &r, display, "No definition found."))
                }
                Operation::References => {
                    need(&client, "referencesProvider", "references")?;
                    let mut params = params;
                    params["context"] = json!({ "includeDeclaration": true });
                    let r = client
                        .request("textDocument/references", params, REQUEST_TIMEOUT)
                        .await?;
                    Ok(locations(&client, &r, display, "No references found."))
                }
                _ => {
                    need(&client, "hoverProvider", "hover")?;
                    let r = client
                        .request("textDocument/hover", params, REQUEST_TIMEOUT)
                        .await?;
                    Ok(hover(&r))
                }
            }
        }
        Operation::Symbols => symbols(mgr, &q, display).await,
        Operation::Diagnostics => diagnostics(mgr, q.path.as_deref(), display).await,
    }
}

/// A text edit from a language server, in its own position units.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextEdit {
    /// 0-based `(line, column)`.
    pub start: (u32, u32),
    pub end: (u32, u32),
    pub new_text: String,
}

/// What a rename changes: each file's edits, in the server's position encoding.
#[derive(Debug, Clone)]
pub struct Rename {
    pub encoding: pos::Encoding,
    pub files: Vec<(PathBuf, Vec<TextEdit>)>,
}

/// Ask the server to rename the symbol at `q` to `new_name`. Nothing is written: the
/// caller applies the edits ([`apply_edits`]) after approval.
pub async fn rename(mgr: &Arc<LspManager>, q: &Query, new_name: &str) -> Result<Rename, String> {
    let new_name = new_name.trim();
    if new_name.is_empty() {
        return Err("`new_name` is required for rename".into());
    }
    let path = q.path.clone().ok_or("`path` is required")?;
    let client = client(mgr, &path).await?;
    need(&client, "renameProvider", "rename")?;
    let text = client.sync(&path).await?;
    let (line, character) = position(&text, q)?;
    let line_text = text.lines().nth(line).unwrap_or_default();
    let params = json!({
        "textDocument": { "uri": pos::path_to_uri(&path) },
        "position": {
            "line": line,
            "character": client.encoding().to_lsp(line_text, character),
        },
        "newName": new_name,
    });
    let r = client
        .request("textDocument/rename", params, REQUEST_TIMEOUT)
        .await?;
    let files = workspace_edit(&r)?;
    if files.is_empty() {
        return Err("the language server found nothing to rename there".into());
    }
    Ok(Rename {
        encoding: client.encoding(),
        files,
    })
}

/// The text edits of a `WorkspaceEdit`, by file. Creating, renaming or deleting files is
/// refused rather than half done.
fn workspace_edit(r: &Value) -> Result<Vec<(PathBuf, Vec<TextEdit>)>, String> {
    let mut by_file: Vec<(PathBuf, Vec<TextEdit>)> = Vec::new();
    let mut add = |uri: &str, edits: &Value| -> Result<(), String> {
        let path = pos::uri_to_path(uri).ok_or_else(|| format!("not a file: {uri}"))?;
        let list = edits
            .as_array()
            .ok_or("the language server sent edits in an unknown form")?;
        let mut parsed = Vec::new();
        for e in list {
            parsed.push(text_edit(e).ok_or("the language server sent an edit without a range")?);
        }
        match by_file.iter_mut().find(|(p, _)| *p == path) {
            Some((_, v)) => v.extend(parsed),
            None => by_file.push((path, parsed)),
        }
        Ok(())
    };
    if let Some(changes) = r["documentChanges"].as_array() {
        for c in changes {
            if c.get("kind").is_some() {
                return Err(
                    "this rename also creates, renames or deletes files, which quack does not do yet"
                        .into(),
                );
            }
            add(
                c["textDocument"]["uri"].as_str().unwrap_or_default(),
                &c["edits"],
            )?;
        }
    } else if let Some(changes) = r["changes"].as_object() {
        for (uri, edits) in changes {
            add(uri, edits)?;
        }
    }
    by_file.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(by_file)
}

fn text_edit(e: &Value) -> Option<TextEdit> {
    let at = |p: &Value| -> Option<(u32, u32)> {
        Some((
            u32::try_from(p["line"].as_u64()?).ok()?,
            u32::try_from(p["character"].as_u64()?).ok()?,
        ))
    };
    Some(TextEdit {
        start: at(&e["range"]["start"])?,
        end: at(&e["range"]["end"])?,
        new_text: e["newText"].as_str()?.to_owned(),
    })
}

/// Apply a server's edits to `text` (LF line endings). Overlapping edits are refused.
pub fn apply_edits(text: &str, edits: &[TextEdit], enc: pos::Encoding) -> Result<String, String> {
    // Byte offset where each line starts, and one past the end.
    let mut starts = vec![0usize];
    starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
    let offset = |(line, col): (u32, u32)| -> Result<usize, String> {
        let line = line as usize;
        let Some(&start) = starts.get(line) else {
            return if line == starts.len() {
                Ok(text.len())
            } else {
                Err(format!(
                    "an edit points past the end of the file (line {})",
                    line + 1
                ))
            };
        };
        let end = starts.get(line + 1).map_or(text.len(), |e| e - 1);
        let content = &text[start..end];
        let chars = enc.from_lsp(content, col);
        Ok(start
            + content
                .char_indices()
                .nth(chars)
                .map_or(content.len(), |(i, _)| i))
    };
    let mut spans = Vec::new();
    for e in edits {
        let (a, b) = (offset(e.start)?, offset(e.end)?);
        if b < a {
            return Err("an edit ends before it starts".into());
        }
        spans.push((a, b, e.new_text.as_str()));
    }
    spans.sort_by_key(|s| (s.0, s.1));
    if spans.windows(2).any(|w| w[1].0 < w[0].1) {
        return Err("the language server sent overlapping edits".into());
    }
    let mut out = text.to_owned();
    for (a, b, new) in spans.into_iter().rev() {
        out.replace_range(a..b, &new.replace("\r\n", "\n"));
    }
    Ok(out)
}

async fn client(mgr: &Arc<LspManager>, path: &Path) -> Result<Arc<Client>, String> {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_default();
    match tokio::time::timeout(TOOL_WAIT, mgr.client(path)).await {
        Ok(Ok(Some(c))) => Ok(c),
        Ok(Ok(None)) => Err(format!(
            "no installed language server covers .{ext} files; {}",
            installed(mgr)
        )),
        Ok(Err(e)) => Err(e),
        Err(_) => Err("the language server is still starting; try again shortly".into()),
    }
}

fn installed(mgr: &LspManager) -> String {
    let list: Vec<String> = mgr
        .installed()
        .iter()
        .map(|(id, ext)| format!("{id} (.{})", ext.join(", .")))
        .collect();
    if list.is_empty() {
        "none is installed".into()
    } else {
        format!("installed: {}", list.join("; "))
    }
}

fn need(client: &Client, capability: &str, what: &str) -> Result<(), String> {
    if client.supports(capability) {
        Ok(())
    } else {
        Err(format!("{} does not provide {what}", client.id))
    }
}

/// 0-based line and character for the query.
fn position(text: &str, q: &Query) -> Result<(usize, usize), String> {
    let lines: Vec<&str> = text.lines().collect();
    let symbol = q.symbol.as_deref().map(str::trim).filter(|s| !s.is_empty());
    match (q.line, symbol) {
        (Some(line), Some(sym)) => {
            let want = (line.max(1) - 1) as usize;
            // Models are often a line or two off; look nearby, nearest first.
            for delta in [0isize, -1, 1, -2, 2] {
                let Some(l) = want.checked_add_signed(delta) else {
                    continue;
                };
                if let Some(col) = lines.get(l).and_then(|t| find_word(t, sym)) {
                    return Ok((l, col));
                }
            }
            Err(format!(
                "`{sym}` is not on line {line} or the lines next to it"
            ))
        }
        (Some(line), None) => {
            let l = (line.max(1) - 1) as usize;
            let t = lines
                .get(l)
                .ok_or_else(|| format!("line {line} is past the end ({} lines)", lines.len()))?;
            let col = match q.column {
                Some(c) => (c.max(1) - 1) as usize,
                None => t.chars().take_while(|c| c.is_whitespace()).count(),
            };
            Ok((l, col))
        }
        (None, Some(sym)) => lines
            .iter()
            .enumerate()
            .find_map(|(i, t)| find_word(t, sym).map(|c| (i, c)))
            .ok_or_else(|| format!("`{sym}` does not appear in the file")),
        (None, None) => Err("give `line` (and `symbol` or `column`), or `symbol`".into()),
    }
}

/// Character index of `word` in `line` as a whole identifier.
fn find_word(line: &str, word: &str) -> Option<usize> {
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    let mut from = 0;
    while let Some(i) = line[from..].find(word) {
        let start = from + i;
        let end = start + word.len();
        let before = line[..start].chars().next_back();
        let after = line[end..].chars().next();
        if !before.is_some_and(ident) && !after.is_some_and(ident) {
            return Some(line[..start].chars().count());
        }
        from = start + word.len().max(1);
        if from >= line.len() {
            break;
        }
    }
    None
}

/// `(path, line0, char0)` of a Location or LocationLink.
fn location(v: &Value) -> Option<(PathBuf, u32, u32)> {
    let uri = v["uri"].as_str().or_else(|| v["targetUri"].as_str())?;
    let range = if v["targetSelectionRange"].is_object() {
        &v["targetSelectionRange"]
    } else if v["range"].is_object() {
        &v["range"]
    } else {
        &v["targetRange"]
    };
    let start = &range["start"];
    Some((
        pos::uri_to_path(uri)?,
        u32::try_from(start["line"].as_u64().unwrap_or(0)).unwrap_or(0),
        u32::try_from(start["character"].as_u64().unwrap_or(0)).unwrap_or(0),
    ))
}

fn locations(client: &Client, r: &Value, display: Display<'_>, none: &str) -> String {
    let items: Vec<&Value> = match r {
        Value::Array(a) => a.iter().collect(),
        Value::Object(_) => vec![r],
        _ => Vec::new(),
    };
    let mut found: Vec<(PathBuf, u32, u32)> = items.into_iter().filter_map(location).collect();
    if found.is_empty() {
        return none.into();
    }
    found.sort();
    found.dedup();
    let total = found.len();
    let mut out = Vec::new();
    let mut current: Option<PathBuf> = None;
    let mut text_cache: Option<(PathBuf, Vec<String>)> = None;
    for (path, line, col) in found.into_iter().take(MAX_LOCATIONS) {
        if current.as_ref() != Some(&path) {
            out.push(display(&path));
            current = Some(path.clone());
        }
        if text_cache.as_ref().is_none_or(|(p, _)| *p != path) {
            let lines = std::fs::read_to_string(&path)
                .map(|t| t.lines().map(str::to_owned).collect())
                .unwrap_or_default();
            text_cache = Some((path.clone(), lines));
        }
        let text = text_cache
            .as_ref()
            .and_then(|(_, l)| l.get(line as usize).cloned())
            .unwrap_or_default();
        let ch = client.encoding().from_lsp(&text, col);
        out.push(format!(
            "  {}:{}: {}",
            line + 1,
            ch + 1,
            clip(text.trim(), 200)
        ));
    }
    if total > MAX_LOCATIONS {
        out.push(format!("… and {} more", total - MAX_LOCATIONS));
    }
    format!(
        "{total} location{}:\n{}",
        if total == 1 { "" } else { "s" },
        out.join("\n")
    )
}

fn hover(r: &Value) -> String {
    let contents = &r["contents"];
    let part = |v: &Value| -> String {
        match v {
            Value::String(s) => s.clone(),
            Value::Object(o) => match (o.get("language"), o.get("value")) {
                (Some(lang), Some(val)) => format!(
                    "```{}\n{}\n```",
                    lang.as_str().unwrap_or_default(),
                    val.as_str().unwrap_or_default()
                ),
                (None, Some(val)) => val.as_str().unwrap_or_default().to_owned(),
                _ => String::new(),
            },
            _ => String::new(),
        }
    };
    let text = match contents {
        Value::Array(a) => a.iter().map(part).collect::<Vec<_>>().join("\n\n"),
        other => part(other),
    };
    let text = text.replace("\r\n", "\n");
    let text = text.trim();
    if text.is_empty() {
        "No hover information here.".into()
    } else {
        clip(text, MAX_HOVER)
    }
}

async fn symbols(mgr: &Arc<LspManager>, q: &Query, display: Display<'_>) -> Result<String, String> {
    let query = q.symbol.as_deref().map(str::trim).unwrap_or_default();
    // A file and no query: the file's outline.
    if let (Some(path), true) = (&q.path, query.is_empty()) {
        let client = client(mgr, path).await?;
        need(&client, "documentSymbolProvider", "document symbols")?;
        client.sync(path).await?;
        let r = client
            .request(
                "textDocument/documentSymbol",
                json!({ "textDocument": { "uri": pos::path_to_uri(path) } }),
                REQUEST_TIMEOUT,
            )
            .await?;
        let mut out = Vec::new();
        outline(&r, 0, &mut out);
        if out.is_empty() {
            return Ok("No symbols in this file.".into());
        }
        let total = out.len();
        out.truncate(MAX_SYMBOLS * 2);
        if total > out.len() {
            out.push(format!("… and {} more", total - out.len()));
        }
        return Ok(format!("{}:\n{}", display(path), out.join("\n")));
    }
    if query.is_empty() {
        return Err("give `symbol` (a name to search for), or `path` for a file's outline".into());
    }
    let clients = match &q.path {
        Some(path) => vec![client(mgr, path).await?],
        None => mgr.clients(),
    };
    if clients.is_empty() {
        return Err(format!(
            "no language server is running yet; give `path` (any file in the language) to \
             start one. {}",
            installed(mgr)
        ));
    }
    let mut found = Vec::new();
    for c in clients {
        if !c.supports("workspaceSymbolProvider") {
            continue;
        }
        let r = c
            .request(
                "workspace/symbol",
                json!({ "query": query }),
                REQUEST_TIMEOUT,
            )
            .await?;
        for s in r.as_array().into_iter().flatten() {
            let Some(name) = s["name"].as_str() else {
                continue;
            };
            let place = location(&s["location"])
                .map(|(p, l, _)| format!("{}:{}", display(&p), l + 1))
                .unwrap_or_default();
            let container = s["containerName"]
                .as_str()
                .filter(|c| !c.is_empty())
                .map(|c| format!(" in {c}"))
                .unwrap_or_default();
            found.push(format!(
                "{name} ({}{container}) {place}",
                kind(s["kind"].as_u64())
            ));
        }
    }
    if found.is_empty() {
        return Ok(format!("No symbols match `{query}`."));
    }
    let total = found.len();
    found.truncate(MAX_SYMBOLS);
    if total > MAX_SYMBOLS {
        found.push(format!(
            "… and {} more; use a more specific name",
            total - MAX_SYMBOLS
        ));
    }
    Ok(found.join("\n"))
}

fn outline(v: &Value, depth: usize, out: &mut Vec<String>) {
    for s in v.as_array().into_iter().flatten() {
        let Some(name) = s["name"].as_str() else {
            continue;
        };
        let line = s["selectionRange"]["start"]["line"]
            .as_u64()
            .or_else(|| s["range"]["start"]["line"].as_u64())
            .or_else(|| s["location"]["range"]["start"]["line"].as_u64())
            .unwrap_or(0);
        out.push(format!(
            "{}{} {name}  (line {})",
            "  ".repeat(depth + 1),
            kind(s["kind"].as_u64()),
            line + 1
        ));
        if depth < 3 {
            outline(&s["children"], depth + 1, out);
        }
    }
}

async fn diagnostics(
    mgr: &Arc<LspManager>,
    path: Option<&Path>,
    display: Display<'_>,
) -> Result<String, String> {
    let mut list: Vec<(PathBuf, Diagnostic)> = Vec::new();
    match path {
        Some(path) => {
            let client = client(mgr, path).await?;
            client
                .changed(path, Duration::from_millis(500), Duration::from_secs(5))
                .await?;
            list.extend(
                client
                    .diagnostics(path)
                    .into_iter()
                    .map(|d| (path.to_path_buf(), d)),
            );
        }
        None => {
            let clients = mgr.clients();
            if clients.is_empty() {
                return Ok(format!(
                    "No language server is running yet, so there are no diagnostics. Give \
                     `path` to check a file. {}",
                    installed(mgr)
                ));
            }
            for c in clients {
                for (p, ds) in c.snapshot() {
                    list.extend(
                        ds.into_iter()
                            .filter(|d| d.severity <= Severity::Warning)
                            .map(|d| (p.clone(), d)),
                    );
                }
            }
        }
    }
    if list.is_empty() {
        return Ok("No problems reported.".into());
    }
    list.sort_by(|a, b| (a.1.severity, &a.0, a.1.line).cmp(&(b.1.severity, &b.0, b.1.line)));
    let errors = list
        .iter()
        .filter(|d| d.1.severity == Severity::Error)
        .count();
    let warnings = list
        .iter()
        .filter(|d| d.1.severity == Severity::Warning)
        .count();
    let total = list.len();
    let mut out: Vec<String> = list
        .iter()
        .take(MAX_LOCATIONS / 2)
        .map(|(p, d)| d.render(&display(p)))
        .collect();
    if total > out.len() {
        out.push(format!("… and {} more", total - out.len()));
    }
    Ok(format!(
        "{errors} error{}, {warnings} warning{}:\n{}",
        if errors == 1 { "" } else { "s" },
        if warnings == 1 { "" } else { "s" },
        out.join("\n")
    ))
}

fn kind(k: Option<u64>) -> &'static str {
    match k.unwrap_or(0) {
        1 => "file",
        2 => "module",
        3 => "namespace",
        4 => "package",
        5 => "class",
        6 => "method",
        7 => "property",
        8 => "field",
        9 => "constructor",
        10 => "enum",
        11 => "interface",
        12 => "function",
        13 => "variable",
        14 => "constant",
        15 => "string",
        16 => "number",
        17 => "boolean",
        18 => "array",
        19 => "object",
        20 => "key",
        21 => "null",
        22 => "enum member",
        23 => "struct",
        24 => "event",
        25 => "operator",
        26 => "type parameter",
        _ => "symbol",
    }
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        format!("{}…", s.chars().take(max).collect::<String>())
    } else {
        s.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(start: (u32, u32), end: (u32, u32), new: &str) -> TextEdit {
        TextEdit {
            start,
            end,
            new_text: new.into(),
        }
    }

    #[test]
    fn edits_apply_in_server_units() {
        let text = "let é = old;\nold(é)\n";
        let enc = pos::Encoding::Utf16;
        let out = apply_edits(
            text,
            &[edit((1, 0), (1, 3), "new"), edit((0, 8), (0, 11), "new")],
            enc,
        );
        assert_eq!(out.as_deref(), Ok("let é = new;\nnew(é)\n"));
        // UTF-8 columns count bytes: `é` is two.
        let out = apply_edits(text, &[edit((0, 9), (0, 12), "new")], pos::Encoding::Utf8);
        assert_eq!(out.as_deref(), Ok("let é = new;\nold(é)\n"));
        // An insert at the very end, and overlaps refused.
        assert_eq!(
            apply_edits("a\n", &[edit((1, 0), (1, 0), "b\n")], enc).as_deref(),
            Ok("a\nb\n")
        );
        assert!(
            apply_edits(
                text,
                &[edit((0, 4), (0, 9), "x"), edit((0, 8), (0, 10), "y")],
                enc
            )
            .is_err()
        );
        assert!(apply_edits(text, &[edit((7, 0), (7, 1), "x")], enc).is_err());
    }

    #[test]
    fn workspace_edits_in_both_forms() {
        let uri = pos::path_to_uri(&std::env::temp_dir().join("a.rs"));
        let e = json!({ "range": { "start": { "line": 0, "character": 1 },
                                   "end": { "line": 0, "character": 2 } }, "newText": "x" });
        let changes =
            workspace_edit(&json!({ "changes": { uri.clone(): [e.clone()] } })).expect("changes");
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].1, [edit((0, 1), (0, 2), "x")]);
        let doc = workspace_edit(&json!({ "documentChanges": [
            { "textDocument": { "uri": uri, "version": 1 }, "edits": [e] }
        ] }))
        .expect("documentChanges");
        assert_eq!(doc, changes);
        let renames = workspace_edit(&json!({ "documentChanges": [
            { "kind": "rename", "oldUri": "file:///a", "newUri": "file:///b" }
        ] }));
        assert!(renames.is_err_and(|e| e.contains("renames")));
    }

    #[test]
    fn positions_from_symbols() {
        let text =
            "fn main() {\n    let total = add(1, 2);\n}\nfn add(a: u32, b: u32) -> u32 { a + b }\n";
        let q = |line: Option<u32>, symbol: Option<&str>, column: Option<u32>| Query {
            path: None,
            line,
            column,
            symbol: symbol.map(str::to_owned),
        };
        assert_eq!(position(text, &q(Some(2), Some("add"), None)), Ok((1, 16)));
        // One line off still finds it.
        assert_eq!(position(text, &q(Some(3), Some("add"), None)), Ok((1, 16)));
        assert_eq!(position(text, &q(None, Some("add"), None)), Ok((1, 16)));
        assert_eq!(position(text, &q(Some(2), None, Some(9))), Ok((1, 8)));
        assert_eq!(position(text, &q(Some(2), None, None)), Ok((1, 4)));
        assert!(position(text, &q(Some(1), Some("missing"), None)).is_err());
        assert!(position(text, &q(None, None, None)).is_err());
    }

    #[test]
    fn whole_words_only() {
        assert_eq!(find_word("let address = add;", "add"), Some(14));
        assert_eq!(find_word("é add", "add"), Some(2));
        assert_eq!(find_word("adder", "add"), None);
    }

    #[test]
    fn hover_shapes() {
        assert_eq!(
            hover(&json!({"contents": {"kind": "markdown", "value": "```rust\nfn x()\r\n```"}})),
            "```rust\nfn x()\n```"
        );
        assert_eq!(
            hover(&json!({"contents": [{"language": "go", "value": "func F()"}, "Docs."]})),
            "```go\nfunc F()\n```\n\nDocs."
        );
        assert_eq!(hover(&Value::Null), "No hover information here.");
    }

    #[test]
    fn operation_aliases() {
        assert_eq!(
            Operation::parse("goto_definition"),
            Some(Operation::Definition)
        );
        assert_eq!(Operation::parse("Outline"), Some(Operation::Symbols));
        assert_eq!(Operation::parse("rename"), None);
    }
}
