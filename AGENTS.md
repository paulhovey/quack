# AGENTS.md

`quack` is a terminal coding agent for open-weight models, written in Rust (edition 2024, tokio + ratatui). Working name chosen for easy global rename. `docs/plan/` contains detailed design docs; `docs/plan/00-architecture.md` is the best starting point.

## Commands

CI (`.github/workflows/ci.yml`) runs with `RUSTFLAGS: -D warnings` on linux-x64, macos-arm64, **windows-x64 and windows-arm64**. Every warning is a CI error.

```sh
cargo build                                   # default-members is quack-cli
cargo build --workspace
cargo test --workspace --locked
RUSTFLAGS="-D warnings" cargo clippy --workspace --all-targets --locked   # what CI runs
cargo fmt --all --check                       # max_width = 100
cargo build -p quack-cli --locked --no-default-features --features proto-openai-chat,lang-common  # minimal feature set, CI checks this
```

- Toolchain pinned to 1.99.1 (`rust-toolchain.toml`); workspace MSRV is 1.97 and checked separately in CI.
- `default-members` is `quack-cli`, so bare `cargo build`/`cargo run` builds only the CLI path.
- Run from a checkout: `QUACK_DEFAULTS_DIR=defaults cargo run` — the defaults bundle is never compiled in; it is resolved at runtime and overridable with `QUACK_DEFAULTS_DIR`. CI sets `QUACK_DEFAULTS_DIR=none` for tests so they never read your real config; do the same locally when running engine tests that load config.
- Scripts (Linux CI): `scripts/no-egress.sh` (loopback-only netns + `strace -e connect`), `scripts/first-frame.sh` (startup budget), `scripts/local-server-ci.sh` (real llama.cpp/Ollama turn).

## Architecture

Single engine, three in-process clients, connected by an Op/Event protocol:

- `quack-protocol`: serde-typed `Op` (client → engine) and `Event` (engine → clients). No async-runtime dependency.
- `quack-core`: the engine (a tokio task owning a session). Clients hold an `EngineHandle { ops: mpsc::Sender<Op>, events: broadcast::Receiver<Event> }`. All three clients (TUI, headless `quack-exec`, ACP `quack-acp`) drive the same engine through the same handle.
- Crate layering: `quack-cli` → clients (`quack-tui`/`quack-exec`/`quack-acp`) → `quack-core` → subsystems (`quack-providers`, `quack-profiles`, `quack-context`, `quack-tools`, `quack-mcp`, `quack-lsp`, `quack-store`, `quack-instructions`); `quack-tools` → `quack-shell`/`quack-sandbox`; everything → `quack-protocol`. **Client crates never depend on each other.**
- Platform-specific code lives only in `quack-shell` (spawn/PTY/kill-tree), `quack-sandbox` (Landlock+seccomp / Seatbelt / restricted token), and path/secret handling in `quack-config`/`quack-providers`.
- Heavy work (tree-sitter, tokenizing, grep, diff) goes on `spawn_blocking`, never the TUI render thread.

## Hard invariants (test- and CI-enforced)

These are the project's core promises; regressions fail CI by design:

- **No built-ins**: no provider ids, base URLs, or MCP commands compiled into the binary. Everything is TOML under `defaults/` (loaded as a layer beneath user/project config). Startup must survive deletion of any single default file ("removal" test).
- **No implicit egress**: the binary contacts only what is configured. Enforced by an in-process refusing proxy test and by `scripts/no-egress.sh` under strace. Never add a network call outside configured providers/MCP.
- **Prefix stability**: system prompt + tool block byte-identical across simulated turns except at epoch boundaries.
- **First frame < 50 ms** (Linux CI budget; `--trace-startup`).

## Gotchas

- **Platform-gated tests must gate their imports too.** CI compiles `--all-targets` with `-D warnings` on Windows; a test marked `#[cfg(unix)]` leaves its imports and helpers as unused-import/dead-code errors on Windows. Gate the imports and any test-only helper functions with the same `#[cfg(...)]` (see `crates/quack-core/tests/trust.rs`). Windows is first-class: the matrix includes windows-arm64.
- Workspace lints: `unsafe_code = deny`; clippy `unwrap_used`, `print_stdout`, `print_stderr`, `dbg_macro`, `todo`, `missing_debug_implementations`, `unused_qualifications` all warn (i.e., fail CI). Use `expect`/proper error types; never print from library/TUI code (tracing goes to file only, never the terminal).
- Error convention: `thiserror` in library crates, `anyhow` in binaries, `miette` for config-facing errors.
- Paths: use `Path`/`PathBuf` everywhere, never string-join with `/`. `dunce` to avoid `\\?\` prefixes on Windows; preserve per-file CRLF/LF and BOMs (the `edit` tool depends on this); case-insensitive path compares on Windows/macOS.
- `rmcp` is pinned (`=3.5.1`) because MCP 2026-07-28 support is new; don't bump casually.
- New external crates must earn their place and are added with `default-features = false` where possible; `cargo-deny` (`deny.toml`) gates licenses/advisories (Apache-2.0-compatible only).
- Feature flags gate protocols/grammars/sandbox/ACP (see `quack-cli` `[features]`). A provider naming a compiled-out protocol must fail to load alone with a clear error.
- Project config dir is `.quack/` under the workspace root (e.g. `.quack/mcp.d/`); untrusted project config runs nothing until trusted (see the trust tests in `quack-core`).
- `defaults/schemas/` JSON Schemas are regenerated with `quack config schema --dir defaults/schemas`.

## Testing

- Engine integration tests live in `crates/quack-core/tests/` (one file per area: `trust.rs`, `agent_loop.rs`, `rewind.rs`, ...). Each binary includes `mod common;` (`tests/common/mod.rs`), which has `#![allow(dead_code)]` and provides `workspace()` (isolated temp config + mock provider), `sse_text()`/`sse_call()`/`sse_raw()` (wiremock SSE reply builders), `turn()` (submit a user turn and wait), and `bodies()` (captured request bodies). The mock LLM is a `wiremock` `MockServer`.
- Snapshot tests use `insta` (`quack-tui` frames rendered at explicit sizes like 80x24/100x30, `quack-lsp`, `quack-mcp`); snapshots live in `src/snapshots/`. Update via `cargo insta review` or `INSTA_UPDATE=always cargo test`.
- Recorded provider SSE fixtures and cross-crate/invariant tests live under top-level `tests/` (currently README + fixtures; see `docs/plan/13-testing-and-evals.md`).
- Tests must not read the developer's real config or keyring: keep `QUACK_DEFAULTS_DIR=none` and construct isolated config via `common::workspace()`.
