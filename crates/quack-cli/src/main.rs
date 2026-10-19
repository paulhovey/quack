//! The `quack` binary: parses arguments, loads config synchronously (tiny files
//! only), starts the engine and hands it to a client.

#![allow(clippy::print_stdout, clippy::print_stderr)]

mod cli;
mod commands;
mod logging;
mod setup;

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;

use clap::Parser;
use quack_config::LoadOptions;
use quack_core::{Engine, Resume};
use quack_exec::exit;

use cli::{Cli, Cmd, ConfigCmd, KeyringCmd};

fn main() -> ExitCode {
    let started = Instant::now();
    let cli = Cli::parse();
    let cwd = match cli.global.cd.clone().map_or_else(std::env::current_dir, Ok) {
        Ok(dir) => dir,
        Err(e) => {
            eprintln!("error: cannot determine working directory: {e}");
            return code(exit::CONFIG_ERROR);
        }
    };
    let mut overrides = cli.global.overrides.clone();
    if let Some(p) = &cli.global.profile {
        overrides.push(format!("model.profile={}", toml_string(p)));
    }
    let mode = match (cli.global.permission_mode, cli.global.yolo) {
        (_, true) => Some(cli::PermissionModeArg::Yolo),
        (m, false) => m,
    };
    // Plan mode is an overlay on the configured mode, set once the engine runs.
    let plan = matches!(mode, Some(cli::PermissionModeArg::Plan));
    if let Some(mode) = mode.filter(|_| !plan) {
        overrides.push(format!("permissions.mode=\"{}\"", mode.as_str()));
    }
    let opts = LoadOptions {
        cwd,
        model: cli.global.model.clone(),
        overrides,
    };
    let loaded = match quack_config::load(&opts) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("{:?}", miette::Report::new(e));
            return code(exit::CONFIG_ERROR);
        }
    };
    let config_loaded = started.elapsed();
    let _log_guard = logging::init(&loaded.dirs.log_dir());
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "starting");

    let resume = match (&cli.global.resume, cli.global.continue_) {
        (Some(id), _) => Resume::Id(id.clone()),
        (None, true) => Resume::Latest,
        (None, false) => Resume::New,
    };
    let command = match (cli.command, cli.print) {
        (Some(cmd), _) => cmd,
        (None, Some(prompt)) => Cmd::Exec(cli::ExecArgs {
            prompt,
            output_format: cli::Format::Text,
        }),
        (None, None) => Cmd::Tui,
    };

    match command {
        Cmd::Config { cmd } => code(match cmd {
            ConfigCmd::Check => commands::config_check(&loaded),
            ConfigCmd::Show { origin } => commands::config_show(&loaded, origin),
            ConfigCmd::Schema { dir } => commands::config_schema(&loaded, dir),
            ConfigCmd::Paths => commands::config_paths(&loaded),
        }),
        Cmd::Doctor => block_on(async move {
            let checks = quack_core::doctor::run(Arc::new(loaded)).await;
            commands::doctor(&checks)
        }),
        Cmd::Setup => block_on(async move {
            let stdin = std::io::stdin();
            let mut io = setup::Io {
                input: &mut stdin.lock(),
                out: &mut std::io::stdout(),
                secret: &commands::read_secret,
            };
            setup::run(Arc::new(loaded), &mut io).await
        }),
        Cmd::Sessions => code(commands::sessions(&loaded)),
        Cmd::Trust { decision } => code(commands::trust(&loaded, decision.map(Into::into))),
        Cmd::Keyring { cmd } => code(match cmd {
            KeyringCmd::Set { name } => commands::keyring_set(&name),
            KeyringCmd::Delete { name } => commands::keyring_delete(&name),
        }),
        Cmd::Eval(args) => block_on(commands::eval(&loaded, opts.clone(), args)),
        #[cfg(feature = "acp")]
        Cmd::Acp => {
            eprintln!("ACP support lands in M6 (docs/plan/12-headless-and-acp.md)");
            code(exit::AGENT_ERROR)
        }
        Cmd::Tui => block_on(async move {
            let config = Arc::new(loaded);
            let engine = Engine::spawn_with(config.clone(), resume);
            if plan {
                engine.submit(plan_op()).await;
            }
            let trace = cli
                .global
                .trace_startup
                .map(|path| quack_tui::StartupTrace {
                    started,
                    config_loaded,
                    path,
                });
            match quack_tui::run(engine, config, trace).await {
                Ok(()) => exit::OK,
                Err(e) => {
                    eprintln!("error: terminal: {e}");
                    exit::AGENT_ERROR
                }
            }
        }),
        Cmd::Exec(args) => {
            let prompt = match commands::read_prompt(&args.prompt) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("error: reading prompt from stdin: {e}");
                    return code(exit::AGENT_ERROR);
                }
            };
            block_on(async move {
                let engine = Engine::spawn_with(Arc::new(loaded), resume);
                if plan {
                    engine.submit(plan_op()).await;
                }
                quack_exec::run(engine, prompt, args.output_format.into()).await
            })
        }
    }
}

/// A TOML string literal (a JSON string is one).
fn toml_string(s: &str) -> String {
    serde_json::Value::String(s.to_owned()).to_string()
}

fn plan_op() -> quack_core::protocol::Op {
    quack_core::protocol::Op::SetPermissionMode {
        mode: "plan".into(),
    }
}

fn block_on(fut: impl Future<Output = i32>) -> ExitCode {
    match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => code(rt.block_on(fut)),
        Err(e) => {
            eprintln!("error: cannot start async runtime: {e}");
            code(exit::AGENT_ERROR)
        }
    }
}

fn code(c: i32) -> ExitCode {
    ExitCode::from(u8::try_from(c).unwrap_or(1))
}
