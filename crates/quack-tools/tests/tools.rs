//! The built-in tools against a temporary workspace.

use std::sync::Arc;
use std::time::Duration;

use quack_shell::ShellKind;
use quack_tools::{ReadTracker, Registry, ToolCtx};
use serde_json::json;
use tokio_util::sync::CancellationToken;

fn ctx(root: &std::path::Path) -> ToolCtx {
    ToolCtx {
        root: root.to_path_buf(),
        shell: ShellKind::detect("auto", None),
        default_timeout: Duration::from_secs(10),
        max_timeout: Duration::from_secs(30),
        cancel: CancellationToken::new(),
        on_output: None,
        reads: Arc::new(ReadTracker::default()),
        memory: None,
        skills: Default::default(),
        lsp: None,
        sandbox: Default::default(),
    }
}

async fn call(
    reg: &Registry,
    ctx: &ToolCtx,
    name: &str,
    args: serde_json::Value,
) -> quack_tools::ToolOutput {
    reg.get(name).expect("tool exists").run(args, ctx).await
}

#[tokio::test]
async fn write_read_edit_search() {
    let dir = tempfile::tempdir().expect("tempdir");
    let reg = Registry::builtin();
    let c = ctx(dir.path());

    let w = call(
        &reg,
        &c,
        "write",
        json!({"path": "src/lib.rs", "content": "fn a() {}\nfn b() {}\n"}),
    )
    .await;
    assert!(w.ok, "{w:?}");
    assert_eq!(w.summary, "+2 -0");

    let r = call(
        &reg,
        &c,
        "read",
        json!({"file_path": "src/lib.rs", "offset": "2"}),
    )
    .await;
    assert!(r.model.contains("   2\tfn b() {}"), "{}", r.model);

    let e = call(
        &reg,
        &c,
        "edit",
        json!({"path": "src/lib.rs", "old_string": "fn b() {}", "new_string": "fn c() {}"}),
    )
    .await;
    assert!(e.ok, "{e:?}");
    assert!(
        std::fs::read_to_string(dir.path().join("src/lib.rs"))
            .expect("read")
            .contains("fn c()")
    );

    // Changed behind the model's back: the next edit is refused until it reads again.
    std::thread::sleep(Duration::from_millis(20));
    std::fs::write(
        dir.path().join("src/lib.rs"),
        "fn a() {}\nfn c() {}\n// external\n",
    )
    .expect("write");
    let stale = call(
        &reg,
        &c,
        "edit",
        json!({"path": "src/lib.rs", "old_string": "fn a", "new_string": "fn z"}),
    )
    .await;
    assert!(
        !stale.ok && stale.model.contains("changed on disk"),
        "{stale:?}"
    );

    let g = call(&reg, &c, "glob", json!({"pattern": "*.rs"})).await;
    assert_eq!(g.model, "src/lib.rs");
    let gr = call(&reg, &c, "grep", json!({"pattern": "fn [ac]"})).await;
    assert!(gr.model.contains("src/lib.rs:1: fn a() {}"), "{}", gr.model);
    let l = call(&reg, &c, "list_dir", json!({})).await;
    assert!(l.model.contains("  src/\n    lib.rs"), "{}", l.model);
}

#[tokio::test]
async fn shell_reports_exit_code() {
    let dir = tempfile::tempdir().expect("tempdir");
    let reg = Registry::builtin();
    let c = ctx(dir.path());
    let out = call(&reg, &c, "shell", json!({"command": "echo hello"})).await;
    assert!(out.ok, "{out:?}");
    assert!(out.model.starts_with("exit code 0\nhello"), "{}", out.model);
}
