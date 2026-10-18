//! The OS sandbox for shell commands and servers (docs/plan/11-security.md#os-sandboxing).

use std::path::Path;
use std::sync::Arc;

use quack_config::Loaded;
use quack_config::settings::SandboxMode;
use quack_lsp::Prepare;
use quack_sandbox::Policy;
use quack_tools::SandboxState;

/// The policy for a sandbox mode: the workspace, temp folders and `[sandbox] writable`.
fn policy(config: &Loaded, mode: SandboxMode) -> Policy {
    let root = &config.dirs.workspace_root;
    let writable = match mode {
        SandboxMode::Off | SandboxMode::ReadOnly => Vec::new(),
        SandboxMode::WorkspaceWrite | SandboxMode::WorkspaceWriteNet => {
            let get = |k: &str| std::env::var_os(k);
            std::iter::once(root.clone())
                .chain(Policy::temp_dirs())
                .chain(config.settings.sandbox.writable.iter().map(|w| {
                    let p = quack_config::expand_home(w, &get);
                    if p.is_absolute() { p } else { root.join(p) }
                }))
                .collect()
        }
    };
    Policy {
        writable,
        network: mode == SandboxMode::WorkspaceWriteNet,
    }
}

/// How shell commands run this session.
pub(crate) fn state(config: &Loaded) -> SandboxState {
    let mode = config.settings.sandbox.mode;
    if mode == SandboxMode::Off {
        return SandboxState::Off;
    }
    match quack_sandbox::available() {
        Ok(_) => SandboxState::On(Arc::new(policy(config, mode))),
        Err(why) => SandboxState::Unavailable(why),
    }
}

/// A warning for the startup panel when the sandbox is configured but can't be used.
pub(crate) fn warning(config: &Loaded) -> Option<String> {
    match state(config) {
        SandboxState::Unavailable(why) => Some(format!(
            "[sandbox] mode = \"{}\", but {why}; every shell command will ask first",
            config.settings.sandbox.mode.label()
        )),
        _ => None,
    }
}

/// The policy for servers (MCP, LSP) whose file sets `sandbox = true`: the configured
/// mode, or `workspace-write` when the sandbox is off. `None` when it is unavailable.
pub(crate) fn server_policy(config: &Loaded) -> Option<Arc<Policy>> {
    quack_sandbox::available().ok()?;
    let mode = match config.settings.sandbox.mode {
        SandboxMode::Off => SandboxMode::WorkspaceWrite,
        m => m,
    };
    Some(Arc::new(policy(config, mode)))
}

/// Starts language servers whose file sets `sandbox = true` inside the sandbox, and
/// refuses to start them when it is unavailable.
pub(crate) fn server_wrapper(config: &Loaded) -> Option<Prepare> {
    let policy = server_policy(config);
    Some(Arc::new(move |spec, program: &Path, args: &[String]| {
        if spec.file.sandbox == Some(true) {
            let policy = policy
                .as_ref()
                .ok_or("the file sets `sandbox = true`, but the sandbox is not available")?;
            quack_sandbox::command(policy, program.as_os_str(), args)
                .map_err(|e| format!("{} must run in the sandbox, but {e}", spec.id))
        } else {
            let mut cmd = tokio::process::Command::new(program);
            cmd.args(args);
            Ok(cmd)
        }
    }))
}
