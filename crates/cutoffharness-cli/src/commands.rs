use cutoffharness_core::protocol::{CheckStatus, DoctorCheck};
use std::io::Read;
use std::path::PathBuf;

use cutoffharness_config::{ItemState, Loaded, load_providers, schema};
use cutoffharness_exec::exit;

pub(crate) fn config_check(loaded: &Loaded) -> i32 {
    let mut errors = 0;
    for d in &loaded.diagnostics {
        eprintln!("{:?}", miette::Report::new(d.clone()));
        errors += 1;
    }
    let providers = load_providers(loaded);
    for item in providers {
        match item.state {
            ItemState::Loaded(_) => {
                println!("ok       provider {:<20} {}", item.id, item.path.display())
            }
            ItemState::Disabled(why) => println!("disabled provider {:<20} {why}", item.id),
            ItemState::Failed(d) => {
                eprintln!("{:?}", miette::Report::new(d));
                errors += 1;
            }
        }
    }
    let profiles = cutoffharness_config::load_dir_items::<cutoffharness_config::profile::ProfileFile>(
        &loaded.item_layers(),
        "profiles.d",
        "profile.toml",
    );
    for item in profiles {
        match item.state {
            ItemState::Loaded(p) => {
                let prompt = item.path.with_file_name(&p.prompt.system);
                if prompt.is_file() {
                    println!("ok       profile  {:<20} {}", item.id, item.path.display());
                } else {
                    eprintln!(
                        "error: profile `{}`: prompt file {} is missing",
                        item.id,
                        prompt.display()
                    );
                    errors += 1;
                }
            }
            ItemState::Disabled(why) => println!("disabled profile  {:<20} {why}", item.id),
            ItemState::Failed(d) => {
                eprintln!("{:?}", miette::Report::new(d));
                errors += 1;
            }
        }
    }
    for w in &loaded.warnings {
        eprintln!("warning: {w}");
    }
    if errors > 0 {
        eprintln!("{errors} error(s)");
        exit::CONFIG_ERROR
    } else {
        println!("configuration ok");
        exit::OK
    }
}

pub(crate) fn config_show(loaded: &Loaded, origin: bool) -> i32 {
    print!("{}", loaded.render(origin));
    exit::OK
}

pub(crate) fn config_schema(loaded: &Loaded, dir: Option<PathBuf>) -> i32 {
    let dir = dir.unwrap_or_else(|| loaded.dirs.user_config.join("schemas"));
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("error: cannot create {}: {e}", dir.display());
        return exit::CONFIG_ERROR;
    }
    for (name, value) in schema::all() {
        let path = dir.join(name);
        let text = serde_json::to_string_pretty(&value).unwrap_or_default() + "\n";
        if let Err(e) = std::fs::write(&path, text) {
            eprintln!("error: cannot write {}: {e}", path.display());
            return exit::CONFIG_ERROR;
        }
        println!("wrote {}", path.display());
    }
    exit::OK
}

pub(crate) fn config_paths(loaded: &Loaded) -> i32 {
    let d = &loaded.dirs;
    let defaults = d
        .defaults
        .as_ref()
        .map_or_else(|| "(none)".into(), |p| p.display().to_string());
    println!("defaults   {defaults}");
    println!("system     {}", d.system.display());
    println!("user       {}", d.user_config.display());
    println!("data       {}", d.user_data.display());
    println!("cache      {}", d.user_cache.display());
    println!("workspace  {}", d.workspace_root.display());
    println!("project    {}", d.project.display());
    exit::OK
}

/// Print the checks; non-zero exit when any failed. Output errors (a closed pipe) are
/// ignored.
pub(crate) fn doctor(checks: &[DoctorCheck]) -> i32 {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "cutoffharness {}\n", env!("CARGO_PKG_VERSION"));
    let width = checks.iter().map(|c| c.name.len()).max().unwrap_or(0);
    for c in checks {
        let mark = match c.status {
            CheckStatus::Ok => "ok  ",
            CheckStatus::Info => "info",
            CheckStatus::Warn => "warn",
            CheckStatus::Fail => "FAIL",
        };
        let mut lines = c.detail.lines();
        let _ = writeln!(
            out,
            "{mark}  {:<width$}  {}",
            c.name,
            lines.next().unwrap_or_default()
        );
        for l in lines {
            let _ = writeln!(out, "      {:<width$}  {l}", "");
        }
    }
    if checks.iter().any(|c| c.status == CheckStatus::Fail) {
        exit::CONFIG_ERROR
    } else {
        exit::OK
    }
}

pub(crate) fn read_prompt(arg: &str) -> std::io::Result<String> {
    if arg != "-" {
        return Ok(arg.to_owned());
    }
    let mut s = String::new();
    std::io::stdin().read_to_string(&mut s)?;
    Ok(s.trim_end().to_owned())
}

pub(crate) fn sessions(loaded: &Loaded) -> i32 {
    let (dir, list) = cutoffharness_core::list_sessions(loaded);
    if list.is_empty() {
        println!("no saved sessions for this project ({})", dir.display());
        return exit::OK;
    }
    let now = cutoffharness_store::time::unix_now();
    println!(
        "{:<14} {:>5} {:>5}  {:<28} title",
        "id", "age", "turns", "model"
    );
    for e in list.iter().take(50) {
        let id: String = e.id.chars().take(12).collect();
        let title: String = e.title.chars().take(60).collect();
        println!(
            "{id:<14} {:>5} {:>5}  {:<28} {title}",
            cutoffharness_store::time::age(e.updated, now),
            e.turns,
            e.model
        );
    }
    println!("\nresume with: cutoffharness --resume <id>   (or --continue for the latest)");
    exit::OK
}

pub(crate) fn trust(loaded: &Loaded, decision: Option<cutoffharness_config::TrustDecision>) -> i32 {
    use cutoffharness_config::ProjectStatus;
    let dir = &loaded.dirs.project;
    let Some(decision) = decision else {
        let state = match loaded.project {
            ProjectStatus::None => {
                println!("no {} in this workspace", dir.display());
                return exit::OK;
            }
            ProjectStatus::Trusted => "trusted",
            ProjectStatus::SettingsOnly => "trusted (settings only)",
            ProjectStatus::Untrusted => "not trusted yet; skipped",
            ProjectStatus::Changed => "changed since it was trusted; skipped",
            ProjectStatus::Ignored => "ignored",
            ProjectStatus::DisabledByPolicy => "disabled by the system policy",
        };
        println!("{}\n  state: {state}", dir.display());
        let summary = loaded.project_summary();
        if !summary.is_empty() {
            println!("  holds: {}", summary.describe());
        }
        println!("\ndecide with: cutoffharness trust trusted | settings-only | ignored");
        return exit::OK;
    };
    match loaded.trust_project(decision) {
        Ok(file) => {
            println!(
                "{}: {} (saved in {})",
                dir.display(),
                decision.as_str(),
                file.display()
            );
            exit::OK
        }
        Err(e) => {
            eprintln!("error: {e}");
            exit::CONFIG_ERROR
        }
    }
}

pub(crate) fn keyring_set(name: &str) -> i32 {
    let (service, user) = cutoffharness_config::keyring_parts(name);
    let secret = match read_secret(&format!("secret for {service}/{user} (input hidden): ")) {
        Ok(s) if !s.is_empty() => s,
        Ok(_) => {
            eprintln!("nothing entered; nothing stored");
            return exit::CONFIG_ERROR;
        }
        Err(e) => {
            eprintln!("cannot read the secret: {e}");
            return exit::CONFIG_ERROR;
        }
    };
    match cutoffharness_config::keyring_set(name, &secret) {
        Ok(()) => {
            eprintln!(
                "stored. Use it in a provider file with:\n  api_key = {{ keyring = \"{name}\" }}"
            );
            exit::OK
        }
        Err(e) => {
            eprintln!("{e}");
            exit::CONFIG_ERROR
        }
    }
}

pub(crate) fn keyring_delete(name: &str) -> i32 {
    match cutoffharness_config::keyring_delete(name) {
        Ok(()) => {
            eprintln!("removed {name}");
            exit::OK
        }
        Err(e) => {
            eprintln!("{e}");
            exit::CONFIG_ERROR
        }
    }
}

/// Read one line without echo from a terminal, or all of stdin when piped.
pub(crate) fn read_secret(prompt: &str) -> std::io::Result<String> {
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() {
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s)?;
        return Ok(s.trim().to_owned());
    }
    use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
    eprint!("{prompt}");
    crossterm::terminal::enable_raw_mode()?;
    let mut s = String::new();
    let result = loop {
        match event::read() {
            Ok(Event::Key(k)) if k.kind != KeyEventKind::Release => match k.code {
                KeyCode::Enter => break Ok(()),
                KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                    s.clear();
                    break Ok(());
                }
                KeyCode::Backspace => {
                    s.pop();
                }
                KeyCode::Char(c) => s.push(c),
                _ => {}
            },
            Ok(Event::Paste(p)) => s.push_str(&p),
            Ok(_) => {}
            Err(e) => break Err(e),
        }
    };
    crossterm::terminal::disable_raw_mode()?;
    eprintln!();
    result.map(|()| s.trim().to_owned())
}
