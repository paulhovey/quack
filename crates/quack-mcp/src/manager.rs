//! The MCP client manager (docs/plan/09-mcp.md#implementation).
//!
//! Every enabled server connects in the background, so servers never hold up startup or a
//! turn: tools appear once their server is ready. Stdio servers that exit are restarted
//! with backoff, up to [`MAX_RESTARTS`] times.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use quack_config::settings::NetworkSettings;
use quack_config::{McpServerFile, glob_match};
use quack_shell::Tree;
use rmcp::ClientHandler;
use rmcp::ErrorData;
use rmcp::model::{
    CallToolRequestParams, ClientCapabilities, ClientConfig, ElicitRequestParams, ElicitResult,
    ElicitationAction, ElicitationCapability, FormElicitationCapability, GetPromptRequestParams,
    Implementation, ProtocolVersion, ReadResourceRequestParams, SamplingCapability,
};
use rmcp::service::{
    ClientLifecycleMode, NotificationContext, Peer, RequestContext, RoleClient, RunningService,
    serve_client_with_lifecycle_and_ct,
};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use serde_json::Value;
use tokio::sync::{mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;

use crate::ask::{Elicited, Form, SampleRequest, ServerAsk};
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

/// One prompt a ready server offers; the user runs it as `/<server>:<prompt>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptInfo {
    pub server: String,
    pub name: String,
    pub description: String,
    pub arguments: Vec<PromptArg>,
}

impl PromptInfo {
    /// The slash command that runs it, without the slash.
    pub fn command(&self) -> String {
        format!("{}:{}", self.server, self.name)
    }

    /// What to type after the command, e.g. `<file> [focus]`.
    pub fn usage(&self) -> String {
        self.arguments
            .iter()
            .map(|a| {
                if a.required {
                    format!("<{}>", a.name)
                } else {
                    format!("[{}]", a.name)
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// The arguments typed after the command. `name=value` pairs set arguments by name
    /// (a value runs to the next pair); otherwise words fill the arguments in order, and
    /// the last one takes the rest of the line.
    pub fn parse_args<'a>(&self, line: &'a str) -> Result<serde_json::Map<String, Value>, String> {
        let line = line.trim();
        let mut out = serde_json::Map::new();
        let named = |w: &'a str| {
            w.split_once('=')
                .filter(|(k, _)| self.arguments.iter().any(|a| a.name == *k))
        };
        if line
            .split_whitespace()
            .next()
            .is_some_and(|w| named(w).is_some())
        {
            let mut key: Option<String> = None;
            let mut value: Vec<&str> = Vec::new();
            for w in line.split_whitespace() {
                if let Some((k, v)) = named(w) {
                    if let Some(k) = key.take() {
                        out.insert(k, value.join(" ").into());
                    }
                    key = Some(k.to_owned());
                    value = vec![v];
                } else {
                    value.push(w);
                }
            }
            if let Some(k) = key {
                out.insert(k, value.join(" ").into());
            }
        } else if !line.is_empty() {
            let mut rest = line;
            for (i, a) in self.arguments.iter().enumerate() {
                if rest.is_empty() {
                    break;
                }
                if i + 1 == self.arguments.len() {
                    out.insert(a.name.clone(), rest.into());
                    rest = "";
                } else {
                    let (word, tail) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
                    out.insert(a.name.clone(), word.into());
                    rest = tail.trim_start();
                }
            }
            if !rest.is_empty() {
                return Err(format!("takes no arguments; got `{rest}`"));
            }
        }
        let missing: Vec<&str> = self
            .arguments
            .iter()
            .filter(|a| a.required && !out.contains_key(&a.name))
            .map(|a| a.name.as_str())
            .collect();
        if !missing.is_empty() {
            return Err(format!(
                "missing {}; usage: /{} {}",
                missing.join(", "),
                self.command(),
                self.usage()
            ));
        }
        Ok(out)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptArg {
    pub name: String,
    pub description: String,
    pub required: bool,
}

/// One resource (or resource template) a ready server lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceInfo {
    pub server: String,
    /// The URI, or for a template the URI template (`file:///{path}`).
    pub uri: String,
    pub name: String,
    pub description: String,
    pub mime_type: String,
    pub template: bool,
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
    pub prompts: Vec<String>,
    /// Resource URIs and URI templates.
    pub resources: Vec<String>,
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
    prompts: Vec<PromptInfo>,
    resources: Vec<ResourceInfo>,
    service: Option<Arc<Service>>,
    /// Order in which servers became ready, so tools of later servers go at the end.
    ready_seq: u64,
    /// Bumped on every (re)connect; events from older connections are ignored.
    generation: u64,
    restarts: u32,
}

type Service = RunningService<RoleClient, Handler>;

struct Conn {
    service: Arc<Service>,
    tree: Option<Tree>,
}

/// Stop a connection; one a request still holds is cancelled instead of awaited.
async fn close(service: Arc<Service>) {
    match Arc::try_unwrap(service) {
        Ok(s) => {
            let _ = s.cancel().await;
        }
        Err(shared) => shared.cancellation_token().cancel(),
    }
}

struct Server {
    spec: ServerSpec,
    inner: Mutex<Inner>,
    conn: tokio::sync::Mutex<Option<Conn>>,
    log: Log,
    /// Forms and sampling requests waiting on the user, and how many have finished:
    /// time spent on them doesn't count toward `call_timeout_secs`.
    asking: AtomicUsize,
    asked: AtomicU64,
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
    /// Where elicitation and sampling requests go; `None`: servers are told quack does
    /// neither.
    asker: Mutex<Option<mpsc::Sender<ServerAsk>>>,
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
                        prompts: Vec::new(),
                        resources: Vec::new(),
                        service: None,
                        ready_seq: 0,
                        generation: 0,
                        restarts: 0,
                    }),
                    conn: tokio::sync::Mutex::new(None),
                    log: Log::default(),
                    asking: AtomicUsize::new(0),
                    asked: AtomicU64::new(0),
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
            asker: Mutex::new(None),
        })
    }

    /// Answer servers' forms and sampling requests through `asker`. Call it before
    /// [`Self::start`]: servers learn what quack can do when they connect.
    pub fn set_asker(&self, asker: mpsc::Sender<ServerAsk>) {
        if let Ok(mut a) = self.asker.lock() {
            *a = Some(asker);
        }
    }

    fn asker(&self) -> Option<mpsc::Sender<ServerAsk>> {
        self.asker.lock().ok().and_then(|a| a.clone())
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
        self.ready(|i| i.tools.clone())
    }

    /// Prompts of ready servers, in the order the servers became ready.
    pub fn prompts(&self) -> Vec<PromptInfo> {
        self.ready(|i| i.prompts.clone())
    }

    /// Resources and resource templates of ready servers.
    pub fn resources(&self) -> Vec<ResourceInfo> {
        self.ready(|i| i.resources.clone())
    }

    fn ready<T>(&self, f: impl Fn(&Inner) -> Vec<T>) -> Vec<T> {
        let mut ready: Vec<(u64, Vec<T>)> = self
            .servers
            .iter()
            .filter_map(|s| {
                let inner = s.inner.lock().ok()?;
                (inner.state == ServerState::Ready).then(|| (inner.ready_seq, f(&inner)))
            })
            .collect();
        ready.sort_by_key(|(seq, _)| *seq);
        ready.into_iter().flat_map(|(_, t)| t).collect()
    }

    pub fn status(&self) -> Vec<ServerStatus> {
        self.servers
            .iter()
            .map(|s| {
                let (state, tools, prompts, resources) = s.inner.lock().map_or_else(
                    |_| {
                        let none = Vec::new;
                        (
                            ServerState::Failed("poisoned".into()),
                            none(),
                            none(),
                            none(),
                        )
                    },
                    |i| {
                        (
                            i.state.clone(),
                            i.tools.iter().map(|t| t.name.clone()).collect(),
                            i.prompts.iter().map(|p| p.name.clone()).collect(),
                            i.resources.iter().map(|r| r.uri.clone()).collect(),
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
                    prompts,
                    resources,
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
            i.service = None;
            i.tools.clear();
            i.prompts.clear();
            i.resources.clear();
        }
        if let Some(conn) = server.conn.lock().await.take() {
            close(conn.service).await;
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
                i.prompts.clear();
                i.resources.clear();
                i.service = None;
                i.generation
            }
            Err(_) => return,
        };
        self.bump();
        let limit = Duration::from_secs(server.spec.file.startup_timeout_secs.max(1));
        let result = tokio::time::timeout(limit, self.establish(idx, generation)).await;
        let outcome = match result {
            Ok(Ok((conn, tools, prompts, resources))) => {
                let service = Arc::clone(&conn.service);
                *server.conn.lock().await = Some(conn);
                let seq = self.seq.fetch_add(1, Ordering::SeqCst);
                let current = self.set(idx, generation, |i| {
                    i.state = ServerState::Ready;
                    i.tools = tools;
                    i.prompts = prompts;
                    i.resources = resources;
                    i.service = Some(service);
                    i.ready_seq = seq;
                });
                if !current && let Some(conn) = server.conn.lock().await.take() {
                    close(conn.service).await;
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
    ) -> Result<(Conn, Vec<ToolInfo>, Vec<PromptInfo>, Vec<ResourceInfo>), String> {
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
                    service: Arc::new(service),
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
                        return Err(
                            "the file sets `sandbox = true`, but the sandbox is not available"
                                .into(),
                        );
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
                    service: Arc::new(service),
                    tree: Some(sp.tree),
                }
            }
        };
        let peer = conn.service.peer();
        let tools = list_tools(peer, &server.spec).await?;
        // A server whose prompts or resources fail to list still serves its tools.
        let id = &server.spec.id;
        let prompts = list_prompts(peer, id).await.unwrap_or_else(|e| {
            tracing::warn!(server = %id, error = %e, "listing prompts failed");
            Vec::new()
        });
        let resources = list_resources(peer, id).await.unwrap_or_else(|e| {
            tracing::warn!(server = %id, error = %e, "listing resources failed");
            Vec::new()
        });
        Ok((conn, tools, prompts, resources))
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
            i.service = None;
            i.tools.clear();
            i.prompts.clear();
            i.resources.clear();
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
        let (s, service) = match self.service(server) {
            Ok(found) => found,
            Err(e) => return fail(e),
        };
        let arguments = match args {
            Value::Object(map) => map,
            Value::Null => serde_json::Map::new(),
            other => {
                return fail(format!("arguments must be a JSON object, got {other}"));
            }
        };
        let params = CallToolRequestParams::new(tool.to_owned()).with_arguments(arguments);
        let result = tokio::select! {
            r = timed(s, service.call_tool(params)) => r,
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
            Err(e) => fail(e),
        }
    }

    /// The server and its connection, or why it can't take a request.
    fn service(&self, server: &str) -> Result<(&Server, Arc<Service>), String> {
        let s = &self.servers[self.index(server)?];
        let (service, state) = match s.inner.lock() {
            Ok(i) => (i.service.clone(), i.state.clone()),
            Err(_) => return Err("internal error".into()),
        };
        match (service, state) {
            (Some(service), _) => Ok((s, service)),
            (None, ServerState::Failed(why)) => {
                Err(format!("MCP server `{server}` is not running: {why}"))
            }
            (None, _) => Err(format!("MCP server `{server}` is not ready yet")),
        }
    }

    /// Get a prompt's messages as one text, with `args` filled in (`prompts/get`).
    pub async fn get_prompt(
        &self,
        server: &str,
        name: &str,
        args: serde_json::Map<String, Value>,
    ) -> Result<String, String> {
        let (s, service) = self.service(server)?;
        let params = GetPromptRequestParams::new(name).with_arguments(args);
        let r = timed(s, service.get_prompt(params))
            .await?
            .map_err(|e| format!("MCP error: {e}"))?;
        Ok(crate::output::prompt(
            &serde_json::to_value(&r).unwrap_or_default(),
        ))
    }

    /// Read a resource (`resources/read`). Never fails: errors become an error result.
    pub async fn read_resource(&self, server: &str, uri: &str) -> CallOutput {
        let fail = |text: String| CallOutput { ok: false, text };
        let (s, service) = match self.service(server) {
            Ok(found) => found,
            Err(e) => return fail(e),
        };
        let read = service.read_resource(ReadResourceRequestParams::new(uri));
        match timed(s, read).await {
            Ok(Ok(r)) => CallOutput {
                ok: true,
                text: crate::output::contents(
                    &serde_json::to_value(&r).unwrap_or_default(),
                    s.spec.file.max_output_tokens as usize * 4,
                ),
            },
            Ok(Err(e)) => fail(format!("MCP error: {e}")),
            Err(e) => fail(e),
        }
    }

    async fn relist(&self, idx: usize, peer: Peer<RoleClient>, what: List) {
        let id = &self.servers[idx].spec.id;
        let generation = self.servers[idx].inner.lock().map_or(0, |i| i.generation);
        let result = match what {
            List::Tools => list_tools(&peer, &self.servers[idx].spec)
                .await
                .map(|tools| self.set(idx, generation, |i| i.tools = tools)),
            List::Prompts => list_prompts(&peer, id)
                .await
                .map(|prompts| self.set(idx, generation, |i| i.prompts = prompts)),
            List::Resources => list_resources(&peer, id)
                .await
                .map(|resources| self.set(idx, generation, |i| i.resources = resources)),
        };
        if let Err(e) = result {
            tracing::warn!(server = %id, error = %e, ?what, "re-listing failed");
        }
    }
}

/// Run a request to `s` within its `call_timeout_secs`. Time the user spends on the
/// server's forms and sampling approvals doesn't count: each one gives the server a new
/// full window.
async fn timed<F: Future>(s: &Server, fut: F) -> Result<F::Output, String> {
    let limit = Duration::from_secs(s.spec.file.call_timeout_secs.max(1));
    tokio::pin!(fut);
    let mut seen = s.asked.load(Ordering::SeqCst);
    loop {
        tokio::select! {
            out = &mut fut => return Ok(out),
            () = tokio::time::sleep(limit) => {
                let asked = s.asked.load(Ordering::SeqCst);
                if s.asking.load(Ordering::SeqCst) == 0 && asked == seen {
                    return Err(format!(
                        "no answer within {}s (call_timeout_secs)",
                        limit.as_secs()
                    ));
                }
                seen = asked;
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum List {
    Tools,
    Prompts,
    Resources,
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

async fn list_prompts(peer: &Peer<RoleClient>, server: &str) -> Result<Vec<PromptInfo>, String> {
    if !peer
        .peer_info()
        .is_some_and(|info| info.capabilities.prompts.is_some())
    {
        return Ok(Vec::new());
    }
    let prompts = peer
        .list_all_prompts()
        .await
        .map_err(|e| format!("listing prompts: {e}"))?;
    Ok(prompts
        .into_iter()
        .map(|p| PromptInfo {
            server: server.to_owned(),
            description: p.description.unwrap_or_default(),
            arguments: p
                .arguments
                .unwrap_or_default()
                .into_iter()
                .map(|a| PromptArg {
                    name: a.name,
                    description: a.description.unwrap_or_default(),
                    required: a.required.unwrap_or(false),
                })
                .collect(),
            name: p.name,
        })
        .collect())
}

/// Resources, then resource templates. Read through JSON, so every spec revision's
/// shape works.
async fn list_resources(
    peer: &Peer<RoleClient>,
    server: &str,
) -> Result<Vec<ResourceInfo>, String> {
    if !peer
        .peer_info()
        .is_some_and(|info| info.capabilities.resources.is_some())
    {
        return Ok(Vec::new());
    }
    let resources = peer
        .list_all_resources()
        .await
        .map_err(|e| format!("listing resources: {e}"))?;
    // Templates are optional; a server without them answers "method not found".
    let templates = peer.list_all_resource_templates().await.unwrap_or_default();
    let info = |v: Value, template: bool| {
        let field = |k: &str| {
            v.get(k)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        ResourceInfo {
            server: server.to_owned(),
            uri: field(if template { "uriTemplate" } else { "uri" }),
            name: field("name"),
            description: field("description"),
            mime_type: field("mimeType"),
            template,
        }
    };
    Ok(resources
        .iter()
        .map(|r| info(serde_json::to_value(r).unwrap_or_default(), false))
        .chain(
            templates
                .iter()
                .map(|t| info(serde_json::to_value(t).unwrap_or_default(), true)),
        )
        .filter(|r| !r.uri.is_empty())
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

/// Re-lists tools, prompts and resources when the server says they changed, and passes
/// its forms and sampling requests to the asker. Roots and logging are left out: both are
/// deprecated in 2026-07-28, and stdio servers start in the workspace root.
#[derive(Clone)]
struct Handler {
    manager: Weak<McpManager>,
    idx: usize,
}

impl Handler {
    /// Send `ask` to the asker and wait for the answer, which `None` means never came.
    /// While waiting, the server's requests don't time out.
    async fn ask<T>(
        &self,
        ask: impl FnOnce(oneshot::Sender<T>) -> ServerAsk,
        ct: &CancellationToken,
    ) -> Option<T> {
        let m = self.manager.upgrade()?;
        let asker = m.asker()?;
        let server = &m.servers[self.idx];
        let (tx, rx) = oneshot::channel();
        server.asking.fetch_add(1, Ordering::SeqCst);
        let answer = match asker.send(ask(tx)).await {
            Ok(()) => tokio::select! {
                a = rx => a.ok(),
                () = ct.cancelled() => None,
            },
            Err(_) => None,
        };
        server.asked.fetch_add(1, Ordering::SeqCst);
        server.asking.fetch_sub(1, Ordering::SeqCst);
        answer
    }

    fn id(&self) -> String {
        self.manager
            .upgrade()
            .map(|m| m.servers[self.idx].spec.id.clone())
            .unwrap_or_default()
    }
}

impl ClientHandler for Handler {
    fn get_info(&self) -> ClientConfig {
        let mut caps = ClientCapabilities::default();
        if self.manager.upgrade().and_then(|m| m.asker()).is_some() {
            caps.elicitation =
                Some(ElicitationCapability::new().with_form(FormElicitationCapability::new()));
            caps.sampling = Some(SamplingCapability::default());
        }
        ClientConfig::new(
            caps,
            Implementation::new("quack", env!("CARGO_PKG_VERSION")),
        )
    }

    async fn create_elicitation(
        &self,
        request: ElicitRequestParams,
        context: RequestContext<RoleClient>,
    ) -> Result<ElicitResult, ErrorData> {
        // quack offers forms only, so a URL request shouldn't come; decline it if it does.
        let ElicitRequestParams::FormElicitationParams {
            message,
            requested_schema,
            ..
        } = request
        else {
            return Ok(ElicitResult::new(ElicitationAction::Decline));
        };
        let schema = serde_json::to_value(&requested_schema).unwrap_or_default();
        let form = Form::from_schema(message, &schema);
        let server = self.id();
        let answer = self
            .ask(
                |reply| ServerAsk::Elicit {
                    server,
                    form,
                    reply,
                },
                &context.ct,
            )
            .await;
        Ok(match answer {
            Some(Elicited::Accept(content)) => {
                ElicitResult::new(ElicitationAction::Accept).with_content(Value::Object(content))
            }
            Some(Elicited::Cancel) => ElicitResult::new(ElicitationAction::Cancel),
            Some(Elicited::Decline) | None => ElicitResult::new(ElicitationAction::Decline),
        })
    }

    #[allow(
        deprecated,
        reason = "sampling is deprecated in 2026-07-28 but still served"
    )]
    async fn create_message(
        &self,
        params: rmcp::model::CreateMessageRequestParams,
        context: RequestContext<RoleClient>,
    ) -> Result<rmcp::model::CreateMessageResult, ErrorData> {
        let request =
            SampleRequest::from_params(&serde_json::to_value(&params).unwrap_or_default())
                .map_err(|e| ErrorData::invalid_params(e, None))?;
        let server = self.id();
        let answer = self
            .ask(
                |reply| ServerAsk::Sample {
                    server,
                    request,
                    reply,
                },
                &context.ct,
            )
            .await
            .unwrap_or_else(|| Err("quack can't answer sampling requests right now".into()))
            .map_err(|e| ErrorData::invalid_request(e, None))?;
        let stop = if answer.truncated {
            "maxTokens"
        } else {
            "endTurn"
        };
        serde_json::from_value(serde_json::json!({
            "model": answer.model,
            "stopReason": stop,
            "role": "assistant",
            "content": {"type": "text", "text": answer.text},
        }))
        .map_err(|e| ErrorData::internal_error(e.to_string(), None))
    }

    async fn on_tool_list_changed(&self, context: NotificationContext<RoleClient>) {
        if let Some(m) = self.manager.upgrade() {
            m.relist(self.idx, context.peer.clone(), List::Tools).await;
        }
    }

    async fn on_prompt_list_changed(&self, context: NotificationContext<RoleClient>) {
        if let Some(m) = self.manager.upgrade() {
            m.relist(self.idx, context.peer.clone(), List::Prompts)
                .await;
        }
    }

    async fn on_resource_list_changed(&self, context: NotificationContext<RoleClient>) {
        if let Some(m) = self.manager.upgrade() {
            m.relist(self.idx, context.peer.clone(), List::Resources)
                .await;
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
    fn prompt_arguments() {
        let arg = |name: &str, required: bool| PromptArg {
            name: name.into(),
            description: String::new(),
            required,
        };
        let p = PromptInfo {
            server: "gh".into(),
            name: "review".into(),
            description: String::new(),
            arguments: vec![arg("pr", true), arg("focus", false)],
        };
        assert_eq!(p.usage(), "<pr> [focus]");
        let get = |line: &str| p.parse_args(line).map(|m| Value::Object(m).to_string());
        assert_eq!(get("12").as_deref(), Ok(r#"{"pr":"12"}"#));
        assert_eq!(
            get("12 error handling only").as_deref(),
            Ok(r#"{"focus":"error handling only","pr":"12"}"#)
        );
        assert_eq!(
            get("focus=the tests pr=7").as_deref(),
            Ok(r#"{"focus":"the tests","pr":"7"}"#)
        );
        let err = get("").expect_err("missing pr");
        assert!(
            err.contains("missing pr; usage: /gh:review <pr> [focus]"),
            "{err}"
        );
        let none = PromptInfo {
            arguments: Vec::new(),
            ..p.clone()
        };
        assert!(none.parse_args("").is_ok());
        assert!(none.parse_args("x").is_err());
    }

    #[test]
    fn tool_names() {
        assert_eq!(
            model_tool_name("git hub", "get.issue"),
            "mcp__git_hub__get_issue"
        );
        assert_eq!(model_tool_name("s", &"x".repeat(100)).len(), 64);
    }
}
