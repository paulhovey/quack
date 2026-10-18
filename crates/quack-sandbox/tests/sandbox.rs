//! A sandboxed shell can write in the workspace and temp, not elsewhere, and can't reach
//! the network. Runs on Linux and macOS (CI covers both).
#![cfg(any(target_os = "linux", target_os = "macos"))]
#![allow(clippy::print_stderr)]

use std::path::Path;
use std::process::Stdio;

use quack_sandbox::{Policy, available, command};

async fn sh(policy: &Policy, script: &str, cwd: &Path) -> (bool, String) {
    let mut cmd = command(policy, "/bin/sh".as_ref(), ["-c", script]).expect("command");
    let out = cmd
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .expect("run");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), text)
}

#[tokio::test]
async fn writes_stay_inside_and_network_is_blocked() {
    if let Err(why) = available() {
        // Old kernels and containers without Landlock: nothing to test, but say so.
        eprintln!("sandbox unavailable here: {why}");
        if cfg!(target_os = "macos") {
            panic!("macOS always has Seatbelt");
        }
        return;
    }
    let base = tempfile::tempdir_in(std::env::var_os("HOME").map_or_else(
        std::env::temp_dir,
        std::path::PathBuf::from,
    ))
    .expect("tempdir");
    let ws = base.path().join("ws");
    let outside = base.path().join("outside");
    std::fs::create_dir_all(&ws).expect("mkdir");
    std::fs::create_dir_all(&outside).expect("mkdir");
    let mut policy = Policy {
        writable: vec![ws.clone()],
        network: false,
    };
    policy.writable.extend(Policy::temp_dirs());

    let (ok, out) = sh(&policy, "echo hi > a.txt && mkdir -p d/e && echo x > d/e/f && rm a.txt && echo done > /dev/null", &ws).await;
    assert!(ok, "{out}");
    assert!(ws.join("d/e/f").exists());

    let script = format!("echo no > '{}'", outside.join("x.txt").display());
    let (ok, out) = sh(&policy, &script, &ws).await;
    assert!(!ok, "{out}");
    assert!(!outside.join("x.txt").exists());
    assert!(quack_sandbox::looks_blocked(&out), "{out}");

    // Reading outside still works.
    std::fs::write(outside.join("r.txt"), "readable").expect("write");
    let (ok, out) = sh(&policy, &format!("cat '{}'", outside.join("r.txt").display()), &ws).await;
    assert!(ok && out.contains("readable"), "{out}");

    // Temp is writable.
    let tmp = tempfile::tempdir().expect("tempdir");
    let (ok, out) = sh(&policy, &format!("echo t > '{}'", tmp.path().join("t").display()), &ws).await;
    assert!(ok, "{out}");

    // A local TCP listener can't be reached without the network, and can with it.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    tokio::spawn(async move {
        loop {
            let _ = listener.accept().await;
        }
    });
    let connect = format!(
        "if command -v nc >/dev/null; then nc -z -w 3 127.0.0.1 {port}; else bash -c 'echo > /dev/tcp/127.0.0.1/{port}'; fi"
    );
    let (ok, out) = sh(&policy, &connect, &ws).await;
    assert!(!ok, "connected without network: {out}");
    policy.network = true;
    let (ok, out) = sh(&policy, &connect, &ws).await;
    assert!(ok, "could not connect with network allowed: {out}");
}

#[tokio::test]
async fn read_only_blocks_every_write() {
    if available().is_err() {
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let policy = Policy::default();
    let (ok, out) = sh(&policy, "echo x > a.txt", dir.path()).await;
    assert!(!ok, "{out}");
    let (ok, out) = sh(&policy, "echo fine > /dev/null && echo yes", dir.path()).await;
    assert!(ok && out.contains("yes"), "{out}");
}

/// What common tools need besides the workspace (`cargo test -p quack-sandbox -- --ignored`).
#[tokio::test]
#[ignore = "manual: runs cargo inside the sandbox"]
async fn cargo_builds_in_the_sandbox() {
    let home = std::path::PathBuf::from(std::env::var_os("HOME").expect("HOME"));
    let base = tempfile::tempdir_in(&home).expect("tempdir");
    let ws = base.path().join("p");
    std::fs::create_dir_all(ws.join("src")).expect("mkdir");
    std::fs::write(ws.join("Cargo.toml"), "[package]\nname = \"p\"\nversion = \"0.1.0\"\nedition = \"2021\"\n").expect("write");
    std::fs::write(ws.join("src/main.rs"), "fn main() { println!(\"built\"); }\n").expect("write");
    let mut policy = Policy { writable: vec![ws.clone()], network: false };
    policy.writable.extend(Policy::temp_dirs());
    let (ok, out) = sh(&policy, "cargo run --offline 2>&1", &ws).await;
    eprintln!("workspace + temp only: ok={ok}\n{out}");
    policy.writable.push(home.join(".cargo"));
    let (ok, out) = sh(&policy, "cargo run --offline 2>&1", &ws).await;
    eprintln!("with ~/.cargo: ok={ok}\n{out}");
}
