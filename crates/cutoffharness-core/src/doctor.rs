//! Environment checks for `cutoffharness doctor` and `/doctor`
//! (docs/plan/01-configuration.md#validation-and-diagnostics).

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;

use cutoffharness_config::{Loaded, ProjectStatus};
use cutoffharness_protocol::{CheckStatus, DoctorCheck};
use cutoffharness_shell::ShellKind;
use cutoffharness_store::memory::Scope;

fn check(name: &str, status: CheckStatus, detail: impl Into<String>) -> DoctorCheck {
    DoctorCheck {
        name: name.into(),
        status,
        detail: detail.into(),
    }
}

/// Every check, in display order. Contacts each configured provider (nothing else).
pub async fn run(config: Arc<Loaded>) -> Vec<DoctorCheck> {
    let providers = crate::models::catalog(config.clone()).await;
    let cfg = config.clone();
    let mut checks = tokio::task::spawn_blocking(move || local(&cfg))
        .await
        .unwrap_or_default();

    // The bundle's providers that cannot be used yet are offers, not problems.
    let (offers, providers): (Vec<_>, Vec<_>) = providers
        .into_iter()
        .partition(|p| p.bundled && !p.available);
    let usable = providers.iter().filter(|p| p.available).count();
    let offered = match offers.len() {
        0 => String::new(),
        n => format!(" · {n} more offered by the defaults bundle"),
    };
    checks.insert(
        2,
        match providers.len() {
            0 => check(
                "providers",
                CheckStatus::Fail,
                format!(
                    "none ready{offered}: run `cutoffharness setup`, or add a file to providers.d (see `config paths`)"
                ),
            ),
            n => check(
                "providers",
                if usable == 0 {
                    CheckStatus::Fail
                } else if usable < n {
                    CheckStatus::Warn
                } else {
                    CheckStatus::Ok
                },
                format!("{usable} of {n} usable{offered}"),
            ),
        },
    );
    for (i, p) in providers.iter().enumerate() {
        let detail = if p.note.is_empty() {
            format!("{} models", p.models.len())
        } else {
            p.note.clone()
        };
        checks.insert(
            3 + i,
            check(
                &format!("  {}", p.id),
                if p.available {
                    CheckStatus::Ok
                } else {
                    CheckStatus::Warn
                },
                detail,
            ),
        );
    }
    let cfg = config.clone();
    let usable: Vec<String> = providers
        .iter()
        .filter(|p| p.available)
        .map(|p| p.id.clone())
        .collect();
    if let Ok(c) = tokio::task::spawn_blocking(move || egress_check(&cfg, &usable)).await {
        checks.push(c);
    }
    checks
}

/// Checks that need no network.
fn local(config: &Loaded) -> Vec<DoctorCheck> {
    let d = &config.dirs;
    let mut out = Vec::new();
    out.push(if config.diagnostics.is_empty() {
        check(
            "config",
            CheckStatus::Ok,
            format!("{} layers read", config.layers.len()),
        )
    } else {
        let files: Vec<String> = config.diagnostics.iter().map(|d| d.one_line()).collect();
        check("config", CheckStatus::Fail, files.join("\n"))
    });
    out.push(match &d.defaults {
        Some(p) => check("defaults", CheckStatus::Ok, p.display().to_string()),
        None => check(
            "defaults",
            CheckStatus::Info,
            "no defaults bundle; only your own provider and profile files are used",
        ),
    });
    // Providers are inserted here by `run`.
    out.push(match config.settings.model.default.as_str() {
        "" => check(
            "model",
            CheckStatus::Info,
            "no [model] default; the first usable model is picked",
        ),
        m => check("model", CheckStatus::Ok, m),
    });
    let (status, project) = match config.project {
        ProjectStatus::None => (CheckStatus::Ok, "no .cutoffharness/"),
        ProjectStatus::Trusted => (CheckStatus::Ok, "trusted"),
        ProjectStatus::SettingsOnly => (CheckStatus::Ok, "trusted (settings only)"),
        ProjectStatus::Untrusted => (
            CheckStatus::Warn,
            "config found but not trusted; skipped (/trust or `cutoffharness trust`)",
        ),
        ProjectStatus::Changed => (
            CheckStatus::Warn,
            "changed since it was trusted; skipped until reviewed (/trust or `cutoffharness trust`)",
        ),
        ProjectStatus::Ignored => (CheckStatus::Info, "ignored (trust.toml)"),
        ProjectStatus::DisabledByPolicy => (CheckStatus::Info, "disabled by policy"),
    };
    out.push(check(
        "project",
        status,
        format!("{project} · {}", d.workspace_root.display()),
    ));

    out.push(mcp_check(config));

    let instructions = crate::env::instructions(config);
    out.push(check(
        "instructions",
        if instructions.sources.is_empty() {
            CheckStatus::Info
        } else {
            CheckStatus::Ok
        },
        if instructions.sources.is_empty() {
            "no AGENTS.md".to_owned()
        } else {
            instructions.sources.join(", ")
        },
    ));

    let shell = ShellKind::detect(
        &config.settings.tools.shell,
        std::env::var("SHELL").ok().as_deref(),
    );
    let program = shell.program();
    out.push(if runs(program, &[version_flag(&shell)]) {
        check(
            "shell",
            CheckStatus::Ok,
            format!("{} · {}", shell.label(), program.display()),
        )
    } else {
        check(
            "shell",
            CheckStatus::Fail,
            format!("{} does not run; set [tools] shell", program.display()),
        )
    });
    out.push(if runs(Path::new("git"), &["--version"]) {
        check("git", CheckStatus::Ok, "found")
    } else {
        check(
            "git",
            CheckStatus::Warn,
            "not found: the workspace root is the current directory and there is no branch info",
        )
    });
    if cfg!(not(any(windows, target_os = "macos"))) {
        out.push(if runs(Path::new("secret-tool"), &["--version"]) {
            check("keyring", CheckStatus::Ok, "secret-tool found")
        } else {
            check(
                "keyring",
                CheckStatus::Info,
                "secret-tool not found; `keyring` secrets need libsecret-tools (Debian/Ubuntu) or libsecret",
            )
        });
    }

    let sessions = crate::session::project_sessions(config).dir;
    out.push(match writable(&sessions) {
        Ok(()) => check("sessions", CheckStatus::Ok, sessions.display().to_string()),
        Err(e) => check(
            "sessions",
            CheckStatus::Fail,
            format!("{}: {e}; sessions will not be saved", sessions.display()),
        ),
    });
    let memory = crate::env::memory_store(config);
    let notes = memory.list(Scope::Global).len() + memory.list(Scope::Project).len();
    out.push(check(
        "memory",
        CheckStatus::Ok,
        format!("{notes} notes · {}", memory.project.display()),
    ));

    let var = |k: &str| std::env::var(k).unwrap_or_default();
    out.push(check(
        "terminal",
        CheckStatus::Info,
        format!(
            "TERM={} COLORTERM={}{}",
            var("TERM"),
            var("COLORTERM"),
            if std::env::var_os("NO_COLOR").is_some() {
                " NO_COLOR"
            } else {
                ""
            }
        ),
    ));
    out
}

fn version_flag(shell: &ShellKind) -> &'static str {
    match shell {
        ShellKind::Cmd(_) => "/?",
        ShellKind::Pwsh(_) | ShellKind::PowerShell(_) => "-Version",
        _ => "--version",
    }
}

fn runs(program: &Path, args: &[&str]) -> bool {
    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

/// Create the directory if needed and write a probe file into it.
fn writable(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let probe = dir.join(".doctor-probe");
    std::fs::write(&probe, b"")?;
    std::fs::remove_file(probe)
}

/// Every host this configuration can send requests to: the usable providers and the
/// enabled remote MCP servers. Nothing else is contacted.
fn egress_check(config: &Loaded, usable: &[String]) -> DoctorCheck {
    let mut hosts: Vec<String> = Vec::new();
    let mut add = |url: &str| {
        let h = cutoffharness_config::url_host(url).to_ascii_lowercase();
        if !hosts.contains(&h) {
            hosts.push(h);
        }
    };
    for item in cutoffharness_config::load_providers(config) {
        if let Some(f) = item.loaded()
            && usable.contains(&item.id)
        {
            add(&f.base_url);
        }
    }
    for item in cutoffharness_config::load_mcp(config) {
        if let Some(f) = item.loaded()
            && !f.url.is_empty()
        {
            add(&f.url);
        }
    }
    let mut detail = if hosts.is_empty() {
        "no hosts".to_owned()
    } else {
        hosts.join(", ")
    };
    let policy = &config.settings.policy;
    let limits: Vec<&str> = policy
        .providers
        .hosts
        .iter()
        .chain(&policy.mcp.hosts)
        .map(String::as_str)
        .collect();
    if !limits.is_empty() {
        detail.push_str(&format!(" · policy allows only {}", limits.join(", ")));
    }
    check("network", CheckStatus::Info, detail)
}

/// MCP files: each one parses, names a transport, and a local server's program is on
/// `PATH`. Nothing is started; `/mcp` shows whether servers connect.
fn mcp_check(config: &Loaded) -> DoctorCheck {
    let items = cutoffharness_config::load_mcp(config);
    let mut lines = Vec::new();
    let mut worst = CheckStatus::Ok;
    let mut enabled = 0;
    for item in &items {
        let problem = match &item.state {
            cutoffharness_config::ItemState::Loaded(f) => {
                enabled += 1;
                match f.transport() {
                    Err(e) => Some(e),
                    Ok("stdio") => {
                        let path = std::env::var_os("PATH");
                        cutoffharness_shell::resolve_program(
                            &f.command,
                            path.as_deref(),
                            &config.dirs.workspace_root,
                        )
                        .err()
                        .map(|e| format!("cannot find {e}"))
                    }
                    Ok(_) => None,
                }
            }
            cutoffharness_config::ItemState::Disabled(_) => continue,
            cutoffharness_config::ItemState::Failed(d) => Some(d.one_line()),
        };
        if let Some(p) = problem {
            worst = CheckStatus::Warn;
            lines.push(format!("{}: {p}", item.id));
        }
    }
    let summary = match enabled {
        0 => return check("mcp", CheckStatus::Info, "no servers in mcp.d"),
        1 => "1 server".to_owned(),
        n => format!("{n} servers"),
    };
    lines.insert(0, summary);
    check("mcp", worst, lines.join("\n"))
}
