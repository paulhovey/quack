use quack_core::protocol::{CheckStatus, DoctorCheck};
use std::io::Read;
use std::path::PathBuf;

use quack_config::{ItemState, Loaded, load_providers, schema};
use quack_exec::exit;

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
    let profiles = quack_config::load_dir_items::<quack_config::profile::ProfileFile>(
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
    let _ = writeln!(out, "quack {}\n", env!("CARGO_PKG_VERSION"));
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
    let (dir, list) = quack_core::list_sessions(loaded);
    if list.is_empty() {
        println!("no saved sessions for this project ({})", dir.display());
        return exit::OK;
    }
    let now = quack_store::time::unix_now();
    println!(
        "{:<14} {:>5} {:>5}  {:<28} title",
        "id", "age", "turns", "model"
    );
    for e in list.iter().take(50) {
        let id: String = e.id.chars().take(12).collect();
        let title: String = e.title.chars().take(60).collect();
        println!(
            "{id:<14} {:>5} {:>5}  {:<28} {title}",
            quack_store::time::age(e.updated, now),
            e.turns,
            e.model
        );
    }
    println!("\nresume with: quack --resume <id>   (or --continue for the latest)");
    exit::OK
}

pub(crate) fn trust(loaded: &Loaded, decision: Option<quack_config::TrustDecision>) -> i32 {
    use quack_config::ProjectStatus;
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
        println!("\ndecide with: quack trust trusted | settings-only | ignored");
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
    let (service, user) = quack_config::keyring_parts(name);
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
    match quack_config::keyring_set(name, &secret) {
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
    match quack_config::keyring_delete(name) {
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

/// The provider with a sign-in named `id`, or the only one when `id` is left out.
#[cfg(feature = "auth-chatgpt")]
fn signed_provider(
    loaded: &Loaded,
    id: Option<String>,
) -> Result<quack_providers::Provider, String> {
    let items = load_providers(loaded);
    let with_auth: Vec<_> = items
        .iter()
        .filter_map(|i| i.loaded().filter(|f| f.auth.is_some()).map(|f| (i, f)))
        .collect();
    let (item, file) = match id {
        Some(id) => {
            let item = items
                .iter()
                .find(|i| i.id == id)
                .ok_or_else(|| format!("no provider `{id}` · see `quack doctor`"))?;
            let file = item
                .loaded()
                .ok_or_else(|| format!("provider `{id}` is not loaded · see `quack doctor`"))?;
            if file.auth.is_none() {
                return Err(format!(
                    "provider `{id}` uses a key, not a sign-in · see `quack keyring set`"
                ));
            }
            (item, file)
        }
        None => match with_auth.as_slice() {
            [one] => *one,
            [] => {
                return Err(
                    "no provider uses a sign-in · add one with an `[auth]` table".to_owned(),
                );
            }
            many => {
                let ids: Vec<&str> = many.iter().map(|(i, _)| i.id.as_str()).collect();
                return Err(format!("name the provider: {}", ids.join(", ")));
            }
        },
    };
    quack_providers::Provider::new(&item.id, file.clone(), &loaded.settings.network, &|k| {
        std::env::var_os(k)
    })
    .map_err(|e| e.to_string())
}

#[cfg(feature = "auth-chatgpt")]
pub(crate) async fn login(
    loaded: &Loaded,
    id: Option<String>,
    fresh: bool,
    no_browser: bool,
) -> i32 {
    let provider = match signed_provider(loaded, id) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            return exit::CONFIG_ERROR;
        }
    };
    let Some(login) = provider.login() else {
        eprintln!("provider `{}` has no sign-in", provider.id);
        return exit::CONFIG_ERROR;
    };
    let open = |url: &str| {
        eprintln!(
            "Continue with ChatGPT in your browser. If it does not open, visit:\n\n  {url}\n"
        );
        if !no_browser {
            open_browser(url);
        }
    };
    match login.sign_in(fresh, open).await {
        Ok(done) => {
            let who = if done.email.is_empty() {
                String::new()
            } else {
                format!(" as {}", done.email)
            };
            eprintln!("signed in{who}.");
            if !done.plan {
                eprintln!(
                    "the account did not allow plan usage, so requests will fail; run \
                     `quack login {}` again and allow it",
                    provider.id
                );
                return exit::CONFIG_ERROR;
            }
            eprintln!(
                "pick a model with `/models` or `quack -m {}/<model>`",
                provider.id
            );
            exit::OK
        }
        Err(e) => {
            eprintln!("{e}");
            exit::CONFIG_ERROR
        }
    }
}

#[cfg(feature = "auth-chatgpt")]
pub(crate) async fn logout(loaded: &Loaded, id: Option<String>) -> i32 {
    let provider = match signed_provider(loaded, id) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            return exit::CONFIG_ERROR;
        }
    };
    let Some(login) = provider.login() else {
        eprintln!("provider `{}` has no sign-in", provider.id);
        return exit::CONFIG_ERROR;
    };
    match login.sign_out().await {
        Ok(true) => {
            eprintln!("signed out of {}", provider.id);
            exit::OK
        }
        Ok(false) => {
            eprintln!(
                "signed out here, but the server did not confirm the revocation; disconnect \
                 quack in ChatGPT settings to be sure"
            );
            exit::OK
        }
        Err(e) => {
            eprintln!("{e}");
            exit::CONFIG_ERROR
        }
    }
}

/// Open a URL in the default browser; the caller has printed it too.
#[cfg(feature = "auth-chatgpt")]
fn open_browser(url: &str) {
    let mut cmd = if cfg!(windows) {
        // `start` in cmd would read the `&`s in the URL as command separators.
        let mut c = std::process::Command::new("rundll32");
        c.args(["url.dll,FileProtocolHandler", url]);
        c
    } else if cfg!(target_os = "macos") {
        let mut c = std::process::Command::new("open");
        c.arg(url);
        c
    } else {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(url);
        c
    };
    let _ = cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
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

/// `quack eval`: run the suite, print a table, write the JSON results.
pub(crate) async fn eval(
    loaded: &Loaded,
    load: quack_config::LoadOptions,
    args: crate::cli::EvalArgs,
) -> i32 {
    use quack_exec::eval;
    let suite = loaded.dirs.cwd.join(&args.suite);
    let tasks = match eval::load_tasks(&suite, &args.tasks) {
        Ok(t) if t.is_empty() => {
            eprintln!("error: no tasks in {}", suite.display());
            return exit::CONFIG_ERROR;
        }
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: {e}");
            return exit::CONFIG_ERROR;
        }
    };
    let opts = eval::EvalOptions {
        load,
        repeat: args.repeat.max(1),
        keep: args.keep,
        planner: args.planner.clone(),
        scratch: loaded
            .dirs
            .user_cache
            .join("eval")
            .join(std::process::id().to_string()),
        env: Default::default(),
    };
    let mut results = Vec::new();
    for task in &tasks {
        for run in 0..opts.repeat {
            eprint!("{} ({}/{})… ", task.id, run + 1, opts.repeat);
            let r = eval::run_task(task, &opts, run).await;
            eprintln!(
                "{}",
                match (&r.skipped, &r.error, r.passed) {
                    (Some(why), _, _) => format!("skipped ({why})"),
                    (None, Some(e), _) => format!("error: {e}"),
                    (None, None, true) => format!("pass ({} steps, {:.0}s)", r.steps, r.seconds),
                    (None, None, false) => format!(
                        "FAIL ({}): {}",
                        r.failure.map_or("?", |f| f.label()),
                        r.failed_checks.join("; ")
                    ),
                }
            );
            results.push(r);
        }
    }
    if opts.keep {
        eprintln!("kept the workspaces: {}", opts.scratch.display());
    } else {
        let _ = std::fs::remove_dir_all(&opts.scratch);
    }
    let first = |f: fn(&eval::RunResult) -> &str| {
        results
            .iter()
            .map(f)
            .find(|s| !s.is_empty())
            .unwrap_or("unknown")
            .to_owned()
    };
    let report = eval::Report {
        harness: concat!("quack ", env!("CARGO_PKG_VERSION")).into(),
        date: eval::today(),
        suite: suite
            .file_name()
            .map_or_else(|| "tasks".into(), |n| n.to_string_lossy().into_owned()),
        model: first(|r| r.model.as_str()),
        profile: first(|r| r.profile.as_str()),
        planner: args.planner.clone(),
        window: loaded.settings.context.window,
        sampling: results
            .iter()
            .find(|r| r.skipped.is_none() && !r.model.is_empty())
            .map(|r| eval::Report::sampling_of(&r.sampling))
            .unwrap_or_default(),
        results,
    };
    println!("{}", eval::table(&report));
    let out = args.out.unwrap_or_else(|| {
        let planned = report
            .planner
            .as_ref()
            .map(|p| format!("-planner-{p}"))
            .unwrap_or_default();
        // The default suite keeps its old names; another's results start with its name.
        let prefix = match report.suite.as_str() {
            "tasks" => String::new(),
            s => format!("{s}-"),
        };
        let window = report
            .window
            .map(|w| format!("-{}k", w / 1024))
            .unwrap_or_default();
        let name = format!(
            "{prefix}{}-{}{planned}{window}.json",
            report.model, report.profile
        )
        .replace(['/', '\\', ':'], "_");
        suite
            .parent()
            .unwrap_or(&suite)
            .join("results")
            .join(&report.date)
            .join(name)
    });
    let out = eval::unused_path(&out);
    let written = out
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| {
            std::fs::write(
                &out,
                serde_json::to_string_pretty(&report).unwrap_or_default() + "\n",
            )
        });
    match written {
        Ok(()) => {
            println!("results: {}", out.display());
            exit::OK
        }
        Err(e) => {
            eprintln!("error: writing {}: {e}", out.display());
            exit::AGENT_ERROR
        }
    }
}
