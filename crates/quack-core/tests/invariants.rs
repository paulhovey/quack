//! M1 invariants (docs/plan/13-testing-and-evals.md#invariant-tests-must-never-regress):
//! removal of default files, no egress, and resume fidelity.

use std::collections::HashMap;
use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use quack_config::{LoadOptions, Loaded, load_with};
use quack_core::protocol::{
    ApprovalDecision, Event, InputItem, LoadState, Op, Subsystem, TurnOverrides,
};
use quack_core::{Engine, EngineHandle, Resume};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn sse(chunks: &[&str]) -> ResponseTemplate {
    let body: String = chunks
        .iter()
        .map(|c| format!("data: {c}\n\n"))
        .collect::<String>()
        + "data: [DONE]\n\n";
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(body)
}

async fn answering_server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(sse(&[
            r#"{"choices":[{"delta":{"content":"Hi."},"finish_reason":"stop"}]}"#,
        ]))
        .mount(&server)
        .await;
    server
}

fn defaults_bundle() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../defaults")
}

/// An isolated configuration with a user-layer `mock` provider, an optional defaults
/// bundle and extra `config.toml` text.
fn configure(root: &Path, base_url: &str, defaults: Option<&Path>, config: &str) -> Loaded {
    let user = root.join("user");
    std::fs::create_dir_all(user.join("providers.d")).expect("mkdir");
    std::fs::write(
        user.join("providers.d").join("mock.toml"),
        format!("base_url = \"{base_url}/v1\"\nmodels = [\"tiny\"]\n"),
    )
    .expect("provider");
    std::fs::write(user.join("config.toml"), config).expect("config");
    let mut vars: HashMap<String, OsString> = HashMap::new();
    vars.insert("QUACK_CONFIG_DIR".into(), user.into_os_string());
    for (k, sub) in [
        ("QUACK_SYSTEM_DIR", "system"),
        ("QUACK_DATA_DIR", "data"),
        ("QUACK_CACHE_DIR", "cache"),
    ] {
        vars.insert(k.into(), root.join(sub).into_os_string());
    }
    vars.insert(
        "QUACK_DEFAULTS_DIR".into(),
        defaults.map_or_else(|| "none".into(), |d| d.as_os_str().to_owned()),
    );
    let ws = root.join("ws");
    std::fs::create_dir_all(&ws).expect("mkdir");
    load_with(&move |k| vars.get(k).cloned(), &LoadOptions::new(ws)).expect("load")
}

async fn next(rx: &mut tokio::sync::broadcast::Receiver<Event>) -> Event {
    tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("event within 10s")
        .expect("channel open")
}

/// Final state of every subsystem, plus warnings and errors, up to `StartupComplete`.
#[derive(Debug, Default)]
struct Startup {
    states: HashMap<Subsystem, LoadState>,
    warnings: Vec<String>,
    errors: Vec<String>,
}

async fn startup(engine: &mut EngineHandle) -> Startup {
    let mut s = Startup::default();
    loop {
        match next(&mut engine.events).await {
            Event::StartupProgress { subsystem, state } => {
                s.states.insert(subsystem, state);
            }
            Event::Warning { message } => s.warnings.push(message),
            Event::Error { error } => s.errors.push(error.message),
            Event::StartupComplete => return s,
            _ => {}
        }
    }
}

/// Run one turn, approving whatever is asked.
async fn turn(engine: &mut EngineHandle, text: &str) {
    engine
        .submit(Op::UserTurn {
            items: vec![InputItem::text(text)],
            overrides: TurnOverrides::default(),
        })
        .await;
    loop {
        match next(&mut engine.events).await {
            Event::ApprovalRequest { call_id, .. } => {
                engine
                    .submit(Op::ApprovalResponse {
                        call_id,
                        decision: ApprovalDecision::AllowOnce,
                    })
                    .await;
            }
            Event::TurnComplete { .. } => return,
            Event::TurnAborted { reason, .. } => panic!("aborted: {reason}"),
            Event::Error { error } => panic!("engine error: {}", error.message),
            _ => {}
        }
    }
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("mkdir");
    for entry in std::fs::read_dir(from).expect("read_dir").flatten() {
        let target = to.join(entry.file_name());
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).expect("copy");
        }
    }
}

fn toml_files(dir: &Path) -> usize {
    std::fs::read_dir(dir).map_or(0, |d| {
        d.flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "toml"))
            .count()
    })
}

/// Everything in the defaults bundle a user or company might delete, relative to it.
fn removable(defaults: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(defaults).expect("defaults").flatten() {
        let name = PathBuf::from(entry.file_name());
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            out.extend(
                std::fs::read_dir(entry.path())
                    .expect("read_dir")
                    .flatten()
                    .map(|e| name.join(e.file_name())),
            );
        }
        out.push(name);
    }
    out.sort();
    out
}

#[tokio::test]
async fn removing_any_default_file_leaves_startup_clean() {
    let server = answering_server().await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let bundle = defaults_bundle();

    let run = |defaults: PathBuf, root: PathBuf| {
        let uri = server.uri();
        async move {
            let loaded = configure(&root, &uri, Some(&defaults), "");
            assert!(loaded.diagnostics.is_empty(), "{:?}", loaded.diagnostics);
            let mut engine = Engine::spawn(Arc::new(loaded));
            let s = startup(&mut engine).await;
            engine.shutdown().await;
            s
        }
    };
    let providers = |s: &Startup| match s.states.get(&Subsystem::Config) {
        Some(LoadState::Ready { detail }) => detail
            .split(" · ")
            .find_map(|p| p.strip_suffix(" providers")?.parse::<usize>().ok())
            .expect("provider count"),
        other => panic!("config: {other:?}"),
    };

    let baseline = run(bundle.clone(), tmp.path().join("baseline")).await;
    assert_eq!(
        providers(&baseline),
        toml_files(&bundle.join("providers.d")) + 1
    );

    let items = removable(&bundle);
    assert!(items.len() > 10, "{items:?}");
    for (n, item) in items.iter().enumerate() {
        let defaults = tmp.path().join(format!("defaults-{n}"));
        copy_dir(&bundle, &defaults);
        let gone = defaults.join(item);
        if gone.is_dir() {
            std::fs::remove_dir_all(&gone).expect("remove");
        } else {
            std::fs::remove_file(&gone).expect("remove");
        }
        let s = run(defaults.clone(), tmp.path().join(format!("run-{n}"))).await;
        let shown = item.display();

        assert!(s.errors.is_empty(), "{shown}: {:?}", s.errors);
        assert_eq!(s.warnings, baseline.warnings, "{shown}");
        for (subsystem, state) in &s.states {
            assert!(
                !matches!(state, LoadState::Failed { .. }),
                "{shown}: {subsystem:?} {state:?}"
            );
        }
        assert!(
            matches!(s.states.get(&Subsystem::Provider), Some(LoadState::Ready { detail }) if detail.starts_with("mock/tiny")),
            "{shown}: {:?}",
            s.states.get(&Subsystem::Provider)
        );
        // Only the removed item is missing: every remaining provider file loads.
        assert_eq!(
            providers(&s),
            toml_files(&defaults.join("providers.d")) + 1,
            "{shown}"
        );
    }
}

/// A forward proxy that records every destination and only passes requests to `allowed`.
struct RecordingProxy {
    url: String,
    seen: Arc<Mutex<Vec<String>>>,
}

impl RecordingProxy {
    async fn start(allowed: SocketAddr) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let url = format!("http://{}", listener.local_addr().expect("addr"));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            while let Ok((client, _)) = listener.accept().await {
                let log = log.clone();
                tokio::spawn(async move {
                    let _ = forward(client, allowed, &log).await;
                });
            }
        });
        Self { url, seen }
    }

    fn destinations(&self) -> Vec<String> {
        let mut d = self.seen.lock().map(|s| s.clone()).unwrap_or_default();
        d.sort();
        d.dedup();
        d
    }
}

/// Handle one proxied request. Plain-HTTP requests to `allowed` are passed on with
/// `connection: close`; anything else (including every `CONNECT`) is refused.
async fn forward(
    mut client: TcpStream,
    allowed: SocketAddr,
    log: &Mutex<Vec<String>>,
) -> std::io::Result<()> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    let head_end = loop {
        let n = client.read(&mut chunk).await?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut request = lines.next().unwrap_or_default().splitn(3, ' ');
    let (verb, target, version) = (
        request.next().unwrap_or_default(),
        request.next().unwrap_or_default(),
        request.next().unwrap_or_default(),
    );
    let (authority, origin_form) = match target.strip_prefix("http://") {
        Some(rest) => match rest.split_once('/') {
            Some((a, p)) => (a.to_owned(), format!("/{p}")),
            None => (rest.to_owned(), "/".to_owned()),
        },
        None => (target.to_owned(), String::new()),
    };
    if let Ok(mut l) = log.lock() {
        l.push(format!("{verb} {authority}"));
    }
    if verb == "CONNECT" || authority != allowed.to_string() {
        client
            .write_all(b"HTTP/1.1 403 Forbidden\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
            .await?;
        return Ok(());
    }

    let headers: Vec<&str> = lines
        .filter(|l| {
            let l = l.to_ascii_lowercase();
            !l.is_empty() && !l.starts_with("connection:") && !l.starts_with("proxy-connection:")
        })
        .collect();
    let length = headers
        .iter()
        .find_map(|h| {
            let (k, v) = h.split_once(':')?;
            k.eq_ignore_ascii_case("content-length")
                .then(|| v.trim().parse::<usize>().ok())?
        })
        .unwrap_or(0);
    while buf.len() < head_end + length {
        let n = client.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let mut upstream = TcpStream::connect(allowed).await?;
    let mut out = format!("{verb} {origin_form} {version}\r\n");
    for h in headers {
        out.push_str(h);
        out.push_str("\r\n");
    }
    out.push_str("connection: close\r\n\r\n");
    upstream.write_all(out.as_bytes()).await?;
    upstream.write_all(&buf[head_end..]).await?;
    tokio::io::copy(&mut upstream, &mut client).await?;
    client.shutdown().await
}

/// Startup plus three turns with the defaults bundle installed reaches nothing but the
/// configured provider, with `offline` on and off. All provider traffic goes through a
/// recording proxy, so any other destination would show up.
#[tokio::test]
async fn no_egress_beyond_the_configured_provider() {
    let server = answering_server().await;
    let mock = *server.address();
    for offline in [false, true] {
        let proxy = RecordingProxy::start(mock).await;
        let tmp = tempfile::tempdir().expect("tempdir");
        let loaded = configure(
            tmp.path(),
            &server.uri(),
            Some(&defaults_bundle()),
            &format!(
                "[network]\noffline = {offline}\nproxy = \"{}\"\n",
                proxy.url
            ),
        );
        let mut engine = Engine::spawn(Arc::new(loaded));
        let s = startup(&mut engine).await;
        assert!(
            matches!(
                s.states.get(&Subsystem::Provider),
                Some(LoadState::Ready { .. })
            ),
            "{s:?}"
        );
        for text in ["one", "two", "three"] {
            turn(&mut engine, text).await;
        }
        engine.shutdown().await;

        let allowed = format!("POST {mock}");
        let destinations = proxy.destinations();
        assert!(
            destinations.contains(&allowed),
            "turns went through the proxy: {destinations:?}"
        );
        assert!(
            destinations.iter().all(|d| d.ends_with(&mock.to_string())),
            "offline = {offline}: unexpected destinations {destinations:?}"
        );
    }
}

/// A system policy's `hosts` disables every provider elsewhere, including the bundle's and
/// the user's own, so nothing else can be contacted or picked.
#[test]
fn policy_hosts_leave_only_allowed_servers() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let system = tmp.path().join("system");
    std::fs::create_dir_all(&system).expect("mkdir");
    std::fs::write(
        system.join("config.toml"),
        "[policy.providers]\nhosts = [\"127.0.0.1\"]\n",
    )
    .expect("policy");
    let loaded = configure(
        tmp.path(),
        "https://elsewhere.example",
        Some(&defaults_bundle()),
        "",
    );
    let items = quack_config::load_providers(&loaded);
    assert!(items.len() > 10, "the bundle loaded");
    for item in &items {
        match item.loaded() {
            Some(f) => assert!(
                quack_config::url_host(&f.base_url) == "127.0.0.1",
                "{} at {} is allowed",
                item.id,
                f.base_url
            ),
            None => assert!(
                matches!(&item.state, quack_config::ItemState::Disabled(why) if why.contains("policy")),
                "{}: {:?}",
                item.id,
                item.state
            ),
        }
    }
    assert!(items.iter().any(|i| i.id == "mock" && i.loaded().is_none()));
}

/// The request after a resume is byte-for-byte the request the live session would have
/// sent: same system prompt, tools, history (tool calls, results, reasoning) and params.
#[tokio::test]
async fn resume_reproduces_the_request_exactly() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_string_contains("Created notes.txt"))
        .respond_with(sse(&[
            r#"{"choices":[{"delta":{"reasoning_content":"The file exists now."}}]}"#,
            r#"{"choices":[{"delta":{"content":"Done."},"finish_reason":"stop"}]}"#,
            r#"{"choices":[],"usage":{"prompt_tokens":300,"completion_tokens":4}}"#,
        ]))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(sse(&[
            r#"{"choices":[{"delta":{"content":"Writing it."}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"write","arguments":"{\"path\":\"notes.txt\",\"content\":\"hi\\n\"}"}}]},"finish_reason":"tool_calls"}]}"#,
        ]))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    let spawn = |resume| {
        // The defaults bundle supplies the shipped `generic` profile and its system prompt.
        let loaded = configure(root, &server.uri(), Some(&defaults_bundle()), "");
        Engine::spawn_with(Arc::new(loaded), resume)
    };

    let mut live = spawn(Resume::New);
    turn(&mut live, "write notes").await;
    turn(&mut live, "second question").await;
    // Each turn ends with an fsync, so this is the state a crash or quit would leave.
    copy_dir(&root.join("data"), &root.join("saved"));
    turn(&mut live, "third question").await;
    live.shutdown().await;

    std::fs::remove_dir_all(root.join("data")).expect("remove");
    copy_dir(&root.join("saved"), &root.join("data"));
    std::fs::remove_file(root.join("ws").join("notes.txt")).expect("reset workspace");
    let mut resumed = spawn(Resume::Latest);
    turn(&mut resumed, "third question").await;
    resumed.shutdown().await;

    let bodies: Vec<serde_json::Value> = server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|r| String::from_utf8_lossy(&r.body).contains("third question"))
        .map(|r| serde_json::from_slice(&r.body).expect("json body"))
        .collect();
    assert_eq!(bodies.len(), 2, "{bodies:#?}");
    assert_eq!(bodies[0], bodies[1]);
    let messages = bodies[0]["messages"].as_array().expect("messages");
    assert_eq!(messages.first().map(|m| &m["role"]), Some(&"system".into()));
    assert!(
        messages.iter().any(|m| m["role"] == "tool"),
        "{messages:#?}"
    );
    assert!(bodies[0]["tools"].as_array().is_some_and(|t| !t.is_empty()));
}
