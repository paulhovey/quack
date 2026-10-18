//! Run one command with merged, ordered output, a timeout and cancellation.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::ShellKind;

/// Output kept in memory per command; the rest is counted but dropped.
const MAX_CAPTURE: usize = 4 * 1024 * 1024;

#[derive(Debug)]
pub struct RunOptions {
    pub cwd: PathBuf,
    pub timeout: Duration,
    pub cancel: CancellationToken,
    /// Receives output chunks as they arrive (for live display).
    pub on_output: Option<mpsc::UnboundedSender<String>>,
    /// Run inside the OS sandbox with this policy.
    pub sandbox: Option<Arc<quack_sandbox::Policy>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunResult {
    /// `None` when killed by a signal, timeout or cancellation.
    pub exit_code: Option<i32>,
    /// stdout and stderr interleaved in arrival order.
    pub output: String,
    pub dropped_bytes: usize,
    pub timed_out: bool,
    pub cancelled: bool,
}

pub async fn run(shell: &ShellKind, script: &str, opts: RunOptions) -> std::io::Result<RunResult> {
    let mut cmd = match &opts.sandbox {
        Some(policy) => {
            quack_sandbox::command(policy, shell.program().as_os_str(), shell.args(script))
                .map_err(std::io::Error::other)?
        }
        None => {
            let mut c = tokio::process::Command::new(shell.program());
            c.args(shell.args(script));
            c
        }
    };
    cmd.current_dir(&opts.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        // Never wait on a pager or a credential prompt.
        .env("GIT_PAGER", "cat")
        .env("PAGER", "cat")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("DEBIAN_FRONTEND", "noninteractive");
    #[cfg(unix)]
    cmd.process_group(0);
    #[cfg(windows)]
    {
        // CREATE_NO_WINDOW: no console window flashes up for GUI-subsystem children.
        cmd.creation_flags(0x0800_0000);
    }

    let mut child = cmd.spawn()?;
    let tree = Tree::new(&child);
    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
    if let Some(out) = child.stdout.take() {
        tokio::spawn(pump(out, tx.clone()));
    }
    if let Some(err) = child.stderr.take() {
        tokio::spawn(pump(err, tx.clone()));
    }
    drop(tx);

    let mut captured = Vec::new();
    let mut dropped = 0usize;
    let mut status = None;
    let mut timed_out = false;
    let mut cancelled = false;
    let deadline = tokio::time::sleep(opts.timeout);
    tokio::pin!(deadline);
    let mut output_open = true;

    loop {
        tokio::select! {
            chunk = rx.recv(), if output_open => match chunk {
                Some(bytes) => {
                    if let Some(live) = &opts.on_output {
                        let _ = live.send(String::from_utf8_lossy(&bytes).into_owned());
                    }
                    let room = MAX_CAPTURE.saturating_sub(captured.len());
                    captured.extend_from_slice(&bytes[..bytes.len().min(room)]);
                    dropped += bytes.len().saturating_sub(room);
                }
                None => output_open = false,
            },
            s = child.wait(), if status.is_none() => {
                status = Some(s?);
            }
            () = &mut deadline => {
                timed_out = true;
                break;
            }
            () = opts.cancel.cancelled() => {
                cancelled = true;
                break;
            }
        }
        // Grandchildren may hold the pipes open after the shell exits; don't wait forever on them.
        if status.is_some() && !output_open {
            break;
        }
        if status.is_some() && output_open {
            match tokio::time::timeout(Duration::from_millis(200), rx.recv()).await {
                Ok(Some(bytes)) => {
                    let room = MAX_CAPTURE.saturating_sub(captured.len());
                    captured.extend_from_slice(&bytes[..bytes.len().min(room)]);
                    dropped += bytes.len().saturating_sub(room);
                    continue;
                }
                Ok(None) | Err(_) => break,
            }
        }
    }

    // On exit too: reap anything the shell left running in the background.
    tree.kill();
    if timed_out || cancelled || status.is_none() {
        let _ = child.kill().await;
    }

    Ok(RunResult {
        exit_code: status.and_then(|s| s.code()),
        output: String::from_utf8_lossy(&captured).into_owned(),
        dropped_bytes: dropped,
        timed_out,
        cancelled,
    })
}

async fn pump(mut r: impl AsyncRead + Unpin, tx: mpsc::UnboundedSender<Vec<u8>>) {
    let mut buf = vec![0u8; 8192];
    loop {
        match r.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(n) => {
                if tx.send(buf[..n].to_vec()).is_err() {
                    return;
                }
            }
        }
    }
}

/// A child's process tree: its process group on Unix (start the child with
/// `process_group(0)`), its Job Object on Windows (closing it ends the tree).
#[derive(Debug)]
pub struct Tree {
    pid: Option<u32>,
    #[cfg(windows)]
    job: Option<crate::job::Job>,
}

impl Tree {
    #[cfg(unix)]
    pub fn new(child: &tokio::process::Child) -> Self {
        Self { pid: child.id() }
    }

    #[cfg(windows)]
    pub fn new(child: &tokio::process::Child) -> Self {
        // A grandchild started in the instant before assignment escapes the job;
        // `taskkill /T` below still finds it while its parent lives.
        let job =
            crate::job::Job::new().filter(|j| child.raw_handle().is_some_and(|h| j.assign(h)));
        if job.is_none() {
            tracing::debug!("no job object for this command; using taskkill");
        }
        Self {
            pid: child.id(),
            job,
        }
    }

    #[cfg(unix)]
    pub fn kill(&self) {
        use nix::sys::signal::{Signal, killpg};
        use nix::unistd::Pid;
        if let Some(pid) = self.pid.and_then(|p| i32::try_from(p).ok()) {
            // ESRCH (group already gone) is the normal case.
            let _ = killpg(Pid::from_raw(pid), Signal::SIGKILL);
        }
    }

    #[cfg(windows)]
    pub fn kill(&self) {
        if let Some(job) = &self.job {
            return job.kill();
        }
        if let Some(pid) = self.pid {
            let _ = std::process::Command::new("taskkill")
                .args(["/T", "/F", "/PID", &pid.to_string()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(timeout_ms: u64) -> RunOptions {
        RunOptions {
            cwd: std::env::temp_dir(),
            timeout: Duration::from_millis(timeout_ms),
            cancel: CancellationToken::new(),
            on_output: None,
            sandbox: None,
        }
    }

    /// Runs on every OS with the default shell (pwsh/powershell/cmd on Windows).
    #[tokio::test]
    async fn default_shell_runs_echo() {
        let sh = ShellKind::detect("auto", std::env::var("SHELL").ok().as_deref());
        let r = run(&sh, "echo hello", opts(20_000)).await.expect("runs");
        assert_eq!(r.exit_code, Some(0), "{r:?}");
        assert!(r.output.contains("hello"), "{r:?}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn captures_both_streams_and_exit_code() {
        let sh = ShellKind::detect("sh", None);
        let r = run(&sh, "echo out; echo err >&2; exit 3", opts(5000))
            .await
            .expect("runs");
        assert_eq!(r.exit_code, Some(3));
        assert!(
            r.output.contains("out") && r.output.contains("err"),
            "{r:?}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_kills_the_group() {
        let sh = ShellKind::detect("sh", None);
        let started = std::time::Instant::now();
        let r = run(&sh, "sleep 30 & sleep 30", opts(300))
            .await
            .expect("runs");
        assert!(r.timed_out);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    /// The job kills the shell and the `ping` it started (CI runs this on Windows).
    #[cfg(windows)]
    #[tokio::test]
    async fn timeout_kills_the_job() {
        let sh = ShellKind::detect("cmd", None);
        let started = std::time::Instant::now();
        let r = run(&sh, "ping -n 30 127.0.0.1 > nul", opts(500))
            .await
            .expect("runs");
        assert!(r.timed_out, "{r:?}");
        assert!(started.elapsed() < Duration::from_secs(10));
    }
}
