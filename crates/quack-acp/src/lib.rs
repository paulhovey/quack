//! Agent Client Protocol agent: `quack acp` (docs/plan/12-headless-and-acp.md#acp-agent-client-protocol).
//!
//! An editor starts `quack acp` and speaks JSON-RPC over its stdin and stdout. Each ACP
//! session is one engine, started in the session's folder with the editor's MCP servers
//! added to `mcp.d`. The editor draws the conversation and answers approvals; questions
//! from `ask_user` are skipped, as in `quack exec`. Nothing but protocol goes to stdout.

mod convert;
mod rpc;
mod session;
mod wire;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use quack_config::McpServerFile;
use quack_core::{EngineHandle, Resume};
use quack_protocol::Event;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};

use rpc::{Peer, RpcError};
use session::{Cmd, Startup};
use wire::McpServer;

/// What an editor asked a new session to start with.
#[derive(Debug)]
pub struct SessionStart {
    /// The session's folder, an absolute path.
    pub cwd: PathBuf,
    pub resume: Resume,
    /// The editor's MCP servers, by id, for `LoadOptions::mcp`.
    pub mcp: Vec<(String, McpServerFile)>,
}

/// Loads the config for a session's folder and starts its engine; `Err` says why not.
pub type Spawn = Arc<dyn Fn(SessionStart) -> Result<EngineHandle, String> + Send + Sync>;

/// Serve one editor connection until it closes its end.
pub async fn serve<R, W>(input: R, output: W, spawn: Spawn) -> std::io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
    let writer = tokio::spawn(async move {
        let mut output = output;
        while let Some(line) = out_rx.recv().await {
            output.write_all(line.as_bytes()).await?;
            output.write_all(b"\n").await?;
            output.flush().await?;
        }
        Ok::<_, std::io::Error>(())
    });
    let mut server = Server {
        peer: Peer::new(out_tx),
        spawn,
        sessions: HashMap::new(),
    };
    let mut lines = BufReader::new(input).lines();
    let read = loop {
        match lines.next_line().await {
            Ok(Some(line)) if line.trim().is_empty() => {}
            Ok(Some(line)) => match serde_json::from_str::<Value>(&line) {
                Ok(msg) => server.handle(msg).await,
                Err(e) => server.peer.respond(
                    Value::Null,
                    Err(RpcError::new(rpc::PARSE_ERROR, format!("not JSON: {e}"))),
                ),
            },
            Ok(None) => break Ok(()),
            Err(e) => break Err(e),
        }
    };
    server.peer.close();
    for (_, s) in server.sessions.drain() {
        s.close().await;
    }
    drop(server);
    let written = writer.await.unwrap_or(Ok(()));
    read.and(written)
}

struct Server {
    peer: Peer,
    spawn: Spawn,
    sessions: HashMap<String, session::Handle>,
}

/// The answer to a request: now, or later from the session (`session/prompt`).
enum Reply {
    Now(Result<Value, RpcError>),
    Later,
}

fn params<T: DeserializeOwned>(p: Value) -> Result<T, RpcError> {
    serde_json::from_value(p).map_err(|e| RpcError::invalid(e.to_string()))
}

fn empty() -> Result<Value, RpcError> {
    Ok(json!({}))
}

impl Server {
    async fn handle(&mut self, msg: Value) {
        let Some(method) = msg.get("method").and_then(Value::as_str).map(str::to_owned) else {
            // A response to one of our requests.
            self.peer.resolve(&msg);
            return;
        };
        let id = msg.get("id").cloned();
        let p = msg.get("params").cloned().unwrap_or(Value::Null);
        let reply = self.dispatch(&method, p, id.clone()).await;
        if let (Some(id), Reply::Now(result)) = (id, reply) {
            self.peer.respond(id, result);
        }
    }

    async fn dispatch(&mut self, method: &str, p: Value, id: Option<Value>) -> Reply {
        let now = |r: Result<Value, RpcError>| Reply::Now(r);
        match method {
            "initialize" => now(params::<wire::Initialize>(p).map(|r| initialize(&r))),
            "authenticate" => now(empty()),
            "session/new" => now(match params::<wire::NewSession>(p) {
                Ok(r) => self.new_session(r).await,
                Err(e) => Err(e),
            }),
            "session/load" => now(match params::<wire::LoadSession>(p) {
                Ok(r) => self.load_session(r).await,
                Err(e) => Err(e),
            }),
            "session/prompt" => {
                let r = match params::<wire::Prompt>(p) {
                    Ok(r) => r,
                    Err(e) => return now(Err(e)),
                };
                let Some(id) = id else {
                    return now(Err(RpcError::invalid("session/prompt needs an id")));
                };
                let cmd = Cmd::Prompt {
                    id,
                    items: convert::prompt_items(r.prompt),
                };
                match self.send(&r.session_id, cmd) {
                    Ok(()) => Reply::Later,
                    Err(e) => now(Err(e)),
                }
            }
            "session/cancel" => {
                if let Ok(r) = params::<wire::SessionRef>(p) {
                    let _ = self.send(&r.session_id, Cmd::Cancel);
                }
                now(Ok(Value::Null))
            }
            "session/set_mode" => now(params::<wire::SetMode>(p).and_then(|r| {
                self.send(&r.session_id, Cmd::Mode(r.mode_id))?;
                empty()
            })),
            "session/set_config_option" => {
                let r = match params::<wire::SetConfigOption>(p) {
                    Ok(r) => r,
                    Err(e) => return now(Err(e)),
                };
                if r.config_id != session::MODEL_OPTION {
                    return now(Err(RpcError::invalid(format!(
                        "unknown option `{}`",
                        r.config_id
                    ))));
                }
                let Some(value) = r.value.as_str() else {
                    return now(Err(RpcError::invalid("the model option takes a model id")));
                };
                let (tx, rx) = oneshot::channel();
                let cmd = Cmd::Model {
                    value: value.to_owned(),
                    reply: tx,
                };
                if let Err(e) = self.send(&r.session_id, cmd) {
                    return now(Err(e));
                }
                now(match rx.await {
                    Ok(Ok(options)) => Ok(json!({"configOptions": options})),
                    Ok(Err(e)) => Err(e),
                    Err(_) => Err(RpcError::internal("the session closed")),
                })
            }
            "session/close" => now(match params::<wire::SessionRef>(p) {
                Ok(r) => match self.sessions.remove(&r.session_id) {
                    Some(s) => {
                        s.close().await;
                        empty()
                    }
                    None => Err(unknown_session(&r.session_id)),
                },
                Err(e) => Err(e),
            }),
            other => now(Err(RpcError::new(
                rpc::METHOD_NOT_FOUND,
                format!("quack does not handle `{other}`"),
            ))),
        }
    }

    fn send(&self, session: &str, cmd: Cmd) -> Result<(), RpcError> {
        let s = self
            .sessions
            .get(session)
            .ok_or_else(|| unknown_session(session))?;
        s.cmds
            .send(cmd)
            .map_err(|_| RpcError::internal("the session has stopped"))
    }

    fn spawn(
        &self,
        cwd: PathBuf,
        resume: Resume,
        servers: Vec<McpServer>,
    ) -> Result<EngineHandle, RpcError> {
        if !cwd.is_absolute() {
            return Err(RpcError::invalid(format!(
                "cwd must be an absolute path, got {}",
                cwd.display()
            )));
        }
        let mut mcp = Vec::new();
        for server in servers {
            match convert::mcp_server(server) {
                Ok(s) => mcp.push(s),
                Err(e) => tracing::warn!(%e, "skipping an editor MCP server"),
            }
        }
        (self.spawn)(SessionStart { cwd, resume, mcp }).map_err(RpcError::internal)
    }

    async fn new_session(&mut self, r: wire::NewSession) -> Result<Value, RpcError> {
        let mut engine = self.spawn(r.cwd, Resume::New, r.mcp_servers)?;
        let mode = engine.config().settings.permissions.mode.label().to_owned();
        let mut startup = Startup::default();
        let id = session::wait_for(&mut engine, &mut startup, |e| match e {
            Event::SessionConfigured { session } => Some(session.id.0.clone()),
            _ => None,
        })
        .await
        .map_err(RpcError::internal)?;
        let handle = session::start(id.clone(), engine, self.peer.clone(), startup);
        self.sessions.insert(id.clone(), handle);
        Ok(json!({"sessionId": id, "modes": session::modes(&mode)}))
    }

    async fn load_session(&mut self, r: wire::LoadSession) -> Result<Value, RpcError> {
        let id = r.session_id;
        if let Some(old) = self.sessions.remove(&id) {
            old.close().await;
        }
        let mut engine = self.spawn(r.cwd, Resume::Id(id.clone()), r.mcp_servers)?;
        let mode = engine.config().settings.permissions.mode.label().to_owned();
        let mut startup = Startup::default();
        let loaded = session::wait_for(&mut engine, &mut startup, |e| match e {
            Event::SessionResumed { items, .. } => Some(Ok(items.clone())),
            Event::Warning { message } if message.starts_with("cannot re") => {
                Some(Err(message.clone()))
            }
            _ => None,
        })
        .await;
        let items = match loaded {
            Ok(Ok(items)) => items,
            Ok(Err(e)) | Err(e) => {
                engine.shutdown().await;
                return Err(RpcError::internal(e));
            }
        };
        for update in convert::replay(items) {
            self.peer
                .notify("session/update", json!({"sessionId": id, "update": update}));
        }
        let handle = session::start(id.clone(), engine, self.peer.clone(), startup);
        self.sessions.insert(id, handle);
        Ok(json!({"modes": session::modes(&mode)}))
    }
}

fn unknown_session(id: &str) -> RpcError {
    RpcError::invalid(format!("no session `{id}` · start one with session/new"))
}

/// The `initialize` result: what quack takes and does.
fn initialize(r: &wire::Initialize) -> Value {
    // The agent answers with the version it speaks; a client that can't use it hangs up.
    if r.protocol_version != wire::PROTOCOL_VERSION {
        tracing::info!(
            asked = r.protocol_version,
            "acp client asked for another version"
        );
    }
    json!({
        "protocolVersion": wire::PROTOCOL_VERSION,
        "agentCapabilities": {
            "loadSession": true,
            "promptCapabilities": {"image": false, "audio": false, "embeddedContext": true},
            "mcpCapabilities": {"http": true, "sse": false},
            "sessionCapabilities": {"close": {}},
        },
        "authMethods": [],
        "agentInfo": {"name": "quack", "title": "quack", "version": env!("CARGO_PKG_VERSION")},
    })
}
