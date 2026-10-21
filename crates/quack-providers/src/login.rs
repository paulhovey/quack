//! Sign in with ChatGPT (`[auth] type = "chatgpt"`): a browser sign-in that lets requests
//! use the account's ChatGPT plan instead of an API key
//! (docs/plan/02-providers.md#sign-in-with-chatgpt).
//!
//! `quack login <provider>` runs the OAuth flow ([`Login::sign_in`]): PKCE, a callback on
//! `127.0.0.1`, and the ID token checked against the issuer's keys. The provider then sends
//! the access token and renews it shortly before it expires ([`Login::bearer`]); renewals
//! are serialized across processes, because each one replaces the refresh token.
//! Credentials live in `<data dir>/auth/<provider>.json`, readable by the owner only. The
//! host id (`host-id`) and the client id the issuer gave this app stay across sign-outs.
//! Only the provider's `issuer` is contacted, and only from these functions.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::error::{ErrorKind, ProviderError, body_message};

/// The client id for a first sign-in; the issuer answers with this app's own.
const FIRST_CLIENT: &str = "dynamic_agent_client";
/// The name the user sees for this app in ChatGPT settings.
const APP_NAME: &str = "quack";
const SCOPE: &str = "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";
/// The scope that lets requests use the plan.
const PLAN_SCOPE: &str = "chatgpt.tokens.use.direct";
const AUTHORIZE_PATH: &str = "api/accounts/authorize";
const TOKEN_PATH: &str = "api/accounts/oauth/token";
const DISCOVERY_PATH: &str = ".well-known/openid-configuration";
const CALLBACK_PATH: &str = "/auth/callback";
/// The port the issuer's examples use; any free one works when it is taken.
const PREFERRED_PORT: u16 = 1455;
/// How long the browser has to finish the sign-in.
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(300);
/// Renew the access token this many seconds before it expires.
const EARLY_SECS: u64 = 120;
/// A lock file older than this was left by a process that died.
const STALE_LOCK: Duration = Duration::from_secs(60);

/// A saved sign-in. Tokens are empty after a sign-out; the client id and account stay, so
/// the next sign-in reuses the registration.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Credentials {
    pub client_id: String,
    /// The account's `sub`, its identity.
    pub subject: String,
    pub email: String,
    #[serde(default)]
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: String,
    #[serde(default)]
    pub id_token: String,
    /// Unix seconds.
    #[serde(default)]
    pub expires_at: u64,
    #[serde(default)]
    pub scopes: Vec<String>,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("client_id", &self.client_id)
            .field("email", &self.email)
            .field("expires_at", &self.expires_at)
            .field("scopes", &self.scopes)
            .finish_non_exhaustive()
    }
}

impl Credentials {
    pub fn signed_in(&self) -> bool {
        !self.refresh_token.is_empty()
    }

    /// Whether the account allowed requests to use its plan.
    pub fn plan_granted(&self) -> bool {
        self.scopes.iter().any(|s| s == PLAN_SCOPE)
    }
}

/// What a sign-in did.
#[derive(Debug, Clone)]
pub struct SignedIn {
    pub email: String,
    /// False when the account declined plan usage: requests will fail until it is allowed.
    pub plan: bool,
}

/// One provider's sign-in.
#[derive(Debug)]
pub struct Login {
    provider: String,
    issuer: String,
    /// The API the tokens are for (the provider's `base_url`).
    resource: String,
    dir: PathBuf,
    client: reqwest::Client,
    cached: tokio::sync::Mutex<Option<Credentials>>,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn auth(msg: impl Into<String>) -> ProviderError {
    ProviderError::new(ErrorKind::Auth, msg)
}

impl Login {
    /// `data_dir` is the user data directory; credentials go in its `auth` folder.
    pub fn new(
        provider: &str,
        issuer: &str,
        resource: &str,
        data_dir: &Path,
        client: reqwest::Client,
    ) -> Self {
        Self {
            provider: provider.to_owned(),
            issuer: issuer.trim_end_matches('/').to_owned(),
            resource: resource.trim_end_matches('/').to_owned(),
            dir: data_dir.join("auth"),
            client,
            cached: tokio::sync::Mutex::new(None),
        }
    }

    fn file(&self) -> PathBuf {
        self.dir.join(format!("{}.json", self.provider))
    }

    fn url(&self, path: &str) -> String {
        format!("{}/{path}", self.issuer)
    }

    /// The saved sign-in, if any.
    pub fn load(&self) -> Option<Credentials> {
        let text = std::fs::read_to_string(self.file()).ok()?;
        serde_json::from_str(&text).ok()
    }

    fn save(&self, c: &Credentials) -> Result<(), String> {
        let text = serde_json::to_string_pretty(c).map_err(|e| e.to_string())?;
        write_private(&self.file(), &text)
    }

    /// An access token for the next request, renewed when it is about to expire.
    pub async fn bearer(&self) -> Result<String, ProviderError> {
        let mut cached = self.cached.lock().await;
        if let Some(c) = cached.as_ref()
            && c.signed_in()
            && c.expires_at > now() + EARLY_SECS
        {
            return Ok(c.access_token.clone());
        }
        let not_signed_in = || {
            auth(format!(
                "not signed in to ChatGPT · run `quack login {}`",
                self.provider
            ))
        };
        let c = self
            .load()
            .filter(Credentials::signed_in)
            .ok_or_else(not_signed_in)?;
        if !c.plan_granted() {
            return Err(auth(format!(
                "the ChatGPT account did not allow plan usage · run `quack login {}` and \
                 allow it, or use an API key",
                self.provider
            )));
        }
        let c = if c.expires_at > now() + EARLY_SECS {
            c
        } else {
            // Another process may be renewing it: take the lock, then look again.
            let _lock = FileLock::acquire(self.dir.join(format!("{}.lock", self.provider)))
                .await
                .map_err(auth)?;
            let c = self
                .load()
                .filter(Credentials::signed_in)
                .ok_or_else(not_signed_in)?;
            if c.expires_at > now() + EARLY_SECS {
                c
            } else {
                let renewed = self.refresh(c).await?;
                self.save(&renewed)
                    .map_err(|e| auth(format!("cannot save the renewed ChatGPT sign-in: {e}")))?;
                renewed
            }
        };
        let token = c.access_token.clone();
        *cached = Some(c);
        Ok(token)
    }

    async fn refresh(&self, mut c: Credentials) -> Result<Credentials, ProviderError> {
        let resp = self
            .client
            .post(self.url(TOKEN_PATH))
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(form(&[
                ("grant_type", "refresh_token"),
                ("client_id", &c.client_id),
                ("refresh_token", &c.refresh_token),
                ("resource", &self.resource),
            ]))
            .send()
            .await
            .map_err(|e| ProviderError::from_reqwest(&e))?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            let code = serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|v| v["error"].as_str().map(str::to_owned))
                .unwrap_or_default();
            if TERMINAL_REFRESH.contains(&code.as_str()) {
                // The sign-in is gone: keep the registration, drop the tokens.
                c.access_token.clear();
                c.refresh_token.clear();
                c.expires_at = 0;
                let _ = self.save(&c);
                return Err(auth(format!(
                    "the ChatGPT sign-in has expired or was revoked ({code}) · run `quack \
                     login {}`",
                    self.provider
                )));
            }
            return Err(ProviderError::from_status(
                status.as_u16(),
                None,
                &format!("renewing the ChatGPT sign-in: {}", body_message(&body)),
            ));
        }
        let t: TokenResponse = serde_json::from_str(&body).map_err(|e| {
            ProviderError::new(
                ErrorKind::Protocol,
                format!("unexpected token response: {e}"),
            )
        })?;
        c.access_token = t.access_token;
        // The refresh token rotates; keep the old one only if none came back.
        if let Some(r) = t.refresh_token.filter(|r| !r.is_empty()) {
            c.refresh_token = r;
        }
        if let Some(id) = t.id_token.filter(|i| !i.is_empty()) {
            c.id_token = id;
        }
        c.expires_at = now() + t.expires_in.unwrap_or(3600);
        if let Some(scope) = t.scope {
            c.scopes = scope.split_whitespace().map(str::to_owned).collect();
        }
        Ok(c)
    }

    /// The issuer's `openid-configuration`, refusing endpoints on another host.
    async fn discovery(&self) -> Result<Value, String> {
        let doc: Value = self
            .client
            .get(self.url(DISCOVERY_PATH))
            .send()
            .await
            .map_err(|e| format!("cannot reach {}: {e}", self.issuer))?
            .error_for_status()
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())?;
        Ok(doc)
    }

    fn same_host(&self, url: &str) -> Result<(), String> {
        let host = quack_config::url_host(url);
        if host.eq_ignore_ascii_case(quack_config::url_host(&self.issuer)) {
            Ok(())
        } else {
            Err(format!(
                "the issuer points at another host ({host}); only {} is contacted",
                self.issuer
            ))
        }
    }

    /// Run the browser sign-in. `open` gets the address to show or open; `fresh` starts a
    /// new registration (another account) instead of reusing the saved one.
    pub async fn sign_in(&self, fresh: bool, open: impl FnOnce(&str)) -> Result<SignedIn, String> {
        let host_id = self.host_id()?;
        let saved = self.load().filter(|c| !fresh && !c.client_id.is_empty());

        let listener = match tokio::net::TcpListener::bind(("127.0.0.1", PREFERRED_PORT)).await {
            Ok(l) => l,
            Err(_) => tokio::net::TcpListener::bind(("127.0.0.1", 0))
                .await
                .map_err(|e| format!("cannot listen on 127.0.0.1: {e}"))?,
        };
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        let redirect = format!("http://127.0.0.1:{port}{CALLBACK_PATH}");
        let state = random_token()?;
        let nonce = random_token()?;
        let verifier = random_token()?;
        let challenge = b64url(
            aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, verifier.as_bytes()).as_ref(),
        );

        let client_id = saved
            .as_ref()
            .map_or(FIRST_CLIENT, |c| c.client_id.as_str());
        let mut params = vec![
            ("response_type", "code"),
            ("client_id", client_id),
            ("redirect_uri", &redirect),
            ("scope", SCOPE),
            ("resource", &self.resource),
            ("state", &state),
            ("nonce", &nonce),
            ("code_challenge", &challenge),
            ("code_challenge_method", "S256"),
            ("ext_agent_host_id", &host_id),
        ];
        match &saved {
            None => params.push(("agent_name_hint", APP_NAME)),
            Some(c) => {
                if !c.id_token.is_empty() {
                    params.push(("id_token_hint", &c.id_token));
                }
                if !c.email.is_empty() {
                    params.push(("login_hint", &c.email));
                }
            }
        }
        open(&format!("{}?{}", self.url(AUTHORIZE_PATH), form(&params)));

        let query = tokio::time::timeout(SIGN_IN_TIMEOUT, callback(&listener, &state))
            .await
            .map_err(|_| "the sign-in was not finished within 5 minutes".to_owned())??;
        let get = |k: &str| query.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
        if let Some(e) = get("error") {
            return Err(match e {
                "access_denied" => "the sign-in was declined in the browser".to_owned(),
                other => format!("the sign-in failed: {other}"),
            });
        }
        let code = get("code").ok_or("the sign-in returned no code")?;
        let client_id = match (&saved, get("client_id")) {
            (Some(c), Some(issued)) if issued != c.client_id => {
                return Err("the sign-in came back for another app registration".into());
            }
            (Some(c), _) => c.client_id.clone(),
            (None, Some(issued)) if issued != FIRST_CLIENT && !issued.is_empty() => {
                issued.to_owned()
            }
            (None, _) => return Err("the sign-in did not finish registering this app".into()),
        };

        let resp = self
            .client
            .post(self.url(TOKEN_PATH))
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(form(&[
                ("grant_type", "authorization_code"),
                ("client_id", &client_id),
                ("code", code),
                ("code_verifier", &verifier),
                ("redirect_uri", &redirect),
                ("resource", &self.resource),
            ]))
            .send()
            .await
            .map_err(|e| format!("cannot reach {}: {e}", self.issuer))?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(format!(
                "the token exchange failed ({status}): {}",
                body_message(&body)
            ));
        }
        let t: TokenResponse =
            serde_json::from_str(&body).map_err(|e| format!("unexpected token response: {e}"))?;
        let id_token = t
            .id_token
            .clone()
            .ok_or("the sign-in returned no ID token")?;
        let claims = self.verify_id_token(&id_token, &client_id, &nonce).await?;
        let subject = claims["sub"]
            .as_str()
            .ok_or("the ID token has no subject")?;
        if let Some(c) = &saved
            && !c.subject.is_empty()
            && c.subject != subject
        {
            return Err(format!(
                "this signed in a different account than the saved one · run `quack login \
                 {} --new` to switch accounts",
                self.provider
            ));
        }
        let scopes: Vec<String> = t
            .scope
            .as_deref()
            .unwrap_or(SCOPE)
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        let c = Credentials {
            client_id,
            subject: subject.to_owned(),
            email: claims["email"].as_str().unwrap_or_default().to_owned(),
            access_token: t.access_token,
            refresh_token: t.refresh_token.unwrap_or_default(),
            id_token,
            expires_at: now() + t.expires_in.unwrap_or(3600),
            scopes,
        };
        if !c.signed_in() {
            return Err("the sign-in returned no refresh token".into());
        }
        self.save(&c)
            .map_err(|e| format!("cannot save the sign-in: {e}"))?;
        *self.cached.lock().await = None;
        Ok(SignedIn {
            plan: c.plan_granted(),
            email: c.email,
        })
    }

    /// Check the ID token's signature against the issuer's keys, and its claims.
    async fn verify_id_token(
        &self,
        token: &str,
        client_id: &str,
        nonce: &str,
    ) -> Result<Value, String> {
        let bad = |why: &str| format!("the ID token is not valid: {why}");
        let mut parts = token.split('.');
        let (Some(h), Some(p), Some(s), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(bad("not a JWT"));
        };
        let header: Value = serde_json::from_slice(&b64url_decode(h).ok_or_else(|| bad("header"))?)
            .map_err(|_| bad("header"))?;
        if header["alg"] != "RS256" {
            return Err(bad("unsupported signature algorithm"));
        }
        let doc = self.discovery().await?;
        let jwks_uri = doc["jwks_uri"].as_str().ok_or("the issuer lists no keys")?;
        self.same_host(jwks_uri)?;
        let keys: Value = self
            .client
            .get(jwks_uri)
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())?;
        let key = keys["keys"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|k| k["kty"] == "RSA" && (header["kid"].is_null() || k["kid"] == header["kid"]))
            .ok_or_else(|| bad("no matching key"))?;
        let n = key["n"]
            .as_str()
            .and_then(b64url_decode)
            .ok_or_else(|| bad("key"))?;
        let e = key["e"]
            .as_str()
            .and_then(b64url_decode)
            .ok_or_else(|| bad("key"))?;
        let sig = b64url_decode(s).ok_or_else(|| bad("signature"))?;
        aws_lc_rs::signature::RsaPublicKeyComponents { n, e }
            .verify(
                &aws_lc_rs::signature::RSA_PKCS1_2048_8192_SHA256,
                format!("{h}.{p}").as_bytes(),
                &sig,
            )
            .map_err(|_| bad("bad signature"))?;
        let claims: Value = serde_json::from_slice(&b64url_decode(p).ok_or_else(|| bad("claims"))?)
            .map_err(|_| bad("claims"))?;
        check_claims(&claims, &self.issuer, client_id, nonce, now()).map_err(bad)?;
        Ok(claims)
    }

    /// Sign out: revoke the refresh token and forget the tokens. `Ok(false)` when the
    /// issuer could not confirm the revocation.
    pub async fn sign_out(&self) -> Result<bool, String> {
        let Some(mut c) = self.load().filter(Credentials::signed_in) else {
            return Ok(true);
        };
        let revoked = match self.revoke(&c).await {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!(error = %e, "revoking the ChatGPT sign-in");
                false
            }
        };
        c.access_token.clear();
        c.refresh_token.clear();
        c.id_token.clear();
        c.expires_at = 0;
        self.save(&c)?;
        *self.cached.lock().await = None;
        Ok(revoked)
    }

    async fn revoke(&self, c: &Credentials) -> Result<(), String> {
        let doc = self.discovery().await?;
        let url = doc["revocation_endpoint"]
            .as_str()
            .ok_or("the issuer lists no revocation endpoint")?;
        self.same_host(url)?;
        let mut last = String::new();
        for wait in [0, 1, 3] {
            tokio::time::sleep(Duration::from_secs(wait)).await;
            let sent = self
                .client
                .post(url)
                .header(
                    reqwest::header::CONTENT_TYPE,
                    "application/x-www-form-urlencoded",
                )
                .body(form(&[
                    ("token", &c.refresh_token),
                    ("token_type_hint", "refresh_token"),
                    ("client_id", &c.client_id),
                ]))
                .send()
                .await;
            match sent {
                Ok(r) if r.status().is_success() => return Ok(()),
                Ok(r) if r.status().is_server_error() => last = r.status().to_string(),
                Ok(r) => return Err(r.status().to_string()),
                Err(e) => last = e.to_string(),
            }
        }
        Err(last)
    }

    /// A stable id for this machine, made on first use and kept across sign-outs.
    fn host_id(&self) -> Result<String, String> {
        let path = self.dir.join("host-id");
        if let Ok(id) = std::fs::read_to_string(&path)
            && !id.trim().is_empty()
        {
            return Ok(id.trim().to_owned());
        }
        let mut b = [0u8; 16];
        aws_lc_rs::rand::fill(&mut b).map_err(|_| "no random source")?;
        b[6] = (b[6] & 0x0f) | 0x40;
        b[8] = (b[8] & 0x3f) | 0x80;
        let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
        let id = format!(
            "urn:uuid:{}-{}-{}-{}-{}",
            &hex[..8],
            &hex[8..12],
            &hex[12..16],
            &hex[16..20],
            &hex[20..]
        );
        write_private(&path, &id)?;
        Ok(id)
    }
}

/// Refresh errors that mean the sign-in is gone and must be repeated.
const TERMINAL_REFRESH: &[&str] = &[
    "invalid_grant",
    "invalid_refresh_token",
    "token_expired",
    "refresh_token_expired",
    "refresh_token_invalidated",
    "refresh_token_reused",
];

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    scope: Option<String>,
}

/// The ID token's issuer, audience, expiry and nonce.
fn check_claims(
    claims: &Value,
    issuer: &str,
    client_id: &str,
    nonce: &str,
    now: u64,
) -> Result<(), &'static str> {
    let iss = claims["iss"].as_str().unwrap_or_default();
    if iss.trim_end_matches('/') != issuer.trim_end_matches('/') {
        return Err("wrong issuer");
    }
    let aud_ok = match &claims["aud"] {
        Value::String(a) => a == client_id,
        Value::Array(a) => a.iter().any(|x| x == client_id),
        _ => false,
    };
    if !aud_ok {
        return Err("wrong audience");
    }
    if claims["exp"].as_u64().is_none_or(|exp| exp <= now) {
        return Err("expired");
    }
    if claims["nonce"].as_str() != Some(nonce) {
        return Err("wrong nonce");
    }
    Ok(())
}

/// Wait for the browser to come back to the callback with our `state`; the query pairs.
async fn callback(
    listener: &tokio::net::TcpListener,
    state: &str,
) -> Result<Vec<(String, String)>, String> {
    loop {
        let (mut stream, _) = listener.accept().await.map_err(|e| e.to_string())?;
        let mut buf = Vec::new();
        let mut chunk = [0u8; 2048];
        while !buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.len() < 16 * 1024 {
            match stream.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
        }
        let head = String::from_utf8_lossy(&buf);
        let target = head
            .lines()
            .next()
            .and_then(|l| l.strip_prefix("GET "))
            .and_then(|l| l.split_whitespace().next())
            .unwrap_or_default();
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        let pairs = parse_query(query);
        let ours = path == CALLBACK_PATH && pairs.iter().any(|(k, v)| k == "state" && v == state);
        let (status, text) = if !ours {
            ("404 Not Found", "Not found.")
        } else if pairs.iter().any(|(k, _)| k == "error") {
            (
                "200 OK",
                "The sign-in did not finish. You can close this tab.",
            )
        } else {
            (
                "200 OK",
                "Signed in. You can close this tab and go back to the terminal.",
            )
        };
        let page = format!("<!doctype html><meta charset=utf-8><title>quack</title><p>{text}</p>");
        let reply = format!(
            "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{page}",
            page.len()
        );
        let _ = stream.write_all(reply.as_bytes()).await;
        let _ = stream.shutdown().await;
        if ours {
            return Ok(pairs);
        }
    }
}

/// A lock file held while a token is renewed, so two processes don't both use a refresh
/// token that works once.
struct FileLock(PathBuf);

impl FileLock {
    async fn acquire(path: PathBuf) -> Result<Self, String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        for _ in 0..200 {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(_) => return Ok(Self(path)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let stale = std::fs::metadata(&path)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.elapsed().ok())
                        .is_some_and(|age| age > STALE_LOCK);
                    if stale {
                        let _ = std::fs::remove_file(&path);
                    } else {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                }
                Err(e) => return Err(format!("cannot lock {}: {e}", path.display())),
            }
        }
        Err(format!(
            "another quack is renewing the sign-in; remove {} if none is",
            path.display()
        ))
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Write a file readable by the owner only, replacing it whole.
fn write_private(path: &Path, text: &str) -> Result<(), String> {
    let dir = path.parent().ok_or("no folder")?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("tmp");
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    let mut f = opts.open(&tmp).map_err(|e| e.to_string())?;
    std::io::Write::write_all(&mut f, text.as_bytes()).map_err(|e| e.to_string())?;
    f.sync_all().map_err(|e| e.to_string())?;
    drop(f);
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

/// 32 random bytes, base64url.
fn random_token() -> Result<String, String> {
    let mut b = [0u8; 32];
    aws_lc_rs::rand::fill(&mut b).map_err(|_| "no random source")?;
    Ok(b64url(&b))
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// Base64url without padding.
fn b64url(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | u32::from(*b) << (16 - 8 * i));
        for i in 0..=chunk.len() {
            out.push(char::from(B64[(n >> (18 - 6 * i)) as usize & 63]));
        }
    }
    out
}

/// Base64url (padding optional) to bytes.
fn b64url_decode(text: &str) -> Option<Vec<u8>> {
    let text = text.trim_end_matches('=');
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0);
    for c in text.bytes() {
        let v = B64.iter().position(|&x| x == c)?;
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

/// `application/x-www-form-urlencoded`, also used for the authorize query.
fn form(pairs: &[(&str, &str)]) -> String {
    let enc = |s: &str| {
        s.bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                    char::from(b).to_string()
                }
                _ => format!("%{b:02X}"),
            })
            .collect::<String>()
    };
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", enc(k), enc(v)))
        .collect::<Vec<_>>()
        .join("&")
}

fn parse_query(query: &str) -> Vec<(String, String)> {
    let dec = |s: &str| {
        let s = s.replace('+', " ");
        let b = s.as_bytes();
        let mut out = Vec::with_capacity(b.len());
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'%'
                && let Some(v) = s
                    .get(i + 1..i + 3)
                    .and_then(|h| u8::from_str_radix(h, 16).ok())
            {
                out.push(v);
                i += 3;
            } else {
                out.push(b[i]);
                i += 1;
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    };
    query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (dec(k), dec(v))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn base64url_round_trips() {
        for n in 0..40u8 {
            let bytes: Vec<u8> = (0..n).map(|i| i.wrapping_mul(37)).collect();
            assert_eq!(b64url_decode(&b64url(&bytes)).as_deref(), Some(&bytes[..]));
        }
        // RFC 7636's example verifier and challenge.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let digest = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, verifier.as_bytes());
        assert_eq!(
            b64url(digest.as_ref()),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn query_and_form_encoding() {
        assert_eq!(form(&[("a b", "x&y=z/")]), "a%20b=x%26y%3Dz%2F");
        assert_eq!(
            parse_query("code=a%2Fb&state=s+t&flag"),
            vec![
                ("code".into(), "a/b".into()),
                ("state".into(), "s t".into()),
                ("flag".into(), String::new())
            ]
        );
    }

    #[test]
    fn id_token_claims_are_checked() {
        let ok = json!({"iss":"https://auth.example","aud":["c1"],"exp":200,"nonce":"n"});
        assert_eq!(
            check_claims(&ok, "https://auth.example/", "c1", "n", 100),
            Ok(())
        );
        assert_eq!(
            check_claims(&ok, "https://other", "c1", "n", 100),
            Err("wrong issuer")
        );
        assert_eq!(
            check_claims(&ok, "https://auth.example", "c2", "n", 100),
            Err("wrong audience")
        );
        assert_eq!(
            check_claims(&ok, "https://auth.example", "c1", "n", 300),
            Err("expired")
        );
        assert_eq!(
            check_claims(&ok, "https://auth.example", "c1", "m", 100),
            Err("wrong nonce")
        );
    }

    #[test]
    fn credentials_never_print_tokens() {
        let c = Credentials {
            access_token: "secret-a".into(),
            refresh_token: "secret-r".into(),
            ..Credentials::default()
        };
        assert!(!format!("{c:?}").contains("secret"));
    }
}
