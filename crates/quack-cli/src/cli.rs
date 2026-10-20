use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(
    name = "quack",
    version,
    about = "A coding agent for open-weight models"
)]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub command: Option<Cmd>,
    #[command(flatten)]
    pub global: Global,
    /// Run one prompt headless and print the answer (same as `exec`).
    #[arg(short = 'p', long = "print", value_name = "PROMPT")]
    pub print: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct Global {
    /// Run as if started in this directory.
    #[arg(short = 'C', long = "cd", global = true, value_name = "DIR")]
    pub cd: Option<PathBuf>,
    /// Model as <provider>/<model>; overrides [model].default.
    #[arg(short = 'm', long, global = true)]
    pub model: Option<String>,
    /// Profile for this run, e.g. `qwen-code`; shorthand for -c model.profile=ID.
    #[arg(long, global = true, value_name = "ID")]
    pub profile: Option<String>,
    /// Override a setting, e.g. -c ui.ascii=true. Repeatable.
    #[arg(short = 'c', long = "config", global = true, value_name = "KEY=VALUE")]
    pub overrides: Vec<String>,
    /// Resume the most recent session in this project.
    #[arg(long = "continue", global = true, conflicts_with = "resume")]
    pub continue_: bool,
    /// Resume a saved session by id (or unique prefix); `quack sessions` lists them.
    #[arg(long, global = true, value_name = "ID")]
    pub resume: Option<String>,
    /// Permission mode for this run; shorthand for -c permissions.mode=MODE.
    #[arg(long, global = true, value_enum)]
    pub permission_mode: Option<PermissionModeArg>,
    /// Shorthand for --permission-mode yolo: nothing asks, no sandbox.
    #[arg(long, global = true, conflicts_with = "permission_mode")]
    pub yolo: bool,
    /// Draw the first frame, write startup timings as JSON to FILE, and exit.
    #[arg(long, hide = true, value_name = "FILE")]
    pub trace_startup: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum PermissionModeArg {
    ReadOnly,
    Ask,
    AutoEdit,
    FullAuto,
    /// No approvals and no sandbox; only the built-in deny list and deny rules apply.
    Yolo,
    /// Plan mode: look around and propose a plan; nothing changes until it is accepted.
    Plan,
}

impl PermissionModeArg {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::Ask => "ask",
            Self::AutoEdit => "auto-edit",
            Self::FullAuto => "full-auto",
            Self::Yolo => "yolo",
            Self::Plan => "plan",
        }
    }
}

#[derive(Debug, Subcommand)]
pub(crate) enum Cmd {
    /// Full-screen terminal UI (the default).
    Tui,
    /// Run one prompt headless.
    Exec(ExecArgs),
    /// Inspect and validate configuration.
    Config {
        #[command(subcommand)]
        cmd: ConfigCmd,
    },
    /// List saved sessions for this project.
    Sessions,
    /// Check the environment: config, directories, terminal.
    Doctor,
    /// Add a provider (a server on this machine, a hosted service, or a URL) and pick the
    /// default model.
    Setup,
    /// Show or decide whether this repo's `.quack/` loads. Without a decision,
    /// prints what it holds and the current state.
    Trust {
        #[arg(value_enum)]
        decision: Option<TrustArg>,
    },
    /// Store or remove API keys in the OS credential store, for `api_key = { keyring = "…" }`.
    Keyring {
        #[command(subcommand)]
        cmd: KeyringCmd,
    },
    /// Run the eval task suite with the selected model and profile.
    Eval(EvalArgs),
    /// Run as a Zed Agent Client Protocol agent on stdio.
    #[cfg(feature = "acp")]
    Acp,
}

#[derive(Debug, Args)]
pub(crate) struct ExecArgs {
    /// The prompt; `-` reads it from stdin.
    pub prompt: String,
    #[arg(long, value_enum, default_value_t = Format::Text)]
    pub output_format: Format,
}

#[derive(Debug, Args)]
pub(crate) struct EvalArgs {
    /// The task suite: a folder of task folders.
    #[arg(long, default_value = "evals/tasks")]
    pub suite: PathBuf,
    /// Only these tasks (folder names), comma-separated.
    #[arg(long, value_delimiter = ',')]
    pub tasks: Vec<String>,
    /// Run each task this many times.
    #[arg(long, default_value_t = 1)]
    pub repeat: u32,
    /// Where to write the JSON results. Default:
    /// `evals/results/<date>/<model>-<profile>.json` next to the suite.
    #[arg(long)]
    pub out: Option<PathBuf>,
    /// Keep each run's workspace (under the cache directory) to look at afterwards.
    #[arg(long)]
    pub keep: bool,
    /// Plan each task with this `<provider>/<model>` in plan mode, then accept the plan
    /// and let the `-m` model carry it out (`[model] planner`).
    #[arg(long)]
    pub planner: Option<String>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum Format {
    Text,
    Json,
    StreamJson,
}

impl From<Format> for quack_exec::OutputFormat {
    fn from(f: Format) -> Self {
        match f {
            Format::Text => Self::Text,
            Format::Json => Self::Json,
            Format::StreamJson => Self::StreamJson,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum TrustArg {
    /// Load everything, including MCP servers and hooks that start programs.
    Trusted,
    /// Load `config.toml` only.
    SettingsOnly,
    /// Never load it, and don't ask again.
    Ignored,
}

impl From<TrustArg> for quack_config::TrustDecision {
    fn from(t: TrustArg) -> Self {
        match t {
            TrustArg::Trusted => Self::Trusted,
            TrustArg::SettingsOnly => Self::SettingsOnly,
            TrustArg::Ignored => Self::Ignored,
        }
    }
}

#[derive(Debug, Subcommand)]
pub(crate) enum KeyringCmd {
    /// Store a secret. Reads it hidden from the terminal, or from stdin when piped.
    Set {
        /// `<service>/<user>`, or `<user>` under the `quack` service.
        name: String,
    },
    /// Remove a stored secret.
    Delete { name: String },
}

#[derive(Debug, Subcommand)]
pub(crate) enum ConfigCmd {
    /// Validate every layer and item file; non-zero exit on errors.
    Check,
    /// Print the effective settings.
    Show {
        /// Annotate each value with the layer it came from.
        #[arg(long)]
        origin: bool,
    },
    /// Write JSON Schema files for editor completion.
    Schema {
        /// Target directory (default: <user config>/schemas).
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// Print the resolved directories.
    Paths,
}
