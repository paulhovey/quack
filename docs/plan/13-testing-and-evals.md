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

## CI

- PR: fmt, clippy (`-D warnings`), unit + snapshot + integration on Linux x64, macOS ARM64, Windows x64 and **Windows ARM64** (GitHub-hosted `windows-11-arm` runner), plus `cargo-deny`, invariant tests and the startup budget.
- **Local-server protocol coverage without a GPU**: a Linux CI job runs `llama-server` and Ollama in Docker on **CPU** with a tiny current model (Qwen3.5-0.8B: GGUF Q4_K_M for llama.cpp, `qwen3.5:0.8b` for Ollama). `scripts/local-server-ci.sh <llama-cpp|ollama>` starts the pinned server image, and `scripts/local-server-turn.sh` asks the model to read a file and reply with the number in it; the job passes when `read` was called and the answer has the number (3 attempts, since tiny models are erratic). It runs locally with `DOCKER=podman`. These models are too small to measure quality, but large enough to exercise streaming, tool-call templates, `/tokenize`, `/props`, `num_ctx`, and prefix-cache reuse (`cached_tokens`/timings). This covers the local adapters even though day-to-day manual testing uses hosted services.
- Nightly: E2E PTY tests, fuzzing of parsers (`cargo-fuzz`, 10 min), an eval run against a hosted open-model service (key from CI secrets, small task subset, budget-capped), and a GPU runner later if one becomes available.
- Release: see [14](14-packaging-and-distribution.md).
