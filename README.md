# quack

A terminal coding agent for open-weight models: self-hosted (llama.cpp, vLLM, Ollama,
SGLang, LM Studio) or hosted open-model services. Written in Rust with a full-screen
ratatui UI.

> **Status: in development.** Usable day to day with local servers and hosted services.
> See [docs/plan/roadmap.md](docs/plan/roadmap.md) for what is done and what is next.

`quack` is a working name, chosen so it is easy to find and replace.

## Features

Everything below works today. Planned work is listed separately at the end.

### Highlights

- **Made for open-weight models.** Works with local servers (llama.cpp, Ollama, vLLM,
  SGLang, LM Studio) and 13 hosted open-model services (OpenRouter, Fireworks, Together,
  DeepInfra, Novita, DeepSeek, Moonshot, Kimi Code, Z.ai, Z.ai Coding, Mistral, Synthetic,
  Charm Hyper).
- **Nothing built in.** Every provider, model profile and MCP server is a small TOML file.
  Delete the file and the capability is gone; startup stays clean (checked by a test for
  every default file).
- **No implicit egress.** The binary contacts only what you configure, checked by a test and
  by a CI run in a loopback-only network. Administrators can limit which hosts providers and
  MCP servers may reach.
- **Short provider files.** Only `base_url` is required. Models are discovered when none are
  listed, and one is picked for you.
- **Linux, macOS and Windows**, x64 and ARM64. Windows is a first-class target: shell
  commands run in Job Objects so they always die cleanly, and PowerShell is supported.
- **Knows when it broke the build.** Language servers report new errors in the result of
  the edit that caused them, so the model fixes them before moving on.
- **Sandboxed by default.** Shell commands can write only in the workspace and have no
  network (Landlock + seccomp on Linux, Seatbelt on macOS), with no helper programs to
  install.
- **Works with your existing agent files.** CLAUDE.md, GEMINI.md, `.cursorrules` and
  Copilot instructions are read, and `/init` merges them into one AGENTS.md.
- **Fast startup.** The first frame draws in about 3–9 ms on Linux.

### Getting started

- `quack setup` (or `/setup`) finds model servers running on this machine, or adds a hosted
  service with its key stored in the OS credential store.
- API keys come from an environment variable, a file, a command, or the OS keyring (Windows,
  macOS, and Linux through the Secret Service). `quack keyring set|delete` manages them.
- `quack doctor` (or `/doctor`) checks config, providers, the model, the project, MCP
  servers, language servers, AGENTS.md, the shell, the sandbox, git, the credential store,
  the sessions folder, memory and the terminal.

### Models

- Streaming replies, with reasoning ("thinking") shown, and retries with backoff that honour
  `Retry-After`.
- Thinking models keep their reasoning across tool calls (sent back as the server
  expects), which DeepSeek requires and MiniMax, Kimi and GLM rely on.
- Messy tool calls are repaired: Hermes and Qwen3-XML calls printed as text become real calls
  (and the raw markup never shows), loose arguments are parsed, duplicate call ids are fixed.
- Loop guards: repetition is caught, and an empty reply gets one nudge.
- A native Ollama adapter that sets the context window per request.
- Mistral's API differences are handled.
- `/models` lists every model of every provider; switch mid-conversation without losing
  history or approvals. `/providers` shows which are usable and why the others are not.
- Model profiles adapt prompts and tools per model family, using Jinja-style templates.
- Speed in the status bar: tokens per second and time to first token.

### Tools

- `read`, `write`, `glob`, `grep`, `list_dir`, `shell` and `lsp`.
- A `todo` plan: the model keeps a checklist for multi-step work, and the current plan is
  shown to it again at every step, so it stays on track through long tasks.
- A forgiving `edit`: exact match first, then ignoring trailing spaces, indentation and blank
  lines. CRLF line endings and BOMs are kept, and a file changed since the agent read it is
  not overwritten.
- Hash-anchored edits (`[tools] edit_format = "hashline"`): `read` tags each line with a
  short content hash, and edits name lines by tag instead of retyping old text. Tags follow
  lines that moved, and a stale tag is refused with a fresh view of that area.
- The shell runs bash, zsh or PowerShell. Timeouts kill the whole process tree, and long
  output is trimmed to its start and end.

### Code intelligence (LSP)

- Language servers for Rust, Go, Python, TypeScript/JavaScript (TypeScript 7's built-in
  server), C/C++, C#, Java, Lua, Bash, TOML and YAML, one small file each. A server is used
  only if it is already installed; quack never installs anything.
- Servers start in the background the first time the agent reads a file they cover, one
  per project folder, and restart if they crash.
- After every edit, new errors and warnings in that file, and new errors it caused in
  other files, are added to the edit's result. Pre-existing problems are not repeated,
  and fixed errors are reported as fixed.
- An `lsp` tool for the model: go to definition, find references, hover (types and
  docs), workspace symbol search, a file's outline, and current diagnostics. Positions
  are given as a line and a symbol name, which small models get right far more often
  than columns.
- Works with servers that push diagnostics and with servers that are asked for them.
- `/lsp` shows each server's state, indexing progress, error and warning counts and
  recent output; `quack doctor` lists which servers are installed.

### Safety

- Permission modes: `read-only`, `ask`, `auto-edit`, `full-auto` and `yolo`.
- `/yolo` toggles yolo mode on and off (back to the mode you were in); `--yolo` starts in
  it. Nothing asks and the sandbox is off; only the built-in deny list and your deny rules
  still apply. The status bar shows YOLO while it is on.
- Allow and deny rules for shell commands, edits, reads and MCP tools, on top of a built-in
  deny floor that cannot be removed.
- Compound commands are split: in `git status && rm -rf x` the `rm` still needs approval, and
  deny rules apply even in `full-auto`.
- Every change shows a diff in the approval box.
- Project trust: a cloned repo's `.quack/` folder runs nothing until you trust it. The prompt
  shows what it would add, and you are asked again when those files change. Also
  `quack trust` and `/trust`.
- Plan mode (`/plan`, Shift-Tab, or `--permission-mode plan`): the agent can look around
  but changes nothing until you accept the plan.
- OS sandbox for shell commands, on by default: commands write only in the workspace and
  temp folders and have no network. `[sandbox] mode` can also be `workspace-write-net`,
  `read-only` or `off`. Reading works everywhere and local sockets keep working. Extra
  writable folders can be listed, and administrators can lock the mode.
- Linux uses Landlock and seccomp, built in, with no extra programs or crates. macOS uses
  Seatbelt.
- If the sandbox blocks a command, the model is told and can ask to rerun it outside the
  sandbox. You are asked every time, even in `full-auto`.
- Local MCP servers and language servers can run in the sandbox too (`sandbox = true` in
  their file).
- Where the sandbox can't run (Windows for now, very old Linux kernels), the default runs
  commands without it and says so; a mode you set yourself fails closed, so every command
  asks first.

### Context

- Built to stay useful in small windows: superseded reads and large old tool outputs are
  pruned.
- Automatic compaction at a threshold or when the server reports the window is full, and
  `/compact [focus]` on demand. If the model cannot summarize, a plain list is kept.
- `/context` shows where the tokens go. Estimates are corrected with the token counts the
  server reports.

### Sessions and undo

- Every session is saved as it happens and survives a crash. `--continue`, `--resume` and
  `/resume` bring one back with exactly the same context.
- `/new`, `/title`, and `quack sessions`.
- `/undo` reverts the last edit.
- `/rewind` (or Esc Esc) goes back to an earlier message: the conversation, the files, or
  both. Files are restored byte for byte, and nothing is rewound if you changed them since
  (`--force` overrides).

### Instructions, skills and memory

- AGENTS.md from the system and user folders, the repo chain down to the current folder,
  nested folders as the agent reaches them, and `@` imports.
- Other agents' files are read where there is no AGENTS.md: CLAUDE.md, GEMINI.md,
  `.cursorrules`, `.windsurfrules`, `.github/copilot-instructions.md`, CONVENTIONS.md, and
  the global `~/.claude/CLAUDE.md` and `~/.gemini/GEMINI.md`. The list is a data file you
  can edit.
- `/init` writes AGENTS.md: it merges those files into it when there are any (and suggests
  pointing them at AGENTS.md), updates an existing one, or writes one from the repo.
- SKILL.md skills, loaded only when needed. `/skills` lists them and `/<skill-name>` runs one.
- Memory: markdown notes for each project and globally, with optional approval before a note
  is saved. `/memory` shows them.

### MCP

- Local (stdio) and remote (streamable HTTP) servers, one TOML file each.
- Large tool sets are deferred and found with a `tool_search` tool, so they do not fill the
  context.
- `/mcp` shows each server's state. Servers restart when they crash.

### Configuration

- Layers: system, user, project, environment and command line.
  `quack config show --origin` shows where each setting came from.
- Settings are checked against JSON schemas, with clear error messages (`quack config check`).
- `/config` lists every setting and changes them. Comments in your files are kept, and
  settings an administrator locked cannot be changed.
- `/reload` applies config changes without losing the conversation.

### Interface

- A full-screen terminal UI with markdown rendering, a slash-command palette and an ASCII
  mode.
- `/copy` copies through the terminal (OSC 52), so it works over SSH.
- `quack exec "..."` runs one turn without the UI, for scripts and CI.

### Project

- Apache-2.0.
- Few dependencies: each one is counted per OS before it is added (the keyring feature adds
  no crates on Linux).
- CI runs a scripted tool-call turn against real llama.cpp and Ollama servers.

### Planned

Not available yet:
- Editor integration (ACP).
- Sandboxing on Windows.
- Renaming symbols through the language server.
- Sub-agents and hooks.
- `/fork` and `/export`.
- Automatic reload when config files change.
- MCP prompts, resources and OAuth.

See the [roadmap](docs/plan/roadmap.md).

## Build and run

```sh
cargo run                                   # full-screen TUI
cargo run -- setup                          # add a provider
cargo run -- exec "explain src/main.rs"     # headless, one turn
cargo run -- doctor                         # check the installation
cargo run -- config check                   # validate every config layer
cargo run -- config show --origin           # effective settings and where each came from
cargo run -- config paths                   # resolved directories
QUACK_DEFAULTS_DIR=defaults cargo run       # use the defaults bundle from this checkout
```

## Layout

| Path | What |
|---|---|
| `crates/` | 17 crates; see [docs/plan/00-architecture.md](docs/plan/00-architecture.md) |
| `defaults/` | The separately shipped defaults bundle (providers, profiles, schemas, …) |
| `docs/plan/` | The implementation plan, one document per area |

## License

Apache-2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE).
