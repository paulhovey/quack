//! A provider ready to use: resolved secrets, one HTTP client, retries and streaming.

use std::time::Duration;

use cutoffharness_config::provider::ReasoningFormat;
use cutoffharness_config::settings::NetworkSettings;
use cutoffharness_config::{Env, ProviderFile, expand_home};
use futures::StreamExt;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::error::{ErrorKind, ProviderError};
use crate::openai_chat::{self, ChunkParser};
use crate::sse::SseDecoder;
use crate::types::{ChatRequest, DiscoveredModel, StreamEvent};

/// Longest wait we honour from a `Retry-After` header.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(60);

#[derive(Debug, Clone)]
pub struct Provider {
    pub id: String,
    pub file: ProviderFile,
    client: reqwest::Client,
    base_url: String,
    headers: HeaderMap,
}

impl Provider {
    /// Build the client and resolve secrets. May run `cmd` secrets, so call it from a blocking context.
    pub fn new(
        id: &str,
        file: ProviderFile,
        network: &NetworkSettings,
        env: Env<'_>,
    ) -> Result<Self, ProviderError> {
        let config = |msg: String| ProviderError::new(ErrorKind::Config, msg);
        match file.api.as_str() {
            "openai-chat" => {}
            other @ ("openai-responses" | "anthropic-messages" | "ollama") => {
                return Err(config(format!(
                    "api `{other}` is not implemented yet; use `openai-chat`"
                )));
            }
            other => return Err(config(format!("unknown api `{other}`"))),
        }

        let mut headers = HeaderMap::new();
        if let Some(key) = &file.api_key {
            let key = key
                .resolve(env)
                .map_err(|e| config(format!("api_key: {e}")))?;
            let name = file.auth_header.as_deref().unwrap_or("Authorization");
            let value = match file.auth_scheme.as_deref() {
                None => format!("Bearer {key}"),
                Some("") => key,
                Some(scheme) => format!("{scheme} {key}"),
            };
            insert_header(&mut headers, name, &value, true)?;
        }
        for (name, secret) in &file.headers {
            let value = secret
                .resolve(env)
                .map_err(|e| config(format!("header `{name}`: {e}")))?;
            insert_header(&mut headers, name, &value, true)?;
        }

        let client = build_client(&file, network, env)?;
        Ok(Self {
            id: id.to_owned(),
            base_url: file.base_url.trim_end_matches('/').to_owned(),
            file,
            client,
            headers,
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}/{path}", self.base_url)
    }

    /// Query the models endpoint.
    pub async fn list_models(&self) -> Result<Vec<DiscoveredModel>, ProviderError> {
        let resp = self
            .client
            .get(self.url(openai_chat::MODELS_PATH))
            .headers(self.headers.clone())
            .timeout(Duration::from_secs(
                self.file.http.timeout_connect_secs.max(1) * 3,
            ))
            .send()
            .await
            .map_err(|e| ProviderError::from_reqwest(&e))?;
        let resp = check_status(resp).await?;
        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| ProviderError::from_reqwest(&e))?;
        Ok(openai_chat::parse_models(&body))
    }

    /// Start a streaming request. Events arrive on the returned channel; it closes after
    /// `Finish` (plus a trailing `Usage`), an error, or cancellation.
    pub fn stream(
        &self,
        req: ChatRequest,
        reasoning: ReasoningFormat,
        cancel: CancellationToken,
    ) -> mpsc::Receiver<Result<StreamEvent, ProviderError>> {
        let (tx, rx) = mpsc::channel(256);
        let this = self.clone();
        tokio::spawn(async move {
            tokio::select! {
                () = cancel.cancelled() => {}
                () = this.run_stream(req, reasoning, &tx) => {}
            }
        });
        rx
    }

    async fn run_stream(
        &self,
        req: ChatRequest,
        reasoning: ReasoningFormat,
        tx: &mpsc::Sender<Result<StreamEvent, ProviderError>>,
    ) {
        let body = openai_chat::build_body(&req);
        let mut attempt = 0;
        let resp = loop {
            let sent = self
                .client
                .post(self.url(openai_chat::CHAT_PATH))
                .headers(self.headers.clone())
                .header(reqwest::header::ACCEPT, "text/event-stream")
                .json(&body)
                .send()
                .await;
            let result = match sent {
                Ok(r) => check_status(r).await,
                Err(e) => Err(ProviderError::from_reqwest(&e)),
            };
            match result {
                Ok(r) => break r,
                Err(e) if e.retryable() && attempt < self.file.http.max_retries => {
                    let wait = backoff(attempt, &e);
                    tracing::warn!(provider = %self.id, attempt, ?wait, error = %e, "retrying");
                    attempt += 1;
                    tokio::time::sleep(wait).await;
                }
                Err(e) => {
                    let _ = tx.send(Err(e)).await;
                    return;
                }
            }
        };

        // Once bytes flow we never retry: the user has already seen part of the answer.
        let mut sse = SseDecoder::default();
        let mut parser = ChunkParser::new(reasoning);
        let mut bytes = resp.bytes_stream();
        while let Some(chunk) = bytes.next().await {
            let chunk = match chunk {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.send(Err(ProviderError::from_reqwest(&e))).await;
                    return;
                }
            };
            for data in sse.feed(&chunk) {
                if !forward(parser.parse(&data), tx).await {
                    return;
                }
            }
        }
        for data in sse.finish() {
            if !forward(parser.parse(&data), tx).await {
                return;
            }
        }
        for ev in parser.finish() {
            let _ = tx.send(Ok(ev)).await;
        }
    }
}

/// Send parsed events; false when the stream should stop.
async fn forward(
    parsed: Result<Vec<StreamEvent>, ProviderError>,
    tx: &mpsc::Sender<Result<StreamEvent, ProviderError>>,
) -> bool {
    match parsed {
        Ok(events) => {
            for ev in events {
                if tx.send(Ok(ev)).await.is_err() {
                    return false;
                }
            }
            true
        }
        Err(e) => {
            let _ = tx.send(Err(e)).await;
            false
        }
    }
}

async fn check_status(resp: reqwest::Response) -> Result<reqwest::Response, ProviderError> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let retry_after = resp
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_secs);
    let body = resp.text().await.unwrap_or_default();
    Err(ProviderError::from_status(
        status.as_u16(),
        retry_after,
        &body,
    ))
}

/// Exponential backoff with jitter: about 0.5s, 1s, 2s, … capped at 30s.
fn backoff(attempt: u32, e: &ProviderError) -> Duration {
    if let ErrorKind::RateLimited {
        retry_after: Some(d),
    } = e.kind
    {
        return d.min(MAX_RETRY_AFTER);
    }
    let base = 500u64.saturating_mul(1 << attempt.min(6)).min(30_000);
    let jitter = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::from(d.subsec_millis()))
        .unwrap_or(0)
        % (base / 4 + 1);
    Duration::from_millis(base + jitter)
}

fn insert_header(
    headers: &mut HeaderMap,
    name: &str,
    value: &str,
    sensitive: bool,
) -> Result<(), ProviderError> {
    let config = |msg: String| ProviderError::new(ErrorKind::Config, msg);
    let name = HeaderName::from_bytes(name.as_bytes())
        .map_err(|_| config(format!("invalid header name `{name}`")))?;
    let mut value = HeaderValue::from_str(value).map_err(|_| {
        config(format!(
            "header `{name}` has characters not allowed in HTTP"
        ))
    })?;
    value.set_sensitive(sensitive);
    headers.insert(name, value);
    Ok(())
}

fn build_client(
    file: &ProviderFile,
    network: &NetworkSettings,
    env: Env<'_>,
) -> Result<reqwest::Client, ProviderError> {
    let config = |msg: String| ProviderError::new(ErrorKind::Config, msg);
    let read = |label: &str, path: &str| {
        std::fs::read(expand_home(path, env)).map_err(|e| config(format!("{label} `{path}`: {e}")))
    };
    let http = &file.http;
    let mut b = reqwest::Client::builder()
        .user_agent(concat!("cutoffharness/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(http.timeout_connect_secs.max(1)))
        .read_timeout(Duration::from_secs(http.timeout_idle_secs.max(1)));

    let proxy = Some(http.proxy.as_str())
        .filter(|p| !p.is_empty())
        .or(Some(network.proxy.as_str()).filter(|p| !p.is_empty()));
    if let Some(p) = proxy {
        b = b.proxy(reqwest::Proxy::all(p).map_err(|e| config(format!("proxy `{p}`: {e}")))?);
    }

    let mut roots = Vec::new();
    for ca in [
        network.ca_bundle.as_str(),
        file.tls.ca_file.as_deref().unwrap_or(""),
    ] {
        if !ca.is_empty() {
            let pem = read("CA file", ca)?;
            roots.extend(
                reqwest::Certificate::from_pem_bundle(&pem)
                    .map_err(|e| config(format!("CA file `{ca}`: {e}")))?,
            );
        }
    }
    if !roots.is_empty() {
        b = b.tls_certs_merge(roots);
    }
    match (&file.tls.client_cert, &file.tls.client_key) {
        (Some(cert), Some(key)) => {
            let mut pem = read("client_cert", cert)?;
            pem.push(b'\n');
            pem.extend(read("client_key", key)?);
            let id = reqwest::Identity::from_pem(&pem)
                .map_err(|e| config(format!("client certificate: {e}")))?;
            b = b.identity(id);
        }
        (None, None) => {}
        _ => {
            return Err(config(
                "tls.client_cert and tls.client_key must be set together".into(),
            ));
        }
    }
    if file.tls.insecure_skip_verify {
        tracing::warn!(provider = ?file.id, "TLS certificate verification is disabled");
        b = b.tls_danger_accept_invalid_certs(true);
    }
    b.build().map_err(|e| config(format!("HTTP client: {e}")))
}
