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
- **Fast startup.** The first frame draws in about 3–9 ms on Linux.

### Getting started

- `quack setup` (or `/setup`) finds model servers running on this machine, or adds a hosted
  service with its key stored in the OS credential store.
- API keys come from an environment variable, a file, a command, or the OS keyring (Windows,
  macOS, and Linux through the Secret Service). `quack keyring set|delete` manages them.
- `quack doctor` (or `/doctor`) checks config, providers, the model, the project, AGENTS.md,
  the shell, git, the credential store, the sessions folder, memory and the terminal.

### Models

- Streaming replies, with reasoning ("thinking") shown, and retries with backoff that honour
  `Retry-After`.
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

- `read`, `write`, `glob`, `grep`, `list_dir` and `shell`.
- A forgiving `edit`: exact match first, then ignoring trailing spaces, indentation and blank
  lines. CRLF line endings and BOMs are kept, and a file changed since the agent read it is
  not overwritten.
- The shell runs bash, zsh or PowerShell. Timeouts kill the whole process tree, and long
  output is trimmed to its start and end.

### Safety

- Permission modes: `read-only`, `ask`, `auto-edit` and `full-auto`.
- Allow and deny rules for shell commands, edits, reads and MCP tools, on top of a built-in
  deny floor that cannot be removed.
- Compound commands are split: in `git status && rm -rf x` the `rm` still needs approval, and
  deny rules apply even in `full-auto`.
- Every change shows a diff in the approval box.
- Project trust: a cloned repo's `.quack/` folder runs nothing until you trust it. The prompt
  shows what it would add, and you are asked again when those files change. Also
  `quack trust` and `/trust`.
- Plan mode (`/plan` or Shift-Tab): the agent can look around but changes nothing until you
  accept the plan.

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

Not available yet: editor integration (ACP), language servers (LSP), OS sandboxing,
sub-agents, hooks, `/fork` and `/export`, automatic reload when config files change, and
MCP prompts, resources and OAuth. See the [roadmap](docs/plan/roadmap.md).

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
