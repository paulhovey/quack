# AGENTS.md

`quack` is a terminal coding agent for open-weight models, written in Rust (edition 2024, tokio + ratatui). The name was chosen so a global rename stays easy. `docs/plan/` holds the design docs: start with `docs/plan/00-architecture.md`, and see `docs/plan/roadmap.md` for what is done. When you finish a feature, update the README, plus the plan doc and roadmap progress if they describe it.

## Commands

CI (`.github/workflows/ci.yml`) runs with `RUSTFLAGS: -D warnings` and `QUACK_DEFAULTS_DIR: none` on linux-x64, macos-arm64, **windows-x64 and windows-arm64**. Every warning fails CI.

```sh
cargo build                                   # default-members is quack-cli
cargo test --workspace --locked
RUSTFLAGS="-D warnings" cargo clippy --workspace --all-targets --locked   # what CI runs
cargo fmt --all --check                       # max_width = 100; run `cargo fmt --all` first
cargo build -p quack-cli --locked --no-default-features --features proto-openai-chat,lang-common  # minimal feature set, CI checks this
cargo run -q -p quack-cli -- config schema --dir defaults/schemas   # after changing any config struct
```

- The toolchain is pinned to 1.99.1 (`rust-toolchain.toml`). The workspace MSRV is 1.97 (`rust-version`), checked by a separate CI job with `cargo check`, so avoid language and std features newer than that.
- `default-members` is `quack-cli`, so a bare `cargo build` or `cargo run` builds only the CLI and its dependencies. Use `-p <crate>` or `--workspace` for the others.
- `cargo-deny` (`deny.toml`) gates licenses and advisories: only Apache-2.0-compatible licenses pass.

## Running it from a checkout

- The defaults bundle (`defaults/`) is never compiled in. The binary finds it at runtime: `QUACK_DEFAULTS_DIR`, then `<exe>/../share/quack/defaults`, `<exe dir>/defaults`, the system share directories, and the user data dir. `target/debug/quack` finds none of these, so run it as `QUACK_DEFAULTS_DIR=defaults target/debug/quack`, or `QUACK_DEFAULTS_DIR=defaults cargo run`.
- quack does not read `.env` files. If a `.env` holds a provider key (for example `CHARM_API_KEY`, which `defaults/providers.d/charm-hyper.toml` reads with `api_key = { env = ... }`), export it in the shell with `set -a; . ./.env; set +a`. Never print, log or commit its values.
- **Live model calls can cost the user money** (pay-as-you-go keys). Prove behaviour against the wiremock mock server in tests. Make a live call only when only a real model can answer the question, and say so when you do.
- `QUACK_DEFAULTS_DIR=defaults target/debug/quack doctor` shows the config layers, the defaults bundle, providers (ready or why not) and reachable hosts, without calling a model.
- `scripts/install.sh --from-source` builds a release binary and installs it, with the bundle, into `~/.local`. `--plucked` installs the binary only. `scripts/package.sh <target>` builds the release archives that `.github/workflows/release.yml` publishes.
- Other scripts:
  - `scripts/no-egress.sh` runs in a loopback-only network namespace with `strace -e connect` (Linux).
  - `scripts/first-frame.sh <ms>` checks the startup budget.
  - `scripts/local-server-ci.sh` and `scripts/local-server-turn.sh` run a real llama.cpp or Ollama turn on CPU.
  - `scripts/eval-matrix.sh`, `scripts/eval-table.py` and `scripts/eval-compare.py` run and compare `quack eval` across models and profiles (see "Evaluating new models").

## Architecture

There is one engine and three in-process clients, connected by an Op/Event protocol:

- `quack-protocol` holds the serde-typed `Op` (client → engine) and `Event` (engine → clients). It has no async-runtime dependency.
- `quack-core` is the engine: a tokio task that owns a session. Clients hold an `EngineHandle { ops: mpsc::Sender<Op>, events: broadcast::Receiver<Event> }`. All three clients (the TUI in `quack-tui`, headless `quack-exec`, and ACP `quack-acp`) drive the same engine through the same handle and never call subsystems directly.
- Crate layering:
  - `quack-cli` → clients → `quack-core` → subsystems (`quack-providers`, `quack-profiles`, `quack-context`, `quack-tools`, `quack-mcp`, `quack-lsp`, `quack-store`, `quack-instructions`, `quack-config`).
  - `quack-tools` → `quack-shell` / `quack-sandbox`.
  - Everything → `quack-protocol`.
  - **Client crates never depend on each other.**
- Platform-specific code lives in:
  - `quack-shell`: spawn, PTY, kill-tree.
  - `quack-sandbox`: Landlock + seccomp, Seatbelt, restricted token.
  - `quack-config`: paths and the keyring. On Linux the keyring shells out to `secret-tool` instead of compiling in a D-Bus stack.
- Heavy work (tree-sitter, tokenizing, grep, diff) goes on `spawn_blocking`, never the TUI render path.

### Where things happen

- **A turn:** `quack-core/src/turn.rs`. `run` loops over model calls (`stream_step`) and tool calls (`run_call`: permission check, approval, execution).
  - Every event a turn emits goes through `TurnCtx::emit`, which also feeds the session stats (`stats.rs`).
  - Anything that should count toward `/stats` either is an event, or is recorded beside the event.
- **Engine state and Op handling:** `quack-core/src/engine.rs`. Adding an `Op` also means adding it to `op_name` (an exhaustive match). Adding an `Event` needs no client changes, because clients match with a wildcard.
- **Edit formats:** `[tools] edit_format` (or the profile's `[defaults] edit_format`) decides which edit tool exists: `edit`, `edit_hashline`, `apply_patch` (`quack-tools/src/patch.rs`), `edit_blocks` (`blocks.rs`), or none (`whole_file`). Each tool's `available()` checks `ToolCtx.edit_format`. A tool that changes several files returns `Subject::Edits`, and the engine gives each file an undo point; `Subject::edited()` lists them.
- **Outlines:** `quack-tools/src/outline.rs` (tree-sitter, behind `lang-common`) serves the `index` tool, outlined `read`s and pruning rule 4. `cargo test -p <crate>` alone builds without `lang-common`; outline tests are gated on the feature, and engine tests check `outline::supported()` first. The workspace build enables it through `quack-cli`.
- **Sub-agents:** `quack-core/src/agent.rs`. The `task` tool (`quack-tools/src/task.rs`) sends a request over `ToolCtx.spawn`, and `run_call` runs it as a nested `turn::run` with its own history, plan and tool surface (`agents.d`). Its events go out wrapped in `Event::Agent` (except approval requests), and its call ids carry the run id (`TurnCtx::key`).
- **Planner split:** with `[model] planner`, plan mode swaps `Conversation.setup` for the planner's, keeping the session's in `editor`. `accept_plan` in `engine.rs` replaces the planner's messages with one brief (`handoff.rs`) and logs a `handoff` record. Never let two models take turns on one history: each switch re-sends it all uncached.
- **Sealed replies:** `Message::Assistant.sealed` holds Anthropic's signed thinking and OpenAI's encrypted reasoning, replayed in order by the adapters. `Provider::run_stream` drops seals from another provider or setup. Code that edits earlier history must call `quack_providers::unseal` (prune and compaction do). For `anthropic-messages` (`Provider::append_only`), per-request text must be written into the history and left there (`TurnCtx::keep_plan`), never added for one request and removed. `core/tests/anthropic.rs` checks this.
- **Sign-ins:** `[auth]` providers (`quack login`, `quack_providers::login`, feature `auth-chatgpt`) get a bearer token per request. The issuer URL comes from the provider file, so it is data like `base_url`.
- **Session log:** `quack-core/src/session.rs`. The header comment lists the record kinds (`message`, `tool`, `usage`, `prune`, `compaction`, `rewind`, ...). `replay` rebuilds history from them and skips kinds it doesn't know, so new kinds are backwards compatible. Anything that must survive `/resume` has to be written as a record.
- **Context:** `quack-core/src/context.rs` handles fitting, pruning and compaction, `threshold()` and `report()` (the `/context` data). The algorithms are in `quack-context`.
- **TUI:**
  - `quack-tui/src/app.rs`: state, keys, how events update state, slash-command handling in `run_command`.
  - `ui.rs`: drawing.
  - `modal.rs`: every list-with-detail picker (`/models`, `/context`, `/stats`, `/help`, ...).
  - `charts.rs`: charts for a modal's details pane (`Visual`: figure tiles, bars, sparklines, meters, a line chart). A `ModalItem`'s `charts` places each one before a `detail` line; in `modal.rs`, the `Detail` builder does this for you. Charts that don't fit the pane are left out. Each is built with `ascii` already applied; the Trend line chart has no ASCII form, so give ASCII mode a sparkline instead.
  - `commands.rs`: the command catalogue and the help key list.

### Profiles and evals

- A profile is a folder in `defaults/profiles.d/` (`profile.toml` + `system.md`). Prompts must be original text; `source` says what the tool names and shapes follow. Family profiles ship with empty `[match]` lists until `quack eval` shows they beat `generic`.
- `quack eval` (`quack-exec/src/eval.rs`) runs `evals/tasks/*`. **Every task calls the model**, so running it costs money on a paid key; prove runner changes with `quack-exec/tests/eval.rs` (mock server) instead.
- Two suites: `evals/tasks` (quick; every model passes it, so it compares cost and catches regressions) and `evals/hard` (`--suite evals/hard`; 10–30 steps a task, meant to separate setups). Hard tasks keep tests in `hidden/`, copied in only after the run, and most share `evals/hard/_projects/orderly` through `base`, with `repo/` holding only what the task changes. Their `solution/` files are copies of orderly files, so after changing orderly rerun the suite test.
- Every failed run gets a `failure` label (`Failure` in `eval.rs`), worked out from the files it changed compared with those `solution/` changes. Keep `solution/` to the files a fix needs, or the labels go wrong.
- A new task needs `task.toml`, `repo/` (or `base`) and `solution/` (files, `ANSWER`, `DELETE`). `cargo test -p quack-exec --test suite` checks that it fails as given and passes when solved, in both suites. Commands use `{python}` and stay portable (no shell quoting tricks: put logic in a `check.py`).
- Check behaviour, not exact text. A `contains` check over several lines fails a correct fix that adds a comment (this happened to `big-file-edit`); import the module in a `check.py` instead.

### Evaluating new models (live calls: costs money, so say so first)

1. **Find the ids and prices.** A provider's `/models` is free (no inference), and discovery reads its `pricing`, so eval results carry the cost. For Charm Hyper: `curl -s -H "Authorization: Bearer $CHARM_API_KEY" https://hyper.charm.land/v1/models` (never echo the key). Estimate before running: one suite pass is about 21 × (3k fresh + 12k cached prompt + 1.5k output) tokens.
2. **Smoke-test one cheap task** to prove the key, the model id and the cost column: `QUACK_DEFAULTS_DIR=defaults target/debug/quack eval -m charm-hyper/<model> --profile generic --tasks fix-typo --out /tmp/smoke.json`.
3. **Run the matrix**: each model with `generic` and its family profile (Qwen → `qwen-code`; GLM, DeepSeek, MiniMax, MiMo → `cc-style`; Kimi → `kimi-code`; gpt-oss → `codex`):
   `scripts/eval-matrix.sh charm-hyper/glm-5.3:generic,cc-style charm-hyper/kimi-k3:generic,kimi-code ...`
   Models run in parallel; `EVAL_ARGS="--suite evals/hard"` (the suite that separates setups; several times the cost of `tasks`), `"--repeat 3"`, `--tasks a,b` or `--planner charm-hyper/kimi-k3` (plan with that model, carry out with the `-m` one; compare it against both models alone) pass through. It ends with `scripts/eval-table.py evals/results/<date>`, which prints one row per model and profile.
4. **Read the signals.** The pass rate saturates on `evals/tasks`; on `evals/hard` it should not:
   - **failure labels** (`eval-table.py` lists them per setup): `bad-calls` and `edit-failed` point at the profile or edit tool, `untested` at the prompt's advice to run tests, `wrong-files`/`missed-files` at search and navigation, `wrong-fix` at the model.
   - **clean passes and process labels**: `unverified` (no command after the last edit), `blind-retry`, `regression`. When pass rates tie, fewer clean passes means the setup is getting lucky; `eval-compare.py` pairs `clean pass` like the pass rate.
   - **parse failures**: the profile's tool names or argument shapes don't suit the model. Any non-zero count is a profile bug.
   - **edit failures**: the edit tool's description or format confuses the model.
   - **steps and prompt tokens**: each step re-sends the whole context, so they decide the cost.
   - **small windows**: a model's full window hides pruning and compaction. Add `-c context.window=16384` (results get a `-16k` name and show as `hard@16k`) and read `peak k` and `compact / pruned`; see `evals/hard/README.md`.
   - **cached %**: a low share means the request prefix is changing somewhere it shouldn't.
   - **tool errors** count shell commands that exit non-zero (a `grep` with no match, `pytest` finding no tests), so they run high for shell-first profiles like `codex`. Look at a session before reading much into them.
5. **Look at a run before blaming the model.** Rerun the task with `--keep`. The session log is in `~/.cache/quack/eval/<pid>/<task>-<run>/data/sessions/*/*/events.jsonl`. Fix false negatives in the check first.
6. **Turning a profile on**: a family profile gets `[match]` globs only when it beats `generic` at `--repeat 3`, with no parse failures and fewer edit failures or steps at the same pass rate. One run per task is noise (±5–10% on steps). Decide with `scripts/eval-compare.py generic.json,generic-2.json family.json,...`: it pairs the setups task by task, and only a difference whose 95% interval leaves out zero counts as "clear".
7. **Record the result**: a dated line in `docs/plan/roadmap.md` (models, profiles, total cost, the decision). The baseline is 2026-10-19: 8 models × 2 profiles on Charm Hyper, all 21/21, $2.42 in total, and no family profile clearly ahead of `generic`.

### Calibrating and tuning (live calls: costs money, so say so first and give the estimate)

`evals/hard` has not been run live yet, so its difficulty is a guess. Calibrate the suite once before trusting any comparison on it, and again after adding or changing tasks.

1. **Calibrate the suite.** Run one cheap and one strong model with `generic`: `EVAL_ARGS="--suite evals/hard" scripts/eval-matrix.sh charm-hyper/deepseek-v4.1-flash:generic charm-hyper/kimi-k3:generic`. One pass (then 14 tasks) cost $0.35–0.55 on the flash models on 2026-10-21; a strong model costs several times that. Check the cost column after the first few tasks. For each task, look at the result:
   - **Both models pass:** the task is too easy and tells you nothing. Make it harder (move the symptom further from the cause, or add a second file that must change), or replace it.
   - **Both fail:** read one session (`--keep`, step 5 above) before blaming the models. A false negative in a hidden test or check is the usual cause. If the task really is too hard, cut it down.
   - **They split:** keep it. Aim for the strong model at about 60–80% and the cheap one clearly lower.

   Then rerun `cargo test -p quack-exec --test suite`, update the task table and the calibration notes in `evals/hard/README.md`, and add a dated line to the roadmap.
2. **Tuning for a new model.** Change one setting at a time, in this order, because each is cheaper to change and more likely to matter than the next. For every comparison, write the decision rule down first (for example: "switch to hashline if the pass rate isn't clearly worse and edit failures are clearly lower"). Then run both sides with `--suite evals/hard --repeat 3` and decide with `scripts/eval-compare.py`. With no clear difference, keep the simpler or current setting.
   1. **Profile:** `generic` against the family profile (step 3 above). Read the failure labels first. `bad-calls` or parse failures mean the tool shapes are wrong for the model, and you fix that in the profile before measuring anything else.
   2. **Edit format:** try this when `edit-failed` or edit failures stand out. Use `-c tools.edit_format=<str_replace|hashline|apply_patch|search_replace_blocks|whole_file>`. Once the comparison says so, the profile's `[defaults] edit_format` changes to match.
   3. **Prompt advice:** change `system.md`/`context.md` when `untested` stands out (the model doesn't run the tests) or `wrong-files`/`missed-files` stands out (it doesn't search). Keep `prefix.rs` passing.
   4. **Reasoning effort and sampling:** set `reasoning.effort` or `sampling` on the model's entry in its provider file, in a user-layer copy, not in `defaults/`, until the comparison decides.
   5. **Planner:** use `--planner <strong model>` with `-m <cheap model>`, and compare it against both models alone. It pays off only if it gets close to the strong model's pass rate for clearly less cost.
3. **Keep variants apart.** Results are named `hard-<model>-<profile>.json`, so a `-c` or provider-file variant would overwrite the baseline. Run variants with `quack eval` directly and pass `--out evals/results/<date>/hard-<model>-<profile>-<variant>.json`. Pass repeats as `--repeat 3` instead of rerunning, so one file holds all the runs.
4. **Budget.** A `--repeat 3` comparison costs about 6 hard-suite passes, roughly $6–12 for one model on today's Charm prices. Measure step 2.1 for every new model. Do steps 2.2–2.5 only when the failure labels point at them, and test changes on `--tasks a,b` (the tasks that showed the problem) before running the whole suite.
5. **Apply the result.** Turn the winning profile on with `[match]` globs (step 6 above), set its `[defaults]`, and record what you tried, the cost and the decision in the roadmap, including comparisons that showed no clear difference, so nobody pays to run them again.

### Adding a slash command

1. Add an entry to `COMMANDS` in `quack-tui/src/commands.rs`. `pending: Some("M2")` marks a command as not built yet; set it to `None` when it works.
2. Add an arm in `App::run_command`. For data that comes from the engine, open the modal with `None` (a placeholder), push an `Op`, and replace the modal when the answering `Event` arrives, only if a modal with that title is still open (see `/context` and `/stats`).
3. If it has a key binding, add the key to `KEYS` in `commands.rs` so `/help` lists it.
4. Update the help modal snapshots (see Testing).

## Hard invariants

These are the project's core promises; tests and CI enforce them:

- **No built-ins**: no provider ids, base URLs or MCP commands are compiled into the binary. Everything is TOML under `defaults/`, loaded as a layer beneath system, user and project config. Startup must survive the deletion of any single default file (`removing_any_default_file_leaves_startup_clean` in `quack-core/tests/invariants.rs`).
- **No implicit egress**: the binary contacts only what is configured. This is enforced by an in-process recording proxy test (`no_egress_beyond_the_configured_provider`) and by `scripts/no-egress.sh`. Never add a network call outside the configured providers and MCP servers.
- **First frame under 50 ms**: the Linux CI budget, measured with the hidden `--trace-startup` flag.
- **Prefix stability**: the system prompt and tool block stay byte-identical across turns except at epoch boundaries, so server KV caches stay warm. `crates/quack-core/tests/prefix.rs` checks it for every shipped profile: each request must extend the one before. Don't put anything that changes per turn (time, counters, the plan) into the system prompt, and nothing that changes per session or project either (workspace, date, AGENTS.md, memory): that belongs in the profile's `context.md`, which goes at the start of the first user message, so the system prompt and tools are cached across sessions too. Per-request extras ride on the last message, the way the plan reminder does in `stream_step`. Resume fidelity (the request after a resume equals the live one byte for byte) is tested in `invariants.rs`.

## Conventions and gotchas

- **User-facing TOML stays minimal.** Provider and model files should work with two lines (`base_url`, `models`). Every new field must be optional with a sensible default, and an unset field should mean the obvious thing (for example, an unset `cost.cached_input` means the `input` price). After changing a config struct, regenerate `defaults/schemas/`.
- **Defaults are data.** Ship more defaults as files under `defaults/`, never as code. The packaged `quack` archives include the bundle; `quack-plucked` archives are the binary only.
- **Dependencies must earn their place.** Before adding a crate, count what it pulls in on each OS (`cargo tree -e normal --target <triple>`). Use `default-features = false` where possible.
- **Platform-gated tests must gate their imports too.** CI compiles `--all-targets` with `-D warnings` on Windows, so a `#[cfg(unix)]` test leaves its imports and helpers as unused-import or dead-code errors there. Gate them with the same `#[cfg(...)]`, as `crates/quack-core/tests/trust.rs` does.
- **Lints:**
  - rustc lints: `unsafe_code = deny`, plus `missing_debug_implementations` and `unused_qualifications` as warnings.
  - clippy: `all`, `unwrap_used`, `print_stdout`, `print_stderr`, `dbg_macro` and `todo` as warnings.
  - All of these fail CI. Use `expect` with a reason, or proper errors. Never print from library or TUI code. Tracing goes to a file (`quack.log` in the log dir), never the terminal.
- **Errors:**
  - `thiserror` in `quack-config` and `quack-providers`.
  - `anyhow` in `quack-cli`.
  - `miette` for config-facing diagnostics.
  - Engine-internal paths often use `Result<_, String>` for messages that go straight to the user.
  - Write error messages as sentences that say what to do next (`· check the provider's api_key`).
- **Paths:**
  - Use `Path`/`PathBuf`; never join strings with `/`.
  - On Windows, `canonicalize` adds a `\\?\` prefix, so don't show canonical paths to users or compare them with configured ones.
  - Path comparisons are case-insensitive on Windows and macOS (`quack-tools/src/paths.rs`).
  - The edit tools keep each file's CRLF/LF and BOM (`quack-tools/src/text.rs`).
- **Windows key events:** crossterm reports both Press and Release on Windows. The TUI acts on Press and Repeat only (`App::on_terminal`); keep it that way for new input handling.
- **Pinned crate:** `rmcp` is pinned (`=3.5.1`) because MCP 2026-07-28 support is new. Don't bump it casually.
- **Features:** cargo features gate the protocols (`proto-openai-chat`, `proto-openai-responses`, `proto-anthropic`, `proto-ollama`), grammars (`lang-common`), HF tokenizers, ACP and the keyring (see `quack-cli` `[features]`). A provider that names a compiled-out protocol must fail to load on its own, with a clear error, while everything else loads.
- **Project config:** it lives in `.quack/` under the workspace root (for example `.quack/mcp.d/`). An untrusted project config runs nothing until the user trusts it (see the trust tests in `quack-core`).
- **TUI drawing** takes `&App`. The only state that drawing writes is in `Cell`s (`scroll`, `view`), which it clamps and remembers for the next key press. Keep everything else in `draw` free of side effects.

## Testing

- **Engine integration tests** live in `crates/quack-core/tests/`, one file per area (`agent_loop.rs`, `compaction.rs`, `edit_formats.rs`, `invariants.rs`, `prefix.rs`, `profiles.rs`, `pruning.rs`, `rewind.rs`, `stats.rs`, `trust.rs`, ...). `prefix.rs` and `profiles.rs` use the real defaults bundle's profiles; a scripted `Respond` (`Script` in `prefix.rs`) answers a sequence of steps in order. The mock LLM is a wiremock `MockServer` that the test starts. Files that use `mod common;` (`tests/common/mod.rs`, `#![allow(dead_code)]`) get:
  - `workspace(root, base_url, models, cwd)`: isolated config, data and cache dirs with one `mock` provider and no defaults bundle. `models` is a TOML array such as `["tiny"]` or `[{ id = "tiny", cost = { input = 1.0, output = 2.0 } }]`.
  - `sse_text()`, `sse_call()`, `sse_raw()`: SSE reply builders. Put a `usage` chunk in `sse_raw` to test token accounting.
  - `turn()`: submit a user turn and wait for it.
  - `until_done()`: events until the turn ends; panics on an error or abort.
  - `bodies()`: the captured request bodies.
- **Ordering mocks:** wiremock answers with the first matching mock. Mount the later-step mock with a specific matcher (for example `body_string_contains("\"role\":\"tool\"")`), and the first-step mock with `.up_to_n_times(1)`.
- **Resume:** spawn a second engine with `Engine::spawn_with(config, Resume::Latest)` on the same temp dir, and wait for `Event::SessionResumed`.
- **TUI tests:**
  - They render frames into ratatui's `TestBackend` at explicit sizes (80x24, 100x30) with the `render()` helper in `ui.rs` tests, and compare them with insta snapshots in `src/snapshots/`.
  - Update snapshots with `INSTA_UPDATE=always cargo test -p quack-tui`, then read the diff (`git diff crates/quack-tui/src/snapshots`) before you accept it.
  - Snapshot lines that end in `Hidden by multi-width symbols` come from the duck emoji and are expected.
  - For behaviour, assert on the rendered string (`s.contains(...)`) and drive keys through `app.on_terminal(Event::Key(...))`.
- `quack-lsp` and `quack-mcp` have their own tests against fixture servers. The top-level `tests/` folder holds only a README so far: recorded provider SSE fixtures are planned (`docs/plan/13-testing-and-evals.md`), not yet in the repo.
- **Tests must not read the developer's real config or keyring.** CI sets `QUACK_DEFAULTS_DIR=none`; do the same locally (`QUACK_DEFAULTS_DIR=none cargo test --workspace`). Build config through `common::workspace()` or `load_with` with an explicit variable map, never from the process environment.
