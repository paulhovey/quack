//! A provider ready to use: resolved secrets, one HTTP client, retries and streaming.

use std::time::Duration;

use futures::StreamExt;
use quack_config::provider::ReasoningFormat;
use quack_config::settings::NetworkSettings;
use quack_config::{Env, ProviderFile, expand_home};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::error::{ErrorKind, ProviderError};
#[cfg(feature = "proto-ollama")]
use crate::ollama;
use crate::openai_chat::{self, ChunkParser};
use crate::sse::SseDecoder;
use crate::types::{ChatRequest, DiscoveredModel, StreamEvent};

/// Longest wait we honour from a `Retry-After` header.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(60);

/// Wire protocol of a provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Api {
    OpenAiChat,
    #[cfg(feature = "proto-ollama")]
    Ollama,
}

#[derive(Debug, Clone)]
pub struct Provider {
    pub id: String,
    pub file: ProviderFile,
    api: Api,
    client: reqwest::Client,
    base_url: String,
    headers: HeaderMap,
    dialect: openai_chat::Dialect,
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
        let api = match file.api.as_str() {
            "openai-chat" => Api::OpenAiChat,
            #[cfg(feature = "proto-ollama")]
            "ollama" => Api::Ollama,
            #[cfg(not(feature = "proto-ollama"))]
            "ollama" => {
                return Err(config(
                    "this build has no `ollama` adapter; use `openai-chat` with `/v1`".into(),
                ));
            }
            other @ ("openai-responses" | "anthropic-messages") => {
                return Err(config(format!(
                    "api `{other}` is not implemented yet; use `openai-chat`"
                )));
            }
            other => return Err(config(format!("unknown api `{other}`"))),
        };

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
        let base_url = match api {
            Api::OpenAiChat => file.base_url.trim_end_matches('/'),
            #[cfg(feature = "proto-ollama")]
            Api::Ollama => ollama::native_base(&file.base_url),
        }
        .to_owned();
        Ok(Self {
            id: id.to_owned(),
            api,
            dialect: openai_chat::Dialect::of(&base_url),
            base_url,
            file,
            client,
            headers,
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}/{path}", self.base_url)
    }

    fn discovery_timeout(&self) -> Duration {
        Duration::from_secs(self.file.http.timeout_connect_secs.max(1) * 3)
    }

    async fn get_json(&self, path: &str) -> Result<serde_json::Value, ProviderError> {
        self.get_json_at(self.url(path)).await
    }

    async fn get_json_at(&self, url: String) -> Result<serde_json::Value, ProviderError> {
        let resp = self
            .client
            .get(url)
            .headers(self.headers.clone())
            .timeout(self.discovery_timeout())
            .send()
            .await
            .map_err(|e| ProviderError::from_reqwest(&e))?;
        let resp = check_status(resp).await?;
        resp.json()
            .await
            .map_err(|e| ProviderError::from_reqwest(&e))
    }

    /// Query the models endpoint.
    pub async fn list_models(&self) -> Result<Vec<DiscoveredModel>, ProviderError> {
        match self.api {
            Api::OpenAiChat => {
                let body = self.get_json(openai_chat::MODELS_PATH).await?;
                let mut models = openai_chat::parse_models(&body);
                // llama-server serves one model with the window it was started with.
                if let [only] = models.as_mut_slice()
                    && openai_chat::is_llama_cpp(&body)
                {
                    let root = self.base_url.strip_suffix("/v1").unwrap_or(&self.base_url);
                    let props = format!("{root}/{}", openai_chat::LLAMA_CPP_PROPS_PATH);
                    if let Ok(p) = self.get_json_at(props).await
                        && let Some(n) = openai_chat::llama_cpp_window(&p)
                    {
                        only.context_window = Some(n);
                    }
                }
                Ok(models)
            }
            #[cfg(feature = "proto-ollama")]
            Api::Ollama => {
                let names = ollama::parse_tags(&self.get_json(ollama::TAGS_PATH).await?);
                // A local server: asking each model for its window is cheap.
                let shown =
                    futures::future::join_all(names.iter().map(|n| self.ollama_show(n))).await;
                Ok(names
                    .into_iter()
                    .zip(shown)
                    .map(|(name, trained)| ollama::discovered(name, trained))
                    .collect())
            }
        }
    }

    #[cfg(feature = "proto-ollama")]
    async fn ollama_show(&self, model: &str) -> Option<u32> {
        let resp = self
            .client
            .post(self.url(ollama::SHOW_PATH))
            .headers(self.headers.clone())
            .timeout(self.discovery_timeout())
            .json(&serde_json::json!({ "model": model }))
            .send()
            .await
            .ok()?;
        let body: serde_json::Value = check_status(resp).await.ok()?.json().await.ok()?;
        ollama::parse_show(&body)
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
        let (path, body, accept) = match self.api {
            Api::OpenAiChat => (
                openai_chat::CHAT_PATH,
                openai_chat::build_body(&req, self.dialect),
                "text/event-stream",
            ),
            #[cfg(feature = "proto-ollama")]
            Api::Ollama => (
                ollama::CHAT_PATH,
                ollama::build_body(&req),
                "application/x-ndjson",
            ),
        };
        let mut attempt = 0;
        let resp = loop {
            let sent = self
                .client
                .post(self.url(path))
                .headers(self.headers.clone())
                .header(reqwest::header::ACCEPT, accept)
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
        let mut wire = match self.api {
            Api::OpenAiChat => Wire::Sse(SseDecoder::default(), ChunkParser::new(reasoning)),
            #[cfg(feature = "proto-ollama")]
            Api::Ollama => Wire::Lines(
                ollama::LineDecoder::default(),
                ollama::LineParser::new(reasoning),
            ),
        };
        let mut bytes = resp.bytes_stream();
        while let Some(chunk) = bytes.next().await {
            let chunk = match chunk {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.send(Err(ProviderError::from_reqwest(&e))).await;
                    return;
                }
            };
            for parsed in wire.feed(&chunk) {
                if !forward(parsed, tx).await {
                    return;
                }
            }
        }
        for parsed in wire.finish() {
            if !forward(parsed, tx).await {
                return;
            }
        }
    }
}

/// Framing plus parsing for each wire format.
enum Wire {
    Sse(SseDecoder, ChunkParser),
    #[cfg(feature = "proto-ollama")]
    Lines(ollama::LineDecoder, ollama::LineParser),
}

type Parsed = Result<Vec<StreamEvent>, ProviderError>;

impl Wire {
    fn feed(&mut self, bytes: &[u8]) -> Vec<Parsed> {
        match self {
            Self::Sse(d, p) => d.feed(bytes).iter().map(|x| p.parse(x)).collect(),
            #[cfg(feature = "proto-ollama")]
            Self::Lines(d, p) => d.feed(bytes).iter().map(|x| p.parse(x)).collect(),
        }
    }

    /// End of stream: trailing frames, then text the parser still holds.
    fn finish(&mut self) -> Vec<Parsed> {
        let mut out: Vec<Parsed> = match self {
            Self::Sse(d, p) => d.finish().iter().map(|x| p.parse(x)).collect(),
            #[cfg(feature = "proto-ollama")]
            Self::Lines(d, p) => d.finish().iter().map(|x| p.parse(x)).collect(),
        };
        out.push(Ok(match self {
            Self::Sse(_, p) => p.finish(),
            #[cfg(feature = "proto-ollama")]
            Self::Lines(_, p) => p.finish(),
        }));
        out
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

/// An HTTP client with the user's proxy and CA settings, for other HTTP services such as
/// remote MCP servers.
pub fn http_client(network: &NetworkSettings, env: Env<'_>) -> Result<reqwest::Client, String> {
    let file: ProviderFile =
        serde_json::from_value(serde_json::json!({ "base_url": "" })).map_err(|e| e.to_string())?;
    build_client(&file, network, env).map_err(|e| e.message)
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
        .user_agent(concat!("quack/", env!("CARGO_PKG_VERSION")))
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
