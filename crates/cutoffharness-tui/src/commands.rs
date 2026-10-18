//! Built-in slash command catalogue (docs/plan/06-tui.md#command-catalogue).

#[derive(Debug, Clone, Copy)]
pub(crate) struct Command {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub args: &'static str,
    pub description: &'static str,
    /// `None` when implemented; otherwise the milestone it lands in.
    pub pending: Option<&'static str>,
}

impl Command {
    pub fn matches(&self, name: &str) -> bool {
        self.name == name || self.aliases.contains(&name)
    }
}

const fn c(
    name: &'static str,
    aliases: &'static [&'static str],
    args: &'static str,
    description: &'static str,
    pending: Option<&'static str>,
) -> Command {
    Command {
        name,
        aliases,
        args,
        description,
        pending,
    }
}

pub(crate) const COMMANDS: &[Command] = &[
    c(
        "models",
        &["model"],
        "[provider/model]",
        "Pick a model for this session; with an argument, switch directly",
        None,
    ),
    c(
        "providers",
        &[],
        "",
        "Provider state, auth and connection tests; add a provider",
        Some("M1"),
    ),
    c(
        "profiles",
        &["profile"],
        "",
        "Model profiles: switch, pin, preview the rendered prompt",
        Some("M2"),
    ),
    c(
        "resume",
        &["sessions"],
        "",
        "Browse and resume past sessions",
        Some("M1"),
    ),
    c(
        "new",
        &["clear"],
        "",
        "Start a fresh session (the current one stays saved)",
        None,
    ),
    c(
        "rewind",
        &[],
        "",
        "Go back to an earlier message: conversation, files or both",
        Some("M3"),
    ),
    c(
        "undo",
        &[],
        "",
        "Revert the last mutating tool call's file changes",
        Some("M3"),
    ),
    c("fork", &[], "", "Branch the session from here", Some("M3")),
    c("title", &[], "<text>", "Rename the session", Some("M1")),
    c(
        "export",
        &[],
        "[md|json]",
        "Write the transcript to a file",
        Some("M3"),
    ),
    c(
        "compact",
        &[],
        "[focus]",
        "Compact the conversation now",
        Some("M2"),
    ),
    c(
        "context",
        &[],
        "",
        "Context breakdown: segments, tokens, cache state",
        Some("M2"),
    ),
    c(
        "config",
        &["settings"],
        "",
        "Settings editor with value origins",
        Some("M1"),
    ),
    c(
        "permissions",
        &[],
        "",
        "Permission mode and allow/ask/deny rules",
        Some("M1"),
    ),
    c(
        "mcp",
        &[],
        "",
        "MCP servers: state, tools, token cost, logs",
        Some("M3"),
    ),
    c(
        "lsp",
        &[],
        "",
        "Language servers: state, indexing, diagnostics",
        Some("M4"),
    ),
    c(
        "skills",
        &[],
        "",
        "Skills by layer: view, enable, invoke",
        Some("M2"),
    ),
    c("commands", &[], "", "Custom commands by layer", Some("M2")),
    c(
        "memory",
        &[],
        "",
        "Browse and edit memory notes",
        Some("M2"),
    ),
    c("hooks", &[], "", "Hooks and their last results", Some("M3")),
    c(
        "instructions",
        &[],
        "",
        "Loaded AGENTS.md chain with sizes",
        Some("M2"),
    ),
    c(
        "init",
        &[],
        "",
        "Generate an AGENTS.md for this repository",
        Some("M2"),
    ),
    c("todo", &[], "", "Toggle the todo pane", Some("M2")),
    c("tasks", &[], "", "Sub-agent views", Some("M5")),
    c(
        "theme",
        &[],
        "",
        "Theme picker with live preview",
        Some("M3"),
    ),
    c("keys", &[], "", "Keybinding viewer and editor", Some("M3")),
    c("doctor", &[], "", "Run environment checks", Some("M1")),
    c(
        "stats",
        &[],
        "",
        "Session usage: tokens, cache hits, TTFT, cost",
        Some("M2"),
    ),
    c(
        "reload",
        &[],
        "",
        "Reload config, instructions and skills",
        Some("M1"),
    ),
    c(
        "cd",
        &[],
        "<path>",
        "Change the workspace directory",
        Some("M3"),
    ),
    c(
        "copy",
        &[],
        "",
        "Copy the last response or a code block",
        Some("M1"),
    ),
    c("help", &["?"], "", "Commands and keys", None),
    c("quit", &["exit"], "", "Quit", None),
];

pub(crate) fn find(name: &str) -> Option<&'static Command> {
    COMMANDS.iter().find(|c| c.matches(name))
}

/// Default key bindings, for the help modal.
pub(crate) const KEYS: &[(&str, &str)] = &[
    ("Enter", "Send the message"),
    ("Shift-Enter / Alt-Enter / Ctrl-J", "New line"),
    ("Esc", "Close modal or popup; otherwise interrupt the turn"),
    ("Ctrl-C", "Clear input, interrupt, or press twice to quit"),
    ("Up / Down", "Input history"),
    ("PageUp / PageDown", "Scroll the transcript"),
    ("/", "Slash commands; Tab completes"),
    ("Ctrl-P", "Command palette (opens help for now)"),
    ("Tab (in modal)", "Switch tab"),
];
