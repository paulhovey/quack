# 07 — Sessions, Memory, Checkpoints

## Sessions

### Goals

- Every session is saved automatically, without the user doing anything, and survives crashes.
- Resume reconstructs **exactly** the context the model last saw, including prunes and compactions.
- Sessions can be forked and rewound. The log format is plain and greppable.

### Storage layout

```
<sessions path>/                       # default <user data>/sessions, configurable
└── <project-key>/                     # sanitized repo path + short hash
    ├── index.jsonl                    # one line per session: id, title, started, updated, model, turns
    └── 2026-10-17T21-14-03_<uuidv7>/
        ├── events.jsonl               # append-only event log (source of truth)
        ├── outputs/<call-id>.log      # full tool outputs referenced from events
        ├── attachments/<sha256>.<ext> # pasted images, large pastes
        └── checkpoints/               # file pre-images (see Checkpoints)
```

Implemented in M1: `session_start`, `resume`, `message` (each chat message exactly as sent, so replay is exact), `tool` (UI summary), `approval`, `usage`. A session directory is created at the first turn, so starting and quitting leaves nothing behind. Resume also repairs a tool call left without a result by a crash.

### Event log format

Each line is one JSON object with `{"v":1, "t":"<type>", "ts":"…", ...}`:

| Type | Content |
|---|---|
| `session_start` | version, cwd, workspace root, git head, OS, cutoffharness version, provider/model/profile |
| `user` | input items (text, file refs, image refs) |
| `assistant` | text, reasoning (if kept), tool calls (internal names + model-facing names) |
| `tool_result` | call id, model-facing payload, UI summary, output file ref, duration, exit status |
| `approval` | call id, decision, by whom (user/rule/hook) |
| `prune` | list of `PruneOp`s ([05](05-context-management.md)) |
| `compaction` | replaced range, summary text, model used |
| `model_switch` | from/to |
| `usage` | per-request token usage, TTFT, cache stats |
| `checkpoint` | checkpoint id, files |
| `rewind` / `fork` | target event index |
| `title` | generated or user-set title |

Rules:
- Append-only. Flush after every event, and `fsync` at the end of each turn.
- On load, a truncated final line (from a crash) is ignored with a warning.
- The schema is versioned with `v`. Readers support all older versions, and migrations happen in memory, never by rewriting files.
- Secrets never appear in the log: provider auth isn't recorded, and env values shown in tool calls are passed through a redaction filter (common key patterns plus configured `[sessions] redact = [...]` regexes).

### Commands

- `cutoffharness --continue` resumes the most recent session in this project. (No short flag: `-c` is the config override.)
- `cutoffharness --resume <id>` resumes by id or unique prefix; `cutoffharness sessions` lists them. `/resume` opens the picker (title, age, turns, model, first message preview, fuzzy search).
- `/fork` starts a new session that branches from the current point. Its `session_start` records `parent` + event index.
- `/rewind` picks a previous user message. The conversation is truncated to before it (by appending a `rewind` event; nothing is deleted), and the user is optionally offered to **restore files** to the matching checkpoint.
- `/title <text>`. Automatic titles come from the `small` model after the first turn, if one is configured. Otherwise the title is the truncated first message, with no extra model call.
- `/export [md|json]` writes a readable transcript.
- `cutoffharness sessions prune --older-than 30d` cleans up. `[sessions] retention_days` does the same automatically.

### Parallel sessions in worktrees

`/fork --worktree` (and `cutoffharness --worktree <name>`) creates a git worktree under `<repo>/.cutoffharness/worktrees/<name>` on a new branch and starts a session there, so several tasks can run side by side without touching each other's files. `/sessions` shows which worktree each session belongs to; finishing offers to merge, keep or delete the worktree. It uses `gix` when possible and falls back to the `git` CLI. Planned for M3 with `/fork`.

### Resume semantics

Replay `events.jsonl` → rebuild history → apply `prune` and `compaction` ops in order → re-render the system prompt.

If the current config renders a different system prompt (AGENTS.md changed, profile changed), the session resumes with the **new** prompt and the UI notes it, because the old prompt isn't cached on the server anymore anyway. If the original provider or model is no longer available, the user picks another, and [model switching](02-providers.md#model-switching) rules apply.

## Memory

### Goals

- Notes are plain markdown that the user can read, edit, grep and commit.
- The location is configurable, globally and per project.
- Memory costs little context: only an index is loaded, and the model reads individual notes on demand.

### Layout

```
<memory.path>/                         # default <user data>/memory, configurable
├── MEMORY.md                          # global index: one line per note
├── notes/<slug>.md                    # global notes
└── projects/<project-key>/
    ├── MEMORY.md
    └── notes/<slug>.md
```

With `memory.project_scope = "repo"`, project memory lives in `<repo>/.cutoffharness/memory/` instead, so a team can commit shared memory.

Note format:

```markdown
---
name: build-requires-protoc
description: Building the server crate needs protoc on PATH; Windows uses the vcpkg one
type: project            # user | project | feedback | reference
updated: 2026-10-17
---

The `server` crate's build.rs invokes protoc...
```

### Behaviour

- Both index files (global + project) are injected through `{{memory_index}}`, capped at `memory.index_max_tokens` (default 1,500). They are injected at session start only, so the prefix stays cache-stable.
- The `memory` tool has the actions `view` (list/read), `create`, `update`, `delete`. The index line is updated automatically when a note is written.
- The model is told (in the profile prompt) what is worth remembering and what is not: no secrets, nothing already in the repo.
- `/memory` opens the memory browser: list, view, edit in `$EDITOR`, delete.
- User approval for memory writes is configurable: `memory.writes = "ask" | "auto"`, default `auto`, because writes are visible in the transcript and easy to undo.

As implemented (M2, 2026-10-18):
- The `memory` tool is lenient, because small models get the arguments wrong:
  - a title such as `Code style` becomes the name `code-style`;
  - a missing `content` or `description` is filled from the other;
  - the type defaults to `project` (project scope) or `user` (global scope).
- `view` without a scope looks in project notes, then global ones.
- Index lines are replaced in place, and lines the user wrote by hand are kept.
- `/memory` lists notes from both scopes with their text and file path. Ctrl-S deletes the selected note.
- The startup panel shows the note count and the project memory folder.
- Memory writes have their own permission subject. They are outside the workspace, so the edit rules and `read-only` mode don't apply to them; only `memory.writes` does.

## Checkpoints (file undo)

maki rewinds only chat history. We also restore **files**.

### Mechanism

Two layers:

1. **Edit-tool pre-images (always on).** Before any `write`/`edit.*`/`lsp.rename` modifies a file, its prior contents are stored content-addressed in `checkpoints/objects/<sha256>`, along with a manifest entry `{checkpoint_id, path, pre_hash | absent}`. This costs little, works without git, and works identically on every OS.
2. **Workspace snapshots (optional; `checkpoints.shell = true`).** Before a mutating `shell` call, take a snapshot of the tracked worktree state using `gix` into a **shadow repository** in the session directory (separate `GIT_DIR`, the user's repo is never touched). This catches files changed by shell commands. It can be slow on very large repos, so it is off by default and the time spent is reported.

A checkpoint is created at each **user turn** boundary (grouping all edits in that turn) and before each mutating tool call.

### As implemented (2026-10-18, ahead of M3)

- **Pre-images:** layer 1 only. Before a `write` or `edit` call runs, the file's bytes are copied to `<session>/checkpoints/NNNNNN.pre`, or the manifest notes that the file did not exist.
- **Manifest:** `manifest.jsonl` records the pre-image, a hash of the file after the call (FNV-1a, used only to spot later changes), and undos. Each pre-image is stored per call, not content-addressed. Sessions are small, and no SHA-256 crate is needed.
- **`/undo [--force]`** reverts the files of the last successful edit call: new files are deleted and changed ones get their bytes back, CRLF included. A file changed since the call is left alone and reported; `--force` overwrites it.
- **The model is told** at the start of the next user message ("The user undid your last change to …"), so it doesn't build on the reverted edit.
- **Not tracked:** shell commands (the shadow-repo snapshots come later).
- **`/rewind`** (or Esc Esc while idle) lists the user messages still in the history, most recent first, with the files changed since each one. The files of a rewind point are those changed by the tool calls in the history after that message, so no separate turn bookkeeping is needed. In the picker, Enter rewinds the conversation and the files, and Ctrl-S rewinds the conversation only. `/rewind <n> files` rewinds the files only, and adds `--force` to overwrite files changed since.
  - **Conflicts:** if any of the files changed again since, nothing is rewound and the conflicts are listed. This differs from `/undo`, which restores what it can, because a half-done rewind would leave the files and the conversation out of step.
  - **Conversation:** the history is cut back to before the message, and a `rewind` record (`{"to": <history length>}`) is appended to the session log; nothing is deleted. Resume replays the cut, the transcript included. The message goes back into the input for editing.
  - **Files only:** the model is told at the start of the next message, as with `/undo`.
  - **Tested:** `core/tests/rewind.rs` restores a CRLF file byte for byte and deletes a file created since. Messages already folded into a compaction summary can't be rewound to.

### Commands

- `/undo` reverts the files changed by the last mutating tool call.
- `/rewind` → pick a turn → choose "conversation only", "files only", or "both".
- Restoring checks for conflicts: if a file has changed since the checkpoint (by the user, outside cutoffharness), restoring shows a diff and asks before overwriting.

## Open questions

- Should we add SQLite (`rusqlite`) for a cross-project session index and full-text search? Proposed: not in M1. `index.jsonl` per project plus `grep` is enough to start. Revisit when cross-project search is wanted.
- Should memory have semantic search (embeddings)? Proposed: no. Embeddings would require an embedding model, and the index + grep approach keeps memory simple and offline.
