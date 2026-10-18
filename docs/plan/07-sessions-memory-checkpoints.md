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

- `cutoffharness --continue` / `-c` resumes the most recent session in this project.
- `cutoffharness --resume [id]` / `/resume` opens the picker (title, age, turns, model, first message preview, fuzzy search).
- `/fork` starts a new session that branches from the current point. Its `session_start` records `parent` + event index.
- `/rewind` picks a previous user message. The conversation is truncated to before it (by appending a `rewind` event; nothing is deleted), and the user is optionally offered to **restore files** to the matching checkpoint.
- `/title <text>`. Automatic titles come from the `small` model after the first turn, if one is configured. Otherwise the title is the truncated first message, with no extra model call.
- `/export [md|json]` writes a readable transcript.
- `cutoffharness sessions prune --older-than 30d` cleans up. `[sessions] retention_days` does the same automatically.

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

## Checkpoints (file undo)

maki rewinds only chat history. We also restore **files**.

### Mechanism

Two layers:

1. **Edit-tool pre-images (always on).** Before any `write`/`edit.*`/`lsp.rename` modifies a file, its prior contents are stored content-addressed in `checkpoints/objects/<sha256>`, along with a manifest entry `{checkpoint_id, path, pre_hash | absent}`. This costs little, works without git, and works identically on every OS.
2. **Workspace snapshots (optional; `checkpoints.shell = true`).** Before a mutating `shell` call, take a snapshot of the tracked worktree state using `gix` into a **shadow repository** in the session directory (separate `GIT_DIR`, the user's repo is never touched). This catches files changed by shell commands. It can be slow on very large repos, so it is off by default and the time spent is reported.

A checkpoint is created at each **user turn** boundary (grouping all edits in that turn) and before each mutating tool call.

### Commands

- `/undo` reverts the files changed by the last mutating tool call.
- `/rewind` → pick a turn → choose "conversation only", "files only", or "both".
- Restoring checks for conflicts: if a file has changed since the checkpoint (by the user, outside cutoffharness), restoring shows a diff and asks before overwriting.

## Open questions

- Should we add SQLite (`rusqlite`) for a cross-project session index and full-text search? Proposed: not in M1. `index.jsonl` per project plus `grep` is enough to start. Revisit when cross-project search is wanted.
- Should memory have semantic search (embeddings)? Proposed: no. Embeddings would require an embedding model, and the index + grep approach keeps memory simple and offline.
