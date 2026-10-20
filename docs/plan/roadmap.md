# Roadmap

Milestones are ordered by dependency and value. Each has **exit criteria** that must pass on **Linux, macOS and Windows** in CI before moving on. Dates are left out on purpose; scope is what's fixed.

## Scaffold (M1 kickoff) — done 2026-10-17

Status: landed. `cargo test --workspace` passes (29 tests) and clippy is clean with `-D warnings` on Linux, plus `cargo clippy` cross-checks for `x86_64-pc-windows-msvc` and `aarch64-apple-darwin`. The TUI was exercised in a real PTY: first output in about 7 ms (debug build), `/help` modal, a stub turn, and clean double Ctrl-C exit. Not yet run: CI itself, `cargo deny`, and the MSRV check, which all need the GitHub remote.

Decided 2026-10-17: the first scaffold creates the **full workspace**. All 17 crates exist, with their dependency edges and placeholder modules, so the architecture can be seen and compiles from day one. Only the M1 crates get real skeletons:

- `quack-protocol`: `Op` / `Event` / `LoadState` types
- `quack-config`: per-OS layer paths, TOML loading for `config.toml` + `providers.d`, miette diagnostics, `config check`
- `quack-cli`: clap commands (`tui` default, `exec`, `config check|show`, `doctor` stubs)
- `quack-tui`: full-screen app loop, startup panel fed by `StartupProgress`, input box, slash-command popup + modal framework with one real modal (`/help`)
- `quack-core`: engine task with an `EngineHandle`, and fake startup subsystems that report progress

Also: Cargo workspace lints/profiles, `rust-toolchain.toml`, CI (Linux x64, macOS ARM64, Windows x64 + ARM64), `cargo-deny`, `LICENSE` (Apache-2.0), `NOTICE`, the `defaults/` directory layout with schema stubs, and `docs/plan/` kept as-is.

## M0 — Spikes (de-risk before committing to designs)

Primary manual test target: **hosted open-model services** (Novita, Fireworks, Together, OpenRouter). Local servers are covered by CPU-only CI containers ([13](13-testing-and-evals.md#ci)). Hosted services run vLLM/SGLang behind the scenes but hide the tool-call parser configuration, so they are a realistic, and sometimes worse, test of tool-call robustness.

| Spike | Question to answer | Output |
|---|---|---|
| Tool-call reliability | Record native tool-call streams from 2+ hosted services for 3–4 current coding models (for example Qwen3-Coder or Qwen3.6, GLM-5.x, Kimi K3, DeepSeek V4), plus llama.cpp `--jinja` and Ollama on CPU with a tiny model. How often do calls leak into content, and do services differ for the same model? | First fixtures in `tests/fixtures/providers/`, list of required text parsers |
| Prefix-cache behaviour | On hosted services: compare `cached_tokens` reporting, cost and TTFT for append-only vs. mid-history edits at 16k/64k/128k. On CPU llama.cpp: confirm cache reuse through `timings`. A GPU measurement is deferred until hardware is available | Numbers that seed the `auto` cache strategy ([05](05-context-management.md)) |
| Windows process & terminal | Job Objects + ConPTY + crossterm key events + pwsh invocation, in a throwaway binary | Confirmed approach for `quack-shell` and the TUI input layer |
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
- Sessions: JSONL log per session plus a per-project index, written as the turn happens (crash-safe, torn last line repaired), `--continue`, `--resume <id>`, `quack sessions`
- `/models`: catalogue of every provider (declared plus discovered models, unavailable ones with the reason), switching mid-session keeps history and approvals; `/models provider/model` switches directly
- TUI: markdown rendering, tool calls with diffs and output, reasoning, approval box, context usage; `exec`: tool activity on stderr, headless denials, `--permission-mode`
- Defaults bundle v0: novita, fireworks, together, openrouter, deepinfra, llama-cpp, ollama, vllm, lmstudio, sglang (none tagged verified yet). The bundle folders now use the layer layout (`providers.d/`, `profiles.d/`, …)
- Verified live against one hosted OpenAI-compatible service (Charm Hyper, `qwen3.8-flash`): a bug-fix task via `exec`, and a shell approval in the TUI

Round 2 (2026-10-18):
- `/resume` picker (and `/resume <id>`), `/new` starts a fresh saved session, `/title`. Session switches wait for the running turn, like queued messages
- `/config` modal: every setting with value, origin and description; `/config set <key> <value>`; Ctrl-S in `/models` saves the default model. Writes keep comments (`toml_edit`), are schema-validated, atomic, and refuse policy-locked keys
- Text tool calls are held back while streaming, so the raw `<tool_call>` markup never shows
- Context estimate corrected by each response's reported prompt tokens (per-model ratio); live context figure after tool results and for servers that report no usage
- Native `ollama` adapter (`proto-ollama`): `/api/chat` NDJSON with `num_ctx` per request, tool calls, `thinking`, discovery via `/api/tags` + `/api/show` (window capped at 32k). The default `ollama.toml` now uses it
- Windows Job Objects with kill-on-close for every shell command (`taskkill /T` fallback)
- `keyring` secrets and `quack keyring set|delete` (feature `keyring`, on by default). Native stores on Windows/macOS (5–6 crates); on Linux the Secret Service through `secret-tool`, with no extra crates

Round 3 (2026-10-18): exit-criteria tests in `crates/quack-core/tests/invariants.rs` (removal, no egress, resume fidelity), `scripts/no-egress.sh` and `scripts/first-frame.sh` as Linux CI steps, hidden `--trace-startup`. Keyring on Linux moved from zbus to `secret-tool` (131 → 0 crates).


Round 4 (2026-10-18): local-server CI (`scripts/local-server-ci.sh`, job `local-servers`). Found by it: llama.cpp discovery reported the trained window (262k) instead of the served one (`-c`); now read from `/props`. Empty replies get one nudge, as planned in [04](04-agent-loop-and-tools.md#loop-guards). Model scope narrowed to current coding families ([03](03-model-profiles.md#why-profiles-exist)).

Round 5 (2026-10-18): the remaining M1 slash commands.
- `/providers`: usable or why not, and the models of each provider.
- `/permissions [mode]`: mode picker and the configured rules.
- `/doctor`: the same checks as `quack doctor`, now real: config, defaults, each provider contacted, model, project, AGENTS.md, shell, git, `secret-tool`, the sessions folder written to, memory and terminal.
- `/copy [code]` through OSC 52.

Speed in the status bar (`63 t/s`, from M2), and a `speed` event with time to first token. Interactive provider setup and rule editing wait for `quack setup` (M2). `/reload` moves to M3 with hot reload.

Still open for M1: the unchecked exit criteria below. `cargo deny`, the macOS/Windows builds of `aws-lc-sys` (reqwest's TLS crypto), the Windows job-kill test and the macOS/Windows keyring stores are only checked in CI or by hand.

- Workspace scaffold, CI matrix (Linux/macOS/Windows), `cargo-deny`, Apache-2.0 + NOTICE
- `quack-protocol` (Op/Event), `quack-core` loop with interrupt and loop guards
- `quack-config`: layers, per-OS paths, `providers.d`, secrets (env/cmd/keyring/file), miette diagnostics, `config check`
- `quack-providers`: `openai-chat` + `ollama` adapters, SSE streaming, retries, hermes/qwen3-xml/think-tag parsers, discovery, `estimate` tokenizer with server correction
- Profiles: loader + `generic` profile only, minijinja rendering, parameter mapping
- Tools: `read`, `write`, `edit.str_replace` (with fuzzy matching), `glob`, `grep`, `list_dir`, `shell` (bash/zsh/pwsh, Job Objects, timeouts, output trimming)
- Permissions: modes + allow/deny rules (string glob only; tree-sitter analysis comes in M3)
- TUI: startup panel, transcript with markdown, input editor, approval dialog with diff, status bar, slash-command palette and modal framework, `/models`, `/resume`, `/config`, `/help`
- Sessions: JSONL log, `--continue`, `--resume` picker
- Defaults bundle v0: `novita`, `fireworks`, `openrouter` (verified), `llama-cpp`, `ollama`, `vllm` (verified for protocol via CI containers; marked `# verified: protocol-only`), `generic` profile

**Exit criteria**
- [ ] Fix a failing test in a sample repo with a current Qwen coding model on **two hosted services**, on Linux, macOS and Windows (x64 + ARM64, pwsh)
- [x] The CPU llama.cpp and Ollama CI jobs complete a scripted tool-call turn: `scripts/local-server-ci.sh` (pinned llama.cpp b11718 and Ollama 0.40.6 images, Qwen3.5-0.8B), the `local-servers` CI job. Passed locally with podman; not yet run on GitHub
- [ ] First frame < 50 ms Linux / < 120 ms Windows. Linux: median 3–9 ms (release, PTY, defaults bundle) via `scripts/first-frame.sh`, a CI step. Windows: not measured yet (needs a console on the runner, or by hand)
- [x] Deleting any default provider file leaves startup clean (`tests/invariants.rs`: every file and folder of the bundle, one at a time)
- [x] No-egress invariant test passes: in-process through a recording proxy, and in CI on Linux with `scripts/no-egress.sh` (loopback-only network namespace plus a `connect()` trace)
- [x] Resume reproduces the context exactly: the request after a resume equals the live session's request byte for byte, including the shipped `generic` system prompt and tool calls. Re-check in M2 once pruning and compaction exist

## Review 2026-10-18: practices check

Compared against current harnesses (Codex CLI, OpenCode, Goose, Pi/oh-my-pi, Claude Code) and the current specs. AGENTS.md, SKILL.md skills, data-only provider/MCP files, JSONL sessions, permission modes, hooks, checkpoints and per-model evals are all current practice and already planned. Changes that came out of it:
- **MCP 2026-07-28** (stateless, breaking) is the M3 target; older revisions are a fallback ([09](09-mcp.md#implementation)).
- **Hash-anchored edits** (`edit.hashline`) join the M2 edit formats, decided per profile by the eval ([04](04-agent-loop-and-tools.md#edit-tools)).
- **Published compatibility results** become a headline feature ([13](13-testing-and-evals.md#publishing-results)).
- **First run in under a minute** through local server detection offered from the startup panel (M2, [14](14-packaging-and-distribution.md#first-run-experience)).
- **Plan mode** and an optional planner/editor model split ([04](04-agent-loop-and-tools.md#plan-mode)).
- **Speed in the status bar** (tokens per second, prompt processing, cache hits) rather than cost ([06](06-tui.md#layout)).
- **Worktree sessions** and **bundles** (M3), and sub-agents moved from M5 to M3.
- **Dependency policy**: keep the crate count low ([00](00-architecture.md#dependency-policy)). The keyring feature went from 131 crates on Linux to none.

Left out on purpose: a hosted marketplace, telemetry-backed features and cloud execution, which conflict with the no-egress promise.

## M2 — Context & profiles: "it stays useful in small windows"

- Tokenizers: `hf-file`, `tiktoken`, `server`. Context ledger, `/context`, cache-hit display
- `index` tool (tree-sitter skeletons) + `read` large-file skeleton behaviour + symbol reads; `lang-common` grammars
- Prune rules 1–6, epoch-based `auto`/`preserve`/`aggressive` strategies, compaction with the `small` model
- Profiles for current coding families only ([03](03-model-profiles.md#why-profiles-exist)): `qwen-code`, `codex`, `cc-style`, `swe-agent`, `kimi-code`. Text and grammar tool-calling modes; `edit.apply_patch`, `edit.whole_file`, `edit.search_replace_blocks`, `edit.hashline`
- Remaining tool-call parsers, only for formats the current families emit (expected: glm, deepseek, kimi, json-block; decided from the M0 fixtures)
- `anthropic-messages` + `openai-responses` adapters
- `quack eval` v1 with about 20 tasks; first `docs/compat.md`, published with the release and linked from the default provider files
- `quack setup` and local server detection from the startup panel; speed in the status bar
- Plan mode (`/plan`, `plan` permission mode); optional `[model] planner`
- AGENTS.md discovery (chain + nested on-demand + imports); memory (`memory` tool, index injection, configurable path)

### Progress (2026-10-18)

- Pruning rules 1 (superseded reads) and 3 (large old outputs). Compaction runs at the threshold, on server overflow (one retry) and with `/compact [focus]`. If the model can't write a summary, a plain list is kept instead. Both pruning and compaction are recorded in the session log and replayed on resume. Details: [05](05-context-management.md#as-implemented-m2-2026-10-18)
- `/context` modal: one band of the segments (system prompt, tools, summary, user, model, tool results with the pruned count) with the compaction point marked, the room left in turns at the recent pace, and a suggestion. The status bar shows room left instead of a gauge
- `/stats`: tokens, cache hits, cost from `[models.cost]` or the models endpoint's `pricing` (logged per request, so it survives resume), where the time went, latency, speed and per-tool counts
- AGENTS.md: system and user files, then the chain from the git root down to the cwd (the first configured name wins in each directory), `@` imports (one level, kept inside the workspace), and nested files attached to the first tool result that touches their directory ([08](08-instructions-skills-commands.md#rules))
- Plan mode: `/plan [task|accept|off]`, Shift-Tab, edits refused and only read-only commands run, the plan request appended to the message ([04](04-agent-loop-and-tools.md#plan-mode)). Since 2026-10-19 the model asks about big decisions with `ask_user` (a picker in the TUI, skipped when headless), records the plan as a `todo` checklist, and a review bar offers accept, accept in auto-edit, or revise by typing. `[model] planner` (2026-10-19): plan mode runs on the planner, and accepting the plan hands the session's model one brief instead of the planner's transcript, so its prompt cache holds; `quack eval --planner` measures it ([04](04-agent-loop-and-tools.md#planner--editor-split))
- Memory: markdown notes plus an index per scope (global, and project in the data dir or the repo), the index in the prompt via `{{ memory_index }}`, a lenient `memory` tool, `memory.writes = "ask"`, and `/memory` ([07](07-sessions-memory-checkpoints.md#behaviour))
- `quack setup` and `/setup`: local server detection (loopback only), hosted services with the key stored in the OS credential store, or any URL, then the default model ([14](14-packaging-and-distribution.md#as-implemented-2026-10-18))
- Mistral Large 4 through Mistral's API: a Mistral dialect in `openai-chat` (strict fields, `none`/`high` effort, reasoning sent back as `thinking` parts, 9-character tool-call ids, `prompt_cache_key`); not yet run live ([02](02-providers.md#mistral-large-4-2026-10-18))
- Default providers are data again: 5 local servers and 13 hosted services in `defaults/providers.d`, each unused until it runs or has a key, and hosted ones never auto-selected. The binary names no server, so deleting the files removes them completely. `[policy.providers] hosts` and `[policy.mcp] hosts` limit what can be contacted at all, and `doctor` lists the reachable hosts ([02](02-providers.md#servers-not-engines-2026-10-18))

Round 2 (2026-10-18), tested against mock servers only; nothing here has been run against a live model yet:
- **Prefix stability** is tested (`core/tests/prefix.rs`): for every shipped profile, and with hashline edits, a session with writes, reads, a plan, memory, plan mode, edits, commands and a compaction. Each request extends the one before it, and the system prompt and tools stay the same through compaction.
- **Edit formats**: `apply_patch` (Codex format, several files per call), `search_replace_blocks` (`edit_blocks`), `whole_file` (only `write`), next to `str_replace` and `hashline`. `[tools] edit_format = "auto"` takes the profile's `edit_format`. Multi-file calls are checked whole before writing, ask once for all their files, and get an undo point per file ([04](04-agent-loop-and-tools.md#as-implemented-2026-10-18))
- **Profiles**: `qwen-code`, `cc-style`, `kimi-code` and `codex`, shipped with empty `[match]` lists (opt-in) until the eval shows each beats `generic`. Profiles gained `exclusive`, `fixed` parameters, parameter `default`s, descriptions from `.md` files, and the `one_to_zero_based`, `join_lines` and `argv` transforms. Unlisted internal tools are now offered under their own names. `--profile` and `[model] profile` choose one for the session ([03](03-model-profiles.md#as-implemented-2026-10-18))
- **`quack eval` v1** with 21 tasks in `evals/tasks/`, each with a reference solution that a test applies ([13](13-testing-and-evals.md#as-implemented-2026-10-18)). `docs/compat.md` waits for real runs. First baseline (2026-10-19, Charm Hyper): qwen3.8-flash/max, glm-5.3(-flash), deepseek-v4.1-flash, deepseek-v4-pro, kimi-k3 and gpt-oss-120b, each with `generic` and its family profile, one run per task. All pass 21/21 with no parse failures, for $2.42 in total. No family profile is clearly ahead of `generic`, so every `[match]` stays empty. The suite is too easy to tell them apart; harder tasks and `--repeat 3` come next. The process is in AGENTS.md ("Evaluating new models"). Then `context.md` (2026-10-19) moved the per-session part of each prompt to the first user message ([03](03-model-profiles.md#as-implemented-2026-10-18)). Rerunning `generic` cut the uncached prompt tokens by 45% on deepseek-v4.1-flash (cost $0.047 → $0.035), by 22% on kimi-k3 (prompt cost −14%; the total moved only $0.38 → $0.36, because this run produced more output) and by 3% on glm-5.3, whose cache already reached past the workspace path. The hard suite (2026-10-19, `evals/hard/`, 14 tasks with hidden tests, most on one shared shop project), failure labels on every failed run, and `scripts/eval-compare.py` (paired comparisons with confidence intervals) are built for the next round; none has been run against a real model yet ([13](13-testing-and-evals.md#as-implemented-2026-10-18))
- **`index` tool** and outlined reads (tree-sitter, `lang-common`: Rust, Python, JavaScript, TypeScript, Go, C, C++; 10 new crates, all MIT/Apache-2.0) ([05](05-context-management.md#as-implemented-2026-10-18-index-and-pruning-rules))
- **Pruning rules 2, 4 and 5**: stale reads, old whole-file reads kept as outlines, failed attempts that a later call fixed. Rule 6 (reasoning) is left out: reasoning is only sent back where the server needs it, so dropping it saves nothing and would break the cache
- **Adapters**: `anthropic-messages` (with cache breakpoints) and `openai-responses` (stateless) ([02](02-providers.md#anthropic-messages-and-openai-responses-2026-10-18))
- **Text tool-call parsers**: `glm`, `deepseek` (V3 and V3.1 forms), `kimi`, and `json-block` (only when chosen), written from the published chat templates, not yet from recorded fixtures
- **Slash commands**: `/profiles` (each profile rendered for the current model; switch for the session or save as default), `/instructions` (the AGENTS.md files the model has, with sizes), custom commands from `commands/` folders with `/commands`, and `/todo` (the whole plan above the input)
- Not done: the `hf-file`, `tiktoken` and `server` tokenizers. The estimate corrected by the server's reported usage already covers budgeting; the first two would add heavy dependencies

**Exit criteria**
- [ ] A 50-step task completes inside a 32k window with a current ~30B Qwen coding model on llama.cpp
- [x] Prefix-stability invariant passes for all default profiles (`core/tests/prefix.rs`)
- [ ] Eval shows profile matching ≥ `generic` for each targeted family, otherwise the profile isn't shipped
- [ ] Additional hosted services (Together, DeepInfra) work through provider files alone, with no code changes

## M3 — Ecosystem: "it plugs into everything, safely"

- MCP via rmcp, targeting spec 2026-07-28 with fallback to older revisions: stdio + streamable HTTP, `mcp.d`, deferred tools + `tool_search`, prompts as commands, resources, sampling (gated), elicitation, `/mcp`
  - Landed early (2026-10-18): stdio + streamable HTTP, `mcp.d`, deferred tools + `tool_search`, `mcp:` permission rules, restarts, `/mcp`, doctor row ([09](09-mcp.md#as-implemented-2026-10-18)). Still open: prompts, resources, sampling, elicitation, OAuth
- Skills (all discovery paths, progressive disclosure, `/skill` invocation); custom commands
- Permission engine v2: tree-sitter-bash / PowerShell analysis, "always allow" writes to config
- Project trust flow
- Hooks (`hooks.d`)
- Checkpoints: edit pre-images, `/undo`, `/rewind` with file restore; optional gix shadow snapshots
- Hot reload of config directories
- `/fork`, `/export`, auto titles; `/fork --worktree` parallel sessions
- Bundles (`bundles/`, `/bundles`)
- Sub-agents (`task` tool) with `/tasks` views (moved from M5)
  - Landed early (2026-10-19): `agents.d` (`explore` read-only on `[model] small`, `general`), `task` with a fresh context and its own todo list, read-only agents in parallel and allowed in plan mode, workers' approvals through the client, `todo` hand-off that ticks plan items, `Event::Plan`, `/tasks`, a `doctor` row ([04](04-agent-loop-and-tools.md#sub-agents)). Mock-server tests only; not yet measured with `quack eval`

Landed early (2026-10-18): the project trust flow (Trust modal, `/trust`, `quack trust`, fingerprinted decisions; [01](01-configuration.md#as-implemented-2026-10-18)), and `/reload` keeping the conversation (folder watching is still to do; [01](01-configuration.md#hot-reload)). `/undo` and `/rewind` from edit pre-images ([07](07-sessions-memory-checkpoints.md#as-implemented-2026-10-18-ahead-of-m3)). Skills discovery, the `skill` tool, `{{ skills }}`, `/<skill>` and `/skills` ([08](08-instructions-skills-commands.md#as-implemented-2026-10-18-ahead-of-m3)). Still to do for skills: other harnesses' folders through the detection prompt. Hooks (2026-10-19): `hooks.d` with all seven events, `match` on tools, targets and prompts, secrets in `env`, enforced system-layer hooks, `on_error`, a `/doctor` row and the `jira` example ([11](11-security.md#as-implemented-2026-10-19-ahead-of-m3)).

**Exit criteria**
- [ ] GitHub MCP server works on Windows via `npx` (`.cmd` resolution) and on Linux/macOS
- [x] A malicious `.quack/mcp.d` in an untrusted repo doesn't execute (`core/tests/trust.rs`: nothing starts until trusted, it starts after the trust reload, and editing it asks again)
- [x] `git status && rm -rf x` is split and needs approval for `rm` in `ask` mode (hand-written splitter for now; deny rules now see every command, even in `full-auto`; [11](11-security.md#as-implemented-2026-10-18-ahead-of-m3))
- [x] Rewind restores files byte-exactly, CRLF files included (`core/tests/rewind.rs`; [07](07-sessions-memory-checkpoints.md#as-implemented-2026-10-18-ahead-of-m3))

## M4 — IDE-grade feedback: "it knows when it broke the build"

- `quack-lsp`: lazy start, document sync, progress, diagnostics-after-edit delta, LSP tools, rename with approval
- Default `lsp.d` files for the main languages (no auto-install)
- Headless `quack exec` with text/json/stream-json, exit codes
- `shell.background` processes
- Startup/perf budgets enforced in CI

Landed early (2026-10-18): `quack-lsp` (lazy start, document sync, push and pull diagnostics, progress, the diagnostics delta after edits, the `lsp` tool, `/lsp`) and the default `lsp.d` files ([10](10-lsp.md#as-implemented-2026-10-18-ahead-of-m4)). Still to do: `lsp.rename`, `didChangeWatchedFiles`, a status-bar count.

**Exit criteria**
- [ ] Edits that introduce type errors get diagnostics in the same tool result (rust-analyzer, pyright, tsserver, gopls) — verified with TypeScript 7 and the fixture server; rust-analyzer, pyright and gopls not yet run here
- [ ] `quack exec` runs in GitHub Actions on all three OSes
- [ ] LSP URIs are correct on Windows (drive letters, spaces) — unit test in `quack-lsp/src/pos.rs` runs in the Windows CI jobs

## M5 — Enterprise hardening: "a company can roll it out"

- Sandboxing: Linux Landlock+seccomp, macOS Seatbelt, **Windows** restricted token/AppContainer (spike outcome), stdio MCP/LSP sandbox option
- Offline policy end-to-end, egress allowlist, `web_fetch` (opt-in, SSRF-protected)
- `[policy]` locks, audit log, `quack doctor` enterprise checks
- mTLS, custom CA, proxy validation; multi-`base_urls` failover
- Packaging: release archives and install scripts in two flavours, `quack` (with defaults) and `quack-plucked` (landed early); still to do: MSI (+ signing), deb/rpm, Homebrew, SBOM, `docs/enterprise.md`, air-gapped build recipe
- Dynamic tree-sitter grammars (`libloading`)
- MCP OAuth for remote servers

Landed early (2026-10-18): the sandbox on Linux and macOS, `[sandbox]` settings, `sandbox: false` with approval, and `sandbox = true` for MCP and LSP servers ([11](11-security.md#as-implemented-2026-10-18-ahead-of-m5)). Windows is still to do.

**Exit criteria**
- [ ] Air-gapped VM: build from vendored sources, run against a local vLLM, zero external connections (verified with a network capture)
- [ ] Sandboxed `full-auto` session can't write outside the workspace or reach the network, on all three OSes (or Windows limitation documented with mitigation) — Linux done (`core/tests/sandbox.rs`), macOS written and tested in CI only, Windows unavailable with commands asking first ([11](11-security.md#as-implemented-2026-10-18-ahead-of-m5))
- [ ] Policy-locked settings can't be overridden from user/project/env/CLI

## M6 — Editor integration & extras

- ACP agent (`quack acp`), tested with Zed
- Optional: code-execution sandbox tool (maki-style), repo map, OpenTelemetry export (opt-in), daemon/socket mode, quack-as-MCP-server

## Cross-cutting, every milestone

- Windows checklist items from each doc
- Fixtures for every new model or server quirk discovered
- Docs updated with each feature (user docs live in `docs/`, separate from `docs/plan/`)
