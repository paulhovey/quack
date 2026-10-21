//! A provider ready to use: resolved secrets, one HTTP client, retries and streaming.

use std::time::Duration;

use futures::StreamExt;
use quack_config::provider::ReasoningFormat;
use quack_config::settings::NetworkSettings;
use quack_config::{Env, ProviderFile, expand_home};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[cfg(feature = "proto-anthropic")]
use crate::anthropic;
use crate::error::{ErrorKind, ProviderError};
#[cfg(feature = "proto-ollama")]
use crate::ollama;
use crate::openai_chat::{self, ChunkParser};
#[cfg(feature = "proto-openai-responses")]
use crate::responses;
use crate::sse::SseDecoder;
use crate::types::{ChatRequest, DiscoveredModel, Message, StreamEvent, fingerprint, merge};

/// Longest wait we honour from a `Retry-After` header.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(60);

/// Whether this build has the adapter for `api` (`openai-chat`, `anthropic-messages`, ...).
pub fn supports(api: &str) -> bool {
    api == "openai-chat"
        || (cfg!(feature = "proto-ollama") && api == "ollama")
        || (cfg!(feature = "proto-anthropic") && api == "anthropic-messages")
        || (cfg!(feature = "proto-openai-responses") && api == "openai-responses")
}

/// Wire protocol of a provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Api {
    OpenAiChat,
    #[cfg(feature = "proto-ollama")]
    Ollama,
    #[cfg(feature = "proto-anthropic")]
    Anthropic,
    #[cfg(feature = "proto-openai-responses")]
    Responses,
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
    /// The file's `extra_body`, under each model's.
    extra: serde_json::Map<String, serde_json::Value>,
    #[cfg(feature = "proto-openai-responses")]
    route: responses::Route,
    #[cfg(feature = "auth-chatgpt")]
    login: Option<std::sync::Arc<crate::login::Login>>,
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
            #[cfg(feature = "proto-anthropic")]
            "anthropic-messages" => Api::Anthropic,
            #[cfg(feature = "proto-openai-responses")]
            "openai-responses" => Api::Responses,
            #[cfg(not(feature = "proto-anthropic"))]
            "anthropic-messages" => {
                return Err(config(
                    "this build has no `anthropic-messages` adapter; use `openai-chat` if the \
                     service offers it"
                        .into(),
                ));
            }
            #[cfg(not(feature = "proto-openai-responses"))]
            "openai-responses" => {
                return Err(config(
                    "this build has no `openai-responses` adapter; use `openai-chat` if the \
                     service offers it"
                        .into(),
                ));
            }
            other => return Err(config(format!("unknown api `{other}`"))),
        };

        let mut headers = HeaderMap::new();
        if let Some(key) = file.api_key.as_ref().filter(|_| file.auth.is_none()) {
            let key = key
                .resolve(env)
                .map_err(|e| config(format!("api_key: {e}")))?;
            // Anthropic's API takes the bare key in `x-api-key`.
            let anthropic = file.api == "anthropic-messages";
            let name = file.auth_header.as_deref().unwrap_or(if anthropic {
                "x-api-key"
            } else {
                "Authorization"
            });
            let value = match file.auth_scheme.as_deref() {
                None if anthropic && file.auth_header.is_none() => key,
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

        #[cfg(feature = "proto-anthropic")]
        if api == Api::Anthropic && !headers.contains_key("anthropic-version") {
            insert_header(&mut headers, "anthropic-version", anthropic::VERSION, false)?;
        }

        let client = build_client(&file, network, env)?;
        #[cfg(feature = "auth-chatgpt")]
        let mut login = None;
        if let Some(a) = &file.auth {
            match a.kind {
                #[cfg(feature = "auth-chatgpt")]
                quack_config::provider::AuthKind::Chatgpt => {
                    if api != Api::Responses {
                        return Err(config(
                            "a ChatGPT sign-in needs `api = \"openai-responses\"`".into(),
                        ));
                    }
                    login = Some(std::sync::Arc::new(crate::login::Login::new(
                        id,
                        &a.issuer,
                        &file.base_url,
                        &quack_config::user_data_dir(env),
                        client.clone(),
                    )));
                }
                #[cfg(not(feature = "auth-chatgpt"))]
                quack_config::provider::AuthKind::Chatgpt => {
                    return Err(config(
                        "this build has no ChatGPT sign-in; use an API key provider".into(),
                    ));
                }
            }
        }
        // Only a ChatGPT sign-in gets this far with `[auth]`.
        #[cfg(feature = "proto-openai-responses")]
        let route = if file.auth.is_some() {
            responses::Route::ChatgptPlan
        } else {
            responses::Route::Api
        };
        let extra = serde_json::to_value(&file.extra_body)
            .ok()
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        let base_url = match api {
            #[cfg(feature = "proto-ollama")]
            Api::Ollama => ollama::native_base(&file.base_url),
            _ => file.base_url.trim_end_matches('/'),
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
            extra,
            #[cfg(feature = "proto-openai-responses")]
            route,
            #[cfg(feature = "auth-chatgpt")]
            login,
        })
    }

    /// The sign-in behind this provider, for `quack login` and `quack logout`.
    #[cfg(feature = "auth-chatgpt")]
    pub fn login(&self) -> Option<&crate::login::Login> {
        self.login.as_deref()
    }

    /// Why this provider can't be used yet, without contacting it: a sign-in not done.
    pub fn not_ready(&self) -> Option<String> {
        #[cfg(feature = "auth-chatgpt")]
        if let Some(login) = &self.login
            && !login.load().is_some_and(|c| c.signed_in())
        {
            return Some(format!(
                "run `quack login {}` to use this provider",
                self.id
            ));
        }
        None
    }

    /// The headers for one request, with a current token for a signed-in provider.
    async fn request_headers(&self) -> Result<HeaderMap, ProviderError> {
        #[cfg(feature = "auth-chatgpt")]
        if let Some(login) = &self.login {
            let mut h = self.headers.clone();
            let token = login.bearer().await?;
            insert_header(&mut h, "Authorization", &format!("Bearer {token}"), true)?;
            return Ok(h);
        }
        Ok(self.headers.clone())
    }

    /// Whether the history should only ever grow: per-request text (the plan reminder) is
    /// then kept in it rather than sent once. Anthropic's signed thinking only verifies
    /// against the exact history it was made after.
    pub fn append_only(&self) -> bool {
        #[cfg(feature = "proto-anthropic")]
        if self.api == Api::Anthropic {
            return true;
        }
        false
    }

    /// Keep a sealed reply only for the provider, system prompt and tools that made it.
    /// OpenAI's encrypted reasoning is also tied to its model; Anthropic's API itself
    /// drops thinking the current model can't read.
    fn keeps(&self, s: &crate::Sealed, model: &str, fp: &str) -> bool {
        let model_ok = s.model == model || {
            #[cfg(feature = "proto-anthropic")]
            {
                self.api == Api::Anthropic
            }
            #[cfg(not(feature = "proto-anthropic"))]
            false
        };
        s.provider == self.id && s.fingerprint == fp && model_ok
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
            .headers(self.request_headers().await?)
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
            #[cfg(feature = "proto-openai-responses")]
            Api::Responses => Ok(openai_chat::parse_models(
                &self.get_json(openai_chat::MODELS_PATH).await?,
            )),
            #[cfg(feature = "proto-anthropic")]
            Api::Anthropic => Ok(openai_chat::parse_models(
                &self
                    .get_json(&anthropic::path(&self.base_url, "models"))
                    .await?,
            )),
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
            .headers(self.request_headers().await.ok()?)
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
        mut req: ChatRequest,
        reasoning: ReasoningFormat,
        tx: &mpsc::Sender<Result<StreamEvent, ProviderError>>,
    ) {
        let fp = fingerprint(&req);
        let model = req.model.clone();
        for m in &mut req.messages {
            if let Message::Assistant { sealed, .. } = m
                && sealed.as_ref().is_some_and(|s| !self.keeps(s, &model, &fp))
            {
                *sealed = None;
            }
        }
        if !self.extra.is_empty() {
            let mut extra = serde_json::Value::Object(self.extra.clone());
            merge(&mut extra, &req.extra_body);
            if let serde_json::Value::Object(m) = extra {
                req.extra_body = m;
            }
        }
        let (path, body, accept) = match self.api {
            Api::OpenAiChat => (
                openai_chat::CHAT_PATH.to_owned(),
                openai_chat::build_body(&req, self.dialect),
                "text/event-stream",
            ),
            #[cfg(feature = "proto-ollama")]
            Api::Ollama => (
                ollama::CHAT_PATH.to_owned(),
                ollama::build_body(&req),
                "application/x-ndjson",
            ),
            #[cfg(feature = "proto-anthropic")]
            Api::Anthropic => (
                anthropic::path(&self.base_url, "messages"),
                anthropic::build_body(&req),
                "text/event-stream",
            ),
            #[cfg(feature = "proto-openai-responses")]
            Api::Responses => (
                responses::RESPONSES_PATH.to_owned(),
                responses::build_body(&req, self.route),
                "text/event-stream",
            ),
        };
        let mut attempt = 0;
        let resp = loop {
            let headers = match self.request_headers().await {
                Ok(h) => h,
                Err(e) => {
                    let _ = tx.send(Err(e)).await;
                    return;
                }
            };
            let sent = self
                .client
                .post(self.url(&path))
                .headers(headers)
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
            Api::OpenAiChat => Wire::Sse(
                SseDecoder::default(),
                SseParser::Chat(ChunkParser::new(reasoning)),
            ),
            #[cfg(feature = "proto-anthropic")]
            Api::Anthropic => Wire::Sse(
                SseDecoder::default(),
                SseParser::Anthropic(anthropic::EventParser::default()),
            ),
            #[cfg(feature = "proto-openai-responses")]
            Api::Responses => Wire::Sse(
                SseDecoder::default(),
                SseParser::Responses(responses::EventParser::default()),
            ),
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

/// The parser behind an SSE stream, per protocol.
enum SseParser {
    Chat(ChunkParser),
    #[cfg(feature = "proto-anthropic")]
    Anthropic(anthropic::EventParser),
    #[cfg(feature = "proto-openai-responses")]
    Responses(responses::EventParser),
}

impl SseParser {
    fn parse(&mut self, data: &str) -> Parsed {
        match self {
            Self::Chat(p) => p.parse(data),
            #[cfg(feature = "proto-anthropic")]
            Self::Anthropic(p) => p.parse(data),
            #[cfg(feature = "proto-openai-responses")]
            Self::Responses(p) => p.parse(data),
        }
    }

    fn finish(&mut self) -> Vec<StreamEvent> {
        match self {
            Self::Chat(p) => p.finish(),
            #[cfg(feature = "proto-anthropic")]
            Self::Anthropic(p) => p.finish(),
            #[cfg(feature = "proto-openai-responses")]
            Self::Responses(p) => p.finish(),
        }
    }
}

/// Framing plus parsing for each wire format.
enum Wire {
    Sse(SseDecoder, SseParser),
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

    let env_str = |key: &str| {
        env(key)
            .or_else(|| std::env::var_os(key))
            .and_then(|v| v.into_string().ok())
            .filter(|s| !s.is_empty())
    };

    let proxy = Some(http.proxy.as_str())
        .filter(|p| !p.is_empty())
        .map(str::to_owned)
        .or_else(|| Some(network.proxy.clone()).filter(|p| !p.is_empty()))
        .or_else(|| {
            [
                "HTTPS_PROXY",
                "https_proxy",
                "HTTP_PROXY",
                "http_proxy",
                "ALL_PROXY",
                "all_proxy",
            ]
            .into_iter()
            .find_map(env_str)
        });
    if let Some(ref p) = proxy {
        b = b.proxy(reqwest::Proxy::all(p).map_err(|e| config(format!("proxy `{p}`: {e}")))?);
    }

    let mut roots = Vec::new();
    let mut ca_files = Vec::new();
    if !network.ca_bundle.is_empty() {
        ca_files.push(network.ca_bundle.clone());
    }
    if let Some(ca) = file.tls.ca_file.as_deref().filter(|s| !s.is_empty()) {
        ca_files.push(ca.to_owned());
    }
    if ca_files.is_empty() {
        ca_files.extend(
            ["SSL_CERT_FILE", "REQUESTS_CA_BUNDLE", "CURL_CA_BUNDLE"]
                .into_iter()
                .find_map(env_str),
        );
    }
    for ca in &ca_files {
        let pem = read("CA file", ca)?;
        roots.extend(
            reqwest::Certificate::from_pem_bundle(&pem)
                .map_err(|e| config(format!("CA file `{ca}`: {e}")))?,
        );
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::ffi::OsString;

    #[test]
    fn proxy_and_ca_bundle_from_env() {
        let file: ProviderFile =
            serde_json::from_value(serde_json::json!({ "base_url": "https://api.example.com" }))
                .expect("provider file");
        let network = NetworkSettings::default();

        // Default empty env succeeds.
        let empty_env: HashMap<String, OsString> = HashMap::new();
        assert!(build_client(&file, &network, &|k| empty_env.get(k).cloned()).is_ok());

        // Environment proxy is picked up.
        let mut proxy_env: HashMap<String, OsString> = HashMap::new();
        proxy_env.insert("HTTPS_PROXY".into(), "http://127.0.0.1:8080".into());
        assert!(build_client(&file, &network, &|k| proxy_env.get(k).cloned()).is_ok());

        // Invalid proxy URL errors clearly.
        let mut bad_proxy_env: HashMap<String, OsString> = HashMap::new();
        bad_proxy_env.insert("HTTPS_PROXY".into(), "not a valid uri".into());
        let err = build_client(&file, &network, &|k| bad_proxy_env.get(k).cloned())
            .expect_err("invalid proxy must fail");
        assert!(err.message.contains("proxy `not a valid uri`"), "{err:?}");

        // CA bundle in environment pointing to non-existent file errors clearly.
        let mut ca_env: HashMap<String, OsString> = HashMap::new();
        ca_env.insert("SSL_CERT_FILE".into(), "/nonexistent/ca.pem".into());
        let err = build_client(&file, &network, &|k| ca_env.get(k).cloned())
            .expect_err("missing ca must fail");
        assert!(
            err.message.contains("CA file `/nonexistent/ca.pem`"),
            "{err:?}"
        );
    }
}
