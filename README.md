# quack

A fast, terminal-native coding agent designed for open-weight models. Run it with local servers
(**llama.cpp, Ollama, vLLM, SGLang, LM Studio**) or hosted open-model services (**OpenRouter,
DeepSeek, Fireworks, Together, Moonshot, Mistral, and more**). It also works with Claude and
OpenAI APIs or ChatGPT accounts.

Written in Rust with a full-screen Ratatui UI, quack delivers sub-10ms startup, OS-level
sandboxing, AST-aware command security, Tree-sitter syntax validation, and background LSP code
intelligence.

[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![Status](https://img.shields.io/badge/status-active%20development-green.svg)](docs/plan/roadmap.md)

---

## Quick Start

### 1. Install

**Linux and macOS:**
```sh
curl -fsSL https://github.com/paulhovey/quack/releases/latest/download/install.sh | sh
```

**Windows (PowerShell):**
```powershell
irm https://github.com/paulhovey/quack/releases/latest/download/install.ps1 | iex
```

*Prebuilt `.deb`, `.rpm`, Homebrew tap formulas, and platform archives are available on the [releases page](https://github.com/paulhovey/quack/releases).*

### 2. Run

```sh
quack
```

On first launch, the interactive `/setup` wizard helps you configure a model in seconds:
- **Local servers**: Automatically detects running instances of Ollama, llama.cpp, vLLM, or LM Studio.
- **Hosted providers**: Configure API keys for OpenRouter, DeepSeek, Fireworks, and others (securely saved in your OS keyring).
- **ChatGPT**: Run `quack login chatgpt` to sign in via your browser.
- **Custom endpoints**: Connect any OpenAI-compatible server URL.

### 3. Common Commands

| Command | Description |
|---|---|
| `quack` | Launch interactive terminal UI |
| `quack setup` | Terminal wizard to add providers and models |
| `quack doctor` | Diagnose environment, sandbox, tools, and provider connections |
| `quack exec "..."` | Run a single turn headlessly for scripts or CI pipelines |
| `quack acp` | Start an Agent Client Protocol server for Zed, JetBrains, Neovim, or VS Code |
| `quack keyring set <id>` | Store an API key securely in the OS credential store |

---

## Highlights

- **Open-weight first**: Built from the ground up for open models with family profiles (Qwen, DeepSeek, GLM, Kimi, Mistral), grammar-guided decoding, and reasoning preservation.
- **Fast and lightweight**: Native Rust binary, sub-10ms startup, low memory footprint, and zero telemetry or implicit network egress.
- **Sandboxed by default**: Commands run in an OS-level sandbox (Landlock and seccomp on Linux, Seatbelt on macOS) restricted to workspace writes with no network access.
- **AST-aware command security**: Tree-sitter parses Bash and PowerShell commands down to AST nodes, ensuring compound commands, subshells, and aliases cannot bypass deny rules.
- **Compiler and syntax aware**: Immediate Tree-sitter syntax validation on edits and background LSP diagnostics that guide the model to fix type or compile errors immediately.
- **Smart context distillation**: Automatically folds passing test sequences (Cargo, Pytest, Go, Jest) and verbose build logs, keeping context compact and prompt caches warm.
- **Transactional multi-file refactoring**: Atomic multi-file changes automatically roll back if any write fails. Revert edits instantly with `/undo` or `/rewind`.
- **Collaborative plan mode**: `/plan` lets you explore solutions and approve checklists before modifying files, with optional dual-model planning.
- **Extensible with MCP & LSP**: Native Model Context Protocol (stdio & HTTP) support, background language servers for 10+ languages, sub-agents, and custom slash commands.
- **Editor integration (ACP)**: Connect quack directly into your editor panel via the Agent Client Protocol.

---

<a id="features"></a>

## Features

### Models & Reasoning

- **Local and hosted engines**: Works with local servers (Ollama, llama.cpp, vLLM, SGLang, LM Studio) and 12+ hosted providers (OpenRouter, Fireworks, Together, DeepInfra, Novita, DeepSeek, Moonshot, Kimi, Mistral, Charm Hyper), plus Anthropic and OpenAI APIs.
- **Thinking and reasoning models**: Native streaming with collapsible reasoning display (`Ctrl-T` or `/thinking`). Reasoning tokens are preserved across tool steps as required by DeepSeek, Kimi, MiniMax, GLM, Claude, and OpenAI.
- **Tool-call repair**: Automatically parses and heals calls returned as text or markup (Hermes, Qwen-XML, GLM, DeepSeek, Kimi formats), recovers loose arguments, and deduplicates call IDs.
- **Guided decoding**: Enforces schema-guided grammar decoding (`ToolCallMode::Grammar`) on supported local engines to eliminate JSON malformations.
- **Family profiles**: Automatically tunes prompt templates, tool definitions, edit formats, and sampling parameters for specific model families (`qwen-code`, `cc-style`, `kimi-code`, `codex`, `dsh`, `vibe`).
- **Live switching**: Change active models mid-conversation with `/models` or inspect provider health with `/providers` without losing conversation history.
- **Token telemetry**: Real-time tokens per second and time-to-first-token tracking in the status bar.

### Tools & Editing

- **Core tool suite**: `read`, `write`, `glob`, `grep`, `list_dir`, `shell`, `lsp`, and `index`.
- **Intelligent code indexing**: `index` generates Tree-sitter symbol outlines (functions, types, methods, and line ranges) for Rust, Python, Go, TypeScript, JavaScript, C, C++, and Bash. Large file reads automatically return outlines so the model reads only relevant definition spans.
- **Flexible edit formats**: Configurable via `[tools] edit_format`:
  - `str_replace`: Fast, forgiving snippet replacement.
  - `hashline`: Line-tagged content headers with function-level block targeting.
  - `apply_patch`: Multi-file transactional patches.
  - `search_replace_blocks`: Multi-block SEARCH/REPLACE operations.
  - `whole_file`: Full file rewrites.
- **Transactional guarantees**: Multi-file refactors check all changes before writing. If any single change fails, all touched files roll back to their original state.
- **Distilled test output**: Build and test outputs (Cargo, Pytest, unittest, Go test, Jest, TAP) fold passing tests (`[... 42 passed tests omitted ...]`), retaining all assertion failures and stack traces while preserving context space.

### Code Intelligence (LSP)

- **Zero-config language servers**: Automatic background integration for Rust (`rust-analyzer`), Go (`gopls`), Python (`pyright`/`basedpyright`), TypeScript/JavaScript, C/C++ (`clangd`), C#, Java, Lua, Bash, TOML, and YAML. Uses existing servers installed on your `PATH`.
- **Immediate edit diagnostics**: As soon as a file is edited, newly introduced compile or type errors are fed back into the tool result so the model fixes them before continuing.
- **Model LSP navigation**: Enables the model to find definitions, locate references, view documentation hovers, inspect diagnostics, and perform project-wide symbol renames.
- **Status bar counters**: Displays live error and warning counts (`✗1 ⚠3`) directly in the UI. Inspect indexing progress and server logs with `/lsp`.

### Safety & Sandboxing

- **Permission modes**: `read-only`, `ask` (default), `auto-edit`, `full-auto`, and `yolo`.
- **OS-level sandboxing**: Shell commands execute in an isolated sandbox (Landlock + seccomp on Linux, Seatbelt on macOS) with workspace-only write access and network access disabled.
- **AST-aware command verification**: Tree-sitter parses shell commands to verify that compound expressions (`&&`, `||`, subshells, command substitutions, aliases) do not violate deny rules.
- **Transparent approvals**: Every file modification displays an interactive diff preview with options to allow once (`y`), allow for session (`a`), permanently allow rule (`A`), or deny (`n`).
- **Project trust**: Untrusted cloned repositories execute no `.quack/` configurations until explicitly trusted via `/trust`.
- **Quick sandbox toggle**: Toggle the sandbox on or off during a session with `/sandbox` while preserving your active permission mode.

### Planning & Sub-Agents

- **Interactive plan mode**: Activate `/plan` (or Shift-Tab) to brainstorm and refine multi-step tasks. The agent explores the workspace, asks clarifying questions, and proposes a checklist for approval before modifying code.
- **Dual-model planning**: Use a large model to architect the plan (`[model] planner = "..."`) while your daily model carries out the implementation steps.
- **Sub-agents**: Delegate work to isolated sub-agents using the `task` tool:
  - `explore`: Read-only scout that searches the codebase and returns file paths and line ranges, keeping exploratory reads out of the primary context.
  - `general`: Executes focused, self-contained implementation tasks.
- **Live checklist tracking**: Dynamic progress checklist displayed above the input box, expandable via `/todo`.

### Context & Sessions

- **Smart token management**: Prunes superseded file reads, obsolete tool outputs, and failed attempts before performing compaction.
- **Lossless compaction**: Compaction preserves the original user prompt and files changed to date.
- **Context analytics**: `/context` visualizes token distribution across prompts, tools, and history; `/stats` graphs cumulative token usage, latency, and session cost.
- **Session persistence**: Every session is written incrementally and survives crashes. Continue seamlessly with `quack --continue` or `/resume`.
- **Time travel and branches**: `/undo` rolls back file changes; `/rewind` reverts files and conversation turns to any previous point; `/fork` branches a session into a new thread.
- **Persistent memory**: Maintains project and global memory notes (`/memory`), auto-detecting corrections and preferences across sessions.
- **Instruction hierarchy**: Automatically loads instructions from `AGENTS.md` (and imports existing `CLAUDE.md`, `.cursorrules`, or Copilot files).

### MCP & Extensibility

- **Model Context Protocol (MCP)**: Supports stdio and streamable HTTP MCP servers configured as simple TOML files.
- **Deferred discovery**: Large tool surfaces are indexed and searched dynamically (`tool_search`), avoiding context bloat.
- **Event hooks**: Run custom scripts on session events (`session_start`, `user_prompt`, `pre_tool`, `post_tool`, `turn_end`) configured in `hooks.d/`.
- **Custom slash commands**: Place markdown prompt templates in `commands/` (e.g. `commands/git/pr.md` becomes `/git:pr`).
- **Bundles**: Group skills, commands, MCP servers, and hooks into shareable directory bundles.

### Interface

- **Full-screen Ratatui TUI**: Markdown formatting, syntax-highlighted code blocks, tables, and optional ASCII mode (`--ascii`).
- **Window title**: The terminal's title shows the duck and what the session is about (`_(•)> fix the login bug`), and marks when a turn runs or quack needs you. `ui.terminal_title = false` turns it off.
- **Smooth scrolling and selection**: Mouse wheel scrolling, click-to-position cursor, and OSC 52 terminal copy integration.
- **Interactive pickers**: Fast, fuzzy-filtered modals for `/models`, `/providers`, `/config`, `/context`, `/stats`, `/doctor`, and `/help`.
- **Status bar**: Real-time indicators for active permission mode, context window utilization, model generation speed, and background tasks.

### Editor Integration (ACP)

`quack acp` implements the [Agent Client Protocol](https://agentclientprotocol.com) over stdio, allowing editors like **Zed, JetBrains, Neovim, and VS Code** to drive quack inside their native agent panels.

Example configuration for Zed (`settings.json`):
```json
{
  "agent_servers": {
    "quack": { "type": "custom", "command": "quack", "args": ["acp"] }
  }
}
```

The editor panel receives streaming replies, reasoning steps, tool diffs, permission approval prompts, and checklist updates directly from quack.

---

## Installation Options

### Prebuilt Packages

Prebuilt archives for all supported targets are published on the [Releases](https://github.com/paulhovey/quack/releases) page:
- **Linux**: Static binaries (musl) for x64 and ARM64, `.deb` and `.rpm` packages.
- **macOS**: Universal archives (x64 and Apple Silicon) and Homebrew tap formulas.
- **Windows**: x64 and ARM64 zip archives and PowerShell installer.

### Minimal / Air-Gapped Installs (`quack-plucked`)

For air-gapped environments or private clusters, `quack-plucked` provides the complete agent (profiles, sub-agents, MCP, ACP, language servers, hooks, local model support) without any hosted service configurations:

```sh
curl -fsSL https://github.com/paulhovey/quack/releases/latest/download/install.sh | sh -s -- --plucked
```

See [docs/enterprise.md](docs/enterprise.md) for enterprise deployment policies and packaging guides.

---

<a id="configuration-reference"></a>

## Configuration

quack contacts only the providers and MCP servers you explicitly configure. Everything is declared in human-readable TOML files across layered directories.

### Configuration Layers

Settings merge in the following order of precedence (higher layers override lower layers):

| Layer | Location | Purpose |
|---|---|---|
| **Defaults** | `<prefix>/share/quack/defaults/` or `QUACK_DEFAULTS_DIR` | Bundled base configurations and profiles |
| **System** | `/etc/quack/` (Linux), `/Library/Application Support/quack/` (macOS), `%ProgramData%\quack\` (Windows) | Administrative policies and enforced locks |
| **User** | `~/.config/quack/` (`$XDG_CONFIG_HOME`) or `%APPDATA%\quack\` | User preferences and private configurations |
| **Project** | `.quack/` at the repository root | Project-specific tools, commands, and rules (active once trusted) |
| **Environment** | `QUACK_MODEL`, `QUACK_CONFIG_DIR`, etc. | Runtime environment variables |
| **CLI Flags** | `-m`, `--profile`, `--permission-mode`, `-c key=value` | Command-line overrides for the active run |

Run `quack config show --origin` to view your effective configuration and see which layer defined each setting.

### Directory Structure

Every layer shares an identical layout:

```text
<layer>/
├── config.toml          # Global engine, UI, and permission settings
├── providers.d/*.toml   # Model servers and endpoints
├── mcp.d/*.toml         # Model Context Protocol servers
├── lsp.d/*.toml         # Language server configurations
├── hooks.d/*.toml       # Event lifecycle scripts
├── agents.d/*.toml      # Sub-agent configurations
├── profiles.d/<id>/     # Model family prompt templates and tool mappings
├── commands/*.md        # Custom slash commands
├── skills/<name>/       # SKILL.md task skill definitions
├── bundles/<name>/      # Modular packages grouping any of the above
└── AGENTS.md            # Project and workspace instructions
```

### Enterprise & Private Server Example

To connect quack exclusively to an internal model server (e.g. vLLM or llama.cpp) and lock down egress:

**1. Define the provider:**
```toml
# /etc/quack/providers.d/corp.toml
name = "Corp LLM"
base_url = "https://llm.internal.example:8000/v1"
api_key = { env = "CORP_LLM_KEY" }
models = [
  { id = "qwen3-coder-480b", alias = "coder", context_window = 262144 },
  { id = "qwen3-coder-30b", alias = "fast", context_window = 131072 },
]
```

**2. Enforce system policy:**
```toml
# /etc/quack/config.toml
[model]
default = "corp/coder"
small = "corp/fast"

[network]
offline = true                 # Remote MCP servers allowed only on local host or egress_allow

[sandbox]
mode = "workspace-write"       # Sandboxed execution; no network egress

[permissions]
deny = ["shell:curl *", "shell:wget *"]

[policy]
locked = ["model.default", "network", "sandbox.mode", "permissions.deny"]
max_permission_mode = "full-auto"
project_config = "trusted-only"

[policy.providers]
allowed = ["corp"]
hosts = ["llm.internal.example"]

[policy.mcp]
hosts = ["*.internal.example"]
```

**3. Verify with `quack doctor`:**
```text
ok    defaults      /usr/share/quack/defaults
ok    providers     1 usable
ok      corp        2 declared models
ok    model         corp/coder
ok    agents        2 agents: explore (read-only, on corp/fast), general
ok    sandbox       workspace-write · Landlock + seccomp
info  network       llm.internal.example · policy allows only llm.internal.example, *.internal.example
```

---

## Configuration Reference

### `config.toml`

**`[model]`**
| Key | Default | Description |
|---|---|---|
| `default` | `""` | Default model in format `<provider>/<model>` or `<provider>/<alias>` |
| `small` | `""` | Fast model for summaries, titles, and `explore` sub-agent |
| `planner` | `""` | Dedicated model for plan mode (falls back to active model) |
| `remember_last` | `true` | Resume with the most recently used model |
| `profile` | `""` | Force a specific prompt profile across all models |

**`[permissions]`**
| Key | Default | Description |
|---|---|---|
| `mode` | `"ask"` | Active mode: `read-only`, `ask`, `auto-edit`, `full-auto`, or `yolo` |
| `allow` | `[]` | Rule patterns executed without prompting (`shell:cargo test*`, `edit:src/**`) |
| `ask` | `[]` | Rule patterns that always prompt, even in `full-auto` |
| `deny` | `[]` | Rule patterns strictly forbidden across all modes |

**`[sandbox]`**
| Key | Default | Description |
|---|---|---|
| `mode` | `"auto"` | Sandbox mode: `auto`, `workspace-write`, `workspace-write-net`, `read-only`, or `off` |
| `writable` | `[]` | Additional writable directories outside the workspace (`["~/.cargo", "~/.cache"]`) |

**`[network]`**
| Key | Default | Description |
|---|---|---|
| `offline` | `false` | When true, restricts network access to local interfaces or `egress_allow` hosts |
| `egress_allow` | `[]` | Allowed remote host globs when `offline = true` |
| `proxy` | `""` | HTTP/HTTPS proxy URL |
| `ca_bundle` | `""` | Path to custom PEM certificate bundle |

**`[policy]`** *(System administrative layer only)*
| Key | Default | Description |
|---|---|---|
| `locked` | `[]` | Configuration keys that lower layers cannot modify |
| `max_permission_mode` | unset | Ceiling on allowed permission mode (e.g. locks out `yolo`) |
| `project_config` | `"trusted-only"` | Project `.quack/` loading policy: `trusted-only`, `never`, or `always` |
| `providers.allowed` | `[]` | Allowed provider ID globs |
| `providers.hosts` | `[]` | Allowed provider host globs |
| `mcp.allowed` | `[]` | Allowed MCP server ID globs |
| `mcp.hosts` | `[]` | Allowed remote MCP host globs |

**`[context]`**
| Key | Default | Description |
|---|---|---|
| `window` | unset | Maximum token ceiling (overrides server limit) |
| `budget_ratio` | `0.85` | Fraction of window reserved for context vs response buffer |
| `compaction_threshold` | `0.80` | Threshold of budget at which automatic pruning and compaction trigger |
| `cache_strategy` | `"auto"` | KV cache optimization: `auto`, `preserve`, or `aggressive` |

**`[tools]`**
| Key | Default | Description |
|---|---|---|
| `edit_format` | `"auto"` | Edit format: `auto`, `str_replace`, `hashline`, `apply_patch`, `search_replace_blocks`, or `whole_file` |
| `shell` | `"auto"` | Shell interpreter: `auto`, `bash`, `zsh`, `pwsh`, `powershell`, `cmd`, or path |
| `default_timeout_secs` | `120` | Maximum execution time before process tree termination |
| `read_outline_tokens` | `2000` | Token threshold above which large file reads return Tree-sitter outlines |

**`[ui]`**
| Key | Default | Description |
|---|---|---|
| `theme` | `"auto"` | Theme name (`dark` or `light`) |
| `show_reasoning` | `"collapsed"` | Initial display state for reasoning: `collapsed`, `expanded`, or `hidden` |
| `startup_panel` | `"auto"` | Startup status overlay: `auto`, `always`, or `never` |
| `ascii` | `false` | Enable pure ASCII rendering mode |
| `mouse` | `true` | Enable mouse wheel scrolling, cursor placement, and drag selection |
| `reduced_motion` | `false` | Replace animated spinners with static indicators |
| `terminal_title` | `true` | Set the terminal window title to the duck and the session title (`_(•)> fix the login bug`); `false` leaves the title alone |

**`[instructions]`, `[skills]`, `[memory]`, `[sessions]`**
| Key | Default | Description |
|---|---|---|
| `instructions.files` | `["AGENTS.md"]` | Instruction filenames checked in workspace root |
| `instructions.other_agents` | `true` | Read existing `CLAUDE.md`, `GEMINI.md`, or `.cursorrules` if `AGENTS.md` is absent |
| `instructions.max_bytes` | `32768` | Maximum byte limit per instruction file |
| `skills.compat_paths` | `[]` | Additional skill discovery directories |
| `memory.path` | `""` | Directory for persistent memory files (defaults to user data directory) |
| `memory.project_scope` | `"data-dir"` | Memory storage scope: `data-dir` or `repo` (`.quack/memory`) |
| `memory.writes` | `"auto"` | Approval policy for new memory notes: `auto` or `ask` |
| `sessions.path` | `""` | Directory for session transcripts |

---

### `providers.d/<id>.toml`

| Key | Default | Description |
|---|---|---|
| `base_url` | required | Server API endpoint URL |
| `api` | `"openai-chat"` | Protocol: `openai-chat`, `openai-responses`, `anthropic-messages`, or `ollama` |
| `api_key` | none | Secret definition (`"sk-..."`, `{ env = "VAR" }`, `{ keyring = "id" }`, `{ file = "path" }`) |
| `models` | `[]` | Declared models or server discovery options |
| `name`, `group` | ID, `""` | Display labels in setup and picker modals |
| `enabled` | `true` | Toggle provider availability |
| `auth_header`, `auth_scheme` | `Authorization`, `Bearer` | HTTP authorization header configuration |
| `headers` | `{}` | Custom HTTP headers sent with requests |
| `http.timeout_connect_secs` | `10` | Connection timeout |
| `http.timeout_idle_secs` | `300` | Stream idle timeout |
| `http.max_retries` | `3` | Maximum automatic retries on transient errors |
| `discovery.enabled` | `false` | Query server `/models` endpoint dynamically at startup |

**Per-model options in `models`:**
| Key | Default | Description |
|---|---|---|
| `id` | required | Model identifier recognized by provider |
| `alias` | none | Short name for CLI and picker selection (`<provider>/<alias>`) |
| `context_window`, `max_output` | from server | Token boundary definitions |
| `profile` | `"auto"` | Model profile ID or `auto` matching |
| `vision` | `false` | Enable image input and `view_image` tool |
| `tool_calling.mode` | `"native"` | `native`, `text` (markup in reply), or `grammar` (guided decoding) |
| `tool_calling.text_format` | `"auto"` | Format for text tool calls: `hermes`, `qwen3-xml`, `glm`, `json-block`, `deepseek`, or `kimi` |
| `reasoning.format` | `"auto"` | Format of thinking tokens: `reasoning_content`, `reasoning`, `think-tags`, or `none` |
| `sampling` | from profile | Model parameters (`temperature`, `top_p`, `top_k`, `min_p`) |
| `cost` | from server | Cost per million tokens: `input`, `output`, `cached_input`, `cache_write` |

---

### `mcp.d/<id>.toml`

| Key | Default | Description |
|---|---|---|
| `command`, `args` | | Executable path and arguments for stdio MCP servers |
| `url` | | Endpoint URL for remote HTTP MCP servers |
| `headers` | `{}` | HTTP headers for remote servers |
| `env` | `{}` | Environment variables passed to stdio server process |
| `cwd` | workspace root | Process working directory |
| `sandbox` | `false` | Run local server inside the OS sandbox |
| `tools.allow`, `tools.deny` | `[]` | Globs filtering exposed tool definitions |
| `tools.auto_approve` | `[]` | Globs of tool names that execute without asking approval |
| `startup_timeout_secs` | `20` | Server initialization timeout |

---

### `lsp.d/<id>.toml`

| Key | Default | Description |
|---|---|---|
| `command`, `args` | required | Language server binary and startup arguments |
| `extensions` | required | Covered file extensions without leading dot (`["rs"]`, `["py"]`) |
| `root_markers` | `[]` | Project root boundary files (`Cargo.toml`, `go.mod`, `pyproject.toml`) |
| `initialization_options` | `{}` | LSP initialization parameters |
| `diagnostics.settle_ms` | `800` | Settle delay before querying diagnostics after edits |
| `diagnostics.min_severity` | `"warning"` | Minimum diagnostic level to report: `error`, `warning`, `info`, or `hint` |

---

### `hooks.d/<id>.toml`

| Key | Default | Description |
|---|---|---|
| `event` | required | Target event: `session_start`, `user_prompt`, `pre_tool`, `post_tool`, `turn_end`, `session_end`, `pre_compact` |
| `command` | required | Executable and arguments (`{dir}` is hook directory, `{python}` is system Python) |
| `match.tool`, `match.pattern` | `""` | Tool name glob and argument pattern matchers |
| `match.prompt` | `""` | Regular expression for `user_prompt` events |
| `timeout_secs` | `10` | Maximum hook execution duration |
| `on_error` | `"warn"` | Failure behavior: `warn`, `allow` (silent continue), or `deny` |

---

### `agents.d/<id>.toml`

| Key | Default | Description |
|---|---|---|
| `description` | required | Sub-agent role description used by caller model |
| `prompt` | `""` | Supplementary system instructions for the sub-agent |
| `tools` | all | Allowed tool names (e.g. `["read", "grep", "index"]`) |
| `read_only` | `false` | Restrict agent to read-only tools; permits parallel runs |
| `model` | caller model | Model override (`small` or `<provider>/<model>`) |
| `max_steps` | none | Maximum execution steps before stopping |

---

## Development

```sh
# Build binary
cargo build

# Run interactive TUI
cargo run

# Run with bundled defaults from checkout
QUACK_DEFAULTS_DIR=defaults cargo run

# Run headless turn
cargo run -- exec "explain src/main.rs"

# Run tests
cargo test --workspace --locked

# Run linter
RUSTFLAGS="-D warnings" cargo clippy --workspace --all-targets --locked

# Format code
cargo fmt --all --check
```

### Repository Layout

| Directory | Contents |
|---|---|
| `crates/` | Modular Rust crates (`quack-core`, `quack-tui`, `quack-tools`, `quack-providers`, etc.) |
| `defaults/` | Default configurations, model profiles, MCP servers, and JSON schemas |
| `scripts/` | Installation scripts, CI helpers, and packaging tools |
| `evals/` | Task evaluation suites (`evals/tasks`, `evals/hard`) and benchmarking runners |
| `docs/plan/` | Architecture specs and technical design documents |

---

## License

Apache-2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE).
