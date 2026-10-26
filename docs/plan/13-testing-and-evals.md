# 13 — Testing & Evals

## Test layers

| Layer | What | Tools |
|---|---|---|
| Unit | Parsers (tool-call formats, think tags, SSE), config merge, permission rules, path normalization, fuzzy edit matching, prune rules | `cargo test`, `proptest` for parsers and edit matching |
| Snapshot | Rendered prompts per profile, tool schemas after mapping, TUI frames, `/context` output, config diagnostics | `insta` |
| Fixture replay | Recorded raw SSE streams from real servers/models through adapters + normalizers | `tests/fixtures/providers/<server>/<model>/*.sse` |
| Integration | Full engine turns against a **mock provider** (scripted responses incl. tool calls), real tools on a temp workspace | `wiremock` or an in-process axum mock |
| MCP/LSP | Real tiny servers: an rmcp test server in-repo; LSP: a minimal fake server + optional real rust-analyzer job | |
| E2E TUI | Spawn binary in a PTY, drive keystrokes, assert on screen | `portable-pty` + `vt100` parser |
| Platform | Everything above on Linux, macOS, Windows | GitHub Actions matrix |

## Invariant tests (must never regress)

- **Prefix stability**: for each default profile, render a session of N simulated turns. The system prompt and tool block must be byte-identical across turns, and must change only at epoch boundaries ([05](05-context-management.md)).
- **No egress**: run startup + 3 turns with a mock provider in an environment where every outbound connection except the mock fails and is recorded. Assert zero attempts. Run once with `offline = true` and once with defaults. Implemented twice: in-process, with all provider traffic sent through a recording proxy that refuses anything but the mock; and on Linux CI with `scripts/no-egress.sh`, which runs the tests in a loopback-only network namespace under `strace -e connect`, so connections from any code (DNS included) are caught, not just our HTTP client.
- **No built-ins**: assert that the release binary contains no provider ids, base URLs or MCP commands. Scan the binary strings for the hostnames in `defaults/` as a smoke check.
- **Resume fidelity**: for recorded sessions with prunes and compactions, the replayed context equals the context logged at the time.
- **Removal**: delete each default file one at a time. Startup must succeed, with only that item missing.
- **Windows paths**: tests of path normalization, CRLF preservation and case-insensitive rules run on Windows CI, not just in simulation.

## Performance budgets (CI-enforced)

| Metric | Budget | How |
|---|---|---|
| Time to first frame | < 50 ms (Linux CI), < 120 ms (Windows CI) | `--trace-startup <file>` (hidden flag: draws one frame, writes JSON, exits), median of 10 runs in a PTY: `scripts/first-frame.sh`. Measured from the top of `main`, so process creation is not included |
| Idle CPU | ~0% | sample after startup with no input |
| Streaming render | ≥ 500 tok/s without dropped frames | mock provider at fixed rate, measure frame times |
| Memory, idle | < 60 MB RSS | |
| Release binary size (default features) | tracked, alert on +10% | |

## Model compatibility matrix & evals

This is where open-model harnesses usually fall down. We treat it as a product feature.

### `quack eval`

- **Task suite** in `evals/tasks/`: small, self-contained repos with a prompt and a verification command (tests pass, file contains X). The suite grows from about 20 tasks (M2) to about 100. Categories: read/navigate, single edit, multi-file edit, fix failing test, use shell, use LSP, long-context.
- Run `quack eval --model <p/m> [--profile <id>] [--tasks …] [--repeat 3]`.
- Optionally, adapters for external suites: Terminal-Bench, SWE-bench Lite/Verified (Docker required, kept out of the default CI).

### Metrics per (model, profile, server)

Pass rate, steps per task, tokens per task (prompt / cached / completion), wall time, tool-call parse failures, edit-match failures (exact vs fuzzy vs failed), loop-guard trips, compactions.

### Outputs

- `evals/results/<date>/<server>-<model>-<profile>.json`
- `docs/compat.md`: a generated table of model × profile × server with pass rate and notes. Its results set default profile `[match]` priorities ([03](03-model-profiles.md#evaluating-profiles)) and default `tool_calling` / `edit_format` values in the profiles.

### Publishing results

The compatibility table is a headline feature, not an internal report: it answers the first question every open-model user has ("will my model work, and with which settings?").
- `docs/compat.md` and a page on the project site list model × quantization × server × profile with pass rate, steps per task and tokens per task, plus the date and harness version.
- `quack eval --submit-file` writes a self-contained result file (no secrets, no prompts from private repos) that users can attach to a PR. Community results are marked as such and kept apart from CI results.
- The default provider and profile files link to the matching rows.
- Results are produced locally; nothing is uploaded by the harness.

### Recording fixtures

`quack debug record --model … --prompt …` saves the raw request/response SSE to the fixture directory, with secrets stripped. Every bug report about a parse failure should end up as a fixture.

## As implemented (2026-10-18)

- **Prefix stability**: `core/tests/prefix.rs`, for every shipped profile and with hashline edits (see [05](05-context-management.md)). Each request must extend the previous one: same system prompt, tools and parameters, earlier messages unchanged, and the newest one changed only by the plan reminder that rides on it. After `/compact` the history is new, but the system prompt and tools are not. For `anthropic-messages`, whose signed thinking needs an unchanged history, `core/tests/anthropic.rs` checks the stricter form: each request's messages start with all of the previous request's, the plan reminder included, across a resume.
- **`quack eval`** (`quack-exec/src/eval.rs`): `quack eval [-m p/m] [--profile id] [--tasks a,b] [--repeat n] [--suite dir] [--out file] [--keep]`. Each run copies the task's `repo/` to a scratch folder under the cache directory, uses a data directory and memory folder of its own, runs in `full-auto` (anything that still asks is denied), and stops at the task's `timeout_secs`. Results go to `evals/results/<date>/<model>-<profile>.json` (a rerun adds `-2`, `-3`, ... rather than overwriting) with the sampling settings sent (from `StatsReport.sampling`; empty means the server's defaults), the tool results pruned and the largest single request's prompt (`pruned`, `peak_prompt`: how full the context got; `scripts/eval-table.py` shows both), and a Markdown table to stdout: pass, steps (model requests), tool calls and errors, parse failures (arguments that are not JSON, unknown tool names), failed edits, prompt/cached/completion tokens, compactions, cost (when the model is priced, by its provider file or its models endpoint) with the total on the summary line, time.
- **Tasks**: 21 in `evals/tasks/` (navigate, single-edit, multi-file, fix-test, shell, create, long-context). Checks are file contents, files created or removed, the final answer, a tool having been called, and commands (`{python}` stands for python3 or python; a task whose `requires` are missing is skipped). Python is the only language the commands use, so the suite runs on every OS. Each task has a `solution/`; `quack-exec/tests/suite.rs` checks that every task fails as given and passes with its solution, and `quack-exec/tests/eval.rs` runs the runner against a mock server.
- **Hard suite** (2026-10-19): the first baseline passed 21/21 for every model and profile, with a median of 2–6 steps per task, so it could only compare cost. `evals/hard/` has 14 tasks meant to take 10–30 steps and to be failed by some models. Nine share one realistic project (`_projects/orderly`, a shop backend of about 1,200 lines with its own tests and conventions) through the task's `base` field, so `repo/` holds only what a task changes: a symptom reported far from its cause (`category-cents`, `delivery-dates` with two causes in two files, `cancel-restock`, `csv-export`), features spanning 2–4 files (`exclusive-coupons`, `tax-included`, `partial-shipment`), a rename whose old name also lives in config strings while a file format must not change (`rename-total`), and a question only running the code answers reliably (`quote-total`). Five stand alone: a test that fails depending on `PYTHONHASHSEED` (`flaky-order`), an API change with callers behind `functools.partial`, a string dispatch table and an import alias (`migrate-fetch`), five bugs behind one test file (`log-parser`), operator precedence in a Rust Pratt parser (`rust-calc`, needs cargo), and a cache built from a spec in JavaScript (`js-lru`, needs node). Each task's `hidden/` (tests the model never sees) is copied in only after the run, so passing the visible tests is not enough. The difficulty is a guess until a calibration run: tasks every model passes or every model fails get replaced.
- **Failure labels**: every failed run gets one `failure` (`Failure` in `eval.rs`), read from what the runner saw and from `changed`, the files the model changed compared with an untouched copy of the start: `timeout`, `error`, `bad-calls` (half or more of its calls didn't parse), `edit-failed` (edits failed and the solution's files never all changed), `no-change`, `wrong-files` (none of the solution's files), `missed-files` (some), `wrong-answer`, `untested` (the right files, never ran a command), `wrong-fix`. Each points at something different to change: the profile's tool shapes, the edit tool, the prompt's "run the tests" advice, or the model.
- **Process labels** (2026-10-25): every run, passed or not, gets `process`, the habits in its tool calls that make the result less trustworthy (`Process` in `eval.rs`): `unverified` (no command after its last successful edit), `blind-retry` (a command that had just failed was run again unchanged, with no edit in between), `regression` (a command that had succeeded failed later). Shell calls compare by their exact command; edits by tool (`edit`, `apply_patch`, `write`), since their summaries name only the file. A pass with none of them is a clean pass: `eval-table.py` shows `clean` beside `pass` and the label counts, and `eval-compare.py` adds a paired `clean pass` metric, both only for results that carry the field (older files show `?`). On a saturated suite, where every setup passes, clean passes can still tell setups apart. `process` is always written, so `[]` means graded and clean.
- **Comparing setups**: `scripts/eval-compare.py A.json[,...] B.json[,...]` pairs two setups task by task (each task counts once however many runs it had) and gives each difference (pass rate, steps, cost, prompt tokens) with a 95% paired-bootstrap interval, marked "clear" only when the interval excludes zero. `scripts/eval-table.py` gained the suite and the failure labels.
- Not yet: `--submit-file`, `docs/compat.md` (needs real runs), LSP tasks, external suites.

## CI

- PR: fmt, clippy (`-D warnings`), unit + snapshot + integration on Linux x64, macOS ARM64, Windows x64 and **Windows ARM64** (GitHub-hosted `windows-11-arm` runner), plus `cargo-deny`, invariant tests and the startup budget.
- **Local-server protocol coverage without a GPU**: a Linux CI job runs `llama-server` and Ollama in Docker on **CPU** with a tiny current model (Qwen3.5-0.8B: GGUF Q4_K_M for llama.cpp, `qwen3.5:0.8b` for Ollama). `scripts/local-server-ci.sh <llama-cpp|ollama>` starts the pinned server image, and `scripts/local-server-turn.sh` asks the model to read a file and reply with the number in it; the job passes when `read` was called and the answer has the number (3 attempts, since tiny models are erratic). It runs locally with `DOCKER=podman`. These models are too small to measure quality, but large enough to exercise streaming, tool-call templates, `/tokenize`, `/props`, `num_ctx`, and prefix-cache reuse (`cached_tokens`/timings). This covers the local adapters even though day-to-day manual testing uses hosted services.
- Nightly: E2E PTY tests, fuzzing of parsers (`cargo-fuzz`, 10 min), an eval run against a hosted open-model service (key from CI secrets, small task subset, budget-capped), and a GPU runner later if one becomes available.
- Release: see [14](14-packaging-and-distribution.md).
