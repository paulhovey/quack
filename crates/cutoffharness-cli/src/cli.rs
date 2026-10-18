use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(
    name = "cutoffharness",
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
    /// Override a setting, e.g. -c ui.ascii=true. Repeatable.
    #[arg(short = 'c', long = "config", global = true, value_name = "KEY=VALUE")]
    pub overrides: Vec<String>,
    /// Resume the most recent session in this project.
    #[arg(long = "continue", global = true, conflicts_with = "resume")]
    pub continue_: bool,
    /// Resume a saved session by id (or unique prefix); `cutoffharness sessions` lists them.
    #[arg(long, global = true, value_name = "ID")]
    pub resume: Option<String>,
    /// Permission mode for this run; shorthand for -c permissions.mode=MODE.
    #[arg(long, global = true, value_enum)]
    pub permission_mode: Option<PermissionModeArg>,
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
}

impl PermissionModeArg {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::Ask => "ask",
            Self::AutoEdit => "auto-edit",
            Self::FullAuto => "full-auto",
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
    /// Store or remove API keys in the OS credential store, for `api_key = { keyring = "…" }`.
    Keyring {
        #[command(subcommand)]
        cmd: KeyringCmd,
    },
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

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum Format {
    Text,
    Json,
    StreamJson,
}

impl From<Format> for cutoffharness_exec::OutputFormat {
    fn from(f: Format) -> Self {
        match f {
            Format::Text => Self::Text,
            Format::Json => Self::Json,
            Format::StreamJson => Self::StreamJson,
        }
    }
}

#[derive(Debug, Subcommand)]
pub(crate) enum KeyringCmd {
    /// Store a secret. Reads it hidden from the terminal, or from stdin when piped.
    Set {
        /// `<service>/<user>`, or `<user>` under the `cutoffharness` service.
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
