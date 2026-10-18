# 00 — Architecture

## Goals

- The engine runs without any UI: the TUI, headless mode and ACP all drive the same engine.
- Startup work is async and observable: every subsystem reports its own progress.
- Optional capabilities (wire protocols, grammars, sandbox backends, ACP) are cargo features, so they can be compiled out.
- The same code paths run on Linux, macOS and Windows. Platform differences are isolated behind small traits.

## Cargo workspace

```
cutoffharness/
├── Cargo.toml                 # [workspace], shared deps, lints, profiles
├── crates/
│   ├── cutoffharness-protocol/      # Op / Event types shared by engine and clients (serde, no tokio)
│   ├── cutoffharness-config/        # TOML loading, layering, schema, policy, trust store
│   ├── cutoffharness-providers/     # Wire adapters, streaming, tool-call normalization, tokenizers
│   ├── cutoffharness-profiles/      # Profile loading, prompt rendering, tool-surface mapping
│   ├── cutoffharness-context/       # Token budget, tree-sitter index, pruning, compaction
│   ├── cutoffharness-tools/         # Built-in tools + tool registry + permission engine
│   ├── cutoffharness-shell/         # Process spawning, PTY, kill trees (per-OS)
│   ├── cutoffharness-sandbox/       # Landlock/seccomp, Seatbelt, Windows restricted token (per-OS)
│   ├── cutoffharness-mcp/           # MCP client manager on top of rmcp
│   ├── cutoffharness-lsp/           # LSP client manager
│   ├── cutoffharness-store/         # Sessions, memory, checkpoints, trust records
│   ├── cutoffharness-instructions/  # AGENTS.md discovery, skills, custom commands
│   ├── cutoffharness-core/          # Agent loop: owns a session, wires everything together
│   ├── cutoffharness-tui/           # ratatui client
│   ├── cutoffharness-exec/          # Headless client (text / json / stream-json)
│   ├── cutoffharness-acp/           # Zed Agent Client Protocol server (feature-gated, later)
│   └── cutoffharness-cli/           # The `cutoffharness` binary: arg parsing, picks a client
├── defaults/                  # Default TOML files. NOT compiled in (see 14-packaging)
│   ├── providers/
│   ├── profiles/
│   ├── mcp/
│   ├── lsp/
│   └── grammars/              # tree-sitter query files (tags.scm etc.)
├── docs/
└── tests/                     # Cross-crate integration tests, fixtures
```

### Dependency direction

```
cutoffharness-cli ─┬─> cutoffharness-tui ──┐
             ├─> cutoffharness-exec ─┼─> cutoffharness-core ─┬─> cutoffharness-providers ─> cutoffharness-config
             └─> cutoffharness-acp ──┘                 ├─> cutoffharness-profiles
                                                 ├─> cutoffharness-context
                                                 ├─> cutoffharness-tools ─> cutoffharness-shell, cutoffharness-sandbox, cutoffharness-lsp
                                                 ├─> cutoffharness-mcp
                                                 ├─> cutoffharness-store
                                                 └─> cutoffharness-instructions
all ─> cutoffharness-protocol
```

Client crates (`tui`, `exec`, `acp`) never depend on each other. `cutoffharness-protocol` has no async-runtime dependency, so it can also be used for ACP translation and tests.

## Engine protocol

Modelled on codex's submission/event split. The client sends **Ops**; the engine emits **Events**. Both are serde types, which keeps them easy to log, replay and test.

```rust
// cutoffharness-protocol (sketch)
pub enum Op {
    UserTurn { items: Vec<InputItem>, overrides: TurnOverrides },
    Interrupt,
    ApprovalResponse { call_id: CallId, decision: ApprovalDecision },
    SetModel { provider: ProviderId, model: ModelId },
    Compact,
    Undo { checkpoint: Option<CheckpointId> },
    ResumeSession { id: SessionId },
    ForkSession { at: EventIndex },
    ReloadConfig,
    Shutdown,
}

pub enum Event {
    // lifecycle
    StartupProgress { subsystem: Subsystem, state: LoadState },   // drives the startup panel
    SessionConfigured { session: SessionInfo },
    // turn
    TurnStarted { turn: TurnId },
    AssistantDelta { text: String },
    ReasoningDelta { text: String },
    ToolCallBegin { call: ToolCallInfo },
    ToolCallOutputDelta { call_id: CallId, chunk: String },
    ToolCallEnd { call_id: CallId, result: ToolResultSummary },
    ApprovalRequest { call_id: CallId, request: ApprovalRequest },
    TurnComplete { turn: TurnId, usage: Usage },
    // context
    ContextUpdate { used: u32, budget: u32, cache_hit_tokens: Option<u32> },
    ContextPruned { report: PruneReport },
    Compacted { summary_tokens: u32 },
    // diagnostics
    Warning { message: String },
    Error { error: EngineError },
}

pub enum LoadState { Pending, Running { detail: String }, Ready { detail: String }, Failed { error: String }, Disabled }
```

The engine runs as a tokio task. A client holds an `EngineHandle { ops: mpsc::Sender<Op>, events: broadcast::Receiver<Event> }`. In headless and ACP mode the same handle is used in-process. A daemon/socket mode is a possible later addition and needs no protocol change.

## Async & threading model

- **tokio**, multi-threaded runtime.
- The TUI render loop runs on the main thread and receives a merged stream of terminal input and engine events through `tokio::select!`. Redraws are coalesced (see [06-tui.md](06-tui.md)).
- CPU-heavy work runs on `spawn_blocking` or a small rayon pool: tree-sitter parsing, tokenization, grep and diffing. It never runs on the render thread.
- Cancellation: every turn owns a `CancellationToken`. `Op::Interrupt` cancels the turn, which stops the in-flight HTTP stream, kills child processes (whole process tree; see [04](04-agent-loop-and-tools.md#shell-tool)), and cancels MCP requests.

## Startup sequence

```
t=0      parse CLI args
         locate config dirs, read config.toml layers (sync, tiny files only)
         init tracing to file (never to the terminal)
t<50ms   enter TUI, draw first frame with startup panel, accept input
async    ├─ load providers.d/*.toml ─> validate ─> (optional) model discovery per provider
         ├─ discover AGENTS.md / skills / commands
         ├─ load memory index
         ├─ spawn MCP servers (parallel, each with timeout)
         ├─ check LSP binaries on PATH (start lazily on first relevant file)
         ├─ load tokenizer for selected model
         └─ warm tree-sitter grammars for languages present in repo (lazy)
```

Each async task reports through `Event::StartupProgress`. If the user submits a prompt before the selected provider is ready, the prompt is queued and the panel shows what it is waiting on. MCP and LSP never block the first turn. Their tools become available once they report Ready.

`cutoffharness --trace-startup` writes a timing breakdown, and CI enforces a time budget (see [13](13-testing-and-evals.md)).

## Platform abstraction

Platform-specific code is confined to:

| Concern | Crate | Linux | macOS | Windows |
|---|---|---|---|---|
| Config/data/cache dirs | `cutoffharness-config` | XDG | `~/Library/...` or XDG override | `%APPDATA%`, `%LOCALAPPDATA%`, `%ProgramData%` |
| Process spawn & kill tree | `cutoffharness-shell` | process groups + `killpg` | process groups | Job Objects |
| PTY | `cutoffharness-shell` | `portable-pty` | `portable-pty` | ConPTY via `portable-pty` |
| Default shell | `cutoffharness-shell` | `$SHELL` or `bash` | `$SHELL` or `zsh` | `pwsh` → `powershell` → Git Bash (configurable) |
| Executable resolution | `cutoffharness-shell` | `PATH` | `PATH` | `PATH` + `PATHEXT` (`npx` → `npx.cmd`) |
| Sandbox | `cutoffharness-sandbox` | Landlock + seccomp | Seatbelt | Restricted token + Job Object (see [11](11-security.md)) |
| Secrets | `cutoffharness-config` | Secret Service | Keychain | Credential Manager (`keyring` crate) |
| TLS roots | `cutoffharness-providers` | `rustls-platform-verifier` | same | same (uses the Windows cert store, so corporate root CAs work) |

Path handling rules, enforced by a lint/test helper:
- Use `Path`/`PathBuf` everywhere. Never build paths by joining strings with `/`.
- Before showing a path to the model, normalize it to forward slashes relative to the workspace root. Accept both separators in tool input.
- Compare paths case-insensitively on Windows and macOS (default filesystems). Use `dunce` to avoid `\\?\` verbatim prefixes in user-facing output.
- Preserve each file's existing line endings when editing (CRLF/LF detection per file).

## Key dependencies (initial picks)

| Area | Crate | Notes |
|---|---|---|
| Runtime | `tokio`, `tokio-util` | `CancellationToken` |
| TUI | `ratatui`, `crossterm` | crossterm supports Windows |
| Config | `serde`, `toml`, `toml_edit` | `toml_edit` for writes that keep user comments |
| Config schema | `schemars` | Generates JSON Schema for taplo/editor completion |
| Errors | `thiserror` (libs), `anyhow` (bin), `miette` | miette for pretty config errors with spans |
| HTTP | `reqwest` (rustls, no native-tls), `eventsource-stream` | |
| TLS | `rustls`, `rustls-platform-verifier` | OS trust store |
| MCP | `rmcp` | Official Rust SDK |
| ACP | `agent-client-protocol` | Feature-gated |
| LSP | `lsp-types`, `async-lsp` (evaluate) or hand-rolled JSON-RPC | |
| Tree-sitter | `tree-sitter`, `tree-sitter-<lang>` behind features | |
| Tokenizers | `tokenizers` (HF), `tiktoken-rs` | Feature-gated; heavy |
| Search | `ignore`, `grep-searcher`, `grep-regex`, `globset` | ripgrep internals |
| Diff | `similar` | |
| PTY/process | `portable-pty`, `which` | |
| Markdown | `pulldown-cmark` | Render to ratatui spans ourselves |
| Highlighting | `tree-sitter-highlight` (reuse grammars) | Avoids syntect startup cost |
| Secrets | `keyring` | |
| Logging | `tracing`, `tracing-subscriber`, `tracing-appender` | Logs to file only |
| CLI | `clap` (derive) | |
| IDs/time | `uuid` (v7), `jiff` | |
| Snapshots | `insta` | |
| Git (checkpoints) | `gix` | Pure Rust; git may not be installed on Windows machines |

Every dependency goes through `cargo-deny`, which checks licenses (Apache-2.0-compatible only), advisories and duplicate crates.

## Cargo features (binary level)

```toml
[features]
default = ["proto-openai-chat", "proto-openai-responses", "proto-anthropic", "proto-ollama",
           "lang-common", "tokenizers-hf", "sandbox", "acp"]
proto-openai-chat = []
proto-openai-responses = []
proto-anthropic = []
proto-ollama = []
lang-common = ["lang-rust", "lang-python", "lang-typescript", "lang-javascript", "lang-go",
               "lang-java", "lang-c", "lang-cpp", "lang-csharp", "lang-bash", "lang-powershell",
               "lang-json", "lang-toml", "lang-yaml", "lang-markdown"]
tokenizers-hf = ["cutoffharness-providers/tokenizers"]
sandbox = ["cutoffharness-tools/sandbox"]
acp = ["dep:cutoffharness-acp"]
```

A minimal enterprise build:

```
cargo build --release --no-default-features --features proto-openai-chat,lang-common,sandbox
```

If a provider TOML names a protocol that was compiled out, the provider fails to load with an error saying which feature is missing. Nothing else is affected.

## Open questions

- Should `cutoffharness-core` also expose a local socket (daemon mode) so several frontends can attach to one session? This is deferred, but the protocol is designed to allow it.
- `async-lsp` vs. a hand-rolled LSP client. Decide during the M4 spike.
