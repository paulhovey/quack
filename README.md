# quack

A terminal coding agent for open-weight models: self-hosted (llama.cpp, vLLM, Ollama,
SGLang, LM Studio) or hosted open-model services. It also works with the Claude and OpenAI
APIs and with a ChatGPT plan, each off until you set it up. Written in Rust with a
full-screen ratatui UI.

> **Status: in development.** Usable day to day with local servers and hosted services.
> See [docs/plan/roadmap.md](docs/plan/roadmap.md) for what is done and what is next.

`quack` is a working name, chosen so it is easy to find and replace.

## Features

Everything below works today. Planned work is listed separately at the end.

### Highlights

- **Made for open-weight models.** Works with local servers (llama.cpp, Ollama, vLLM,
  SGLang, LM Studio) and 12 hosted open-model services (OpenRouter, Fireworks, Together,
  DeepInfra, Novita, DeepSeek, Moonshot, Kimi Code, Z.ai, Mistral, Synthetic, Charm Hyper).
  The Z.ai GLM Coding Plan can't be offered: Z.ai allows it only in
  [its listed tools](https://docs.z.ai/devpack/tool/others). Use the pay-as-you-go Z.ai API
  instead.
- **Claude and GPT too, if you want them.** The Anthropic and OpenAI APIs with a key, or a
  ChatGPT Plus or Pro plan through `quack login chatgpt`. Their reasoning is kept across
  steps the way each API requires. Claude subscriptions can't be offered: Anthropic allows
  them only in its own apps.
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

- Run `quack`. If no model is ready, `/setup` opens on its own: it finds model servers on this
  machine, adds a hosted service with its key stored in the OS credential store, or adds any
  OpenAI-compatible server by its URL. `quack setup` asks the same questions in a plain terminal.
- API keys come from an environment variable, a file, a command, or the OS keyring (Windows,
  macOS, and Linux through the Secret Service). `quack keyring set|delete` manages them.
- `quack login chatgpt` signs in to a ChatGPT plan in the browser; `quack logout chatgpt`
  revokes it. The sign-in is saved in the data folder, readable by you only, and renewed
  while in use.
- `quack doctor` (or `/doctor`) checks config, providers, the model, the project, MCP
  servers, language servers, AGENTS.md, the shell, the sandbox, git, the credential store,
  the sessions folder, memory and the terminal.

### Models

- Streaming replies, with reasoning ("thinking") shown as a one-line summary that Ctrl-T
  (or `/thinking`) expands, and retries with backoff that honour `Retry-After`. Set
  `ui.show_reasoning` to `expanded` or `hidden` to change how it starts.
- Thinking models keep their reasoning across tool calls (sent back as the server
  expects), which DeepSeek requires and MiniMax, Kimi and GLM rely on.
- Messy tool calls are repaired: calls printed as text in the Hermes, Qwen3-XML, GLM,
  DeepSeek or Kimi formats become real calls (and the raw markup never shows), loose
  arguments are parsed, duplicate call ids are fixed. Fenced JSON calls are taken too when
  a model's `tool_calling.text_format = "json-block"`.
- Loop guards: repetition is caught, and an empty reply gets one nudge.
- A native Ollama adapter that sets the context window per request.
- Besides OpenAI-compatible chat: the Anthropic Messages API (`api = "anthropic-messages"`,
  which several open-model hosts also serve) and the OpenAI Responses API
  (`api = "openai-responses"`). Claude's signed thinking and OpenAI's encrypted reasoning go
  back unchanged with each request, so the model keeps its reasoning between steps and
  across `/resume`. With Claude the history only ever grows, which that requires and which
  keeps the prompt cache warm. Cache writes are priced apart where the API bills them.
- Mistral's API differences are handled.
- `/models` lists every model of every provider; switch mid-conversation without losing
  history or approvals. `/providers` shows which are usable and why the others are not.
- quack starts with the model you last switched to (remembered in `state.toml` in the data
  folder), falling back to `model.default` when it is unavailable. A project's
  `model.default`, `QUACK_MODEL` and `-m` still win; `model.remember_last = false` turns
  it off.
- Model profiles adapt prompts and tools per model family, using Jinja-style templates:
  tool names, parameter names, descriptions, the edit format and sampling. Besides
  `generic`, the bundle has `qwen-code`, `cc-style` (GLM, MiniMax, DeepSeek, MiMo), `zcode`
  (GLM, Z.ai's ZCode shapes), `kimi-code`, `codex` (gpt-oss), `dsh` (DeepSeek Harness
  shapes) and `vibe` (Mistral's Vibe shapes). They are used only when chosen (`profile = "qwen-code"`
  on a model, `--profile`, `[model] profile`, or `/profiles`) until the eval shows they beat
  `generic`. The per-session part of the prompt (workspace, date, AGENTS.md, memory) goes
  at the start of the first user message (`context.md`), so the system prompt and tools are
  the same in every project and hosted servers' prompt caches reuse them. `/profiles` previews each profile's rendered prompt and tool names for the
  current model, switches the session to one (Enter), or saves it as the default (Ctrl-S).
- `quack eval` runs a task suite (`evals/tasks`, 21 small tasks: finding things, single and
  multi-file edits, fixing failing tests, shell, long files) with a model and profile, and
  reports pass rate, steps, tool errors, unparsable calls, failed edits, tokens and time.
  `--suite evals/hard` runs 14 longer tasks with hidden tests, most on one shared shop
  project, meant to separate models and profiles that all pass the small suite. Each
  failed run is labelled with why (`no-change`, `wrong-files`, `edit-failed`, `untested`,
  `wrong-fix`, ...), and `scripts/eval-compare.py` compares two setups task by task with
  confidence intervals. `--planner <provider/model>` plans each task with that model first.
- Speed in the status bar: tokens per second and time to first token.

### Tools

- `read`, `write`, `glob`, `grep`, `list_dir`, `shell`, `lsp` and `index`.
- `index` outlines a file (functions, types, methods with their line ranges, nested) or a
  whole folder, for Rust, Python, JavaScript, TypeScript, Go (with receiver types, e.g. `Scope.Define`), C, C++ and shell scripts (tree-sitter). `read` of a
  large source file without a range returns that outline instead of the whole file, and
  `read` with `symbol = "Type::method"` reads one definition. `[tools] read_outline_tokens`
  sets the size (2,000 tokens; 0 turns it off).
- Your own tree-sitter queries without recompiling: put `tags.scm` (outlines) or
  `highlights.scm` (TUI colours) in `grammars/<lang>/` of the user config dir or a trusted
  project's `.quack/`. A file replaces the built-in query, or adds to it when its first
  line is `; extends`, so a project can outline a framework's macros (`route!(index)`) as
  definitions. A query that doesn't compile falls back to the built-in one, and
  `quack doctor` says why.
- A `todo` plan: the model keeps a checklist for multi-step work, and the current plan is
  shown to it again at every step, so it stays on track through long tasks. A line above
  the input shows progress and the step in hand; `/todo` expands it into the whole list.
- Sub-agents: the `task` tool starts an agent from `agents.d/` with a fresh context, its
  own todo list and a narrower set of tools, and only its final report comes back, so the
  main conversation stays small. `explore` searches read-only (on `[model] small` when
  set), and several started in one reply run in parallel. `general` makes a
  self-contained change, one at a time, with its edits asking for approval like any
  other. A task can carry out one item of the plan (`todo: 3`), which ticks off when it
  succeeds. Its steps show under the `task` line, and `/tasks` lists every run with
  what it did and its report. Add your own agents as two-line TOML files.
- A forgiving `edit`: exact match first, then ignoring trailing spaces, indentation and blank
  lines. CRLF line endings and BOMs are kept, and a file changed since the agent read it is
  not overwritten.
- Edit formats, set by `[tools] edit_format` or by the model's profile:
  - `hashline`: `read` and `grep` put a `[path#tag]` header over
    each file's numbered lines, and edits name lines by number with that tag instead of
    retyping old text. `block` edits name a line and take its whole function or class,
    so no end line has to be counted. Numbers from an earlier read follow the lines that
    moved; lines that changed since, or that no output showed, are refused with a fresh
    view of that area (the retry then works). A lone off-by-one range that orphans a
    closing bracket is mended when that is what makes the file parse again.
  - `str_replace`: the `edit` tool above (replace unique snippet).
  - `apply_patch`: the Codex patch format, adding, deleting, moving and changing several
    files in one call.
  - `search_replace_blocks`: SEARCH/REPLACE blocks under each file's path (`edit_blocks`).
  - `whole_file`: no edit tool; files are rewritten with `write`.

  Multi-file changes are checked whole before anything is written, and `/undo` puts back
  every file a call changed.
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
  still apply. The status bar shows a red YOLO badge while it is on.
- Allow and deny rules for shell commands, edits, reads and MCP tools, on top of a built-in
  deny floor that cannot be removed.
- Compound commands are split: in `git status && rm -rf x` the `rm` still needs approval, and
  deny rules apply even in `full-auto`. Commands are parsed with tree-sitter-bash, so those
  inside subshells, `if`/`for`/`while` bodies, `$(…)`, backticks, heredocs and literal
  `bash -c '…'` scripts are checked one by one; a line that doesn't parse, or runs a
  computed command name (`$cmd -rf x`), never matches an allow rule.
- Every change shows a diff in the approval box.
- Project trust: a cloned repo's `.quack/` folder runs nothing until you trust it. The prompt
  shows what it would add, and you are asked again when those files change. Also
  `quack trust` and `/trust`.
- Plan mode (`/plan`, Shift-Tab, or `--permission-mode plan`): the agent looks around, asks
  you about the few big decisions (with options and its recommendation; pick one, type your
  own answer, or skip), and proposes the plan as a checklist. A bar then offers to carry it
  out, carry it out with edits allowed, or keep planning; just type to ask for changes.
  Nothing changes until you accept. With `[model] planner = "<provider>/<model>"`, plan
  mode runs on that model (say, a large one) and your usual model carries the plan out.
  Accepting hands it one message (your requests, your answers, the plan) instead of the
  planner's transcript, so its prompt cache still holds. Read-only sub-agents (`explore`)
  run in plan mode too, so the planner can search widely without filling its context.
- `ask_user`: the agent can ask you questions in any mode, the same way. Headless runs skip
  them and the agent says what it assumed.
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
- `/sandbox` (or `/sandbox off|on`) turns the sandbox off for the rest of the session and
  back on, without changing the permission mode: plan, ask and full-auto keep asking what
  they asked before, but commands run unsandboxed. The status bar shows "no sandbox" while
  it is off. A system policy that locks `sandbox.mode` refuses it.

### Context

- Built to stay useful in small windows. Before compacting, old tool results are pruned:
  reads superseded by a later read or edit, reads of files changed on disk since, old
  whole-file reads (only their outline is kept), failed attempts that a later call fixed,
  and large old outputs (cut to their start and end).
- Automatic compaction at a threshold or when the server reports the window is full, and
  `/compact [focus]` on demand. If the model cannot summarize, a plain list is kept.
- `/context` shows where the tokens go as one band, each part in its own fill, with the
  compaction point marked, and charts the context turn by turn against that point. It
  also says how many turns are left at the recent pace and what would free the most room. Estimates are corrected with the token counts the server
  reports.
- `/stats` adds up the session: tokens sent and received, cache hits, cost, where the time
  went (the model, tools, waiting for your approval), latency and speed, and each tool's
  calls and failures, drawn as charts: headline figures, bars, sparklines of every
  request and the cost climbing over the session. Totals survive `/resume`. Cost comes from the price the provider's
  models endpoint lists (Charm Hyper, OpenRouter and others do), or from the model's price per
  million tokens in its provider file, which wins: `cost = { input = 0.3, output = 1.2 }`.

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
- `/instructions` shows which of these files the model has, how big each is, and its text.
- Other agents' files are read where there is no AGENTS.md: CLAUDE.md, GEMINI.md,
  `.cursorrules`, `.windsurfrules`, `.github/copilot-instructions.md`, CONVENTIONS.md, and
  the global `~/.claude/CLAUDE.md` and `~/.gemini/GEMINI.md`. The list is a data file you
  can edit.
- `/init` writes AGENTS.md: it merges those files into it when there are any (and suggests
  pointing them at AGENTS.md), updates an existing one, or writes one from the repo.
- SKILL.md skills, loaded only when needed. `/skills` lists them and `/<skill-name>` runs one.
- Custom slash commands: a markdown prompt in `commands/` (in your config folder, or
  `.quack/commands` in a trusted project) becomes a command: `commands/review.md` is
  `/review`, `commands/git/pr.md` is `/git:pr`. `{{ args }}` (or `$ARGUMENTS`) is replaced
  with what you type after the name, and front matter can set a `description` and `args`
  hint. They appear in the `/` popup; `/commands` lists them.
- Memory: markdown notes for each project and globally, with optional approval before a note
  is saved. `/memory` shows them.

### MCP

- Local (stdio) and remote (streamable HTTP) servers, one TOML file each.
- Large tool sets are deferred and found with a `tool_search` tool, so they do not fill the
  context.
- A server's prompts are slash commands: `/github:review 12`, or `/github:review pr=12
  focus=tests` to name the arguments. `/commands` lists them.
- The model can list a server's resources and read them (`list_mcp_resources`,
  `read_mcp_resource`). Reads are allowed unless a rule such as
  `mcp:github/resources/read` says otherwise.
- A server can ask you to fill in a short form while a tool runs (elicitation): each field
  is a question, as when the model asks. Skip a required field or press Ctrl-S to decline.
- A server can ask your model for a reply (sampling). Each request asks for approval as
  `mcp:<server>/sampling`; an allow rule with that name lets it through without asking.
  The tokens count in `/stats`. Headless runs decline both.
- `/mcp` shows each server's state, tools, prompts and resources. Servers restart when they
  crash.

### Hooks

- Programs you write, in any language (Python works everywhere), that run on agent
  events: `session_start`, `user_prompt`, `pre_tool`, `post_tool`, `turn_end`,
  `session_end` and `pre_compact`. One TOML file each in `hooks.d/`.
- A hook gets the event as JSON on stdin. It can add context for the model (print text, or
  `{"additional_context": "..."}`), block a prompt or a tool call (`"decision": "deny"`, or
  exit 2 with the reason on stderr), skip or force the approval prompt, or rewrite a call's
  arguments.
- `match` picks what starts it: a regular expression on the prompt, or a tool name and a
  glob on the command or path. Secrets for the hook (tokens) come from the environment,
  the OS keyring, a file or a command, as for providers.
- `on_error` decides what a crash or a timeout means: carry on with a warning (the
  default), carry on quietly, or deny.
- Hooks in the system config folder are enforced: users cannot replace or turn them off. A
  project's `.quack/hooks.d` runs nothing until you trust the project.
- Example: `defaults/hooks.d/jira.toml` adds every Jira issue a prompt links to, fetched
  with your personal access token and given to the model as XML.

```toml
# hooks.d/jira.toml
event = "user_prompt"
match = { prompt = 'https?://\S+/browse/[A-Z][A-Z0-9_]+-\d+' }
command = ["{python}", "{dir}/jira/jira.py"]
env = { JIRA_URL = "https://jira.example.com", JIRA_TOKEN = { keyring = "jira" } }
```

### Configuration

- Layers: system, user, project, environment and command line.
  `quack config show --origin` shows where each setting came from.
- Settings are checked against JSON schemas, with clear error messages (`quack config check`).
- `/config` lists every setting and changes them. Comments in your files are kept, and
  settings an administrator locked cannot be changed.
- `/reload` applies config changes without losing the conversation.

### Interface

- A full-screen terminal UI with markdown rendering and an ASCII mode. Code blocks are
  syntax-highlighted with the same tree-sitter grammars once their closing fence arrives;
  large ones are highlighted on a background thread and show plain until then.
- A scrollable history with a scrollbar: PageUp/PageDown by page, Shift-Up/Down by line,
  Ctrl-Home/Ctrl-End to the top or the latest. The view holds still while a reply streams
  in below. The wheel scrolls and a click places the input cursor; this takes over the
  mouse, so hold Shift to select text, or set `ui.mouse = false` to give it back.
- An input box that wraps long lines and grows to six rows. Up/Down move between its rows
  and step through history from the top or bottom row.
- A slash-command palette that highlights the letters you typed and shows what doesn't fit
  on the row (the rest of a description, aliases) underneath. Every picker (`/models`,
  `/config`, `/help`, …) works the same way, with tabs, match highlighting and a scrollbar.
- A status bar with the permission mode as a coloured badge, the room left before
  compaction in tokens and in turns at the recent pace (yellow, then red, as it runs
  out), generation speed, and what the model is doing right
  now (thinking, writing, running a tool) with a running clock.
- The model's plan as a live checklist, plus a strip above the input showing progress and
  the step in hand.
- Tool calls coloured by kind, with how long slow ones took. Diffs get a coloured gutter.
  Each turn ends with a short line: time taken, tools called, tokens out.
- The git branch in the header, and a welcome screen with the first few things worth
  knowing.
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
- `/fork` and `/export`.
- Automatic reload when config files change.
- MCP OAuth.

See the [roadmap](docs/plan/roadmap.md).

## Install

Linux and macOS:

```sh
curl -fsSL https://github.com/paulhovey/quack/releases/latest/download/install.sh | sh
```

Windows (PowerShell):

```powershell
irm https://github.com/paulhovey/quack/releases/latest/download/install.ps1 | iex
```

This installs the binary and the defaults: provider files for local servers and hosted
services, model profiles, language servers and MCP examples. Then run `quack`; it walks you
through adding a provider.

For organisations that ship their own files, `quack-plucked` is the duck without its
feathers: the binary alone. Add `--plucked` (`sh -s -- --plucked`, or `-Plucked` on Windows).
It never touches an existing defaults folder, so it is also how to update just the binary.

Other options: `--prefix` (default `~/.local`, or `%LOCALAPPDATA%\Programs\quack`),
`--version v0.2.0`, `--archive FILE` for offline installs, `--from-source` to build this
checkout, and `--uninstall`. Prebuilt archives for each platform are on the
[releases page](https://github.com/paulhovey/quack/releases).

To try a checkout without installing a release:

```sh
scripts/install.sh --from-source     # builds with cargo, installs into ~/.local
```

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
| `defaults/` | The defaults bundle (providers, profiles, schemas, …), installed beside the binary except with `--plucked` |
| `scripts/` | Install scripts, release packaging, CI helpers |
| `evals/` | The `quack eval` task suite and its results |
| `docs/plan/` | The implementation plan, one document per area |

## License

Apache-2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE).
