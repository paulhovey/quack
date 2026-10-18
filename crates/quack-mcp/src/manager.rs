//! The MCP client manager (docs/plan/09-mcp.md#implementation).
//!
//! Every enabled server connects in the background, so servers never hold up startup or a
//! turn: tools appear once their server is ready. Stdio servers that exit are restarted
//! with backoff, up to [`MAX_RESTARTS`] times.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use quack_config::settings::NetworkSettings;
use quack_config::{McpServerFile, glob_match};
use quack_shell::Tree;
use rmcp::ClientHandler;
use rmcp::model::{
    CallToolRequestParams, ClientCapabilities, ClientConfig, Implementation, ProtocolVersion,
};
use rmcp::service::{
    ClientLifecycleMode, NotificationContext, Peer, RoleClient, RunningService,
    serve_client_with_lifecycle_and_ct,
};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use serde_json::Value;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::spawn::{self, Log};

/// Restarts of a server that keeps exiting before it is left failed.
pub const MAX_RESTARTS: u32 = 3;

/// A server file with its id.
#[derive(Debug, Clone)]
pub struct ServerSpec {
    pub id: String,
    pub file: McpServerFile,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerState {
    Starting,
    Ready,
    Failed(String),
    Stopped,
}

/// One tool a ready server offers.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolInfo {
    pub server: String,
    /// The server's name for it.
    pub name: String,
    pub description: String,
    pub schema: Value,
    /// The server says it changes nothing (`readOnlyHint`).
    pub read_only: bool,
    /// Matches the server file's `tools.auto_approve`.
    pub auto_approve: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerStatus {
    pub id: String,
    /// `stdio` or `http`.
    pub transport: String,
    /// The command line or the URL.
    pub target: String,
    pub description: String,
    pub state: ServerState,
    pub tools: Vec<String>,
    /// The server's last stderr lines and log messages.
    pub log: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallOutput {
    pub ok: bool,
    pub text: String,
}

struct Inner {
    state: ServerState,
    tools: Vec<ToolInfo>,
    peer: Option<Peer<RoleClient>>,
    /// Order in which servers became ready, so tools of later servers go at the end.
    ready_seq: u64,
    /// Bumped on every (re)connect; events from older connections are ignored.
    generation: u64,
    restarts: u32,
}

struct Conn {
    service: RunningService<RoleClient, Handler>,
    tree: Option<Tree>,
}

struct Server {
    spec: ServerSpec,
    inner: Mutex<Inner>,
    conn: tokio::sync::Mutex<Option<Conn>>,
    log: Log,
}

pub struct McpManager {
    servers: Vec<Arc<Server>>,
    root: PathBuf,
    network: NetworkSettings,
    /// For local servers whose file sets `sandbox = true`.
    sandbox: Option<Arc<quack_sandbox::Policy>>,
    cancel: CancellationToken,
    shutting_down: AtomicBool,
    seq: AtomicU64,
    changed: watch::Sender<u64>,
}

impl std::fmt::Debug for McpManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpManager")
            .field("servers", &self.servers.len())
            .finish()
    }
}

impl McpManager {
    /// A manager for `specs`; nothing starts until [`Self::start`].
    pub fn new(specs: Vec<ServerSpec>, root: PathBuf, network: NetworkSettings) -> Arc<Self> {
        Self::with_sandbox(specs, root, network, None)
    }

    /// Like [`Self::new`]; servers whose file sets `sandbox = true` run under `sandbox`,
    /// and refuse to start without it.
    pub fn with_sandbox(
        specs: Vec<ServerSpec>,
        root: PathBuf,
        network: NetworkSettings,
        sandbox: Option<Arc<quack_sandbox::Policy>>,
    ) -> Arc<Self> {
        let servers = specs
            .into_iter()
            .map(|spec| {
                Arc::new(Server {
                    spec,
                    inner: Mutex::new(Inner {
                        state: ServerState::Stopped,
                        tools: Vec::new(),
                        peer: None,
                        ready_seq: 0,
                        generation: 0,
                        restarts: 0,
                    }),
                    conn: tokio::sync::Mutex::new(None),
                    log: Log::default(),
                })
            })
            .collect();
        Arc::new(Self {
            servers,
            root,
            network,
            sandbox,
            cancel: CancellationToken::new(),
            shutting_down: AtomicBool::new(false),
            seq: AtomicU64::new(1),
            changed: watch::Sender::new(0),
        })
    }

    pub fn is_empty(&self) -> bool {
        self.servers.is_empty()
    }

    /// Connect every server in the background.
    pub fn start(self: &Arc<Self>) {
        for idx in 0..self.servers.len() {
            tokio::spawn(Arc::clone(self).connect(idx));
        }
    }

    /// Bumped whenever a server's state or tools change.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }

    fn bump(&self) {
        self.changed.send_modify(|n| *n += 1);
    }

    /// Tools of ready servers: servers in the order they became ready, each server's
    /// tools in its own order.
    pub fn tools(&self) -> Vec<ToolInfo> {
        let mut ready: Vec<(u64, Vec<ToolInfo>)> = self
            .servers
            .iter()
            .filter_map(|s| {
                let inner = s.inner.lock().ok()?;
                (inner.state == ServerState::Ready).then(|| (inner.ready_seq, inner.tools.clone()))
            })
            .collect();
        ready.sort_by_key(|(seq, _)| *seq);
        ready.into_iter().flat_map(|(_, t)| t).collect()
    }

    pub fn status(&self) -> Vec<ServerStatus> {
        self.servers
            .iter()
            .map(|s| {
                let (state, tools) = s.inner.lock().map_or_else(
                    |_| (ServerState::Failed("poisoned".into()), Vec::new()),
                    |i| {
                        (
                            i.state.clone(),
                            i.tools.iter().map(|t| t.name.clone()).collect(),
                        )
                    },
                );
                let f = &s.spec.file;
                let (transport, target) = match f.transport() {
                    Ok("http") => ("http", f.url.clone()),
                    Ok(_) => ("stdio", {
                        let mut parts = vec![f.command.clone()];
                        parts.extend(f.args.iter().cloned());
                        parts.join(" ")
                    }),
                    Err(_) => ("?", String::new()),
                };
                ServerStatus {
                    id: s.spec.id.clone(),
                    transport: transport.into(),
                    target,
                    description: f.description.clone(),
                    state,
                    tools,
                    log: s
                        .log
                        .lock()
                        .map(|l| l.iter().cloned().collect())
                        .unwrap_or_default(),
                }
            })
            .collect()
    }

    /// Stop and start a server again (`/mcp restart`).
    pub async fn restart(self: &Arc<Self>, id: &str) -> Result<(), String> {
        let idx = self.index(id)?;
        if let Ok(mut i) = self.servers[idx].inner.lock() {
            i.restarts = 0;
        }
        self.stop(idx).await;
        Arc::clone(self).connect(idx).await;
        Ok(())
    }

    /// Stop every server.
    pub async fn shutdown(&self) {
        self.shutting_down.store(true, Ordering::SeqCst);
        self.cancel.cancel();
        for idx in 0..self.servers.len() {
            self.stop(idx).await;
        }
    }

    fn index(&self, id: &str) -> Result<usize, String> {
        self.servers
            .iter()
            .position(|s| s.spec.id == id)
            .ok_or_else(|| format!("no MCP server `{id}`"))
    }

    async fn stop(&self, idx: usize) {
        let server = &self.servers[idx];
        if let Ok(mut i) = server.inner.lock() {
            i.generation += 1;
            i.state = ServerState::Stopped;
            i.peer = None;
            i.tools.clear();
        }
        if let Some(conn) = server.conn.lock().await.take() {
            let _ = conn.service.cancel().await;
            if let Some(tree) = conn.tree {
                tree.kill();
            }
        }
        self.bump();
    }

    fn set(&self, idx: usize, generation: u64, f: impl FnOnce(&mut Inner)) -> bool {
        let Ok(mut i) = self.servers[idx].inner.lock() else {
            return false;
        };
        if i.generation != generation {
            return false;
        }
        f(&mut i);
        drop(i);
        self.bump();
        true
    }

    async fn connect(self: Arc<Self>, idx: usize) {
        let server = Arc::clone(&self.servers[idx]);
        let generation = match server.inner.lock() {
            Ok(mut i) => {
                i.generation += 1;
                i.state = ServerState::Starting;
                i.tools.clear();
                i.peer = None;
                i.generation
            }
            Err(_) => return,
        };
        self.bump();
        let limit = Duration::from_secs(server.spec.file.startup_timeout_secs.max(1));
        let result = tokio::time::timeout(limit, self.establish(idx, generation)).await;
        let outcome = match result {
            Ok(Ok((conn, tools))) => {
                let peer = conn.service.peer().clone();
                *server.conn.lock().await = Some(conn);
                let seq = self.seq.fetch_add(1, Ordering::SeqCst);
                let current = self.set(idx, generation, |i| {
                    i.state = ServerState::Ready;
                    i.tools = tools;
                    i.peer = Some(peer);
                    i.ready_seq = seq;
                });
                if !current && let Some(conn) = server.conn.lock().await.take() {
                    let _ = conn.service.cancel().await;
                }
                return;
            }
            Ok(Err(e)) => e,
            Err(_) => format!("did not start within {}s", limit.as_secs()),
        };
        let last = server
            .log
            .lock()
            .ok()
            .and_then(|l| l.back().cloned())
            .map_or_else(String::new, |l| format!(" · stderr: {l}"));
        tracing::warn!(server = %server.spec.id, error = %outcome, "MCP server failed to start");
        self.set(idx, generation, |i| {
            i.state = ServerState::Failed(format!("{outcome}{last}"));
        });
    }

    async fn establish(
        self: &Arc<Self>,
        idx: usize,
        generation: u64,
    ) -> Result<(Conn, Vec<ToolInfo>), String> {
        let server = &self.servers[idx];
        let file = server.spec.file.clone();
        let handler = Handler {
            manager: Arc::downgrade(self),
            idx,
        };
        let lifecycle = ClientLifecycleMode::Auto {
            preferred_versions: vec![ProtocolVersion::V_2026_07_28],
            legacy_version: Some(ProtocolVersion::LATEST_WITH_INITIALIZE),
        };
        let ct = self.cancel.child_token();
        let conn = match file.transport()? {
            "http" => {
                self.check_egress(&file.url)?;
                let (client, headers) = http_parts(&file, &self.network).await?;
                let config = StreamableHttpClientTransportConfig::with_uri(file.url.clone())
                    .custom_headers(headers);
                let transport = StreamableHttpClientTransport::with_client(client, config);
                let service = serve_client_with_lifecycle_and_ct(handler, transport, lifecycle, ct)
                    .await
                    .map_err(|e| e.to_string())?;
                Conn {
                    service,
                    tree: None,
                }
            }
            _ => {
                let root = self.root.clone();
                let f = file.clone();
                let prepared = tokio::task::spawn_blocking(move || spawn::prepare(&f, &root))
                    .await
                    .map_err(|e| e.to_string())??;
                let sandbox = match (file.sandbox, &self.sandbox) {
                    (Some(true), Some(p)) => Some(p.clone()),
                    (Some(true), None) => {
                        return Err("the file sets `sandbox = true`, but the sandbox is not available".into());
                    }
                    _ => None,
                };
                let sp = spawn::spawn(prepared, sandbox.as_deref(), server.log.clone())?;
                let served = serve_client_with_lifecycle_and_ct(
                    handler,
                    (sp.stdout, sp.stdin),
                    lifecycle,
                    ct,
                )
                .await;
                let mut child = sp.child;
                let service = match served {
                    Ok(s) => s,
                    Err(e) => {
                        sp.tree.kill();
                        let _ = child.kill().await;
                        return Err(e.to_string());
                    }
                };
                tokio::spawn(Arc::clone(self).watch_exit(idx, generation, child));
                Conn {
                    service,
                    tree: Some(sp.tree),
                }
            }
        };
        let tools = list_tools(conn.service.peer(), &server.spec).await?;
        Ok((conn, tools))
    }

    /// A stdio server exited: mark it failed and start it again, with backoff.
    async fn watch_exit(
        self: Arc<Self>,
        idx: usize,
        generation: u64,
        mut child: tokio::process::Child,
    ) {
        let status = child.wait().await;
        if self.shutting_down.load(Ordering::SeqCst) {
            return;
        }
        let how = status.map_or_else(|e| e.to_string(), |s| s.to_string());
        let mut restart_in = None;
        let current = self.set(idx, generation, |i| {
            i.peer = None;
            i.tools.clear();
            if i.restarts < MAX_RESTARTS {
                i.restarts += 1;
                restart_in = Some(Duration::from_secs(1 << i.restarts));
                i.state = ServerState::Failed(format!(
                    "exited ({how}); restarting ({}/{MAX_RESTARTS})",
                    i.restarts
                ));
            } else {
                i.state = ServerState::Failed(format!("exited ({how}); /mcp restart tries again"));
            }
        });
        if !current {
            return;
        }
        tracing::warn!(server = %self.servers[idx].spec.id, %how, "MCP server exited");
        if let Some(delay) = restart_in {
            tokio::select! {
                () = tokio::time::sleep(delay) => {}
                () = self.cancel.cancelled() => return,
            }
            let still = self.servers[idx]
                .inner
                .lock()
                .is_ok_and(|i| i.generation == generation);
            if still {
                reconnect(self, idx).await;
            }
        }
    }

    /// In offline mode, remote servers must be on this machine or in `egress_allow`.
    fn check_egress(&self, url: &str) -> Result<(), String> {
        if !self.network.offline {
            return Ok(());
        }
        let host = quack_config::url_host(url);
        let local = quack_config::is_loopback(url);
        if local
            || self
                .network
                .egress_allow
                .iter()
                .any(|g| glob_match(g, host))
        {
            Ok(())
        } else {
            Err(format!(
                "blocked by network.offline: add `{host}` to network.egress_allow to use it"
            ))
        }
    }

    /// Call `tool` on `server`. Never fails: errors become an error result for the model.
    pub async fn call(
        &self,
        server: &str,
        tool: &str,
        args: Value,
        cancel: &CancellationToken,
    ) -> CallOutput {
        let fail = |text: String| CallOutput { ok: false, text };
        let Ok(idx) = self.index(server) else {
            return fail(format!("no MCP server `{server}`"));
        };
        let s = &self.servers[idx];
        let (peer, state) = match s.inner.lock() {
            Ok(i) => (i.peer.clone(), i.state.clone()),
            Err(_) => return fail("internal error".into()),
        };
        let Some(peer) = peer else {
            return fail(match state {
                ServerState::Failed(why) => format!("MCP server `{server}` is not running: {why}"),
                _ => format!("MCP server `{server}` is not ready yet"),
            });
        };
        let arguments = match args {
            Value::Object(map) => map,
            Value::Null => serde_json::Map::new(),
            other => {
                return fail(format!("arguments must be a JSON object, got {other}"));
            }
        };
        let params = CallToolRequestParams::new(tool.to_owned()).with_arguments(arguments);
        let limit = Duration::from_secs(s.spec.file.call_timeout_secs.max(1));
        let result = tokio::select! {
            r = tokio::time::timeout(limit, peer.call_tool(params)) => r,
            () = cancel.cancelled() => return fail("interrupted by the user".into()),
        };
        let max_chars = s.spec.file.max_output_tokens as usize * 4;
        match result {
            Ok(Ok(r)) => {
                let value = serde_json::to_value(&r).unwrap_or_default();
                let (is_error, text) = crate::output::render(&value, max_chars);
                CallOutput {
                    ok: !is_error,
                    text,
                }
            }
            Ok(Err(e)) => fail(format!("MCP error: {e}")),
            Err(_) => fail(format!(
                "no answer within {}s (call_timeout_secs)",
                limit.as_secs()
            )),
        }
    }

    async fn relist(&self, idx: usize, peer: Peer<RoleClient>) {
        let spec = &self.servers[idx].spec;
        match list_tools(&peer, spec).await {
            Ok(tools) => {
                let generation = self.servers[idx].inner.lock().map_or(0, |i| i.generation);
                self.set(idx, generation, |i| i.tools = tools);
            }
            Err(e) => tracing::warn!(server = %spec.id, error = %e, "re-listing tools failed"),
        }
    }
}

/// `connect` again from inside a connection's own task; boxed to break the async cycle.
fn reconnect(m: Arc<McpManager>, idx: usize) -> std::pin::Pin<Box<dyn Future<Output = ()> + Send>> {
    Box::pin(m.connect(idx))
}

async fn list_tools(peer: &Peer<RoleClient>, spec: &ServerSpec) -> Result<Vec<ToolInfo>, String> {
    let offers_tools = peer
        .peer_info()
        .is_none_or(|info| info.capabilities.tools.is_some());
    if !offers_tools {
        return Ok(Vec::new());
    }
    let tools = peer
        .list_all_tools()
        .await
        .map_err(|e| format!("listing tools: {e}"))?;
    let rules = &spec.file.tools;
    Ok(tools
        .into_iter()
        .filter(|t| rules.allow.is_empty() || rules.allow.iter().any(|g| glob_match(g, &t.name)))
        .filter(|t| !rules.deny.iter().any(|g| glob_match(g, &t.name)))
        .map(|t| ToolInfo {
            server: spec.id.clone(),
            auto_approve: rules.auto_approve.iter().any(|g| glob_match(g, &t.name)),
            read_only: t
                .annotations
                .as_ref()
                .and_then(|a| a.read_only_hint)
                .unwrap_or(false),
            description: t.description.as_deref().unwrap_or_default().to_owned(),
            schema: Value::Object((*t.input_schema).clone()),
            name: t.name.into_owned(),
        })
        .collect())
}

type Headers = std::collections::HashMap<
    quack_providers::reqwest::header::HeaderName,
    quack_providers::reqwest::header::HeaderValue,
>;

/// The HTTP client and the file's headers, with secrets resolved off the runtime.
async fn http_parts(
    file: &McpServerFile,
    network: &NetworkSettings,
) -> Result<(quack_providers::reqwest::Client, Headers), String> {
    use quack_providers::reqwest::header::{HeaderName, HeaderValue};
    let (file, network) = (file.clone(), network.clone());
    tokio::task::spawn_blocking(move || {
        let get = |k: &str| std::env::var_os(k);
        let client = quack_providers::http_client(&network, &get)?;
        let mut headers = Headers::new();
        for (k, secret) in &file.headers {
            let v = secret
                .resolve(&get)
                .map_err(|e| format!("header `{k}`: {e}"))?;
            let name = HeaderName::from_bytes(k.as_bytes())
                .map_err(|_| format!("invalid header name `{k}`"))?;
            let mut value = HeaderValue::from_str(&v)
                .map_err(|_| format!("header `{k}` has invalid characters"))?;
            value.set_sensitive(true);
            headers.insert(name, value);
        }
        Ok((client, headers))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Re-lists tools when the server says they changed. Roots and logging are left out:
/// both are deprecated in 2026-07-28, and stdio servers start in the workspace root.
#[derive(Clone)]
struct Handler {
    manager: Weak<McpManager>,
    idx: usize,
}

impl ClientHandler for Handler {
    fn get_info(&self) -> ClientConfig {
        ClientConfig::new(
            ClientCapabilities::default(),
            Implementation::new("quack", env!("CARGO_PKG_VERSION")),
        )
    }

    async fn on_tool_list_changed(&self, context: NotificationContext<RoleClient>) {
        if let Some(m) = self.manager.upgrade() {
            m.relist(self.idx, context.peer.clone()).await;
        }
    }
}

/// The model-facing name of an MCP tool: `mcp__<server>__<tool>`, limited to the characters
/// and length every tool-calling API accepts.
pub fn model_tool_name(server: &str, tool: &str) -> String {
    let raw = format!("mcp__{server}__{tool}");
    raw.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_names() {
        assert_eq!(
            model_tool_name("git hub", "get.issue"),
            "mcp__git_hub__get_issue"
        );
        assert_eq!(model_tool_name("s", &"x".repeat(100)).len(), 64);
    }
}
