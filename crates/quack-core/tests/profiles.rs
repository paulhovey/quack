//! The shipped profiles: each loads without warnings, names its source, offers its own tool
//! names, and its calls reach the internal tools.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use quack_config::{LoadOptions, Loaded, load_with};
use quack_core::protocol::{Event, InputItem, Op, TurnOverrides};
use quack_core::{Engine, EngineHandle};
use serde_json::{Value, json};
use wiremock::matchers::{body_string_contains, method};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn defaults_bundle() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../defaults")
}

fn sse(chunk: Value) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(format!("data: {chunk}\n\ndata: [DONE]\n\n"))
}

fn configure(root: &Path, base_url: &str, profile: &str) -> Loaded {
    let user = root.join("user");
    std::fs::create_dir_all(user.join("providers.d")).expect("mkdir");
    std::fs::write(
        user.join("providers.d").join("mock.toml"),
        format!(
            "base_url = \"{base_url}/v1\"\nmodels = [{{ id = \"tiny\", profile = \"{profile}\" }}]\n"
        ),
    )
    .expect("provider");
    std::fs::write(
        user.join("config.toml"),
        "[permissions]\nmode = \"full-auto\"\n",
    )
    .expect("config");
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
        defaults_bundle().into_os_string(),
    );
    let ws = root.join("ws");
    std::fs::create_dir_all(&ws).expect("mkdir");
    load_with(&move |k| vars.get(k).cloned(), &LoadOptions::new(ws)).expect("load")
}

async fn next(engine: &mut EngineHandle) -> Event {
    tokio::time::timeout(Duration::from_secs(10), engine.events.recv())
        .await
        .expect("event within 10s")
        .expect("channel open")
}

async fn turn(engine: &mut EngineHandle, text: &str) {
    engine
        .submit(Op::UserTurn {
            items: vec![InputItem::text(text)],
            overrides: TurnOverrides::default(),
        })
        .await;
    loop {
        match next(engine).await {
            Event::TurnComplete { .. } => return,
            Event::TurnAborted { reason, .. } => panic!("aborted: {reason}"),
            Event::Error { error } => panic!("engine error: {}", error.message),
            _ => {}
        }
    }
}

fn shipped() -> Vec<String> {
    let mut ids: Vec<String> = std::fs::read_dir(defaults_bundle().join("profiles.d"))
        .expect("profiles.d")
        .flatten()
        .filter(|e| e.path().join("profile.toml").is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    ids.sort();
    ids
}

/// The request's tools: name → parameter names.
fn tools(body: &Value) -> HashMap<String, Vec<String>> {
    body["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .map(|t| {
            let f = &t["function"];
            let params = f["parameters"]["properties"]
                .as_object()
                .map(|p| p.keys().cloned().collect())
                .unwrap_or_default();
            (f["name"].as_str().unwrap_or_default().to_owned(), params)
        })
        .collect()
}

#[tokio::test]
async fn every_shipped_profile_loads_cleanly() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(sse(
            json!({"choices":[{"delta":{"content":"ok"},"finish_reason":"stop"}]}),
        ))
        .mount(&server)
        .await;
    for profile in shipped() {
        let file: quack_config::profile::ProfileFile = toml::from_str(
            &std::fs::read_to_string(
                defaults_bundle()
                    .join("profiles.d")
                    .join(&profile)
                    .join("profile.toml"),
            )
            .expect("read"),
        )
        .expect("parse");
        assert!(!file.source.trim().is_empty(), "{profile}: no source");

        let tmp = tempfile::tempdir().expect("tempdir");
        let mut engine = Engine::spawn(Arc::new(configure(tmp.path(), &server.uri(), &profile)));
        let mut warnings = Vec::new();
        loop {
            match next(&mut engine).await {
                Event::Warning { message } => warnings.push(message),
                Event::StartupComplete => break,
                _ => {}
            }
        }
        let ours: Vec<&String> = warnings.iter().filter(|w| w.contains("profile")).collect();
        assert!(ours.is_empty(), "{profile}: {ours:?}");
        turn(&mut engine, &format!("hello {profile}")).await;
        engine.shutdown().await;

        let body: Value = server
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .filter_map(|r| serde_json::from_slice::<Value>(&r.body).ok())
            .rfind(|b| b.to_string().contains(&format!("hello {profile}")))
            .expect("request");
        let offered = tools(&body);
        // `skill` and `lsp` are offered only with skills or language servers installed.
        for t in file
            .tools
            .iter()
            .filter(|t| !matches!(t.maps_to.as_deref(), Some("skill" | "lsp")))
        {
            assert!(
                offered.contains_key(&t.name),
                "{profile}: `{}` not offered",
                t.name
            );
            for p in t.params.keys() {
                assert!(
                    offered[&t.name].contains(p),
                    "{profile}: `{}` has no parameter `{p}`: {:?}",
                    t.name,
                    offered[&t.name]
                );
            }
        }
        let system = body["messages"][0]["content"].as_str().expect("system");
        assert!(
            !system.contains("{{") && !system.contains("{%"),
            "{profile}"
        );
    }
}

#[tokio::test]
async fn codex_shell_takes_an_argument_list() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_string_contains("\"role\":\"tool\""))
        .respond_with(sse(
            json!({"choices":[{"delta":{"content":"done"},"finish_reason":"stop"}]}),
        ))
        .mount(&server)
        .await;
    let args = json!({"command": ["bash", "-lc", "echo argv-ok"]}).to_string();
    Mock::given(method("POST"))
        .respond_with(sse(json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"shell","arguments":args}}]},"finish_reason":"tool_calls"}]})))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut engine = Engine::spawn(Arc::new(configure(tmp.path(), &server.uri(), "codex")));
    turn(&mut engine, "run it").await;
    engine.shutdown().await;
    let bodies: Vec<Value> = server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter_map(|r| serde_json::from_slice::<Value>(&r.body).ok())
        .collect();
    let offered = tools(&bodies[0]);
    let mut names: Vec<&str> = offered.keys().map(String::as_str).collect();
    names.sort_unstable();
    // Exclusive: only its own tools (memory and skill are offered when available).
    assert!(
        names.iter().all(|n| [
            "apply_patch",
            "ask_user",
            "memory",
            "shell",
            "skill",
            "update_plan"
        ]
        .contains(n)),
        "{names:?}"
    );
    assert_eq!(
        bodies[0]["tools"]
            .as_array()
            .and_then(|t| t.iter().find(|t| t["function"]["name"] == "shell"))
            .map(|t| t["function"]["parameters"]["properties"]["command"]["type"].clone()),
        Some(json!("array"))
    );
    let last = bodies.last().expect("second request").to_string();
    assert!(last.contains("argv-ok"), "{last}");
}

#[tokio::test]
async fn profiles_are_listed_previewed_and_switched() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(sse(
            json!({"choices":[{"delta":{"content":"ok"},"finish_reason":"stop"}]}),
        ))
        .mount(&server)
        .await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut engine = Engine::spawn(Arc::new(configure(tmp.path(), &server.uri(), "auto")));
    loop {
        if let Event::StartupComplete = next(&mut engine).await {
            break;
        }
    }
    engine.submit(Op::ListProfiles).await;
    let (active, forced, profiles) = loop {
        if let Event::Profiles {
            active,
            forced,
            profiles,
        } = next(&mut engine).await
        {
            break (active, forced, profiles);
        }
    };
    assert_eq!((active.as_str(), forced), ("generic", false));
    assert_eq!(profiles.len(), shipped().len());
    let qwen = profiles
        .iter()
        .find(|p| p.id == "qwen-code")
        .expect("qwen-code");
    assert!(
        qwen.tools.contains(&"read_file".to_owned()),
        "{:?}",
        qwen.tools
    );
    assert!(
        qwen.system.contains("todo_write"),
        "rendered with its own tool names"
    );
    let codex = profiles.iter().find(|p| p.id == "codex").expect("codex");
    assert_eq!(codex.edit_format, "apply_patch");
    let generic = profiles
        .iter()
        .find(|p| p.id == "generic")
        .expect("generic");
    assert_eq!(generic.edit_format, "str_replace");
    assert!(generic.matches);

    engine
        .submit(Op::SetProfile {
            id: "qwen-code".into(),
        })
        .await;
    let session = loop {
        if let Event::SessionConfigured { session } = next(&mut engine).await {
            break session;
        }
    };
    assert_eq!(session.profile, "qwen-code");
    turn(&mut engine, "after the switch").await;
    // A reload keeps the chosen profile.
    engine.submit(Op::ReloadConfig).await;
    let session = loop {
        if let Event::SessionConfigured { session } = next(&mut engine).await {
            break session;
        }
    };
    assert_eq!(session.profile, "qwen-code");
    engine.shutdown().await;
    let body: Value = server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter_map(|r| serde_json::from_slice::<Value>(&r.body).ok())
        .rfind(|b| b.to_string().contains("after the switch"))
        .expect("request");
    assert!(tools(&body).contains_key("read_file"));
}
