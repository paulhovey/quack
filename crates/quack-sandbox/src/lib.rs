//! OS sandboxing for shell commands and servers (docs/plan/11-security.md#os-sandboxing).
//!
//! A sandboxed process can read everything, write only inside [`Policy::writable`] (plus
//! `/dev/null` and similar), and, unless [`Policy::network`] is set, open no network
//! connections. Local (Unix-domain) sockets keep working.
//!
//! - Linux: Landlock restricts writes and a seccomp filter refuses network sockets. Both
//!   are applied in the child just before it runs the program, and bind everything it
//!   starts.
//! - macOS: the program runs under `sandbox-exec` with a generated Seatbelt profile.
//! - Windows: not available yet; [`available`] says so.

use std::ffi::OsStr;
use std::path::PathBuf;

use tokio::process::Command;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Policy {
    /// Folders (or files) that may be written. Empty means read-only.
    pub writable: Vec<PathBuf>,
    /// Whether network connections are allowed.
    pub network: bool,
}

impl Policy {
    /// The usual places programs write besides the workspace: the temp folders.
    pub fn temp_dirs() -> Vec<PathBuf> {
        let mut dirs = vec![std::env::temp_dir()];
        if cfg!(unix) {
            dirs.push("/tmp".into());
            dirs.push("/var/tmp".into());
        }
        dirs.dedup();
        dirs
    }
}

/// The mechanism in use on this machine, or why sandboxing is not available.
pub fn available() -> Result<&'static str, String> {
    #[cfg(target_os = "linux")]
    return linux::available();
    #[cfg(target_os = "macos")]
    return macos::available();
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    Err("the sandbox is not available on this OS yet".into())
}

/// A command that runs `program` with `args` inside the sandbox. Set the directory,
/// environment and pipes on it as usual.
pub fn command<I, S>(policy: &Policy, program: &OsStr, args: I) -> Result<Command, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    #[cfg(target_os = "linux")]
    return linux::command(policy, program, args);
    #[cfg(target_os = "macos")]
    return macos::command(policy, program, args);
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (policy, program, args.into_iter().count());
        Err("the sandbox is not available on this OS yet".into())
    }
}

/// Whether a failed command's output looks like the sandbox stopped it.
pub fn looks_blocked(output: &str) -> bool {
    const SIGNS: &[&str] = &[
        "Permission denied",
        "Operation not permitted",
        "Read-only file system",
        "EACCES",
        "EPERM",
        "Could not resolve host",
        "Temporary failure in name resolution",
        "Name or service not known",
        "nodename nor servname provided",
        "Network is unreachable",
        "Failed to connect",
        "Couldn't connect",
        "getaddrinfo",
        "network is unreachable",
        "dial tcp",
    ];
    SIGNS.iter().any(|s| output.contains(s))
}
