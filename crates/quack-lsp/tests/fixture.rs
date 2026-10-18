//! The client against the fixture server: lazy start, diagnostics deltas after edits,
//! and the `lsp` tool's queries.
#![allow(clippy::print_stderr)]

use std::path::Path;
use std::sync::Arc;

use quack_config::LspServerFile;
use quack_lsp::ops::{self, Operation, Query};
use quack_lsp::{LspManager, ServerSpec};

fn manager(root: &Path) -> Arc<LspManager> {
    let file: LspServerFile = toml::from_str(&format!(
        "command = {:?}\nextensions = [\"fx\"]\n[diagnostics]\nsettle_ms = 150\nmax_wait_ms = 3000\n",
        env!("CARGO_BIN_EXE_lsp-fixture-server")
    ))
    .expect("parse");
    let spec = ServerSpec::resolve("fixture".into(), file, root);
    assert!(spec.installed(), "{:?}", spec.program);
    LspManager::new(vec![spec], root.to_path_buf(), None)
}

fn display(root: &Path) -> impl Fn(&Path) -> String + '_ {
    move |p: &Path| {
        p.strip_prefix(root)
            .unwrap_or(p)
            .to_string_lossy()
            .replace('\\', "/")
    }
}

#[tokio::test]
async fn diagnostics_after_edits_and_queries() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().canonicalize().expect("canonical");
    let a = root.join("a.fx");
    std::fs::write(&a, "fn add() {}\nfn main() { add }\n// WARN old\n").expect("write");
    let mgr = manager(&root);
    let show = display(&root);
    assert!(mgr.covers(&a) && !mgr.covers(&root.join("a.rs")));
    assert_eq!(mgr.status()[0].state, "idle");

    // An edit that adds an error here and breaks another file.
    let before = mgr.before_edit(&a).await.expect("covered");
    std::fs::write(
        &a,
        "fn add() {}\nfn main() { add }\n// WARN old\nlet x = ERROR; // BREAK_OTHER\n",
    )
    .expect("write");
    let report = mgr.after_edit(&a, &before, &show).await.expect("report");
    // The file was not open before, so its old warning counts as new this once.
    assert!(report.contains("a.fx:4:9 error[F1]: found ERROR"), "{report}");
    assert!(report.contains("other.fx:1:1 error: other is broken"), "{report}");
    assert_eq!(mgr.status()[0].state, "ready");
    assert_eq!(mgr.status()[0].errors, 2);

    // Fixing it reports the fix, not the pre-existing warning.
    let before = mgr.before_edit(&a).await.expect("covered");
    std::fs::write(&a, "fn add() {}\nfn main() { add }\n// WARN old\n").expect("write");
    let report = mgr.after_edit(&a, &before, &show).await.expect("report");
    assert_eq!(report, "diagnostics: 1 error fixed, none new");

    let q = |line, symbol: &str| Query {
        path: Some(a.clone()),
        line,
        column: None,
        symbol: Some(symbol.to_owned()).filter(|s| !s.is_empty()),
    };
    let def = ops::run(&mgr, Operation::Definition, q(Some(2), "add"), &show)
        .await
        .expect("definition");
    assert_eq!(def, "1 location:\na.fx\n  1:4: fn add() {}");
    let refs = ops::run(&mgr, Operation::References, q(Some(1), "add"), &show)
        .await
        .expect("references");
    assert!(refs.starts_with("2 locations:"), "{refs}");
    let hover = ops::run(&mgr, Operation::Hover, q(Some(2), "main"), &show)
        .await
        .expect("hover");
    assert_eq!(hover, "hover: main");
    let outline = ops::run(&mgr, Operation::Symbols, q(None, ""), &show)
        .await
        .expect("outline");
    assert_eq!(outline, "a.fx:\n  function add  (line 1)\n  function main  (line 2)");
    let found = ops::run(
        &mgr,
        Operation::Symbols,
        Query {
            symbol: Some("ma".into()),
            ..Query::default()
        },
        &show,
    )
    .await
    .expect("workspace symbols");
    assert_eq!(found, "main (function) a.fx:2");
    let diags = ops::run(&mgr, Operation::Diagnostics, Query::default(), &show)
        .await
        .expect("diagnostics");
    assert_eq!(diags, "0 errors, 1 warning:\na.fx:3:4 warning: found WARN");
    let err = ops::run(&mgr, Operation::Hover, q(Some(1), "nope"), &show)
        .await
        .expect_err("missing symbol");
    assert!(err.contains("`nope` is not on line 1"), "{err}");

    mgr.shutdown().await;
    assert_eq!(mgr.status()[0].state, "idle");
}

#[tokio::test]
async fn missing_program_is_reported_not_started() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file: LspServerFile =
        toml::from_str("command = \"quack-no-such-server\"\nextensions = [\"fx\"]\n")
            .expect("parse");
    let spec = ServerSpec::resolve("ghost".into(), file, dir.path());
    let mgr = LspManager::new(vec![spec], dir.path().to_path_buf(), None);
    let a = dir.path().join("a.fx");
    assert!(!mgr.covers(&a));
    assert!(mgr.before_edit(&a).await.is_none());
    let s = &mgr.status()[0];
    assert_eq!(s.state, "not installed");
    assert!(s.detail.contains("not on PATH"), "{}", s.detail);
}

/// Against a real rust-analyzer (`cargo test -p quack-lsp -- --ignored`).
#[tokio::test]
#[ignore = "needs rust-analyzer and cargo on PATH"]
async fn real_rust_analyzer() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().canonicalize().expect("canonical");
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .expect("write");
    std::fs::create_dir(root.join("src")).expect("mkdir");
    let main = root.join("src/main.rs");
    std::fs::write(&main, "fn add(a: u32) -> u32 { a }\nfn main() { add(1); }\n").expect("write");
    let file: LspServerFile = toml::from_str(
        "command = \"rust-analyzer\"\nextensions = [\"rs\"]\nroot_markers = [\"Cargo.toml\"]\n[diagnostics]\nmax_wait_ms = 60000\n",
    )
    .expect("parse");
    let mgr = LspManager::new(
        vec![ServerSpec::resolve("rust-analyzer".into(), file, &root)],
        root.clone(),
        None,
    );
    let show = display(&root);
    mgr.touch(&main);
    let before = mgr.before_edit(&main).await.expect("covered");
    std::fs::write(
        &main,
        "fn add(a: u32) -> u32 { a }\nfn main() { let s: String = add(1); }\n",
    )
    .expect("write");
    let started = std::time::Instant::now();
    let report = mgr.after_edit(&main, &before, &show).await.expect("report");
    eprintln!("after {:?}:\n{report}", started.elapsed());
    assert!(report.contains("src/main.rs:2:"), "{report}");
    assert!(report.contains("mismatched types"), "{report}");
    let def = ops::run(
        &mgr,
        Operation::Definition,
        Query {
            path: Some(main.clone()),
            line: Some(2),
            column: None,
            symbol: Some("add".into()),
        },
        &show,
    )
    .await
    .expect("definition");
    eprintln!("{def}");
    assert!(def.contains("1:4: fn add"), "{def}");
    let hover = ops::run(
        &mgr,
        Operation::Hover,
        Query {
            path: Some(main.clone()),
            line: Some(1),
            column: None,
            symbol: Some("add".into()),
        },
        &show,
    )
    .await
    .expect("hover");
    eprintln!("{hover}");
    assert!(hover.contains("fn add(a: u32) -> u32"), "{hover}");
    mgr.shutdown().await;
}

/// Against TypeScript 7's language server (`cargo test -p quack-lsp -- --ignored`).
#[tokio::test]
#[ignore = "needs TypeScript 7 (`tsc --lsp`) on PATH"]
async fn real_typescript() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().canonicalize().expect("canonical");
    std::fs::write(root.join("tsconfig.json"), "{\"compilerOptions\":{\"strict\":true}}")
        .expect("write");
    let a = root.join("a.ts");
    let b = root.join("b.ts");
    std::fs::write(&a, "export function add(a: number): number { return a; }\n").expect("write");
    std::fs::write(&b, "import { add } from \"./a\";\nconst n: number = add(1);\n").expect("write");
    let text = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../defaults/lsp.d/typescript.toml"),
    )
    .expect("default file");
    let file: LspServerFile = toml::from_str(&text).expect("parse");
    let mgr = LspManager::new(
        vec![ServerSpec::resolve("typescript".into(), file, &root)],
        root.clone(),
        None,
    );
    let show = display(&root);
    mgr.touch(&b);
    let before = mgr.before_edit(&a).await.expect("covered");
    std::fs::write(&a, "export function add(a: number): number { return \"x\"; }\n")
        .expect("write");
    let started = std::time::Instant::now();
    let report = mgr.after_edit(&a, &before, &show).await.expect("report");
    eprintln!("after {:?}:\n{report}", started.elapsed());
    assert!(report.contains("a.ts:1:"), "{report}");
    assert!(report.contains("not assignable"), "{report}");
    let def = ops::run(
        &mgr,
        Operation::Definition,
        Query {
            path: Some(b.clone()),
            line: Some(2),
            column: None,
            symbol: Some("add".into()),
        },
        &show,
    )
    .await
    .expect("definition");
    eprintln!("{def}");
    assert!(def.contains("a.ts"), "{def}");
    let refs = ops::run(
        &mgr,
        Operation::References,
        Query {
            path: Some(a.clone()),
            line: Some(1),
            column: None,
            symbol: Some("add".into()),
        },
        &show,
    )
    .await
    .expect("references");
    eprintln!("{refs}");
    mgr.shutdown().await;
}
