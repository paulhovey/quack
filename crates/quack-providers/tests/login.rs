//! Sign in with ChatGPT against a mock issuer and API: the browser flow, token renewal,
//! and that nothing is sent before a sign-in.
#![cfg(feature = "auth-chatgpt")]

use std::path::Path;
use std::sync::{Arc, Mutex};

use quack_config::ProviderFile;
use quack_config::provider::ReasoningFormat;
use quack_config::settings::NetworkSettings;
use quack_providers::login::Credentials;
use quack_providers::{ChatRequest, ErrorKind, Message, Provider, ResponseBuilder};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const PLAN_SCOPES: &str =
    "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";

fn provider(server: &MockServer, data: &Path) -> Provider {
    let file: ProviderFile = toml::from_str(&format!(
        "api = \"openai-responses\"\nbase_url = \"{0}/v1\"\n\
         auth = {{ type = \"chatgpt\", issuer = \"{0}\" }}\nmodels = [\"m\"]\n",
        server.uri()
    ))
    .expect("provider toml");
    let data = data.as_os_str().to_owned();
    let env = move |k: &str| (k == "QUACK_DATA_DIR").then(|| data.clone());
    Provider::new("chatgpt", file, &NetworkSettings::default(), &env).expect("provider")
}

fn save(data: &Path, c: &Credentials) {
    let dir = data.join("auth");
    std::fs::create_dir_all(&dir).expect("dir");
    std::fs::write(
        dir.join("chatgpt.json"),
        serde_json::to_string(c).expect("json"),
    )
    .expect("write");
}

fn saved(data: &Path) -> Credentials {
    let text = std::fs::read_to_string(data.join("auth").join("chatgpt.json")).expect("read");
    serde_json::from_str(&text).expect("parse")
}

fn signed_in(expires_at: u64) -> Credentials {
    Credentials {
        client_id: "oaiapp_1".into(),
        subject: "user-1".into(),
        email: "a@example.com".into(),
        access_token: "a1".into(),
        refresh_token: "r1".into(),
        id_token: "id".into(),
        expires_at,
        scopes: PLAN_SCOPES.split(' ').map(str::to_owned).collect(),
    }
}

const SSE: &str = "data: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"delta\":\"Hi\"}\n\n\
data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":3,\"output_tokens\":1}}}\n\n";

async fn ask(p: &Provider) -> Result<String, quack_providers::ProviderError> {
    let req = ChatRequest {
        model: "m".into(),
        messages: vec![Message::User {
            content: "hi".into(),
        }],
        ..ChatRequest::default()
    };
    let mut rx = p.stream(req, ReasoningFormat::Auto, CancellationToken::new());
    let mut b = ResponseBuilder::default();
    while let Some(ev) = rx.recv().await {
        b.push(&ev?);
    }
    match b.into_message() {
        Message::Assistant { content, .. } => Ok(content),
        _ => Ok(String::new()),
    }
}

#[tokio::test]
async fn nothing_is_sent_before_a_sign_in() {
    let server = MockServer::start().await;
    let data = tempfile::tempdir().expect("tempdir");
    let p = provider(&server, data.path());
    assert!(
        p.not_ready()
            .is_some_and(|w| w.contains("quack login chatgpt"))
    );
    let e = p.list_models().await.expect_err("not signed in");
    assert_eq!(e.kind, ErrorKind::Auth);
    let e = ask(&p).await.expect_err("not signed in");
    assert!(e.message.contains("quack login chatgpt"), "{}", e.message);
    assert!(
        server
            .received_requests()
            .await
            .unwrap_or_default()
            .is_empty()
    );
}

#[tokio::test]
async fn an_expiring_token_is_renewed_once_and_used() {
    let server = MockServer::start().await;
    let data = tempfile::tempdir().expect("tempdir");
    save(data.path(), &signed_in(0));
    Mock::given(method("POST"))
        .and(path("/api/accounts/oauth/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .and(body_string_contains("refresh_token=r1"))
        .and(body_string_contains("client_id=oaiapp_1"))
        .and(body_string_contains(format!(
            "resource={}",
            form_escape(&format!("{}/v1", server.uri()))
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "a2",
            "refresh_token": "r2",
            "expires_in": 3600,
            "scope": PLAN_SCOPES,
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .and(header("authorization", "Bearer a2"))
        .and(body_string_contains("\"store\":false"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(SSE),
        )
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .and(header("authorization", "Bearer a2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "models": [
                { "slug": "gpt-x", "display_name": "GPT X", "visibility": "list" },
                { "slug": "internal", "visibility": "hide" },
            ]
        })))
        .mount(&server)
        .await;

    let p = provider(&server, data.path());
    assert_eq!(p.not_ready(), None);
    assert_eq!(ask(&p).await.expect("first"), "Hi");
    assert_eq!(ask(&p).await.expect("second"), "Hi");
    let ids: Vec<String> = p
        .list_models()
        .await
        .expect("models")
        .into_iter()
        .map(|m| m.id)
        .collect();
    assert_eq!(ids, ["gpt-x"]);
    // The rotated refresh token was saved.
    let c = saved(data.path());
    assert_eq!(
        (c.access_token.as_str(), c.refresh_token.as_str()),
        ("a2", "r2")
    );
}

#[tokio::test]
async fn a_revoked_sign_in_asks_for_a_new_one() {
    let server = MockServer::start().await;
    let data = tempfile::tempdir().expect("tempdir");
    save(data.path(), &signed_in(0));
    Mock::given(method("POST"))
        .and(path("/api/accounts/oauth/token"))
        .respond_with(
            ResponseTemplate::new(400).set_body_json(json!({ "error": "refresh_token_reused" })),
        )
        .mount(&server)
        .await;
    let p = provider(&server, data.path());
    let e = ask(&p).await.expect_err("revoked");
    assert_eq!(e.kind, ErrorKind::Auth);
    assert!(e.message.contains("quack login chatgpt"), "{}", e.message);
    let c = saved(data.path());
    assert!(!c.signed_in());
    // The registration stays for the next sign-in.
    assert_eq!(c.client_id, "oaiapp_1");
}

/// What the browser saw, for the token endpoint to answer with.
#[derive(Default)]
struct Seen {
    nonce: String,
    challenge: String,
}

/// The token endpoint: checks the PKCE verifier and returns an ID token signed with `key`.
struct TokenEndpoint {
    key: Arc<aws_lc_rs::rsa::KeyPair>,
    issuer: String,
    seen: Arc<Mutex<Seen>>,
}

impl Respond for TokenEndpoint {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let body = String::from_utf8_lossy(&req.body).into_owned();
        let field = |k: &str| {
            body.split('&')
                .find_map(|p| p.strip_prefix(&format!("{k}=")))
                .unwrap_or_default()
                .to_owned()
        };
        let seen = self.seen.lock().expect("lock");
        let verifier = field("code_verifier");
        let digest = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, verifier.as_bytes());
        if field("code") != "c1"
            || field("client_id") != "oaiapp_1"
            || b64(digest.as_ref()) != seen.challenge
        {
            return ResponseTemplate::new(400).set_body_json(json!({"error": "invalid_grant"}));
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time")
            .as_secs();
        let claims = json!({
            "iss": self.issuer,
            "aud": "oaiapp_1",
            "sub": "user-1",
            "email": "a@example.com",
            "exp": now + 3600,
            "nonce": seen.nonce,
        });
        ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "a1",
            "refresh_token": "r1",
            "id_token": jwt(&self.key, &claims),
            "expires_in": 3600,
            "scope": PLAN_SCOPES,
        }))
    }
}

#[tokio::test]
async fn sign_in_registers_checks_the_id_token_and_signs_out() {
    let server = MockServer::start().await;
    let data = tempfile::tempdir().expect("tempdir");
    let key =
        Arc::new(aws_lc_rs::rsa::KeyPair::generate(aws_lc_rs::rsa::KeySize::Rsa2048).expect("key"));
    let (n, e) = rsa_public(&key);
    Mock::given(method("GET"))
        .and(path("/.well-known/openid-configuration"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "jwks_uri": format!("{}/jwks", server.uri()),
            "revocation_endpoint": format!("{}/revoke", server.uri()),
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/jwks"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "keys": [{ "kty": "RSA", "kid": "k1", "n": b64(&n), "e": b64(&e) }]
        })))
        .mount(&server)
        .await;
    let seen = Arc::new(Mutex::new(Seen::default()));
    Mock::given(method("POST"))
        .and(path("/api/accounts/oauth/token"))
        .respond_with(TokenEndpoint {
            key,
            issuer: server.uri(),
            seen: seen.clone(),
        })
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/revoke"))
        .and(body_string_contains("token=r1"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let p = provider(&server, data.path());
    let login = p.login().expect("login");
    let browser = seen.clone();
    let done = login
        .sign_in(false, move |url| {
            // Play the browser: read the request, come back to the callback.
            let q: Vec<(String, String)> = url
                .split_once('?')
                .map(|(_, q)| q)
                .unwrap_or_default()
                .split('&')
                .filter_map(|p| p.split_once('='))
                .map(|(k, v)| (k.to_owned(), unescape(v)))
                .collect();
            let get = |k: &str| {
                q.iter()
                    .find(|(n, _)| n == k)
                    .map(|(_, v)| v.clone())
                    .unwrap_or_default()
            };
            assert_eq!(get("client_id"), "dynamic_agent_client");
            assert_eq!(get("agent_name_hint"), "quack");
            assert_eq!(get("code_challenge_method"), "S256");
            assert!(get("scope").contains("chatgpt.tokens.use.direct"));
            assert!(get("ext_agent_host_id").starts_with("urn:uuid:"));
            {
                let mut s = browser.lock().expect("lock");
                s.nonce = get("nonce");
                s.challenge = get("code_challenge");
            }
            let back = format!(
                "{}?code=c1&state={}&client_id=oaiapp_1",
                get("redirect_uri"),
                get("state")
            );
            tokio::spawn(async move {
                let _ = reqwest::get(back).await;
            });
        })
        .await
        .expect("signed in");
    assert_eq!(done.email, "a@example.com");
    assert!(done.plan);
    let c = saved(data.path());
    assert_eq!(
        (c.client_id.as_str(), c.subject.as_str()),
        ("oaiapp_1", "user-1")
    );
    assert!(c.signed_in());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(data.path().join("auth").join("chatgpt.json"))
            .expect("meta")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    assert_eq!(login.sign_out().await, Ok(true));
    let c = saved(data.path());
    assert!(!c.signed_in());
    assert_eq!(c.client_id, "oaiapp_1");
}

fn form_escape(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                char::from(b).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
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
    String::from_utf8(out).expect("utf8")
}

fn b64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | u32::from(*b) << (16 - 8 * i));
        for i in 0..=chunk.len() {
            out.push(char::from(A[(n >> (18 - 6 * i)) as usize & 63]));
        }
    }
    out
}

fn jwt(key: &aws_lc_rs::rsa::KeyPair, claims: &Value) -> String {
    let head = b64(json!({"alg": "RS256", "kid": "k1"}).to_string().as_bytes());
    let body = b64(claims.to_string().as_bytes());
    let signed = format!("{head}.{body}");
    let mut sig = vec![0; key.public_modulus_len()];
    key.sign(
        &aws_lc_rs::signature::RSA_PKCS1_SHA256,
        &aws_lc_rs::rand::SystemRandom::new(),
        signed.as_bytes(),
        &mut sig,
    )
    .expect("sign");
    format!("{signed}.{}", b64(&sig))
}

/// `n` and `e` from the key's PKCS#1 public key: SEQUENCE { INTEGER n, INTEGER e }.
fn rsa_public(key: &aws_lc_rs::rsa::KeyPair) -> (Vec<u8>, Vec<u8>) {
    use aws_lc_rs::signature::KeyPair;
    let der = key.public_key().as_ref().to_vec();
    fn len(der: &[u8], at: &mut usize) -> usize {
        let first = der[*at];
        *at += 1;
        if first < 0x80 {
            return usize::from(first);
        }
        let mut n = 0;
        for _ in 0..(first & 0x7f) {
            n = (n << 8) | usize::from(der[*at]);
            *at += 1;
        }
        n
    }
    let mut at = 1;
    len(&der, &mut at);
    let mut int = || {
        assert_eq!(der[at], 0x02);
        at += 1;
        let n = len(&der, &mut at);
        let v = der[at..at + n].to_vec();
        at += n;
        let skip = v.iter().take_while(|b| **b == 0).count();
        v[skip..].to_vec()
    };
    let n = int();
    let e = int();
    (n, e)
}
