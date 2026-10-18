# Roadmap

Milestones are ordered by dependency and value. Each has **exit criteria** that must pass on **Linux, macOS and Windows** in CI before moving on. Dates are left out on purpose; scope is what's fixed.

## Scaffold (M1 kickoff) — done 2026-10-17

Status: landed. `cargo test --workspace` passes (29 tests) and clippy is clean with `-D warnings` on Linux, plus `cargo clippy` cross-checks for `x86_64-pc-windows-msvc` and `aarch64-apple-darwin`. The TUI was exercised in a real PTY: first output in about 7 ms (debug build), `/help` modal, a stub turn, and clean double Ctrl-C exit. Not yet run: CI itself, `cargo deny`, and the MSRV check, which all need the GitHub remote.

Decided 2026-10-17: the first scaffold creates the **full workspace**. All 17 crates exist, with their dependency edges and placeholder modules, so the architecture can be seen and compiles from day one. Only the M1 crates get real skeletons:

- `cutoffharness-protocol`: `Op` / `Event` / `LoadState` types
- `cutoffharness-config`: per-OS layer paths, TOML loading for `config.toml` + `providers.d`, miette diagnostics, `config check`
- `cutoffharness-cli`: clap commands (`tui` default, `exec`, `config check|show`, `doctor` stubs)
- `cutoffharness-tui`: full-screen app loop, startup panel fed by `StartupProgress`, input box, slash-command popup + modal framework with one real modal (`/help`)
- `cutoffharness-core`: engine task with an `EngineHandle`, and fake startup subsystems that report progress

Also: Cargo workspace lints/profiles, `rust-toolchain.toml`, CI (Linux x64, macOS ARM64, Windows x64 + ARM64), `cargo-deny`, `LICENSE` (Apache-2.0), `NOTICE`, the `defaults/` directory layout with schema stubs, and `docs/plan/` kept as-is.

## M0 — Spikes (de-risk before committing to designs)

Primary manual test target: **hosted open-model services** (Novita, Fireworks, Together, OpenRouter). Local servers are covered by CPU-only CI containers ([13](13-testing-and-evals.md#ci)). Hosted services run vLLM/SGLang behind the scenes but hide the tool-call parser configuration, so they are a realistic, and sometimes worse, test of tool-call robustness.

| Spike | Question to answer | Output |
|---|---|---|
| Tool-call reliability | Record native tool-call streams from 2+ hosted services for 3–4 models (Qwen3-Coder, GLM-4.x, Kimi K2, gpt-oss), plus llama.cpp `--jinja` and Ollama on CPU with a tiny model. How often do calls leak into content, and do services differ for the same model? | First fixtures in `tests/fixtures/providers/`, list of required text parsers |
| Prefix-cache behaviour | On hosted services: compare `cached_tokens` reporting, cost and TTFT for append-only vs. mid-history edits at 16k/64k/128k. On CPU llama.cpp: confirm cache reuse through `timings`. A GPU measurement is deferred until hardware is available | Numbers that seed the `auto` cache strategy ([05](05-context-management.md)) |
| Windows process & terminal | Job Objects + ConPTY + crossterm key events + pwsh invocation, in a throwaway binary | Confirmed approach for `cutoffharness-shell` and the TUI input layer |
| Startup budget | ratatui first frame with tokio + config parse, on Windows and Linux | Baseline numbers for [13](13-testing-and-evals.md) budgets |
| tree-sitter-powershell | Is the grammar good enough to classify commands for permissions? | Go/no-go; fallback = "ask for all PowerShell except allowlisted cmdlets" |

## M1 — Foundation: "it can code with a local model"

### Progress (2026-10-17)

Landed, with tests (unit, mock-server integration, TUI snapshots) and clippy clean on Linux plus Windows/macOS cross-checks:
- `openai-chat` adapter: SSE streaming, retries with backoff and `Retry-After`, normalized errors, `reasoning_content`/`reasoning`/`<think>` handling, tool-call delta assembly, model discovery
- Secrets: `env`, `file`, `cmd`, plain (`keyring` gives a clear "not supported yet" error)
- Simplified provider files: only `base_url` is required, `api` defaults to `openai-chat`, `models` takes plain ids, discovery when no models are listed, auto-selection when `[model].default` is unset
- Agent loop: multi-step tool use, approvals, interrupt, repetition guard, lenient argument parsing, duplicate call-id repair
- Tools: `read`, `write`, `edit` (exact → trailing-whitespace → indentation → blank-line fuzzy matching, CRLF/BOM preserved, stale-read guard), `glob`, `grep`, `list_dir`, `shell` (process-group kill on Unix, `taskkill /T` on Windows, output head/tail trimming)
- Permissions: modes, `shell:`/`edit:`/`read:` rules, compiled-in deny floor, session approvals; allow rules never match compound shell commands
- Profiles: loader, selection (`[match]` globs, `generic` fallback, compiled-in empty), minijinja rendering, tool renames and transforms; `generic` profile shipped
- AGENTS.md from the user layer and workspace root
- Text tool-call fallback: `hermes` and `qwen3-xml` calls printed in content are promoted to real calls (`tool_calling.text_format`, `"none"` disables)
- Sessions: JSONL log per session plus a per-project index, written as the turn happens (crash-safe, torn last line repaired), `--continue`, `--resume <id>`, `cutoffharness sessions`
- `/models`: catalogue of every provider (declared plus discovered models, unavailable ones with the reason), switching mid-session keeps history and approvals; `/models provider/model` switches directly
- TUI: markdown rendering, tool calls with diffs and output, reasoning, approval box, context usage; `exec`: tool activity on stderr, headless denials, `--permission-mode`
- Defaults bundle v0: novita, fireworks, together, openrouter, deepinfra, llama-cpp, ollama, vllm, lmstudio, sglang (none tagged verified yet). The bundle folders now use the layer layout (`providers.d/`, `profiles.d/`, …)
- Verified live against one hosted OpenAI-compatible service (Charm Hyper, `qwen3.8-flash`): a bug-fix task via `exec`, and a shell approval in the TUI

Still open for M1: `/resume` picker and `/config` modal, saving a default model from `/models`, streaming suppression of text tool calls (they are cleaned up after the step), native `ollama` adapter, Windows Job Objects, `estimate` correction from usage, `keyring`, and the exit criteria below. `cargo deny` and the macOS/Windows builds of `aws-lc-sys` (reqwest's TLS crypto) are only checked in CI.

- Workspace scaffold, CI matrix (Linux/macOS/Windows), `cargo-deny`, Apache-2.0 + NOTICE
- `cutoffharness-protocol` (Op/Event), `cutoffharness-core` loop with interrupt and loop guards
- `cutoffharness-config`: layers, per-OS paths, `providers.d`, secrets (env/cmd/keyring/file), miette diagnostics, `config check`
- `cutoffharness-providers`: `openai-chat` + `ollama` adapters, SSE streaming, retries, hermes/qwen3-xml/think-tag parsers, discovery, `estimate` tokenizer with server correction
- Profiles: loader + `generic` profile only, minijinja rendering, parameter mapping
- Tools: `read`, `write`, `edit.str_replace` (with fuzzy matching), `glob`, `grep`, `list_dir`, `shell` (bash/zsh/pwsh, Job Objects, timeouts, output trimming)
- Permissions: modes + allow/deny rules (string glob only; tree-sitter analysis comes in M3)
- TUI: startup panel, transcript with markdown, input editor, approval dialog with diff, status bar, slash-command palette and modal framework, `/models`, `/resume`, `/config`, `/help`
- Sessions: JSONL log, `--continue`, `--resume` picker
- Defaults bundle v0: `novita`, `fireworks`, `openrouter` (verified), `llama-cpp`, `ollama`, `vllm` (verified for protocol via CI containers; marked `# verified: protocol-only`), `generic` profile

**Exit criteria**
- [ ] Fix a failing test in a sample repo with Qwen3-Coder on **two hosted services**, on Linux, macOS and Windows (x64 + ARM64, pwsh)
- [ ] The CPU llama.cpp and Ollama CI jobs complete a scripted tool-call turn
- [ ] First frame < 50 ms Linux / < 120 ms Windows
- [ ] Deleting any default provider file leaves startup clean
- [ ] No-egress invariant test passes
- [ ] Resume reproduces the context exactly

## M2 — Context & profiles: "it stays useful in small windows"

- Tokenizers: `hf-file`, `tiktoken`, `server`. Context ledger, `/context`, cache-hit display
- `index` tool (tree-sitter skeletons) + `read` large-file skeleton behaviour + symbol reads; `lang-common` grammars
- Prune rules 1–6, epoch-based `auto`/`preserve`/`aggressive` strategies, compaction with the `small` model
- Profiles: `qwen-code`, `codex`, `cc-style`, `swe-agent`, `kimi-code`. Text and grammar tool-calling modes; `edit.apply_patch`, `edit.whole_file`, `edit.search_replace_blocks`
- Remaining tool-call parsers (mistral, glm, json-block, deepseek, kimi)
- `anthropic-messages` + `openai-responses` adapters
- `cutoffharness eval` v1 with about 20 tasks; first `docs/compat.md`
- AGENTS.md discovery (chain + nested on-demand + imports); memory (`memory` tool, index injection, configurable path)

**Exit criteria**
- [ ] A 50-step task completes inside a 32k window with Qwen3-Coder-30B on llama.cpp
- [ ] Prefix-stability invariant passes for all default profiles
- [ ] Eval shows profile matching ≥ `generic` for each targeted family, otherwise the profile isn't shipped
- [ ] Additional hosted services (Together, DeepInfra) work through provider files alone, with no code changes

## M3 — Ecosystem: "it plugs into everything, safely"

- MCP via rmcp: stdio + streamable HTTP, `mcp.d`, deferred tools + `tool_search`, prompts as commands, resources, sampling (gated), elicitation, `/mcp`
- Skills (all discovery paths, progressive disclosure, `/skill` invocation); custom commands
- Permission engine v2: tree-sitter-bash / PowerShell analysis, "always allow" writes to config
- Project trust flow
- Hooks (`hooks.d`)
- Checkpoints: edit pre-images, `/undo`, `/rewind` with file restore; optional gix shadow snapshots
- Hot reload of config directories
- `/fork`, `/export`, auto titles

**Exit criteria**
- [ ] GitHub MCP server works on Windows via `npx` (`.cmd` resolution) and on Linux/macOS
- [ ] A malicious `.cutoffharness/mcp.d` in an untrusted repo doesn't execute
- [ ] `git status && rm -rf x` is split and needs approval for `rm` in `ask` mode
- [ ] Rewind restores files byte-exactly, CRLF files included

## M4 — IDE-grade feedback: "it knows when it broke the build"

- `cutoffharness-lsp`: lazy start, document sync, progress, diagnostics-after-edit delta, LSP tools, rename with approval
- Default `lsp.d` files for the main languages (no auto-install)
- Headless `cutoffharness exec` with text/json/stream-json, exit codes
- `shell.background` processes
- Startup/perf budgets enforced in CI

**Exit criteria**
- [ ] Edits that introduce type errors get diagnostics in the same tool result (rust-analyzer, pyright, tsserver, gopls)
- [ ] `cutoffharness exec` runs in GitHub Actions on all three OSes
- [ ] LSP URIs are correct on Windows (drive letters, spaces)

## M5 — Enterprise hardening: "a company can roll it out"

- Sandboxing: Linux Landlock+seccomp, macOS Seatbelt, **Windows** restricted token/AppContainer (spike outcome), stdio MCP/LSP sandbox option
- Offline policy end-to-end, egress allowlist, `web_fetch` (opt-in, SSRF-protected)
- `[policy]` locks, audit log, `cutoffharness doctor` enterprise checks
- mTLS, custom CA, proxy validation; multi-`base_urls` failover
- Packaging: cargo-dist, MSI (+ signing), deb/rpm, Homebrew, defaults bundle as a separate package, SBOM, `docs/enterprise.md`, air-gapped build recipe
- Sub-agents (`task` tool) with `/tasks` views
- Dynamic tree-sitter grammars (`libloading`)
- MCP OAuth for remote servers

**Exit criteria**
- [ ] Air-gapped VM: build from vendored sources, run against a local vLLM, zero external connections (verified with a network capture)
- [ ] Sandboxed `full-auto` session can't write outside the workspace or reach the network, on all three OSes (or Windows limitation documented with mitigation)
- [ ] Policy-locked settings can't be overridden from user/project/env/CLI

## M6 — Editor integration & extras

- ACP agent (`cutoffharness acp`), tested with Zed
- Optional: code-execution sandbox tool (maki-style), repo map, OpenTelemetry export (opt-in), daemon/socket mode, cutoffharness-as-MCP-server

## Cross-cutting, every milestone

- Windows checklist items from each doc
- Fixtures for every new model or server quirk discovered
- Docs updated with each feature (user docs live in `docs/`, separate from `docs/plan/`)
