//! Run one program directly (no shell): input on stdin, stdout and stderr kept apart, a
//! timeout and cancellation. Used for hooks, which answer on stdout.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

use crate::Tree;

/// Output kept per stream; the rest is dropped.
const MAX_CAPTURE: usize = 1024 * 1024;

#[derive(Debug)]
pub struct ExecOptions {
    pub cwd: PathBuf,
    /// The whole environment: nothing else is inherited.
    pub env: BTreeMap<String, OsString>,
    pub stdin: Vec<u8>,
    pub timeout: Duration,
    pub cancel: CancellationToken,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecResult {
    /// `None` when killed by a signal, timeout or cancellation.
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    pub cancelled: bool,
}

pub async fn exec(
    program: &Path,
    args: &[String],
    opts: ExecOptions,
) -> std::io::Result<ExecResult> {
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args)
        .current_dir(&opts.cwd)
        .env_clear()
        .envs(&opts.env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW

    let mut child = cmd.spawn()?;
    let tree = Tree::new(&child);
    // A program that never reads its input must not block the write.
    if let Some(mut stdin) = child.stdin.take() {
        let input = opts.stdin;
        tokio::spawn(async move {
            let _ = stdin.write_all(&input).await;
            let _ = stdin.shutdown().await;
        });
    }
    let out = child.stdout.take().map(|r| tokio::spawn(capture(r)));
    let err = child.stderr.take().map(|r| tokio::spawn(capture(r)));

    let (mut status, mut timed_out, mut cancelled) = (None, false, false);
    tokio::select! {
        s = child.wait() => status = Some(s?),
        () = tokio::time::sleep(opts.timeout) => timed_out = true,
        () = opts.cancel.cancelled() => cancelled = true,
    }
    // On exit too: nothing the program started outlives it.
    tree.kill();
    if status.is_none() {
        let _ = child.kill().await;
    }
    let collect = |h: Option<tokio::task::JoinHandle<Vec<u8>>>| async move {
        let bytes = match h {
            // Grandchildren may hold the pipe open; don't wait on them for long.
            Some(h) => tokio::time::timeout(Duration::from_millis(500), h)
                .await
                .ok()
                .and_then(Result::ok)
                .unwrap_or_default(),
            None => Vec::new(),
        };
        String::from_utf8_lossy(&bytes).into_owned()
    };
    Ok(ExecResult {
        exit_code: status.and_then(|s| s.code()),
        stdout: collect(out).await,
        stderr: collect(err).await,
        timed_out,
        cancelled,
    })
}

async fn capture(mut r: impl AsyncRead + Unpin) -> Vec<u8> {
    let mut kept = Vec::new();
    let mut buf = vec![0u8; 8192];
    loop {
        match r.read(&mut buf).await {
            Ok(0) | Err(_) => return kept,
            Ok(n) => {
                let room = MAX_CAPTURE.saturating_sub(kept.len());
                kept.extend_from_slice(&buf[..n.min(room)]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(stdin: &str, timeout_ms: u64) -> ExecOptions {
        ExecOptions {
            cwd: std::env::temp_dir(),
            env: crate::inherited_env(None),
            stdin: stdin.as_bytes().to_vec(),
            timeout: Duration::from_millis(timeout_ms),
            cancel: CancellationToken::new(),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stdin_in_streams_apart() {
        let sh = Path::new("/bin/sh");
        let args = ["-c".to_owned(), "cat; echo oops >&2; exit 2".to_owned()];
        let r = exec(sh, &args, opts("{\"a\":1}", 5000))
            .await
            .expect("runs");
        assert_eq!(r.exit_code, Some(2));
        assert_eq!(r.stdout, "{\"a\":1}");
        assert_eq!(r.stderr.trim(), "oops");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_kills_it() {
        let sh = Path::new("/bin/sh");
        let args = ["-c".to_owned(), "sleep 30 & sleep 30".to_owned()];
        let started = std::time::Instant::now();
        let r = exec(sh, &args, opts("", 300)).await.expect("runs");
        assert!(r.timed_out);
        assert_eq!(r.exit_code, None);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    /// `findstr` reads stdin and prints the lines that match; a missing file goes to stderr.
    #[cfg(windows)]
    #[tokio::test]
    async fn stdin_in_streams_apart() {
        let findstr = crate::resolve_program(
            "findstr",
            std::env::var_os("PATH").as_deref(),
            &std::env::temp_dir(),
        )
        .expect("findstr");
        let r = exec(&findstr, &["x".to_owned()], opts("x1\r\ny2\r\n", 10_000))
            .await
            .expect("runs");
        assert_eq!(r.exit_code, Some(0), "{r:?}");
        assert_eq!(r.stdout.trim(), "x1", "{r:?}");
        let args = ["x".to_owned(), "no-such-file.txt".to_owned()];
        let r = exec(&findstr, &args, opts("", 10_000)).await.expect("runs");
        assert_ne!(r.exit_code, Some(0), "{r:?}");
        assert!(r.stdout.is_empty() && !r.stderr.is_empty(), "{r:?}");
    }
}
