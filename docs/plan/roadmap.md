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
- `/context` modal: one band of the segments (system prompt, tools, summary, user, model, tool results with the pruned count) with the compaction point marked, the room left in turns at the recent pace, and a suggestion. The status bar shows context growing towards the model's maximum context (turning yellow and red as compaction approaches)
- `/stats`: tokens, cache hits, cost from `[models.cost]` or the models endpoint's `pricing` (logged per request, so it survives resume), where the time went, latency, speed and per-tool counts
- AGENTS.md: system and user files, then the chain from the git root down to the cwd (the first configured name wins in each directory), `@` imports (one level, kept inside the workspace), and nested files attached to the first tool result that touches their directory ([08](08-instructions-skills-commands.md#rules))
- Plan mode: `/plan [task|accept|off]`, Shift-Tab, edits refused and only read-only commands run, the plan request appended to the message ([04](04-agent-loop-and-tools.md#plan-mode)). Since 2026-10-19 the model asks about big decisions with `ask_user` (a picker in the TUI, skipped when headless), records the plan as a `todo` checklist, and a review bar offers accept, accept in auto-edit, or revise by typing. `[model] planner` (2026-10-19): plan mode runs on the planner, and accepting the plan hands the session's model one brief instead of the planner's transcript, so its prompt cache holds; `quack eval --planner` measures it ([04](04-agent-loop-and-tools.md#planner--editor-split))
- Memory: markdown notes plus an index per scope (global, and project in the data dir or the repo), the index in the prompt via `{{ memory_index }}`, a lenient `memory` tool, `memory.writes = "ask"`, and `/memory` ([07](07-sessions-memory-checkpoints.md#behaviour))
- `quack setup` and `/setup`: local server detection (loopback only), hosted services with the key stored in the OS credential store, or any URL, then the default model ([14](14-packaging-and-distribution.md#as-implemented-2026-10-18))
- Setup in the TUI (2026-10-20): `/setup` opens on its own when no model is ready, and has tabs for this machine, hosted services (the key is typed into a masked prompt and stored in the OS credential store) and any server by URL, then picks the model from `/models`. A `quack-plucked` user no longer needs to know about `quack setup` ([14](14-packaging-and-distribution.md#setup-in-the-tui-2026-10-20))
- Mistral Large 4 through Mistral's API: a Mistral dialect in `openai-chat` (strict fields, `none`/`high` effort, reasoning sent back as `thinking` parts, 9-character tool-call ids, `prompt_cache_key`); not yet run live ([02](02-providers.md#mistral-large-4-2026-10-18))
- Default providers are data again: 5 local servers and 13 hosted services in `defaults/providers.d`, each unused until it runs or has a key, and hosted ones never auto-selected. The binary names no server, so deleting the files removes them completely. `[policy.providers] hosts` and `[policy.mcp] hosts` limit what can be contacted at all, and `doctor` lists the reachable hosts ([02](02-providers.md#servers-not-engines-2026-10-18))

Round 2 (2026-10-18), tested against mock servers only; nothing here has been run against a live model yet:
- **Prefix stability** is tested (`core/tests/prefix.rs`): for every shipped profile, and with hashline edits, a session with writes, reads, a plan, memory, plan mode, edits, commands and a compaction. Each request extends the one before it, and the system prompt and tools stay the same through compaction.
- **Edit formats**: `apply_patch` (Codex format, several files per call), `search_replace_blocks` (`edit_blocks`), `whole_file` (only `write`), next to `str_replace` and `hashline`. `[tools] edit_format = "auto"` takes the profile's `edit_format`. Multi-file calls are checked whole before writing, ask once for all their files, and get an undo point per file ([04](04-agent-loop-and-tools.md#as-implemented-2026-10-18))
- **Profiles**: `qwen-code`, `cc-style`, `kimi-code` and `codex`, shipped with empty `[match]` lists (opt-in) until the eval shows each beats `generic`. Profiles gained `exclusive`, `fixed` parameters, parameter `default`s, descriptions from `.md` files, and the `one_to_zero_based`, `join_lines` and `argv` transforms. Unlisted internal tools are now offered under their own names. `--profile` and `[model] profile` choose one for the session ([03](03-model-profiles.md#as-implemented-2026-10-18))
- **`quack eval` v1** with 21 tasks in `evals/tasks/`, each with a reference solution that a test applies ([13](13-testing-and-evals.md#as-implemented-2026-10-18)). `docs/compat.md` waits for real runs. First baseline (2026-10-19, Charm Hyper): qwen3.8-flash/max, glm-5.3(-flash), deepseek-v4.1-flash, deepseek-v4-pro, kimi-k3 and gpt-oss-120b, each with `generic` and its family profile, one run per task. All pass 21/21 with no parse failures, for $2.42 in total. No family profile is clearly ahead of `generic`, so every `[match]` stays empty. The suite is too easy to tell them apart; harder tasks and `--repeat 3` come next. The process is in AGENTS.md ("Evaluating new models"). Then `context.md` (2026-10-19) moved the per-session part of each prompt to the first user message ([03](03-model-profiles.md#as-implemented-2026-10-18)). Rerunning `generic` cut the uncached prompt tokens by 45% on deepseek-v4.1-flash (cost $0.047 → $0.035), by 22% on kimi-k3 (prompt cost −14%; the total moved only $0.38 → $0.36, because this run produced more output) and by 3% on glm-5.3, whose cache already reached past the workspace path. The hard suite (2026-10-19, `evals/hard/`, 14 tasks with hidden tests, most on one shared shop project), failure labels on every failed run, and `scripts/eval-compare.py` (paired comparisons with confidence intervals) are built for the next round; none has been run against a real model yet ([13](13-testing-and-evals.md#as-implemented-2026-10-18))
- **First hard-suite profile decisions** (2026-10-20, Charm Hyper, prompted by tuning for `openrouter/z-ai/glm-5.3-flash` and `openrouter/deepseek/deepseek-v4.1-flash-1017`; the same model ids sit on Charm Hyper, so the runs carry over). Two new profiles written from each vendor's own harness — `zcode` (tool names follow zai-org/ZCode, Apache-2.0) and `dsh` (follow deepseek-ai/deepseek-harness, MIT; lowercase tools, `todo_write`, bash-first), prompts original. Small suite first (21 tasks, 6 setups, $0.17): all 21/21, zero parse failures. Then `--suite evals/hard --repeat 3` (42 runs each): deepseek-v4.1-flash-1017 `dsh` 42/42 vs `generic` 42/42, steps 367 vs 424, tool errors 7 vs 14, parse 0 vs 1, $0.364 vs $0.449 — steps, cost and prompt tokens all clearly lower (`eval-compare` intervals exclude zero); glm-5.3-flash `zcode` 37/42 vs 35/42 (not clear), steps 482 vs 539 (clearly lower), tool errors 11 vs 21, edit failures 0 vs 1, $0.309 vs $0.344. Decision: `[match]` enabled — `zcode` for `*glm-5*`, `dsh` for `*deepseek-v4*` and `deepseek-flash`, both priority 50; `cc-style`'s proposed globs slimmed to MiniMax/MiMo. Total spend $1.64. Also shipped the same day, prepared but unmeasured (no OpenRouter key): `vibe` (Mistral Vibe tool shapes, Apache-2.0) for `openrouter/mistralai/mistral-large-4-0`, and `openrouter/meta/muse-spark-open` with the model card's recommended sampling pinned (temperature 1.0, top_p 1.0, reasoning effort high/xhigh) but no profile — Meta's Muse Code harness is closed and their prompting guide says to pass tools natively, so it starts on `generic`
- **Mistral and Meta measured live on OpenRouter** (2026-10-21; the key held $5.94, so the runs were scoped to fit and $3.56 was spent). `mistralai/mistral-large-4-0`: small suite 21/21 under both profiles, `vibe` $0.158 vs `generic` $0.228 (cache 80% vs 63%), parse 0 vs 2. One hard pass each — `--repeat 3` would cost about $8: both 14/14, but `generic` (running the hashline default that shipped the same day) hit 12 parse and 23 edit failures, 236 steps, $1.70, against `vibe`'s 0 and 1, 172 steps, $0.94; even at n=14 eval-compare calls steps, cost and prompt tokens clearly lower. Decision: `vibe` keeps `[match]` empty — the rule needs `--repeat 3` — and the numbers sit in its `profile.toml` comment; a `generic` run with `-c tools.edit_format=str_replace` (about $1.70) would separate the tool-name effect from the edit-format one. `meta/muse-spark-open` (no profile, by design): `generic` beats `cc-style` on the small suite (21/21 both, 85 vs 99 steps, $0.225 vs $0.249), so it stays on `generic` with the card's sampling pinned. A $0.07 spot-check on `openrouter/deepseek/deepseek-v4.1-flash-1017` the same day confirms the Charm decision transfers: `dsh` auto-matches through the OpenRouter id and wins there too (21/21 both, 72 vs 100 steps, 0 vs 7 edit failures, $0.024 vs $0.042)
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

## Proprietary model APIs (2026-10-19)

Optional support for Claude and GPT, as data like every other provider ([02](02-providers.md#claude-openai-and-chatgpt-plans-2026-10-19)):
- `anthropic.toml` and `openai.toml` (API keys), `chatgpt.toml` (a ChatGPT plan through `quack login chatgpt`, feature `auth-chatgpt`). None is contacted until set up; the sign-in server comes from the file and is subject to `[policy.providers] hosts`. Claude subscriptions are left out: Anthropic's terms forbid third-party tools from using them.
- Sealed replies: Claude's signed thinking and OpenAI's encrypted reasoning are kept with the message, in order, and sent back to the provider and setup that made them; pruning and compaction drop them. For `anthropic-messages` the plan reminder is kept in the history instead of riding on the newest message, so every request extends the one before it (`core/tests/anthropic.rs`).
- Adapters: Claude effort (`output_config.effort`), a third cache breakpoint, cache-write usage and `cost.cache_write`; Responses `prompt_cache_key`, `phase`, cache writes, and the ChatGPT plan route (tools in a namespace, no sampling or output cap). Provider files gained `extra_body` and `[auth]`.
- Tested against mock servers only (the browser sign-in end to end with a signed ID token, renewal, revocation; replay and filtering of sealed replies). Not yet run live; no family profile turned on for these models.

## M3 — Ecosystem: "it plugs into everything, safely"

- MCP via rmcp, targeting spec 2026-07-28 with fallback to older revisions: stdio + streamable HTTP, `mcp.d`, deferred tools + `tool_search`, prompts as commands, resources, sampling (gated), elicitation, `/mcp`
  - Landed early (2026-10-18): stdio + streamable HTTP, `mcp.d`, deferred tools + `tool_search`, `mcp:` permission rules, restarts, `/mcp`, doctor row ([09](09-mcp.md#as-implemented-2026-10-18)). Prompts as `/<server>:<prompt>` commands and resources through `list_mcp_resources`/`read_mcp_resource` landed 2026-10-21 ([09](09-mcp.md#prompts-and-resources-2026-10-21)), then elicitation (forms as `ask_user` questions) and gated sampling (`mcp:<server>/sampling`), both also over 2026-07-28 `input_required` ([09](09-mcp.md#elicitation-and-sampling-2026-10-21)). Still open: OAuth
- Skills (all discovery paths, progressive disclosure, `/skill` invocation); custom commands
- Permission engine v2: tree-sitter-bash / PowerShell analysis, "always allow" writes to config
  - Landed early (2026-10-21): tree-sitter-bash analysis ([11](11-security.md#tree-sitter-bash-2026-10-21)). Still open: PowerShell, "always allow" writes
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
- [x] `git status && rm -rf x` is split and needs approval for `rm` in `ask` mode (tree-sitter-bash since 2026-10-21, with the hand-written splitter for builds without grammars; deny rules see every command, even in `full-auto`; [11](11-security.md#tree-sitter-bash-2026-10-21))
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
  - Landed early (2026-10-21): `grammars/<lang>/*.scm` query overrides for the compiled-in grammars, layered and trust-gated, with `; extends` ([05](05-context-management.md#as-implemented-2026-10-18-index-and-pruning-rules)). Loading grammar libraries is still open and needs `unsafe`
- MCP OAuth for remote servers

Landed early (2026-10-18): the sandbox on Linux and macOS, `[sandbox]` settings, `sandbox: false` with approval, and `sandbox = true` for MCP and LSP servers ([11](11-security.md#as-implemented-2026-10-18-ahead-of-m5)). Windows is still to do.

Landings (2026-10-21):
- **FrontierHarness Eval (Terminal-Bench & DeepSWE), partial first run**: 2 of the 30 tasks ran on Charm Hyper (`kimi-k3`, the model the baselines use) in a Runta runtime, so this is no pass rate yet. Both passed: `terminal-bench/regex-log` (8 turns, $0.14) and `datacurve/anko-typed-variable-bindings` (47 turns, $1.13, cache 95.6%); the other 28 never ran. The report calls itself not comparable, since its egress allowlist differs from the baselines'. 4 of the 6 anko attempts scored 0 because the binary was missing from the task container (`quack: command not found`), and usage was never reported, so costs came from a price table. The adapter now uploads and checks the binary before each run and fails the trial if it doesn't start, and it reads token counts and cost from quack's session log, which lives in `/logs/agent` so it survives a timeout. Next: the full 30-task run (about $20–35).
- **Hashline measured, rebuilt, and back to opt-in**: `generic` defaulted to hashline for a day; it is back on `str_replace`. Hard suite, `--repeat 3`, `deepseek-v4.1-flash` (str_replace 42/42, 424 steps, 0 edit failures, 4.60M prompt) and `glm-5.3-flash` (35/42, 539, 1, 4.96M), six hashline passes ($5.02) plus about $0.35 of diagnostics. Per-line tags: deepseek 40/42, 466 steps, 34 edit failures, 6.57M; glm 36/42, 644, 47, 7.33M (glm made up tags after `grep`, which showed none). Rebuilt after oh-my-pi's hashline mode: one `[path#tag]` per file over plain numbered lines, edits by number, numbers from older versions moved through a diff ([04](04-agent-loop-and-tools.md#edit-tools)). The edit result went from a window of fresh lines (pass 3: deepseek 460 steps, 12 edit failures; glm 578, 18) to nothing (pass 4: more steps, models guessed new numbers) to one line with the new numbers and tag (pass 5: edit failures 0 and 3, but glm 701 steps, 8.72M, after edits that applied to a range one line off and broke the code: 8 syntax errors in a kept sample, none with `str_replace`). Pass 6 adds a tree-sitter guard against such edits: deepseek 40/42, 453 steps, 7 edit failures, 5.74M, no clear difference from `str_replace`; glm 35/42, 672, 14, 7.52M, steps and prompt clearly worse. Decision rule, set before the runs: the default only if steps and prompt tokens aren't clearly worse, so no. Hashline stays as `edit_format = "hashline"` for models that fail at exact matches, where it could win.
- **Hashline hardened on the failure modes the runs showed** (2026-10-21): three behaviors ported from oh-my-pi's hashline mode to close the measured gap. `block` edits (`{"block": N}` / `after_block`) replace or insert after the whole definition around a line from the file's outline, so the `end` of a range, the common wrong number, never has to be counted; unseen-lines enforcement refuses edits to lines the cited output never showed (elided middles, outlines, numbers from memory) and reveals them so a straight retry works; and a lone off-by-one range that orphans closers is mended by taking them in when that restores the parse, instead of costing a refusal round-trip. Tool description rewritten to teach tight ranges and the new shapes ([04](04-agent-loop-and-tools.md#edit-tools)). Measured the same day, `deepseek-v4.1-flash`, hard suite `--repeat 3` ($0.65 with diagnostics): 41/42, 464 steps, 12 edit failures, 0 parse, 5.79M prompt (93% cached) against `str_replace` 42/42, 424 steps, 0, 4.60M. Paired compare: pass, steps and cost not clearly different, prompt tokens clearly worse (+28k a task, about +26%); no clear difference against the best earlier iterate either. The 12 edit failures were the two designed soft kinds (unseen-line refusals that a straight retry cleared; syntax refusals whose fix needed new content, not a boundary, so the mender rightly left them), and the one failure was a model wrong-fix (js-lru TTL) with clean edits: the port works as designed, it just doesn't pay for a model that already matches text exactly. Decision: `str_replace` stays the default; hashline remains `edit_format = "hashline"` for models that fail at exact matches.
- **Tree-sitter Outline for Go**: Extended tree-sitter queries for Go methods to capture receiver types (`(recv Receiver) Method`), enabling qualified symbol resolution (e.g. `Scope.Define` and `Scope::Define`).
- **Container Egress Proxies and CA Discovery**: `quack-providers` automatically resolves `HTTPS_PROXY`/`HTTP_PROXY` and `SSL_CERT_FILE`/`REQUESTS_CA_BUNDLE` from the process environment when unspecified in config, ensuring seamless operation in isolated container benchmarks (Pier/Harbor).
- **tree-sitter-bash permissions**: shell lines are parsed with tree-sitter-bash, so commands in subshells, loop bodies, backticks, heredocs and literal `bash -c` scripts are checked one by one ([11](11-security.md#tree-sitter-bash-2026-10-21)).
- **Grammar query overrides**: `grammars/<lang>/{tags,highlights,injections,locals}.scm` in any active layer replace or (`; extends`) add to the built-in queries; a `grammars` doctor row ([05](05-context-management.md#as-implemented-2026-10-18-index-and-pruning-rules)).
- **TUI syntax highlighting**: closed code blocks are highlighted with `tree-sitter-highlight`, off-thread for large blocks and for a language's first block ([06](06-tui.md#rendering-performance)). Two new crates (`tree-sitter-bash`, `tree-sitter-highlight`, both MIT); everything they pull in was already in the tree.
- **Markdown tables in TUI** (2026-10-22): GFM tables rendered with column alignment (left/center/right), proportional width budget allocation with word-wrapping within cells, and an ASCII fallback ([06](06-tui.md#rendering-performance)).
- **Mouse text selection in TUI** (2026-10-22): drag in the transcript or the input box to select; letting go copies through OSC 52 and the status bar says how much. Copies leave out the transcript's leads (duck, `›`, tool icons) and the indent the rows share, and keep the input's real line breaks; Ctrl-C with a selection showing copies again instead of interrupting ([06](06-tui.md#windows-terminals)).
- **Replacing API keys in the TUI** (2026-10-22): `/key <provider>`, Enter in `/providers` and Ctrl-S in `/setup` replace a provider's key. `/providers` and `/setup` show where each key is read from. The session uses the new key from its next request, with no restart ([06](06-tui.md#command-catalogue)).
- **Static Musl Packaging & Adapters**: Added `--target` to `scripts/install.sh` for static musl compilation and checked in official Harbor and Pier benchmark adapters (`evals/adapters/quack_adapter.py` and `evals/adapters/install-quack.sh`).
- **Post-edit Tree-sitter syntax validation & fuzzy matching** (2026-10-22): edits in `edit`, `write`, and `apply_patch` validate syntax against Tree-sitter if the file previously parsed cleanly, providing an immediate soft warning with line number to the model. `edit` gained fallback strategies for stripped `read` line numbers and smart quotes/dashes.
- **Shell test & build output distillation** (2026-10-22): `shell` tool output automatically folds long runs of passing tests ($\ge 3$ lines) and compiler package lines ($\ge 4$ lines) in the model-facing output while preserving errors, stack traces, failure diffs, and the raw detail.
- **Cycle and edit-thrashing loop guard** (2026-10-22): `turn.rs` detects non-consecutive duplicate mutating tool calls in the same turn, interrupting `edit -> test -> edit(identical) -> test` loops.
- **Living workspace diff breadcrumbs** (2026-10-22): modified files in the session are summarized at turn boundaries (`[Modified files in this session: ...]`), preventing multi-file amnesia without mutating earlier turns or breaking prefix cache stability.
- **Transactional multi-file refactors** (2026-10-22): multi-file patch application (`apply_patch`, `edit_blocks`) tracks written files and rolls back all changes to their pre-images if any file fails during application.
- **Grammar & guided decoding support** (2026-10-22): `ToolCallMode::Grammar` passed through `ChatRequest.tool_mode`, mapping to `"format": "json"` for Ollama and `"response_format": {"type": "json_object"}` for OpenAI-compatible local engines.

**Exit criteria**
- [ ] Air-gapped VM: build from vendored sources, run against a local vLLM, zero external connections (verified with a network capture)
- [ ] Sandboxed `full-auto` session can't write outside the workspace or reach the network, on all three OSes (or Windows limitation documented with mitigation) — Linux done (`core/tests/sandbox.rs`), macOS written and tested in CI only, Windows unavailable with commands asking first ([11](11-security.md#as-implemented-2026-10-18-ahead-of-m5))
- [x] Policy-locked settings can't be overridden from user/project/env/CLI (2026-10-21): `locks_hold_against_every_higher_layer` (user, trusted project, `QUACK_MODEL`, `-m`, `-c`, whole-table and unset locks, `/config set`), `QUACK_SYSTEM_DIR` ignored when the platform's system layer holds a `[policy]`, and at runtime `core/tests/policy.rs` (`/permissions`, `/yolo`) and `sandbox.rs` (`/sandbox off`). Windows caveat: `%ProgramData%` is itself an environment variable, so a policy that must hold against the user there waits for a registry location ([01](01-configuration.md#precedence))

## M6 — Editor integration & extras

- ACP agent (`quack acp`), tested with Zed
- Optional: code-execution sandbox tool (maki-style), repo map, OpenTelemetry export (opt-in), daemon/socket mode, quack-as-MCP-server

## Cross-cutting, every milestone

- Windows checklist items from each doc
- Fixtures for every new model or server quirk discovered
- Docs updated with each feature (user docs live in `docs/`, separate from `docs/plan/`)
