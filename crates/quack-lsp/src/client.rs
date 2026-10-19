//! One language server process: requests and responses, the server's own requests,
//! document sync, diagnostics and progress.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use quack_shell::Tree;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{ChildStdin, Command};
use tokio::sync::{oneshot, watch};

use crate::diag::{Diagnostic, Snapshot};
use crate::pos::{self, Encoding};
use crate::rpc;

/// Lines of stderr kept for `/lsp`.
const LOG_LINES: usize = 40;
const INITIALIZE_TIMEOUT: Duration = Duration::from_secs(30);
/// Open files whose diagnostics are pulled after an edit, when files depend on each other.
const MAX_PULLED: usize = 20;
/// Files larger than this are not sent to the server.
const MAX_DOCUMENT: u64 = 4 * 1024 * 1024;

type Pending = Mutex<HashMap<i64, oneshot::Sender<Result<Value, String>>>>;

#[derive(Debug, Default)]
struct State {
    diagnostics: Snapshot,
    /// Sequence number of the last `publishDiagnostics` per file.
    published: HashMap<PathBuf, u64>,
    seq: u64,
    /// Work in progress (`$/progress`): token → title.
    progress: HashMap<String, String>,
    docs: HashMap<PathBuf, Doc>,
}

#[derive(Debug)]
struct Doc {
    version: i64,
    text: String,
}

/// What the client needs to start a server.
pub struct Launch {
    pub id: String,
    pub command: Command,
    pub root: PathBuf,
    pub initialization_options: Value,
    pub settings: Value,
    pub language_ids: Box<dyn Fn(&str) -> String + Send + Sync>,
}

impl std::fmt::Debug for Launch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Launch")
            .field("id", &self.id)
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

enum SyncStep {
    Open,
    Change(i64),
}

pub struct Client {
    pub id: String,
    pub root: PathBuf,
    stdin: tokio::sync::Mutex<ChildStdin>,
    next_id: AtomicI64,
    pending: Arc<Pending>,
    state: Arc<Mutex<State>>,
    changed: Arc<watch::Sender<u64>>,
    alive: Arc<AtomicBool>,
    caps: OnceLock<Value>,
    encoding: OnceLock<Encoding>,
    log: Arc<Mutex<VecDeque<String>>>,
    tree: Tree,
    language_ids: Box<dyn Fn(&str) -> String + Send + Sync>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("id", &self.id)
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

impl Client {
    /// Start the server and complete `initialize`.
    pub async fn start(
        launch: Launch,
        manager_changed: Arc<watch::Sender<u64>>,
    ) -> Result<Arc<Self>, String> {
        let Launch {
            id,
            mut command,
            root,
            initialization_options,
            settings,
            language_ids,
        } = launch;
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);
        #[cfg(windows)]
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        let mut child = command
            .spawn()
            .map_err(|e| format!("cannot start {id}: {e}"))?;
        let tree = Tree::new(&child);
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            return Err("the server's pipes are not available".into());
        };
        let log: Arc<Mutex<VecDeque<String>>> = Arc::default();
        if let Some(stderr) = child.stderr.take() {
            let log = log.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::trace!(target: "lsp", "{line}");
                    push(&log, line);
                }
            });
        }

        let client = Arc::new(Self {
            id,
            root,
            stdin: tokio::sync::Mutex::new(stdin),
            next_id: AtomicI64::new(1),
            pending: Arc::default(),
            state: Arc::default(),
            changed: manager_changed,
            alive: Arc::new(AtomicBool::new(true)),
            caps: OnceLock::new(),
            encoding: OnceLock::new(),
            log,
            tree,
            language_ids,
        });

        let reader = Reader {
            pending: client.pending.clone(),
            state: client.state.clone(),
            changed: client.changed.clone(),
            alive: client.alive.clone(),
            log: client.log.clone(),
            settings,
            root_uri: pos::path_to_uri(&client.root),
        };
        let weak = Arc::downgrade(&client);
        tokio::spawn(async move {
            let mut child = child;
            reader.run(stdout, weak).await;
            // Reap the process; the tree is killed when the client is dropped.
            let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
        });

        let root_uri = pos::path_to_uri(&client.root);
        let name = client
            .root
            .file_name()
            .map_or_else(|| "workspace".into(), |n| n.to_string_lossy().into_owned());
        let params = json!({
            "processId": std::process::id(),
            "clientInfo": { "name": quack_config::APP_NAME, "version": env!("CARGO_PKG_VERSION") },
            "rootUri": root_uri,
            "rootPath": client.root.display().to_string(),
            "workspaceFolders": [{ "uri": root_uri, "name": name }],
            "initializationOptions": initialization_options,
            "capabilities": capabilities(),
        });
        let result = match client
            .request("initialize", params, INITIALIZE_TIMEOUT)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                // The server's own words are usually the useful part (e.g. a rustup proxy
                // with no rust-analyzer component behind it).
                tokio::time::sleep(Duration::from_millis(100)).await;
                let said = client
                    .log()
                    .into_iter()
                    .rev()
                    .find(|l| !l.trim().is_empty());
                return Err(match said {
                    Some(line) => format!("initialize: {e}: {}", line.trim()),
                    None => format!("initialize: {e}"),
                });
            }
        };
        let caps = result["capabilities"].clone();
        let _ = client
            .encoding
            .set(Encoding::from_server(caps["positionEncoding"].as_str()));
        let _ = client.caps.set(caps);
        client.notify("initialized", json!({})).await?;
        Ok(client)
    }

    pub fn alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    pub fn encoding(&self) -> Encoding {
        self.encoding.get().copied().unwrap_or(Encoding::Utf16)
    }

    /// Whether the server advertises a capability, e.g. `definitionProvider`.
    pub fn supports(&self, capability: &str) -> bool {
        self.caps
            .get()
            .is_some_and(|c| !matches!(c[capability], Value::Null | Value::Bool(false)))
    }

    pub fn log(&self) -> Vec<String> {
        self.log
            .lock()
            .map(|l| l.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Titles of work the server reports as running (indexing, `cargo check`, …).
    pub fn progress(&self) -> Vec<String> {
        self.state
            .lock()
            .map(|s| s.progress.values().cloned().collect())
            .unwrap_or_default()
    }

    pub fn snapshot(&self) -> Snapshot {
        self.state
            .lock()
            .map(|s| s.diagnostics.clone())
            .unwrap_or_default()
    }

    pub fn diagnostics(&self, path: &Path) -> Vec<Diagnostic> {
        self.state
            .lock()
            .ok()
            .and_then(|s| s.diagnostics.get(&pos::key(path)).cloned())
            .unwrap_or_default()
    }

    fn seq(&self) -> u64 {
        self.state.lock().map(|s| s.seq).unwrap_or_default()
    }

    pub async fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, String> {
        if !self.alive() {
            return Err(format!("{} is not running", self.id));
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        if let Ok(mut p) = self.pending.lock() {
            p.insert(id, tx);
        }
        let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        if let Err(e) = rpc::write(&mut *self.stdin.lock().await, &msg).await {
            self.forget(id);
            return Err(format!("{}: {e}", self.id));
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => Err(format!("{} exited", self.id)),
            Err(_) => {
                self.forget(id);
                let _ = self.notify("$/cancelRequest", json!({ "id": id })).await;
                Err(format!(
                    "{} did not answer {method} in {}s",
                    self.id,
                    timeout.as_secs()
                ))
            }
        }
    }

    fn forget(&self, id: i64) {
        if let Ok(mut p) = self.pending.lock() {
            p.remove(&id);
        }
    }

    pub async fn notify(&self, method: &str, params: Value) -> Result<(), String> {
        let msg = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        rpc::write(&mut *self.stdin.lock().await, &msg)
            .await
            .map_err(|e| format!("{}: {e}", self.id))
    }

    /// Send the file's current content if the server hasn't seen it: `didOpen` the first
    /// time, `didChange` (full text) when it changed on disk. Returns the text.
    pub async fn sync(&self, path: &Path) -> Result<String, String> {
        let key = pos::key(path);
        let uri = pos::path_to_uri(path);
        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        let text = if size > MAX_DOCUMENT {
            None
        } else {
            // A file just written can be briefly unreadable (an antivirus or an indexer
            // holds it, mostly on Windows), so try again before giving up.
            let mut delay = Duration::from_millis(25);
            loop {
                match std::fs::read(path) {
                    Ok(b) => break Some(String::from_utf8_lossy(&b).into_owned()),
                    Err(e) if delay <= Duration::from_millis(200) => {
                        tracing::debug!(
                            error = %e,
                            "cannot read {} yet, retrying",
                            path.display()
                        );
                        tokio::time::sleep(delay).await;
                        delay *= 2;
                    }
                    Err(_) => break None,
                }
            }
        };
        let Some(text) = text else {
            let open = self
                .state
                .lock()
                .is_ok_and(|mut s| s.docs.remove(&key).is_some());
            if open {
                self.notify(
                    "textDocument/didClose",
                    json!({ "textDocument": { "uri": uri } }),
                )
                .await?;
            }
            return Err(format!("cannot read {}", path.display()));
        };
        let action = self
            .state
            .lock()
            .ok()
            .and_then(|mut s| match s.docs.get_mut(&key) {
                None => {
                    s.docs.insert(
                        key.clone(),
                        Doc {
                            version: 1,
                            text: text.clone(),
                        },
                    );
                    Some(SyncStep::Open)
                }
                Some(d) if d.text != text => {
                    d.version += 1;
                    d.text.clone_from(&text);
                    Some(SyncStep::Change(d.version))
                }
                Some(_) => None,
            });
        match action {
            Some(SyncStep::Open) => {
                let ext = path
                    .extension()
                    .map(|e| e.to_string_lossy().into_owned())
                    .unwrap_or_default();
                self.notify(
                    "textDocument/didOpen",
                    json!({ "textDocument": {
                        "uri": uri,
                        "languageId": (self.language_ids)(&ext),
                        "version": 1,
                        "text": text,
                    }}),
                )
                .await?;
            }
            Some(SyncStep::Change(version)) => {
                self.notify(
                    "textDocument/didChange",
                    json!({
                        "textDocument": { "uri": uri, "version": version },
                        "contentChanges": [{ "text": text }],
                    }),
                )
                .await?;
            }
            None => {}
        }
        Ok(text)
    }

    /// Sync the file after an edit, save it, and wait for diagnostics to settle. `Err` when
    /// the file could not be sent at all, so the caller can say why diagnostics are missing.
    pub async fn changed(
        &self,
        path: &Path,
        settle: Duration,
        max_wait: Duration,
    ) -> Result<(), String> {
        let started = Instant::now();
        let since = self.seq();
        let mut rx = self.changed.subscribe();
        self.sync(path).await?;
        let uri = pos::path_to_uri(path);
        let _ = self
            .notify(
                "textDocument/didSave",
                json!({ "textDocument": { "uri": uri } }),
            )
            .await;
        if self.supports("diagnosticProvider") {
            self.pull_after_change(path, max_wait).await;
            // A server that only answers pulls has nothing more to send.
            if !self.pushes() {
                return Ok(());
            }
        }
        let key = pos::key(path);
        let mut quiet_since = Instant::now();
        loop {
            let (seen, busy) = self.state.lock().map_or((true, false), |s| {
                (
                    s.published.get(&key).is_some_and(|n| *n > since),
                    !s.progress.is_empty(),
                )
            });
            let now = Instant::now();
            // Nothing published yet: some servers send nothing for an unchanged result, so
            // give up waiting for a first message after twice the settle time.
            let waited_enough = seen || now - started >= settle * 2;
            if (waited_enough && !busy && now - quiet_since >= settle)
                || now - started >= max_wait
                || !self.alive()
            {
                return Ok(());
            }
            let wake = (quiet_since + settle).max(now + Duration::from_millis(20));
            let wake = wake.min(started + max_wait);
            tokio::select! {
                r = rx.changed() => {
                    if r.is_err() {
                        return Ok(());
                    }
                    quiet_since = Instant::now();
                }
                () = tokio::time::sleep_until(wake.into()) => {}
            }
        }
    }

    /// Whether the server has pushed diagnostics (`publishDiagnostics`) this session.
    fn pushes(&self) -> bool {
        self.state.lock().is_ok_and(|s| !s.published.is_empty())
    }

    /// Pull diagnostics for the changed file and, when the server says files depend on
    /// each other, for the other open files.
    async fn pull_after_change(&self, path: &Path, timeout: Duration) {
        let key = pos::key(path);
        let mut files = vec![key.clone()];
        let cross = self
            .caps
            .get()
            .is_some_and(|c| c["diagnosticProvider"]["interFileDependencies"] == true);
        if cross && let Ok(s) = self.state.lock() {
            files.extend(
                s.docs
                    .keys()
                    .filter(|k| **k != key)
                    .take(MAX_PULLED - 1)
                    .cloned(),
            );
        }
        let pulls = files.into_iter().map(|f| async move {
            let r = self
                .request(
                    "textDocument/diagnostic",
                    json!({ "textDocument": { "uri": pos::path_to_uri(&f) } }),
                    timeout,
                )
                .await;
            (f, r)
        });
        for (file, r) in futures::future::join_all(pulls).await {
            let Ok(r) = r else { continue };
            if r["kind"] == "unchanged" {
                continue;
            }
            let list: Vec<Diagnostic> = r["items"]
                .as_array()
                .map(|a| a.iter().filter_map(Diagnostic::from_lsp).collect())
                .unwrap_or_default();
            if let Ok(mut s) = self.state.lock() {
                if list.is_empty() {
                    s.diagnostics.remove(&file);
                } else {
                    s.diagnostics.insert(file, list);
                }
            }
        }
    }

    /// `shutdown`, `exit`, then kill whatever is left.
    pub async fn stop(&self) {
        if self.alive() {
            let _ = self
                .request("shutdown", Value::Null, Duration::from_secs(2))
                .await;
            let _ = self.notify("exit", Value::Null).await;
            for _ in 0..10 {
                if !self.alive() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
        self.tree.kill();
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.tree.kill();
    }
}

fn push(log: &Mutex<VecDeque<String>>, line: String) {
    if let Ok(mut l) = log.lock() {
        if l.len() == LOG_LINES {
            l.pop_front();
        }
        l.push_back(line);
    }
}

/// The part of the client the reader task needs.
struct Reader {
    pending: Arc<Pending>,
    state: Arc<Mutex<State>>,
    changed: Arc<watch::Sender<u64>>,
    alive: Arc<AtomicBool>,
    log: Arc<Mutex<VecDeque<String>>>,
    settings: Value,
    root_uri: String,
}

impl Reader {
    async fn run(self, stdout: tokio::process::ChildStdout, client: std::sync::Weak<Client>) {
        let mut r = BufReader::new(stdout);
        loop {
            let msg = match rpc::read(&mut r).await {
                Ok(Some(m)) => m,
                Ok(None) => break,
                Err(e) => {
                    push(
                        &self.log,
                        format!("[quack] bad message from the server: {e}"),
                    );
                    break;
                }
            };
            let method = msg["method"].as_str();
            match (method, msg.get("id")) {
                (None, Some(id)) => self.response(id, &msg),
                (Some(m), Some(id)) => {
                    let result = self.server_request(m, &msg["params"]);
                    let reply = match result {
                        Ok(v) => json!({ "jsonrpc": "2.0", "id": id, "result": v }),
                        Err(e) => json!({ "jsonrpc": "2.0", "id": id,
                            "error": { "code": -32601, "message": e } }),
                    };
                    let Some(c) = client.upgrade() else { break };
                    let _ = rpc::write(&mut *c.stdin.lock().await, &reply).await;
                }
                (Some(m), None) => self.notification(m, &msg["params"]),
                (None, None) => {}
            }
        }
        self.alive.store(false, Ordering::SeqCst);
        if let Ok(mut p) = self.pending.lock() {
            for (_, tx) in p.drain() {
                let _ = tx.send(Err("the server exited".into()));
            }
        }
        if let Ok(mut s) = self.state.lock() {
            s.progress.clear();
        }
        self.changed.send_modify(|n| *n += 1);
    }

    fn response(&self, id: &Value, msg: &Value) {
        let Some(id) = id.as_i64() else { return };
        let tx = self.pending.lock().ok().and_then(|mut p| p.remove(&id));
        if let Some(tx) = tx {
            let r = match msg.get("error") {
                Some(e) => Err(e["message"].as_str().unwrap_or("error").to_owned()),
                None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
            };
            let _ = tx.send(r);
        }
    }

    fn server_request(&self, method: &str, params: &Value) -> Result<Value, String> {
        match method {
            "workspace/configuration" => Ok(Value::Array(
                params["items"]
                    .as_array()
                    .map(|items| {
                        items
                            .iter()
                            .map(|i| section(&self.settings, i["section"].as_str()))
                            .collect()
                    })
                    .unwrap_or_default(),
            )),
            "workspace/workspaceFolders" => {
                Ok(json!([{ "uri": self.root_uri, "name": "workspace" }]))
            }
            "window/workDoneProgress/create"
            | "client/registerCapability"
            | "client/unregisterCapability"
            | "window/showMessageRequest"
            | "workspace/diagnostic/refresh"
            | "workspace/semanticTokens/refresh"
            | "workspace/inlayHint/refresh"
            | "workspace/codeLens/refresh" => Ok(Value::Null),
            // Edits only happen through quack's own tools, with approval.
            "workspace/applyEdit" => {
                Ok(json!({ "applied": false, "failureReason": "not supported" }))
            }
            other => Err(format!("{other} is not supported")),
        }
    }

    fn notification(&self, method: &str, params: &Value) {
        match method {
            "textDocument/publishDiagnostics" => {
                let Some(path) = params["uri"].as_str().and_then(pos::uri_to_path) else {
                    return;
                };
                let list: Vec<Diagnostic> = params["diagnostics"]
                    .as_array()
                    .map(|a| a.iter().filter_map(Diagnostic::from_lsp).collect())
                    .unwrap_or_default();
                if let Ok(mut s) = self.state.lock() {
                    let key = pos::key(&path);
                    s.seq += 1;
                    let seq = s.seq;
                    s.published.insert(key.clone(), seq);
                    if list.is_empty() {
                        s.diagnostics.remove(&key);
                    } else {
                        s.diagnostics.insert(key, list);
                    }
                }
            }
            "$/progress" => {
                let token = match &params["token"] {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                let value = &params["value"];
                if let Ok(mut s) = self.state.lock() {
                    match value["kind"].as_str() {
                        Some("begin") => {
                            let title = value["title"].as_str().unwrap_or("working");
                            s.progress.insert(token, title.to_owned());
                        }
                        Some("end") => {
                            s.progress.remove(&token);
                        }
                        _ => return,
                    }
                }
            }
            "window/logMessage" | "window/showMessage" => {
                if let Some(m) = params["message"].as_str() {
                    push(&self.log, m.lines().next().unwrap_or_default().to_owned());
                }
                return;
            }
            _ => return,
        }
        self.changed.send_modify(|n| *n += 1);
    }
}

/// The value at a dotted `section` of the settings table (`null` when absent).
fn section(settings: &Value, name: Option<&str>) -> Value {
    let Some(name) = name.filter(|n| !n.is_empty()) else {
        return settings.clone();
    };
    if let Some(v) = settings.get(name) {
        return v.clone();
    }
    name.split('.')
        .try_fold(settings, |v, part| v.get(part))
        .cloned()
        .unwrap_or(Value::Null)
}

fn capabilities() -> Value {
    json!({
        "general": { "positionEncodings": ["utf-8", "utf-16"] },
        "workspace": {
            "configuration": true,
            "workspaceFolders": true,
            "symbol": { "dynamicRegistration": false },
        },
        "window": { "workDoneProgress": true },
        "textDocument": {
            "synchronization": { "didSave": true, "dynamicRegistration": false },
            "publishDiagnostics": { "relatedInformation": false, "versionSupport": true },
            "definition": { "linkSupport": false },
            "references": {},
            "hover": { "contentFormat": ["markdown", "plaintext"] },
            "documentSymbol": { "hierarchicalDocumentSymbolSupport": true },
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configuration_sections() {
        let s = json!({ "python": { "analysis": { "typeCheckingMode": "strict" } }, "rust-analyzer.cargo.features": "all" });
        assert_eq!(
            section(&s, Some("python.analysis")),
            json!({ "typeCheckingMode": "strict" })
        );
        assert_eq!(
            section(&s, Some("rust-analyzer.cargo.features")),
            json!("all")
        );
        assert_eq!(section(&s, Some("missing")), Value::Null);
        assert_eq!(section(&s, None), s);
    }
}
