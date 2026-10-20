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
        ask: None,
        reads: Arc::new(ReadTracker::default()),
        memory: None,
        skills: Default::default(),
        lsp: None,
        sandbox: Default::default(),
        plan: Default::default(),
        agents: Default::default(),
        spawn: None,
        edit_format: quack_config::settings::EditFormat::StrReplace,
        outline_above: 0,
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

#[cfg(feature = "lang-common")]
#[tokio::test]
async fn index_and_outlined_reads() {
    let dir = tempfile::tempdir().expect("tempdir");
    let reg = Registry::builtin();
    let mut c = ctx(dir.path());
    let mut src = String::from("pub struct Big;\n\nimpl Big {\n");
    for i in 0..200 {
        src.push_str(&format!("    pub fn f{i}(&self) -> u32 {{\n"));
        for k in 0..6 {
            src.push_str(&format!("        let v{k} = {i} * {k} + self.offset();\n"));
        }
        src.push_str(&format!("        {i}\n    }}\n\n"));
    }
    src.push_str("}\n");
    std::fs::create_dir_all(dir.path().join("src")).expect("mkdir");
    std::fs::write(dir.path().join("src").join("big.rs"), &src).expect("write");
    std::fs::write(
        dir.path().join("src").join("small.py"),
        "def g():\n    return 1\n",
    )
    .expect("write");

    let i = call(&reg, &c, "index", json!({"path": "src/big.rs"})).await;
    assert!(i.ok, "{i:?}");
    assert!(
        i.model.starts_with("src/big.rs (2004 lines, rust)"),
        "{}",
        i.model
    );
    assert!(i.model.contains("pub fn f7(&self) -> u32 {"), "{}", i.model);
    let d = call(&reg, &c, "index", json!({})).await;
    assert!(
        d.model.contains("src/small.py (2 lines): fn g"),
        "{}",
        d.model
    );
    assert!(
        d.model
            .contains("src/big.rs (2004 lines): type Big, impl Big"),
        "{}",
        d.model
    );

    // Off: the whole file. On: the outline, unless a range or symbol is asked for.
    let whole = call(&reg, &c, "read", json!({"path": "src/big.rs"})).await;
    assert!(whole.model.contains("pub fn f199"), "{}", whole.summary);
    c.outline_above = 2000;
    let o = call(&reg, &c, "read", json!({"path": "src/big.rs"})).await;
    assert!(o.model.contains("so this is its outline"), "{}", o.model);
    assert!(o.model.len() < src.len() / 2);
    let part = call(
        &reg,
        &c,
        "read",
        json!({"path": "src/big.rs", "offset": 4, "limit": 3}),
    )
    .await;
    assert!(part.model.contains("pub fn f0"), "{}", part.model);
    let sym = call(
        &reg,
        &c,
        "read",
        json!({"path": "src/big.rs", "symbol": "Big::f150"}),
    )
    .await;
    assert!(
        sym.model.contains("pub fn f150") && sym.model.contains("let v5 = 150 * 5"),
        "{}",
        sym.model
    );
    assert!(!sym.model.contains("f151"), "{}", sym.model);
    let small = call(&reg, &c, "read", json!({"path": "src/small.py"})).await;
    assert!(small.model.contains("return 1"), "{}", small.model);
    let missing = call(
        &reg,
        &c,
        "read",
        json!({"path": "src/big.rs", "symbol": "nope"}),
    )
    .await;
    assert!(
        !missing.ok && missing.model.contains("index"),
        "{}",
        missing.model
    );
}
