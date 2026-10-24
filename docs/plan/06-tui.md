# 06 — TUI

## Goals

- **First frame in under 50 ms** on a warm cache, measured from process start. Input is accepted immediately.
- Show the user what is loading, what is ready, and what failed, without blocking them.
- Stay smooth while streaming at hundreds of tokens per second, and when scrolling transcripts with thousands of messages.
- **Full-screen** (alternate screen) application, decided 2026-10-17. Configuration is done through **slash commands that open large modals**, as in most harnesses. Editing TOML by hand is always possible but never required for common tasks.
- Look and behave the same on Windows Terminal, iTerm2/WezTerm/Kitty/Alacritty/Ghostty, GNOME Terminal, and VS Code's terminal.

## Startup panel

The panel is drawn at the top of the transcript area during startup. It collapses to a single status-bar line when everything is ready (`ui.startup_panel = "auto"`).

```
 quack 0.1.0 · ~/src/project (main*)
 ✓ config        3 layers · 4 providers · 2 profiles                 12ms
 ✓ provider      corp-vllm · qwen3-coder (262k ctx)                  84ms
 ⠋ tokenizer     loading qwen3-coder.json
 ✓ instructions  AGENTS.md (2) · 5 skills · 3 commands               9ms
 ? compat        found CLAUDE.md, .claude/skills (4) · [L]oad [I]gnore [D]etails
 ⠋ mcp           github: starting · jira: ready (14 tools)
 ✗ lsp           rust-analyzer: not found on PATH  (disable: lsp.d/rust-analyzer.toml)
 – web_fetch     disabled (offline)
```

- Each row comes from `Event::StartupProgress`.
- A failed row shows the exact file to edit or delete. Enter on a row opens a detail view with the full error.
- A prompt submitted before the provider is ready is queued: `⏳ queued — waiting for provider`.

## Layout

```
┌ transcript (scrollable, virtualized) ──────────────────────────────────┐
│ › user message                                                        │
│ ● assistant text (markdown rendered)                                  │
│   ▸ thinking (collapsed, 1.2k tok)                                    │
│   ⚙ edit src/main.rs  +12 −3   ✓                         [d]iff       │
│   ⚙ shell cargo test  ✗ exit 101 (2.1s)                  [o]utput     │
├ todo (optional side pane, toggle) ─────────────────────────────────────┤
├ input ─────────────────────────────────────────────────────────────────┤
│ > _                                                                   │
├ status ────────────────────────────────────────────────────────────────┤
│ corp-vllm/qwen3-coder · qwen-code · ask · 41.2k/220k (88% cached) · 63 t/s │
└────────────────────────────────────────────────────────────────────────┘
```

Choice bars (2026-10-19) sit between the transcript and the input, taking rows from the transcript (the startup panel hides while one shows): the model's `ask_user` questions, and the plan review after a plan-mode turn. Both number their choices, preselect one, and keep the input live so the user can type an answer or a revision instead ([04](04-agent-loop-and-tools.md#plan-mode)).

Speed in the status bar: local and on-prem users care more about speed than dollars. The status bar shows the last response's generation speed (tokens per second, from server timings when reported, otherwise measured), and a details line in `/context` shows prompt processing time, time to first token and cache hits. Cost is shown only when the model is priced: by its provider file, or by the `pricing` its provider's models endpoint lists.

## Slash commands & modals

Typing `/` opens a completion popup above the input. It fuzzy-matches built-in commands, custom commands ([08](08-instructions-skills-commands.md)), skills and MCP prompts, and shows a one-line description for each. Ctrl-P opens the same list as a full command palette. Enter runs the command. Commands that need choices open a modal. Commands with arguments (`/title fix auth`) run directly.

### Modal framework

All configuration UIs share one modal widget, so they look and behave the same:

```
┌ Models ─────────────────────────────────────────────── [Session] [Default] ┐
│ filter: qwen_                                                              │
│ ┌ list ───────────────────────────────┐ ┌ details ───────────────────────┐ │
│ │ corp-vllm                    ● ready│ │ Qwen/Qwen3-Coder-480B-A35B     │ │
│ │ ▸ qwen3-coder        262k  qwen-code│ │ provider   corp-vllm           │ │
│ │   qwen3-4b            32k  generic  │ │ protocol   openai-chat         │ │
│ │ local-llama                  ● ready│ │ context    262,144 (max out 32k)│ │
│ │   qwen3-coder-30b    128k  qwen-code│ │ profile    qwen-code (auto)    │ │
│ │ novita                    ✗ no key  │ │ tools      native · qwen3-xml  │ │
│ │   glm-4.6            200k  cc-style │ │ reasoning  none                │ │
│ │                                     │ │ file  ~/.config/quack/ │ │
│ │                                     │ │       providers.d/corp-vllm.toml│ │
│ └─────────────────────────────────────┘ └────────────────────────────────┘ │
│ ↑↓ move  ⏎ use for session  ^D set default  ^E edit file  ^R refresh  esc  │
└────────────────────────────────────────────────────────────────────────────┘
```

- **Size**: about 90% × 85% of the terminal, centred. Below 80×24 it switches to a full-screen single-column layout, with the details pane shown on a separate view.
- **Parts**: title, optional tabs, a filter line (always focused, so typing filters straight away), a list pane, a details/preview pane, and a footer listing the key bindings.
- **Shared behaviour**: fuzzy filter (`nucleo`), ↑/↓ or j/k to move, Tab to switch panes or tabs, Enter for the main action, Esc to close (or go back one level), `?` for help on that modal.
- **Edit file** (Ctrl-E) opens the item's backing TOML file in `$EDITOR`. The file is hot-reloaded when the editor exits.
- **Writes go through `toml_edit`**, so comments and formatting are preserved. Every write asks for a **scope** when more than one applies: *Session only* / *Project* (`.quack/`) / *User*. The system layer is never written from the UI.
- **Policy-locked values** show a 🔒 (ASCII: `[locked]`) with the locking file, and can't be edited.
- **Async data** (model discovery, MCP status) streams in. Rows show a spinner until they are ready, and the modal never blocks.
- Modals are pure views over engine state plus Ops, so headless and ACP clients get the same operations through the protocol.

### Command catalogue

| Command | Aliases | Opens / does |
|---|---|---|
| `/models` | `/model` | Models modal (above). Switch for session, set default (`[model].default`), set `small` model, refresh discovery, set unknown context window |
| `/providers` | | Provider list: state, protocol, base URL, auth status. Actions: enable/disable, test connection, set API key (keyring/env/cmd wizard), edit file, **add provider** (pick a local engine or hosted service from setup's list, or start blank; writes a new `providers.d/<id>.toml`) |
| `/key` | | `/key <provider>`: replace a provider's API key, for example after rotating it (a hidden prompt). A key in the OS credential store is replaced under its own name and the provider file stays as it is; a key read from elsewhere (`env`, `file`, `cmd`, a value) moves to the store, and the provider file in the user layer is pointed at it. Providers in use take the new key at once: clones of a `Provider` share its headers, so a running turn and the planner pick it up too. With no argument it opens `/providers`, where Enter on a provider with a key does the same, as Ctrl-S does on a ready hosted service in `/setup` (2026-10-22) |
| `/setup` | | Find model servers on this machine (loopback ports only) and add one: one row per server and model, Enter writes `providers.d/<id>.toml` to the user layer and switches to the model. Tabs for hosted services (enter the key; it goes to the OS credential store) and any server by its URL. Opens on its own when startup ends with no model ([14](14-packaging-and-distribution.md#setup-in-the-tui-2026-10-20)) |
| `/profiles` | `/profile` | Each profile rendered for the current model: system prompt, tool names, edit format, and which one `auto` picks. Enter switches the session (`/profiles <id>`, `/profiles auto`), Ctrl-S saves `[model] profile`. Built 2026-10-18; pinning per model and the token cost are still to do |
| `/resume` | `/sessions` | Session browser for this project (Tab switches to all projects): title, age, turns, model, preview pane with the transcript head. Actions: resume, fork, rename, export, delete (with confirmation) |
| `/new` | `/clear` | Start a fresh session (the current one stays saved) |
| `/rewind` | (Esc Esc) | Pick an earlier user message. Choose conversation / files / both |
| `/undo` | | Revert the last mutating tool call's file changes |
| `/fork` | | Branch the session from here. `--worktree` runs the branch in its own git worktree ([07](07-sessions-memory-checkpoints.md#parallel-sessions-in-worktrees)) |
| `/plan` | | Enter plan mode: read-only exploration and a few questions, ending in a todo plan with a review bar to accept or revise ([04](04-agent-loop-and-tools.md#plan-mode)) |
| `/bundles` | | Installed bundles with their contents; turn each on or off ([08](08-instructions-skills-commands.md#bundles)) |
| `/title <text>` | | Rename the session |
| `/export [md\|json]` | | Write transcript to a file |
| `/compact [focus]` | | Compact now, with optional focus instructions |
| `/context` | | Context breakdown modal: segments, tokens, prunable items, cache state; actions: prune now, drop an item |
| `/config` | `/settings` | **Settings editor**: tabs mirror `config.toml` tables (Model, UI, Context, Tools, Permissions, Memory, Sessions, Network). Each row shows the effective value, its source layer and a description from the JSON Schema. Edit inline with type-aware widgets (toggle, enum picker, number, string, list) |
| `/permissions` | | Mode picker plus allow/ask/deny rule lists per layer; add/remove rules; shows the compiled-in deny floor (read-only) |
| `/mcp` | | MCP servers: state, tool count, token cost, deferred flag, logs. Actions: restart, enable/disable (session or persistent), view tools, add server (template or blank) |
| `/lsp` | | LSP servers: state, indexing progress, diagnostics count; restart, enable/disable |
| `/skills` | | Skill list by layer; view SKILL.md; enable/disable; invoke |
| `/commands` | | Custom commands from each layer's `commands/` folder: name, arguments, description, file (built 2026-10-18). Typing `/<name> args` runs one |
| `/memory` | | Memory browser: global/project tabs, view, edit, delete, open folder |
| `/hooks` | | Hooks list (read-only for system layer), last run results |
| `/instructions` | | The AGENTS.md files in the system prompt, then those attached to tool results so far, each with its size and text (built 2026-10-18). Opening in an editor is still to do; `/init` generates one |
| `/init` | | Generate AGENTS.md for this repo |
| `/todo` | | Expand the plan line above the input into the whole checklist, and back (built 2026-10-18; above the input rather than a side pane, so it works at any width) |
| `/tasks` | | Sub-agent runs of this session, newest first: model, state, each call and the report (built 2026-10-19). Under a running `task` line the transcript shows the agent's last steps |
| `/theme` | | Theme picker with live preview |
| `/keys` | | Keybinding viewer/editor |
| `/doctor` | | Runs `quack doctor` checks, shown as a modal report |
| `/stats` | | Session usage: tokens sent and received, cache hits, cost (when priced), where the time went (model, tools, approvals, between turns), latency and speed, per-tool calls and failures. Drawn with ratatui charts: figure tiles, bars, meters, per-request sparklines (the engine keeps the last 200 requests) and a line of the running cost |
| `/reload` | | Read config, providers, MCP servers, instructions and skills again, keeping the conversation ([01](01-configuration.md#hot-reload)) |
| `/trust` | `[trusted\|settings-only\|ignored]` | Decide whether this repo's `.quack/` loads; opens on its own when a decision is needed ([01](01-configuration.md#as-implemented-2026-10-18)) |
| `/cd <path>` | | Change the workspace directory (updates roots, LSP, instructions) |
| `/copy` | | Copy the last response (or pick a code block) to the clipboard. Implemented as `/copy [code]` through OSC 52: no clipboard library is needed, and it works over SSH. Inside tmux it needs `set -g set-clipboard on` |
| `/help` | `?` | Commands and keys |
| `/quit` | `/exit` | Quit |

Built-in commands are implemented in code. Custom commands with the same name are rejected, with a warning naming the file.

### Other modals

The **approval dialog** shows a diff view for edits (side-by-side at 160 columns or wider), a command preview for shell calls with the parsed sub-commands highlighted, and the actions *once / session / always (choose scope) / deny / deny with feedback*. The **startup detail** view opens when you press Enter on a startup panel row.

## Input editor

- Multi-line with Shift-Enter / Alt-Enter for newlines. Ctrl-J also works, for terminals that can't distinguish Shift-Enter. The kitty keyboard protocol is enabled when supported.
- History (↑/↓, Ctrl-R search), persisted per project.
- `@` fuzzy file completion (via `ignore` walker + `nucleo` matcher), `/` command completion (built-in + custom commands + skills), `!` shell escape.
- Bracketed paste. Large pastes collapse into a `[pasted 340 lines]` chip that expands on demand. Image paste and drag-drop paths attach images when the model supports vision. As implemented (2026-10-24): a paste whose every token (after stripping `file://`) is an existing image file attaches them — the drag-drop case — while anything else inserts as text. Attachments show as a chips line above the input (`[image] name 1024x768 · …`, Backspace on empty input removes the last), and Enter with attachments but no text sends them alone.
- An external editor (Ctrl-G opens `$EDITOR` / `%EDITOR%` / `notepad`) for long prompts.

## Rendering performance

- **Redraw coalescing**: engine events mark the UI dirty. A frame ticker redraws at most every 16 ms (60 fps), and only when dirty. Streaming deltas append into a buffer and never trigger an immediate draw.
- **Virtualized transcript**: each message caches its rendered `Vec<Line>` keyed by width. Only visible messages are laid out. A resize invalidates layout lazily.
- **Incremental markdown**: during streaming, only the trailing open block is re-parsed. Earlier blocks are frozen.
- **Highlighting**: `tree-sitter-highlight`, reusing the compiled grammars. Highlighting runs off-thread for large code blocks, which render plain first and are highlighted when ready.
  - *As implemented (2026-10-21)*: `quack-tui/src/highlight.rs` keeps a cache of highlighted runs keyed by language and code. `draw` only reads it. The app fills it from `AssistantDelta`, `AssistantRewrite`, `SessionResumed` and `/reload`, for closed fences only (`markdown::code_blocks`), so a block still streaming in stays plain. A block is highlighted on the spot when it is under 8 KiB and its language's queries are already compiled. Otherwise it goes to `spawn_blocking` (the first block in a language compiles its queries, about 25 ms on average in a release build) and comes back over a channel the event loop selects on. Blocks over 256 KiB stay plain. Fence tags map to languages by name or extension (`rust,ignore`, `py`, `sh`). Colours come from the theme's existing styles (`Theme::syntax`), so light, dark and `NO_COLOR` follow. Diffs and tool output are not highlighted yet.
- **No blocking I/O on the render path.** Everything goes through the engine or a background task.
- **Markdown tables**: GFM tables rendered with column alignment (left/center/right), proportional width budget allocation with word-wrapping within cells, and an ASCII fallback.

## Keybindings

Defaults are listed in `quack keys --print`. Users override them in `config.toml [keys]`. Notable defaults: Esc interrupts the turn (double Esc opens the rewind picker), Ctrl-C clears input or interrupts, and a second Ctrl-C quits. Shift-Tab cycles permission mode, and Ctrl-T shows or hides the model's thinking (`/thinking`). PageUp/PageDown and Shift-Up/Down scroll the transcript, Ctrl-Home/Ctrl-End jump to its top and bottom, and the todo pane is toggled with `/todo`.

## Themes

- `auto` detects light or dark background (OSC 11 query with timeout; `COLORFGBG` fallback).
- Themes are TOML files in `themes/`. Two are compiled in (dark, light) because a broken theme must not break the UI. Others ship in defaults.
- Colour depth is detected (truecolor / 256 / 16), and themes are down-sampled to match.
- `NO_COLOR` is honoured.

## Windows terminals

- crossterm reports both **Press and Release** key events on Windows. Filter to `KeyEventKind::Press` (and `Repeat`) everywhere. This is a common ratatui bug.
- Supported: Windows 10 1809+ and Windows 11, on x64 and ARM64. VT processing is required. On legacy conhost without VT, print a clear message suggesting Windows Terminal.
- Unicode width: use `unicode-width` and avoid ambiguous-width glyphs in chrome. Provide `ui.ascii = true` for fonts that lack box-drawing or spinner glyphs.
- Clipboard: OSC 52 where supported, with `arboard` as a fallback (Windows clipboard API) for "copy last response" and "copy code block".
- Mouse: on by default (`ui.mouse = true`): the wheel scrolls the transcript and a left click places the input cursor. Capture breaks native text selection in most terminals, so quack selects itself (`select.rs`): a drag in the transcript or the input marks the text, and letting go copies it through OSC 52, with "copied N chars" in the status bar. The selection is kept in content rows, so it stays on its text while the view scrolls (dragging past the top or bottom edge scrolls along). Copied transcript text leaves out each row's lead (duck, `›`, tool icon) and the indent the rows share; input text keeps its real line breaks, not the wrapping. Any key, click or resize clears it, and Ctrl-C while it shows copies it again instead of interrupting. Shift-drag still gives the terminal's own selection, and `ui.mouse = false` turns capture off; `/reload` follows the setting.

## Accessibility

- `ui.reduced_motion` replaces spinners with static indicators.
- A screen-reader-friendly mode (`--plain`) disables the alt screen and box drawing, and prints linear output, close to headless text mode with input.

## Testing

- `ratatui::backend::TestBackend` + `insta` snapshot tests for every widget and overlay at 80×24 and 200×50.
- Scripted UI tests: feed a recorded Event stream and keystrokes, then snapshot the frames.
- A startup time check in CI ([13](13-testing-and-evals.md)).

## Open questions

- Text selection in full-screen mode: resolved 2026-10-22, in-app drag-to-copy (see Windows terminals above).
- Should we support a split-pane diff view (side-by-side) for approvals on wide terminals? Proposed: yes, automatically at ≥160 columns.
