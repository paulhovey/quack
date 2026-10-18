//! AGENTS.md: the chain from the git root to the cwd, and nested files on demand.

mod common;

use std::sync::Arc;

use common::{bodies, sse_call, sse_text, turn};
use quack_core::Engine;
use wiremock::matchers::{body_string_contains, method};
use wiremock::{Mock, MockServer};

#[tokio::test]
async fn chain_at_start_and_nested_files_with_the_first_read() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_string_contains("PKG-RULES"))
        .respond_with(sse_text("done"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(sse_call(
            "c1",
            "read",
            serde_json::json!({"path": "pkg/lib.txt"}),
        ))
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().expect("tempdir");
    let ws = tmp.path().join("ws");
    std::fs::create_dir_all(ws.join(".git")).expect("git root");
    std::fs::create_dir_all(ws.join("app")).expect("mkdir");
    std::fs::create_dir_all(ws.join("pkg")).expect("mkdir");
    std::fs::write(ws.join("AGENTS.md"), "ROOT-RULES").expect("write");
    std::fs::write(ws.join("app/AGENTS.md"), "APP-RULES").expect("write");
    std::fs::write(ws.join("pkg/AGENTS.md"), "PKG-RULES").expect("write");
    std::fs::write(ws.join("pkg/lib.txt"), "library").expect("write");
    let loaded = common::workspace(tmp.path(), &server.uri(), r#"["tiny"]"#, "app");
    assert_eq!(loaded.dirs.workspace_root, ws);

    let mut engine = Engine::spawn(Arc::new(loaded));
    turn(&mut engine, "look at pkg").await;
    let all = bodies(&server).await;
    let first = &all[0];
    assert!(first.contains("ROOT-RULES") && first.contains("APP-RULES"));
    assert!(
        !first.contains("PKG-RULES"),
        "nested files wait until needed"
    );
    let second = &all[1];
    assert!(second.contains("[instructions for pkg/"), "{second}");
    engine.shutdown().await;
}

#[tokio::test]
async fn memory_index_in_the_prompt_and_notes_saved_by_the_tool() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_string_contains("saved project note"))
        .respond_with(sse_text("noted"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(sse_call(
            "c1",
            "memory",
            serde_json::json!({"action": "create", "name": "Uses pnpm", "content": "Use pnpm, never npm."}),
        ))
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().expect("tempdir");
    // A profile whose prompt is only the memory index.
    let profile = tmp.path().join("user/profiles.d/mem");
    std::fs::create_dir_all(&profile).expect("mkdir");
    std::fs::write(
        profile.join("profile.toml"),
        "source = \"test\"\n[match]\nmodel = [\"*\"]\n",
    )
    .expect("write");
    std::fs::write(profile.join("system.md"), "MEMORY: {{ memory_index }}").expect("write");
    let global = tmp.path().join("data/memory");
    std::fs::create_dir_all(&global).expect("mkdir");
    std::fs::write(
        global.join("MEMORY.md"),
        "- [tabs](notes/tabs.md) — prefers tabs\n",
    )
    .expect("write");
    let loaded = common::workspace(tmp.path(), &server.uri(), r#"["tiny"]"#, "");

    let mut engine = Engine::spawn(Arc::new(loaded));
    turn(&mut engine, "remember pnpm").await;
    let first = bodies(&server).await.remove(0);
    assert!(
        first.contains("MEMORY: global memory:\\n- [tabs](notes/tabs.md) — prefers tabs"),
        "{first}"
    );
    let projects = global.join("projects");
    let project = std::fs::read_dir(&projects)
        .expect("projects dir")
        .next()
        .expect("one project")
        .expect("entry")
        .path();
    let note = std::fs::read_to_string(project.join("notes/uses-pnpm.md")).expect("note");
    assert!(note.contains("Use pnpm, never npm."));
    engine.shutdown().await;
}

#[tokio::test]
async fn skills_are_listed_loaded_by_the_tool_and_run_by_name() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_string_contains("STEP-ONE"))
        .respond_with(sse_text("done"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(sse_call(
            "c1",
            "skill",
            serde_json::json!({"name": "release"}),
        ))
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().expect("tempdir");
    let profile = tmp.path().join("user/profiles.d/sk");
    std::fs::create_dir_all(&profile).expect("mkdir");
    std::fs::write(
        profile.join("profile.toml"),
        "source = \"test\"\n[match]\nmodel = [\"*\"]\n",
    )
    .expect("write");
    std::fs::write(profile.join("system.md"), "SKILLS: {{ skills }}").expect("write");
    let skill = tmp.path().join("ws/.agents/skills/release");
    std::fs::create_dir_all(&skill).expect("mkdir");
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nname: release\ndescription: Cut a release\n---\nSTEP-ONE: bump the version.\n",
    )
    .expect("write");
    let loaded = common::workspace(tmp.path(), &server.uri(), r#"["tiny"]"#, "");

    let mut engine = Engine::spawn(Arc::new(loaded));
    turn(&mut engine, "ship it").await;
    let all = bodies(&server).await;
    assert!(
        all[0].contains("SKILLS: - release: Cut a release"),
        "{}",
        all[0]
    );
    assert!(all[0].contains("\"name\":\"skill\""), "the tool is offered");
    assert!(
        all[1].contains("STEP-ONE"),
        "the tool result holds the body"
    );

    // `/release now` expands into the skill's instructions plus the request.
    turn(&mut engine, "/release now").await;
    let last = bodies(&server).await.pop().expect("request");
    assert!(last.contains("<skill name=\\\"release\\\"") && last.contains("now"));
    engine.shutdown().await;
}

/// Another agent's file is read where there is no AGENTS.md, the merge is offered, and
/// `/init` asks the model to do it.
#[tokio::test]
async fn other_agents_files_are_read_and_merge_is_offered() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(sse_text("ok"))
        .mount(&server)
        .await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let ws = tmp.path().join("ws");
    std::fs::create_dir_all(ws.join(".git")).expect("git root");
    std::fs::write(ws.join("CLAUDE.md"), "CLAUDE-RULES").expect("write");
    std::fs::create_dir_all(tmp.path().join("user")).expect("mkdir");
    std::fs::write(
        tmp.path().join("user").join("compat.toml"),
        "[instructions]\nproject = [\"CLAUDE.md\", \".cursor/rules/\"]\n",
    )
    .expect("compat");
    let loaded = common::workspace(tmp.path(), &server.uri(), r#"["tiny"]"#, "");
    let mut engine = Engine::spawn(Arc::new(loaded));
    let mut offered = false;
    loop {
        let e = tokio::time::timeout(std::time::Duration::from_secs(10), engine.events.recv())
            .await
            .expect("event")
            .expect("open");
        match e {
            quack_core::protocol::Event::Notice { message } => {
                offered = message.contains("CLAUDE.md") && message.contains("/init");
            }
            quack_core::protocol::Event::StartupComplete => break,
            _ => {}
        }
    }
    assert!(offered, "no offer to merge");
    engine
        .submit(quack_core::protocol::Op::InitInstructions)
        .await;
    common::until_done(&mut engine).await;
    let first = &bodies(&server).await[0];
    assert!(first.contains("CLAUDE-RULES"), "{first}");
    assert!(first.contains("Consolidate this project's agent instructions into AGENTS.md"));
    engine.shutdown().await;
}
