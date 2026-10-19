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
        "Each provider: usable or why not, and its models",
        None,
    ),
    c(
        "setup",
        &[],
        "",
        "Find model servers on this machine and add one as a provider",
        None,
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
        "[id]",
        "Browse and resume past sessions; with an id, resume directly",
        None,
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
        "[n]",
        "Go back to an earlier message: conversation, files or both (Esc Esc)",
        None,
    ),
    c(
        "undo",
        &[],
        "[--force]",
        "Revert the files changed by the last edit (not shell commands)",
        None,
    ),
    c(
        "fork",
        &[],
        "[--worktree]",
        "Branch the session from here",
        Some("M3"),
    ),
    c(
        "plan",
        &[],
        "[task|accept|off]",
        "Plan mode: look around without changing anything, then accept or revise the plan",
        None,
    ),
    c("title", &[], "<text>", "Rename the session", None),
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
        "Summarize earlier conversation now, keeping the current turn",
        None,
    ),
    c(
        "context",
        &[],
        "",
        "What is using the context, segment by segment",
        None,
    ),
    c(
        "config",
        &["settings"],
        "[set <key> <value>]",
        "Settings with their origins; saves changes to the user config.toml",
        None,
    ),
    c(
        "bundles",
        &[],
        "",
        "Installed bundles of skills, commands and servers",
        Some("M3"),
    ),
    c(
        "permissions",
        &[],
        "[mode]",
        "Switch the permission mode; see the allow/ask/deny rules",
        None,
    ),
    c(
        "yolo",
        &[],
        "[on|off]",
        "Toggle yolo: nothing asks and no sandbox (the built-in deny list still applies)",
        None,
    ),
    c(
        "mcp",
        &[],
        "[restart <id>]",
        "MCP servers: state, tools, token cost, logs; ^s restarts one",
        None,
    ),
    c(
        "lsp",
        &[],
        "",
        "Language servers: state, indexing, diagnostics",
        None,
    ),
    c(
        "skills",
        &[],
        "",
        "Installed skills; run one with /<name> [request]",
        None,
    ),
    c("commands", &[], "", "Custom commands by layer", Some("M2")),
    c(
        "memory",
        &[],
        "",
        "Memory notes: read them, delete with ^s",
        None,
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
        "Write AGENTS.md, merging CLAUDE.md and other agents' files into it",
        None,
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
    c(
        "doctor",
        &[],
        "",
        "Environment checks: config, providers, shell, git, storage",
        None,
    ),
    c(
        "stats",
        &[],
        "",
        "This session: tokens, cost, cache hits, where the time went, tools used",
        None,
    ),
    c(
        "reload",
        &[],
        "",
        "Read config, providers, MCP, language servers and skills again",
        None,
    ),
    c(
        "trust",
        &[],
        "[trusted|settings-only|ignored]",
        "Decide whether this repo's .quack/ loads",
        None,
    ),
    c(
        "cd",
        &[],
        "<path>",
        "Change the workspace directory",
        Some("M3"),
    ),
    c(
        "thinking",
        &["think"],
        "[show|hide]",
        "Show or hide the model's thinking, when it sends any (Ctrl-T)",
        None,
    ),
    c(
        "copy",
        &[],
        "[code]",
        "Copy the last response, or its last code block, to the clipboard",
        None,
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
    ("PageUp / PageDown", "Scroll the transcript a page"),
    ("Shift-Up / Shift-Down", "Scroll the transcript a line"),
    (
        "Ctrl-Home / Ctrl-End",
        "Jump to the top or bottom of the transcript",
    ),
    ("Ctrl-T", "Show or hide the model's thinking"),
    ("/", "Slash commands; Tab completes"),
    ("Ctrl-P", "Command palette (opens help for now)"),
    ("Shift-Tab", "Cycle mode: ask → auto-edit → plan"),
    ("Tab (in modal)", "Switch tab"),
    (
        "Ctrl-S (in modal)",
        "Secondary action, e.g. save as default in /models",
    ),
];
