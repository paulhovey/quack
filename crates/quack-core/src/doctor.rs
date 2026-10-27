//! Environment checks for `quack doctor` and `/doctor`
//! (docs/plan/01-configuration.md#validation-and-diagnostics).

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;

use quack_config::settings::SandboxMode;
use quack_config::{Loaded, ProjectStatus};
use quack_protocol::{CheckStatus, DoctorCheck};
use quack_shell::ShellKind;
use quack_store::memory::Scope;

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
                    "none ready{offered}: run `quack setup`, or add a file to providers.d (see `config paths`)"
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
            "no defaults bundle; your own files, plus the built-in generic profile and sub-agents",
        ),
    });
    // Providers are inserted here by `run`.
    out.push(
        match (
            config.remembered_model(),
            config.settings.model.default.as_str(),
        ) {
            (Some(last), "") => check("model", CheckStatus::Ok, format!("{last} (last used)")),
            (Some(last), d) => check(
                "model",
                CheckStatus::Ok,
                format!("{last} (last used; {d} if it is unavailable)"),
            ),
            (None, "") => check(
                "model",
                CheckStatus::Info,
                "no [model] default; the first usable model is picked",
            ),
            (None, m) => check("model", CheckStatus::Ok, m),
        },
    );
    let (status, project) = match config.project {
        ProjectStatus::None => (CheckStatus::Ok, "no .quack/"),
        ProjectStatus::Trusted => (CheckStatus::Ok, "trusted"),
        ProjectStatus::SettingsOnly => (CheckStatus::Ok, "trusted (settings only)"),
        ProjectStatus::Untrusted => (
            CheckStatus::Warn,
            "config found but not trusted; skipped (/trust or `quack trust`)",
        ),
        ProjectStatus::Changed => (
            CheckStatus::Warn,
            "changed since it was trusted; skipped until reviewed (/trust or `quack trust`)",
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
    out.push(hooks_check(config));
    out.push(agents_check(config));
    out.push(lsp_check(config));
    out.push(grammars_check(config));

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
    out.push(sandbox_check(config));
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

fn lsp_check(config: &Loaded) -> DoctorCheck {
    let (specs, warnings) = crate::lsp::load(config);
    if !warnings.is_empty() {
        return check("lsp", CheckStatus::Fail, warnings.join("\n"));
    }
    if specs.is_empty() {
        return check("lsp", CheckStatus::Info, "no servers in lsp.d");
    }
    let installed: Vec<&str> = specs
        .iter()
        .filter(|s| s.installed())
        .map(|s| s.id.as_str())
        .collect();
    if installed.is_empty() {
        return check(
            "lsp",
            CheckStatus::Info,
            format!(
                "none of {} servers installed; each lsp.d file says how to install it",
                specs.len()
            ),
        );
    }
    check(
        "lsp",
        CheckStatus::Ok,
        format!(
            "{} of {} installed: {}",
            installed.len(),
            specs.len(),
            installed.join(", ")
        ),
    )
}

fn grammars_check(config: &Loaded) -> DoctorCheck {
    use quack_tools::grammar::Grammars;
    let g = Grammars::from_layers(config.item_layers().iter().map(|(_, d)| d.as_path()));
    let overrides = g.overrides();
    let problems = g.check();
    if !Grammars::compiled_in() {
        let mut detail =
            "not in this build (feature lang-common): no outlines, plain code".to_owned();
        if !overrides.is_empty() {
            detail.push_str(&format!(
                "; {} query files in grammars/ are unused",
                overrides.len()
            ));
        }
        return check("grammars", CheckStatus::Info, detail);
    }
    if !problems.is_empty() {
        return check("grammars", CheckStatus::Warn, problems.join("\n"));
    }
    let detail = if overrides.is_empty() {
        format!(
            "{} built in, no overrides",
            quack_tools::grammar::LANGUAGES.len()
        )
    } else {
        let files: Vec<String> = overrides
            .iter()
            .map(|(lang, file, _)| format!("{lang}/{file}"))
            .collect();
        format!("overrides: {}", files.join(", "))
    };
    check("grammars", CheckStatus::Ok, detail)
}

fn sandbox_check(config: &Loaded) -> DoctorCheck {
    let mode = config.settings.sandbox.mode;
    match (mode, quack_sandbox::available()) {
        (SandboxMode::Auto, Ok(how)) => check(
            "sandbox",
            CheckStatus::Ok,
            format!("auto: workspace-write · {how}"),
        ),
        (SandboxMode::Auto, Err(why)) => check(
            "sandbox",
            CheckStatus::Warn,
            format!("auto: unavailable here, commands run without one · {why}"),
        ),
        (SandboxMode::Off, Ok(how)) => check(
            "sandbox",
            CheckStatus::Info,
            format!("off ({how} available: remove [sandbox] mode to use it)"),
        ),
        (SandboxMode::Off, Err(why)) => check(
            "sandbox",
            CheckStatus::Info,
            format!("off · unavailable: {why}"),
        ),
        (m, Ok(how)) => check("sandbox", CheckStatus::Ok, format!("{} · {how}", m.label())),
        (m, Err(why)) => check(
            "sandbox",
            CheckStatus::Warn,
            format!(
                "{} configured, but {why}; every shell command asks first",
                m.label()
            ),
        ),
    }
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
        let h = quack_config::url_host(url).to_ascii_lowercase();
        if !hosts.contains(&h) {
            hosts.push(h);
        }
    };
    for item in quack_config::load_providers(config) {
        if let Some(f) = item.loaded()
            && usable.contains(&item.id)
        {
            add(&f.base_url);
            if let Some(auth) = &f.auth {
                add(&auth.issuer);
            }
        }
    }
    for item in quack_config::load_mcp(config) {
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
    let items = quack_config::load_mcp(config);
    let mut lines = Vec::new();
    let mut worst = CheckStatus::Ok;
    let mut enabled = 0;
    for item in &items {
        let problem = match &item.state {
            quack_config::ItemState::Loaded(f) => {
                enabled += 1;
                match f.transport() {
                    Err(e) => Some(e),
                    Ok("stdio") => {
                        let path = std::env::var_os("PATH");
                        quack_shell::resolve_program(
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
            quack_config::ItemState::Disabled(_) => continue,
            quack_config::ItemState::Failed(d) => Some(d.one_line()),
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

/// Hook files: each one parses and checks, and its program (and any `{dir}` file it names)
/// exists. Nothing runs.
/// `agents.d`: the sub-agents the `task` tool offers, and files that failed to load.
fn agents_check(config: &Loaded) -> DoctorCheck {
    let (agents, failed) = crate::env::agents(config);
    if agents.is_empty() && failed.is_empty() {
        return check(
            "agents",
            CheckStatus::Info,
            "no agents in agents.d; the task tool is not offered",
        );
    }
    let names: Vec<String> = agents
        .iter()
        .map(|(id, a)| {
            let model = match a.model.trim() {
                "" => String::new(),
                "small" if config.settings.model.small.is_empty() => {
                    ", small model unset: uses the session's".into()
                }
                "small" => format!(", on {}", config.settings.model.small),
                m => format!(", on {m}"),
            };
            match (a.read_only, model.is_empty()) {
                (false, true) => id.clone(),
                (ro, _) => {
                    let ro = if ro { "read-only" } else { "" };
                    let model = if ro.is_empty() {
                        model.trim_start_matches(", ").to_owned()
                    } else {
                        model
                    };
                    format!("{id} ({ro}{model})")
                }
            }
        })
        .collect();
    let mut lines = vec![format!("{} agents: {}", agents.len(), names.join(", "))];
    lines.extend(failed);
    let worst = if lines.len() > 1 {
        CheckStatus::Warn
    } else {
        CheckStatus::Ok
    };
    check("agents", worst, lines.join("\n"))
}

fn hooks_check(config: &Loaded) -> DoctorCheck {
    let mut lines = Vec::new();
    let mut worst = CheckStatus::Ok;
    let mut enabled = 0;
    for item in quack_config::load_hooks(config) {
        let problem = match &item.state {
            quack_config::ItemState::Loaded(f) => {
                enabled += 1;
                let dir = item.path.parent().unwrap_or(Path::new(""));
                f.check()
                    .err()
                    .or_else(|| crate::hooks::command_problem(f, dir, &config.dirs.workspace_root))
            }
            quack_config::ItemState::Disabled(_) => continue,
            quack_config::ItemState::Failed(d) => Some(d.one_line()),
        };
        if let Some(p) = problem {
            worst = CheckStatus::Warn;
            lines.push(format!("{}: {p}", item.id));
        }
    }
    let summary = match enabled {
        0 => return check("hooks", CheckStatus::Info, "no hooks in hooks.d"),
        1 => "1 hook".to_owned(),
        n => format!("{n} hooks"),
    };
    lines.insert(0, summary);
    check("hooks", worst, lines.join("\n"))
}
