//! JSON-RPC 2.0 over newline-delimited JSON, ACP's stdio transport.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

pub(crate) const PARSE_ERROR: i64 = -32700;
pub(crate) const METHOD_NOT_FOUND: i64 = -32601;
pub(crate) const INVALID_PARAMS: i64 = -32602;
pub(crate) const INTERNAL_ERROR: i64 = -32603;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RpcError {
    pub code: i64,
    pub message: String,
}

impl RpcError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(INVALID_PARAMS, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(INTERNAL_ERROR, message)
    }
}

type Pending = Mutex<HashMap<i64, oneshot::Sender<Result<Value, RpcError>>>>;

/// The client end of the connection. Every line goes through one writer task, so
/// messages from different sessions never interleave.
#[derive(Debug, Clone)]
pub(crate) struct Peer {
    out: mpsc::UnboundedSender<String>,
    pending: Arc<Pending>,
    next_id: Arc<AtomicI64>,
}

impl Peer {
    pub fn new(out: mpsc::UnboundedSender<String>) -> Self {
        Self {
            out,
            pending: Arc::default(),
            next_id: Arc::new(AtomicI64::new(1)),
        }
    }

    fn send(&self, msg: &Value) {
        // A closed writer means the client went away; the read loop ends on its own.
        let _ = self.out.send(msg.to_string());
    }

    pub fn notify(&self, method: &str, params: impl Serialize) {
        let params = serde_json::to_value(params).unwrap_or(Value::Null);
        self.send(&json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    pub fn respond(&self, id: Value, result: Result<Value, RpcError>) {
        let msg = match result {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err(e) => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": e.code, "message": e.message},
            }),
        };
        self.send(&msg);
    }

    /// Send a request to the client and wait for its answer.
    pub async fn request(&self, method: &str, params: impl Serialize) -> Result<Value, RpcError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        if let Ok(mut pending) = self.pending.lock() {
            pending.insert(id, tx);
        }
        let params = serde_json::to_value(params).unwrap_or(Value::Null);
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        rx.await
            .unwrap_or_else(|_| Err(RpcError::internal("the connection closed")))
    }

    /// A response from the client to one of [`request`](Self::request)'s messages.
    pub fn resolve(&self, msg: &Value) {
        let Some(id) = msg.get("id").and_then(Value::as_i64) else {
            return;
        };
        let Some(tx) = self.pending.lock().ok().and_then(|mut p| p.remove(&id)) else {
            return;
        };
        let result = match msg.get("error") {
            Some(e) => Err(RpcError::new(
                e.get("code")
                    .and_then(Value::as_i64)
                    .unwrap_or(INTERNAL_ERROR),
                e.get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("the client returned an error"),
            )),
            None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
        };
        let _ = tx.send(result);
    }

    /// Fail every request still waiting for the client, e.g. when it disconnects.
    pub fn close(&self) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.clear();
        }
    }
}
