//! Seatbelt on macOS: the program runs under `sandbox-exec` with a generated profile.
//! Writable paths are passed as parameters (`-D`), so they need no escaping.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use tokio::process::Command;

use crate::Policy;

const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

/// Devices programs commonly write to.
const DEVICES: &[&str] = &["/dev/null", "/dev/zero", "/dev/tty", "/dev/dtracehelper"];

pub(crate) fn available() -> Result<&'static str, String> {
    if Path::new(SANDBOX_EXEC).exists() {
        Ok("Seatbelt")
    } else {
        Err(format!("{SANDBOX_EXEC} is missing"))
    }
}

/// Seatbelt matches real paths: `/tmp` is `/private/tmp`, `$TMPDIR` is under
/// `/private/var/folders`.
fn real(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

pub(crate) fn profile(writable: usize, network: bool) -> String {
    let mut p = String::from("(version 1)\n(allow default)\n(deny file-write*\n  (require-all\n");
    for i in 0..writable {
        p.push_str(&format!("    (require-not (subpath (param \"W{i}\")))\n"));
    }
    for d in DEVICES {
        p.push_str(&format!("    (require-not (literal \"{d}\"))\n"));
    }
    p.push_str("    (require-not (regex #\"^/dev/fd/\"))\n    (require-not (regex #\"^/dev/ttys\"))))\n");
    if !network {
        p.push_str(
            "(deny network-outbound (remote ip))\n(deny network-inbound (local ip))\n(deny network-bind (local ip))\n",
        );
    }
    p
}

pub(crate) fn command<I, S>(policy: &Policy, program: &OsStr, args: I) -> Result<Command, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    available()?;
    let mut cmd = Command::new(SANDBOX_EXEC);
    for (i, w) in policy.writable.iter().enumerate() {
        let mut def = OsString::from(format!("W{i}="));
        def.push(real(w).as_os_str());
        cmd.arg("-D").arg(def);
    }
    cmd.arg("-p")
        .arg(profile(policy.writable.len(), policy.network))
        .arg("--")
        .arg(program)
        .args(args);
    Ok(cmd)
}
