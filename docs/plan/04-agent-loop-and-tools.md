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

As implemented (M2, 2026-10-18; `todo` and `ask_user` don't exist yet):
- Plan mode is an overlay on the session's permissions. Edits are refused. Shell commands run only if they read (`ls`, `cat`, `rg`, `find` without `-exec`/`-delete`, `git status|log|diff|show|…`; every command in a pipeline or list must read, and nothing may be redirected to a file) or match an allow rule. Reads follow the normal rules.
- The request for a plan (`plan.md` in the profile, or a compiled-in default) is appended to each user message sent in plan mode. It is stored in the history, so a resume sends exactly the same request.
- `/plan` enters plan mode and `/plan <task>` enters it and sends the task. `/plan accept` returns to the previous mode and sends "Carry out the plan above." `/plan off` leaves without carrying it out. Shift-Tab cycles ask → auto-edit → plan.
- After each turn in plan mode the TUI reminds the user how to accept or revise the plan. The status bar shows `plan`.
- A mode set during startup is applied once a model is ready. A session switch resets the mode to the configured one.

### Planner / editor split

`[model] planner = "<provider/model>"` (optional) lets a larger model write the plan and the default model carry it out, or the reverse (a hosted model edits from a plan written by an on-prem one). The planner gets only read-only tools. This builds on the existing `small` role; nothing changes when `planner` is unset.

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
| `ask_user` | Interactive | Ask the user a structured question |
| `web_fetch` | ReadOnly | **Disabled by default.** Unavailable when `offline = true` |
| `task` | Mutating | Sub-agent with its own context. M3 ([roadmap](roadmap.md)) |

Each tool lives in `cutoffharness-tools/src/<tool>.rs` and implements:

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
- **Format selection**: `config [tools].edit_format` overrides the model TOML, which overrides the profile default. `auto` uses the profile default.

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
- **Process trees**: Unix uses `setsid` + `killpg`. Windows assigns every child to a Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, so closing the job kills the whole tree. This also prevents orphaned processes when cutoffharness exits or crashes.

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
