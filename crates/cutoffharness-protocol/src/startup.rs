use serde::{Deserialize, Serialize};

/// A subsystem that reports progress during startup. One row each in the startup panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Subsystem {
    Config,
    Project,
    Provider,
    Profile,
    Tokenizer,
    Instructions,
    Compat,
    Memory,
    Mcp,
    Lsp,
}

impl Subsystem {
    /// All subsystems, in startup panel order.
    pub const ALL: [Subsystem; 10] = [
        Self::Config,
        Self::Project,
        Self::Provider,
        Self::Profile,
        Self::Tokenizer,
        Self::Instructions,
        Self::Compat,
        Self::Memory,
        Self::Mcp,
        Self::Lsp,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Config => "config",
            Self::Project => "project",
            Self::Provider => "provider",
            Self::Profile => "profile",
            Self::Tokenizer => "tokenizer",
            Self::Instructions => "instructions",
            Self::Compat => "compat",
            Self::Memory => "memory",
            Self::Mcp => "mcp",
            Self::Lsp => "lsp",
        }
    }
}

/// Progress of one subsystem.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum LoadState {
    Pending,
    Running {
        detail: String,
    },
    Ready {
        detail: String,
    },
    /// Waiting on a user decision (project trust, compat files). Never blocks input.
    NeedsAttention {
        detail: String,
    },
    Failed {
        error: String,
    },
    Disabled {
        reason: String,
    },
}

impl LoadState {
    /// True once the subsystem will make no further progress without user action.
    pub fn is_settled(&self) -> bool {
        !matches!(self, Self::Pending | Self::Running { .. })
    }
}
