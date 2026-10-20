# 04 — Agent Loop & Tools

## Turn lifecycle

```
Op::UserTurn
  │
  ├─ 1. expand input: @file mentions, /command templates, pasted images
  ├─ 2. build request: profile.render(history, tools)  → ChatRequest       (03)
  ├─ 3. budget check: context.plan(request)  → maybe prune/compact first    (05)
  ├─ 4. adapter.stream(request)                                            (02)
  │      └─ emit AssistantDelta / ReasoningDelta / ToolCallBegin as they arrive
  ├─ 5. on finish:
  │      ├─ no tool calls          → TurnComplete
  │      └─ tool calls             → 6
  ├─ 6. for each call: profile maps name/params → internal call
  │      ├─ permission engine: allow / deny / ask (ApprovalRequest)         (11)
  │      ├─ pre-tool hooks                                                  (11)
  │      ├─ execute (parallel for read-only, sequential for mutating)
  │      ├─ post-tool hooks, LSP diagnostics collection                     (10)
  │      └─ format result via profile, append to history
  ├─ 7. persist events to session log                                      (07)
  └─ 8. loop to step 2 until: no tool calls | max_steps | interrupt | fatal error
```

### Plan mode

`/plan` (or Shift-Tab to `plan`) runs turns with the `read-only` permission set plus `todo` and `ask_user`, and a short system addendum asking for a plan instead of changes. The plan is shown as a reviewable message; **Accept** switches back to the previous mode and sends "carry out the plan" as the next turn, **Revise** keeps planning. The plan is stored in the session so `/resume` restores it. The addendum is appended at the end of the prompt, not the start, to keep the cached prefix ([05](05-context-management.md#stable-prefix-layout)).

As implemented (M2, 2026-10-18; questions and the review bar 2026-10-19):
- Plan mode is an overlay on the session's permissions. Edits are refused. Shell commands run only if they read (`ls`, `cat`, `rg`, `find` without `-exec`/`-delete`, `git status|log|diff|show|…`; every command in a pipeline or list must read, and nothing may be redirected to a file) or match an allow rule. Reads follow the normal rules.
- The request for a plan (`plan.md` in the profile, or a compiled-in default) is appended to each user message sent in plan mode. It is stored in the history, so a resume sends exactly the same request.
- `/plan` enters plan mode and `/plan <task>` enters it and sends the task. `/plan accept` returns to the previous mode and sends "Carry out the plan above." `/plan off` leaves without carrying it out. Shift-Tab cycles ask → auto-edit → plan.
- The default request asks the model to explore first, then to ask the user with `ask_user` about the few decisions that would change the plan in a big way (scope, approach, a trade-off, an unclear requirement), batched in one call with options and its recommendation, and never about what the code or docs answer. It then records the plan as a `todo` checklist (all pending) and replies with a short summary: decisions, files, how to check. The accepted checklist is the one that ticks off during execution.
- After each turn in plan mode the TUI shows a review bar above the input: the checklist and three choices (carry it out in the mode plan mode started from; carry it out in `auto-edit`, offered when that mode was `ask` or `read-only`; keep planning). Enter or `1`–`3` picks, Esc keeps planning, and typing writes a revision that Enter sends as the next plan-mode message. `/plan accept` and `/plan off` still work. The status bar shows `plan`.
- A mode set during startup is applied once a model is ready. A session switch resets the mode to the configured one.

### Planner / editor split

`[model] planner = "<provider/model>"` (optional) lets a larger model write the plan and the default model carry it out, or the reverse (a hosted model edits from a plan written by an on-prem one). The planner gets only read-only tools. This builds on the existing `small` role; nothing changes when `planner` is unset.

As implemented (2026-10-19): each model has its own server-side prompt cache, so the two must not take turns on one conversation, since every switch would re-send everything uncached. There is one handoff instead:
- Entering plan mode builds the planner's setup (its own profile, prompt and tools) and keeps the session's own setup aside. Turns wait for it. The planner reads the whole conversation once, uncached, and then caches its own steps. The todo list is shared, so the checklist it writes carries over. `/models` and `/profiles` switches keep the todo list too.
- `Op::AcceptPlan { mode }` (the review bar and `/plan accept`) leaves plan mode and puts the session's model back. The messages added since the planner's first turn are replaced by one brief (`quack-core/src/handoff.rs`): the user's requests (without the plan-mode request), their `ask_user` answers, and the planner's last reply. The brief is sent as the next turn. The session model's next request extends its own earlier request with the brief, so its cache holds, and it never sees the planner's tool names or arguments. `/plan off` and Shift-Tab go back without a brief, and the planner's messages stay in the history.
- The session log gets a `handoff` record (`to`: the history length to cut back to). The brief follows as an ordinary `message`, so a resume rebuilds the same history. The transcript keeps the planning. `model_switch` records carry a `role`.
- Starting a new or saved session goes back to the session's model. `/reload` keeps it and plans with the planner again if plan mode is still on. If the planner cannot load, the session's own model plans, with a warning.
- `quack eval --planner <provider/model>` runs each task in plan mode on the planner and then accepts the plan, so the split can be compared with either model alone (the result file and `scripts/eval-table.py` carry a `planner` column). Tested with the mock server in `core/tests/planner.rs` and `exec/tests/eval.rs`; not yet measured live.

### Sub-agents

As implemented (2026-10-19, ahead of M3): the `task` tool starts an agent from `agents.d/<id>.toml` (`description`, `prompt`; optional `tools`, `read_only`, `model`, `max_steps`; schema `agent.json`). The bundle ships `explore` (read-only search: `read`, `list_dir`, `glob`, `grep`, `index`, `lsp`, `shell`; `model = "small"`) and `general` (every tool). With no agents the tool is not offered; a project's `agents.d` loads only when the project is trusted.
- **Running one.** `task` (in `quack-tools`) sends a `TaskRequest` over `ToolCtx.spawn`, the way `ask_user` sends questions, and `run_call` runs it with `agent::run` (`quack-core/src/agent.rs`): an ordinary `turn::run` on a fresh history, with the parent's turn id, its approvals table and its cancellation. Only the final reply goes back, as `Report from the <id> agent: …`; a run that stops early (interrupted, step limit, provider error) returns an error with the last thing the agent said.
- **Its setup.** Built on first use from the model's setup and kept for the session per (agent, parent model, parent profile), so each agent's prefix stays cached from run to run: the profile's system prompt plus the agent's `prompt`, the parent's tool surface (same names and shapes) cut to `tools`, and never `task` or `ask_user`. Edit tools are dropped for read-only agents, and MCP tools come only with `tools = []` or `"mcp"`. `model = "small"` uses `[model] small`; a model that cannot load falls back to the turn's model with a warning. Each agent has its own `Plan`, so its `todo` calls never touch the parent's checklist.
- **Permissions.** Read-only agents run on a copy of the session's permissions with plan mode's rules (edits refused, only reading commands). Others share the session's permissions, so "allow for this session" carries over both ways, and their edits get undo points under the parent's turn. In plan mode `Subject::Agent { read_only: false }` is denied, so only explorers run there; the plan request (`plan_note` in `engine.rs`) points the planner at them when any are installed.
- **Concurrency.** Consecutive `task` calls for read-only agents in one reply run together (`join_all`); results go back in call order. Others run one at a time. A per-turn lock shows one approval or question at a time.
- **Events.** A run's events reach clients wrapped in `Event::Agent { id, inner }` between `AgentStarted` (with the `task` call id) and `AgentFinished`. Approval requests are not wrapped, so every client answers them unchanged; sub-agent call ids are prefixed with the run id (`general-1/call_0`) so they never collide with the parent's. Their usage counts in `/stats`. `UserPrompt` and `TurnEnd` hooks do not run for sub-agents; tool hooks do.
- **The plan.** `task` takes `todo` (an item's number or text): the item goes `in_progress` when the agent starts and `done` when it finishes, or back to `pending` if it fails. `Event::Plan` reports every change to the checklist (from `todo` or `task`), which also fixes the TUI's plan strip for profiles that rename `todo`.
- **Session log.** Each run writes `agents/<run>.jsonl` in the session folder, and the parent's log gets an `agent` record (replay skips it) and an `agent_usage` record per request the agent makes, so `/stats` after a resume counts the agents' tokens and cost as the live session did. Resume needs nothing more: the parent's history holds the report.
- **TUI.** Under a running `task` line, the agent's last steps; when done, the start of its report. `/tasks` lists every run of the session (newest first) with its model, state, each call and its text. Runs are kept in the client, so `/tasks` starts empty after `/resume`.
- Tested with the mock server in `core/tests/agents.rs` (report, plan ticking, plan-mode refusal, parallel runs, approvals, prefix stability of the agent's own requests). Not yet run against a live model or measured with `quack eval`.

### Loop guards

- `max_steps_per_turn` (default 100), configurable. When reached, the loop pauses and asks the user whether to continue.
- **Repetition detection**: the same tool call with identical arguments three times in a row, or the same failing edit twice, injects a short system note ("this call has failed N times; try a different approach"). A further repeat pauses the loop. Small models get stuck in loops often, so this matters.
- **Empty response handling**: a response with no text and no tool calls is retried once with a nudge, then surfaced to the user.
- **Malformed tool call**: the parse or validation error goes back to the model as a tool result. After three consecutive parse failures, the turn switches to `text` tool-calling mode, if the profile allows it, and records that decision for the session.

### Concurrency

- A tool declares `kind: ReadOnly | Mutating | Interactive`. Consecutive read-only calls in one response run in parallel (bounded by `max_parallel_tools`, default 8). Mutating calls run in order. Results are appended in the order the model issued the calls.
- Interrupt (Esc/Ctrl-C in the TUI) cancels the HTTP stream and any running tools. Partial assistant text is kept in history, marked as interrupted.

### Queued input

While a turn is running the user can type:
- **Enter**: queue the message as the next turn.
- **Steer** (Ctrl-Enter by default): inject the message at the next tool-result boundary of the current turn, as a user message appended after the tool results. The model sees it on its next step.

## Internal tool registry

Internal tools have stable internal names and schemas. Profiles map onto them ([03](03-model-profiles.md)).

| Internal tool | Kind | Purpose |
|---|---|---|
| `read` | ReadOnly | Read a file or line range. Large files return a tree-sitter skeleton plus a hint, not full contents ([05](05-context-management.md)) |
| `index` | ReadOnly | Tree-sitter skeleton of a file or directory: symbols with line ranges (maki-style) |
| `glob` | ReadOnly | File discovery via `ignore` + `globset`. Respects .gitignore |
| `grep` | ReadOnly | ripgrep library. Results capped, grouped by file, with context lines |
| `list_dir` | ReadOnly | Shallow tree, capped |
| `write` | Mutating | Create or overwrite a file |
| `edit.str_replace` | Mutating | Exact-match replace, with fuzzy fallback (below) |
| `edit.multi` | Mutating | Several str_replace edits on one file, applied atomically |
| `edit.apply_patch` | Mutating | Codex `apply_patch` format (ported, Apache-2.0) |
| `edit.whole_file` | Mutating | Model sends the full new content. The diff is shown for approval |
| `edit.search_replace_blocks` | Mutating | Aider-style `<<<<<<< SEARCH / ======= / >>>>>>> REPLACE` blocks, for text mode |
| `edit.hashline` | Mutating | Hash-anchored edits: `read` tags each line with a short content hash, and edits name lines by hash instead of retyping old text (below) |
| `shell` | Mutating | Run a command (see below) |
| `shell.background` | Mutating | Start a long-running process (dev servers, watchers). Tools `shell.output` / `shell.kill` read and stop it |
| `lsp.*` | ReadOnly/Mutating | definition, references, hover, symbols, diagnostics, rename ([10](10-lsp.md)) |
| `memory` | Mutating | view/create/update/delete notes ([07](07-sessions-memory-checkpoints.md)) |
| `todo` | Mutating | Session task list, shown in the UI |
| `skill` | ReadOnly | Load a skill's body on demand ([08](08-instructions-skills-commands.md)) |
| `tool_search` | ReadOnly | Search deferred tools (MCP) and activate them ([09](09-mcp.md)) |
| `ask_user` | Mutating | Ask the user up to 4 questions with options (built 2026-10-19, below) |
| `web_fetch` | ReadOnly | **Disabled by default.** Unavailable when `offline = true` |
| `task` | Mutating | Sub-agent with its own context (built 2026-10-19, [below](#sub-agents)) |

As implemented (2026-10-18): `todo` replaces the whole checklist on each call (items with `pending` / `in_progress` / `done`; the shapes models commonly send are accepted). While anything is unfinished, the plan is appended to the last message of every request, not stored in the history, so it survives long tool output and compaction without touching the cached prefix. It needs no approval in any mode. This follows the component study in [arXiv 2609.20804](https://arxiv.org/abs/2609.20804) (persistent plan, re-shown each step: an accuracy aid for weaker models, a cost saver for stronger ones). The TUI shows it as a tool line, a strip above the input, and `/todo` for the whole list.

As implemented (2026-10-19): `ask_user` takes `questions: [{question, header?, options?: [label | {label, description}], recommended?}]`, at most 4 questions and 6 options each; a single `{question, options}`, `choices`, and a `(recommended)` mark in a label are accepted too. It needs no approval in any mode (`Subject::Plan`). The engine sends `Event::Questions` and waits for `Op::AnswerQuestions` (one `Answer` per question: `picked`, `text` or `skipped`, plus `stop_asking`); the wait counts as user time in `/stats`, and a `questions` record goes in the session log. The model reads `Q: … / A: …` lines, a skip as "decide yourself and say what you assumed", and `stop_asking` as no more questions. Headless runs (`quack exec`, `quack eval`) answer at once with everything skipped. The TUI shows a picker above the input: Up/Down or `1`–`9` and Enter pick (the recommendation is preselected), typing answers in the user's own words, Esc skips, Left goes back, Ctrl-S skips the rest and stops the asking. Profiles map it like any tool (`AskUserQuestion` in `cc-style`).

Each tool lives in `quack-tools/src/<tool>.rs` and implements:

```rust
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;
    fn kind(&self) -> ToolKind;
    fn schema(&self) -> &schemars::schema::RootSchema;
    fn permission_subject(&self, args: &Value) -> PermissionSubject;   // e.g. Shell("git status"), Edit(path)
    async fn run(&self, args: Value, ctx: ToolCtx) -> ToolResult;       // ctx: cwd, cancel, sandbox, lsp, budget
}
```

`ToolResult` carries both a **model-facing** payload (already budget-trimmed) and a **UI-facing** payload (full diff, full output). The UI never has to re-run anything to show details.

## Edit tools

These are the main source of failures with open models, so they get the most hardening.

- **Fuzzy matching for `str_replace`**, tried in order: exact match → match with trailing whitespace normalized → match with leading indentation normalized (then re-indent the replacement) → line-level match ignoring blank-line differences. Any non-exact match is reported in the result ("matched with whitespace normalization"). Multiple matches are an error unless `count` says otherwise, and the error lists the line numbers of each match.
- **Stale-read guard**: an edit to a file that changed on disk after the model last read it (by mtime + hash) is rejected with a fresh snippet of the relevant region.
- **Line endings and encoding**: detect CRLF/LF and the BOM per file and preserve them. Treat the model's `\n` as the file's newline. Refuse non-UTF-8 files unless forced.
- **Checkpoint before write**: store the pre-image ([07](07-sessions-memory-checkpoints.md)).
- **Post-edit feedback**: return a compact diff summary. When an LSP server is running, also return new diagnostics for the edited file, if any arrive within the settle window ([10](10-lsp.md)).
- **Hash-anchored edits (`edit.hashline`, M2)**: when a profile selects this format, `read` returns lines as `<line>:<hash>| <text>`, where the hash is a few characters derived from the line's content. Edits reference a range by its start and end hashes (`replace`, `insert_after`, `delete`). The model never has to reproduce old text exactly, which is where small models fail. A hash that no longer matches is rejected with a fresh tagged snippet of that region, so retries converge. Hashes survive line drift from earlier edits. One 2026 harness benchmark (by an implementer, so treat it with care) found it beat patch-style edits for 14 of 16 models; open-model evidence is thin and mixed, and `insert_after` is reported to invite duplicated lines. It ships as an option, and the eval decides which profiles default to it.
- **Hashline as implemented (2026-10-18)**: `[tools] edit_format = "hashline"` turns it on for the session; `read` then shows `<line>:<tag>|<text>` (3 base-32 characters of FNV-1a over the line without trailing whitespace) and `edit_hashline` replaces `edit`. Edits are `replace` (start..end), `insert_after` (a tag, or 0 for the top) and `delete`, several per call against one read, applied bottom-up and refused if they overlap. A tag is matched at its line number, then at the nearest line with that hash within 50 lines; otherwise the edit is refused with a fresh tagged view. A bare hash works when it is unique. The result shows the changed area with fresh tags. Not chosen by default for any profile: the large gains reported elsewhere could not be traced to a primary benchmark.
- **Format selection**: `config [tools].edit_format` overrides the model TOML, which overrides the profile default. `auto` uses the profile default.

### As implemented (2026-10-18)

`[tools] edit_format` (or the profile's `[defaults] edit_format` when it is `auto`) picks the one edit tool the model gets:

| Format | Tool | Notes |
|---|---|---|
| `str_replace` (default) | `edit` | Exact, then fuzzy matching. `expected_replacements > 1` counts as `replace_all` |
| `hashline` | `edit_hashline` | `read` tags lines |
| `apply_patch` | `apply_patch` | Codex format: Add/Delete/Update/Move, `@@` anchors, `*** End of File`. Hunks are found exactly, then ignoring trailing whitespace, then indentation |
| `search_replace_blocks` | `edit_blocks` | SEARCH/REPLACE blocks under each file's path; each SEARCH is matched like `old_string`; an empty SEARCH creates a file |
| `whole_file` | none | `write` alone, described as rewriting the whole file |

`write`'s description names whichever edit tool is offered. A call that changes several files is planned completely before anything is written, so a hunk that does not match leaves every file as it was. It is one permission subject (`Subject::Edits`): the strictest answer for any of its files, and "allow for the session" covers each file. Each file gets its own undo point under the call, so `/undo` restores all of them. Line endings and BOMs are kept. `core/tests/edit_formats.rs` covers the three new formats through the engine.

`index` outlines a file or a directory (see [05](05-context-management.md#as-implemented-2026-10-18-index-and-pruning-rules)).

## Shell tool

### Command execution

- One command per call, executed by the configured shell:
  - Linux/macOS: `$SHELL -lc` is avoided because login shells are slow. Run `bash -c` / `zsh -c` with a captured environment snapshot taken once at startup from a login shell (codex/maki approach).
  - Windows: `pwsh -NoLogo -NoProfile -NonInteractive -Command` if installed, otherwise `powershell.exe` with the same flags. Fall back to `cmd /C` only if configured. Git Bash is used when `tools.shell = "bash"` and it is found.
- The profile's tool description tells the model which shell it is writing for, rendered from `{{shell}}`. Models default to bash, so this matters a lot on Windows.
- **Working directory** persists across calls in a session (tracked; `cd` is detected by parsing). **Environment** does not persist.
- **Timeouts**: per-call `timeout` parameter, clamped to a configured maximum. On timeout, the whole process tree is killed.
- **Output**: stdout and stderr are interleaved with order preserved and streamed to the UI. The model sees head and tail trimmed to the budget, with the full output saved to `<session dir>/outputs/<call>.log` and a note telling the model how to read more (`read` on that path, or `grep`).
- **Interactive commands**: no stdin by default (`/dev/null` or `NUL`). Commands known to page or prompt get env overrides: `GIT_PAGER=cat`, `PAGER=cat`, `GIT_TERMINAL_PROMPT=0`, `DEBIAN_FRONTEND=noninteractive`. PTY mode is opt-in per call, for tools that need a TTY.
- **Process trees**: Unix uses `setsid` + `killpg`. Windows assigns every child to a Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, so closing the job kills the whole tree. This also prevents orphaned processes when quack exits or crashes.

### User shell escape

`!cmd` in the input runs a command directly, and its output is added to context. `!!cmd` runs it without adding the output to context (maki's `!!`).

### Output filters (optional)

maki-style output compression: when a configured filter (e.g. `rtk`) is present, it pipes known-noisy commands (`cargo build`, `npm install`, test runners) through that filter. Filters are configured in TOML. None are installed by default.

## Windows specifics checklist

- [ ] `pwsh` detection and version check in `doctor`
- [ ] Executable resolution with `PATHEXT` (`which` crate) for shell, MCP and LSP commands
- [ ] Job Objects for kill-on-close
- [ ] ConPTY via `portable-pty` for PTY mode
- [ ] CRLF preservation in edit tools
- [ ] Long path support (`\\?\` internally, `dunce` for display). Document enabling `LongPathsEnabled`
- [ ] Case-insensitive path comparison for permission rules and stale-read tracking
- [ ] Ctrl-C delivery to child process groups (`GenerateConsoleCtrlEvent`) before a hard kill
- [ ] `cmd`-style quoting pitfalls documented in the `shell` tool description when `cmd` is used

## Open questions

- Should an `undo_last_edit` tool be exposed to the model? Proposed: no. Undo is user-driven only.
- Should the shell support persistent environment variables across calls (a real long-lived shell session)? Proposed: no in M1 because it is fragile across OSes. Revisit later.
