//! Starting a stdio server: program lookup (with `PATHEXT` on Windows), a minimal
//! environment, and the process tree killed with the server.

use std::collections::{BTreeMap, VecDeque};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use quack_config::McpServerFile;
use quack_shell::Tree;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};

/// Lines of stderr kept for `/mcp` and failure messages.
pub(crate) const LOG_LINES: usize = 60;

pub(crate) type Log = Arc<Mutex<VecDeque<String>>>;

pub(crate) fn push_log(log: &Log, line: String) {
    if let Ok(mut l) = log.lock() {
        if l.len() == LOG_LINES {
            l.pop_front();
        }
        l.push_back(line);
    }
}

pub(crate) struct Spawned {
    pub child: Child,
    pub tree: Tree,
    pub stdin: ChildStdin,
    pub stdout: ChildStdout,
}

/// The program, arguments, directory and environment, with secrets resolved. Runs
/// `cmd` secrets, so call it off the async runtime.
pub(crate) struct Prepared {
    program: PathBuf,
    args: Vec<String>,
    cwd: PathBuf,
    env: BTreeMap<String, OsString>,
}

pub(crate) fn prepare(file: &McpServerFile, root: &Path) -> Result<Prepared, String> {
    let get = |k: &str| std::env::var_os(k);
    let cwd = match file.cwd.as_str() {
        "" => root.to_path_buf(),
        c => root.join(quack_config::expand_home(c, &get)),
    };
    let mut env = quack_shell::inherited_env(file.inherit_env.as_deref());
    for (k, secret) in &file.env {
        let v = secret
            .resolve(&get)
            .map_err(|e| format!("env `{k}`: {e}"))?;
        env.insert(k.clone(), v.into());
    }
    let search = env.get("PATH").cloned().or_else(|| get("PATH"));
    let program = quack_shell::resolve_program(&file.command, search.as_deref(), &cwd)?;
    Ok(Prepared {
        program,
        // `~/notes` means the home folder, as it would in a shell.
        args: file
            .args
            .iter()
            .map(|a| {
                if a.starts_with("~/") || a.starts_with("~\\") {
                    quack_config::expand_home(a, &get).display().to_string()
                } else {
                    a.clone()
                }
            })
            .collect(),
        cwd,
        env,
    })
}

pub(crate) fn spawn(
    p: Prepared,
    sandbox: Option<&quack_sandbox::Policy>,
    log: Log,
) -> Result<Spawned, String> {
    let mut cmd = match sandbox {
        Some(policy) => quack_sandbox::command(policy, p.program.as_os_str(), &p.args)?,
        None => {
            let mut c = tokio::process::Command::new(&p.program);
            c.args(&p.args);
            c
        }
    };
    cmd.current_dir(&p.cwd)
        .env_clear()
        .envs(&p.env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", p.program.display()))?;
    let tree = Tree::new(&child);
    let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        return Err("the server's pipes are not available".into());
    };
    if let Some(stderr) = child.stderr.take() {
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!(target: "mcp", "{line}");
                push_log(&log, line);
            }
        });
    }
    Ok(Spawned {
        child,
        tree,
        stdin,
        stdout,
    })
}
