//! The OS credential store: Credential Manager on Windows, Keychain on macOS, and the
//! Secret Service elsewhere. The Secret Service is reached through `secret-tool` (from
//! libsecret) rather than a D-Bus stack compiled in, which would cost over 100 crates.
//!
//! Every function takes `(service, user)` and reports failures as plain text.

/// The stored value, or `None` when nothing is stored.
pub fn get(service: &str, user: &str) -> Result<Option<String>, String> {
    imp::get(service, user)
}

pub fn set(service: &str, user: &str, value: &str) -> Result<(), String> {
    imp::set(service, user, value)
}

pub fn delete(service: &str, user: &str) -> Result<(), String> {
    imp::delete(service, user)
}

#[cfg(any(windows, target_os = "macos"))]
mod imp {
    use std::sync::LazyLock;

    use keyring_core::{Entry, Error};

    static STORE: LazyLock<Result<(), String>> = LazyLock::new(|| {
        #[cfg(windows)]
        let store = windows_native_keyring_store::Store::new();
        #[cfg(target_os = "macos")]
        let store = apple_native_keyring_store::keychain::Store::new();
        keyring_core::set_default_store(store.map_err(|e| e.to_string())?);
        Ok(())
    });

    fn entry(service: &str, user: &str) -> Result<Entry, String> {
        (*STORE).clone()?;
        Entry::new(service, user).map_err(|e| e.to_string())
    }

    pub fn get(service: &str, user: &str) -> Result<Option<String>, String> {
        match entry(service, user)?.get_password() {
            Ok(v) => Ok(Some(v)),
            Err(Error::NoEntry) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    pub fn set(service: &str, user: &str, value: &str) -> Result<(), String> {
        entry(service, user)?
            .set_password(value)
            .map_err(|e| e.to_string())
    }

    pub fn delete(service: &str, user: &str) -> Result<(), String> {
        match entry(service, user)?.delete_credential() {
            Ok(()) | Err(Error::NoEntry) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod imp {
    use std::io::Write;
    use std::process::{Command, Output, Stdio};

    const TOOL: &str = "secret-tool";

    fn run(args: &[&str], input: Option<&str>) -> Result<Output, String> {
        let mut child = Command::new(TOOL)
            .args(args)
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => format!(
                    "`{TOOL}` not found; install libsecret-tools (Debian/Ubuntu) or libsecret \
                     (Fedora/Arch), or use `cmd`, `env` or `file` instead"
                ),
                _ => format!("{TOOL}: {e}"),
            })?;
        if let (Some(value), Some(mut stdin)) = (input, child.stdin.take()) {
            stdin
                .write_all(value.as_bytes())
                .map_err(|e| format!("{TOOL}: {e}"))?;
        }
        child.wait_with_output().map_err(|e| format!("{TOOL}: {e}"))
    }

    fn failure(out: &Output) -> String {
        let err = String::from_utf8_lossy(&out.stderr);
        match err.trim() {
            "" => format!("{TOOL} failed ({})", out.status),
            msg => format!("{TOOL}: {msg}"),
        }
    }

    pub fn get(service: &str, user: &str) -> Result<Option<String>, String> {
        let out = run(&["lookup", "service", service, "username", user], None)?;
        // `lookup` exits non-zero with no output when nothing matches.
        if !out.status.success() {
            return if out.stderr.is_empty() {
                Ok(None)
            } else {
                Err(failure(&out))
            };
        }
        let v =
            String::from_utf8(out.stdout).map_err(|_| "stored value is not UTF-8".to_owned())?;
        Ok(Some(v.strip_suffix('\n').unwrap_or(&v).to_owned()))
    }

    pub fn set(service: &str, user: &str, value: &str) -> Result<(), String> {
        let label = format!("{service}/{user}");
        let out = run(
            &[
                "store", "--label", &label, "service", service, "username", user,
            ],
            Some(value),
        )?;
        out.status
            .success()
            .then_some(())
            .ok_or_else(|| failure(&out))
    }

    pub fn delete(service: &str, user: &str) -> Result<(), String> {
        let out = run(&["clear", "service", service, "username", user], None)?;
        // `clear` also exits non-zero when nothing matched; that is fine for a delete.
        if out.status.success() || out.stderr.is_empty() {
            Ok(())
        } else {
            Err(failure(&out))
        }
    }
}
