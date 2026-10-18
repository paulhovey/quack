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
 cutoffharness 0.1.0 · ~/src/project (main*)
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
│ corp-vllm/qwen3-coder · qwen-code · ask · 41.2k/220k (88% cached) · ⚠1│
└────────────────────────────────────────────────────────────────────────┘
```

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
│ │                                     │ │ file  ~/.config/cutoffharness/ │ │
│ │                                     │ │       providers.d/corp-vllm.toml│ │
│ └─────────────────────────────────────┘ └────────────────────────────────┘ │
│ ↑↓ move  ⏎ use for session  ^D set default  ^E edit file  ^R refresh  esc  │
└────────────────────────────────────────────────────────────────────────────┘
```

- **Size**: about 90% × 85% of the terminal, centred. Below 80×24 it switches to a full-screen single-column layout, with the details pane shown on a separate view.
- **Parts**: title, optional tabs, a filter line (always focused, so typing filters straight away), a list pane, a details/preview pane, and a footer listing the key bindings.
- **Shared behaviour**: fuzzy filter (`nucleo`), ↑/↓ or j/k to move, Tab to switch panes or tabs, Enter for the main action, Esc to close (or go back one level), `?` for help on that modal.
- **Edit file** (Ctrl-E) opens the item's backing TOML file in `$EDITOR`. The file is hot-reloaded when the editor exits.
- **Writes go through `toml_edit`**, so comments and formatting are preserved. Every write asks for a **scope** when more than one applies: *Session only* / *Project* (`.cutoffharness/`) / *User*. The system layer is never written from the UI.
- **Policy-locked values** show a 🔒 (ASCII: `[locked]`) with the locking file, and can't be edited.
- **Async data** (model discovery, MCP status) streams in. Rows show a spinner until they are ready, and the modal never blocks.
- Modals are pure views over engine state plus Ops, so headless and ACP clients get the same operations through the protocol.

### Command catalogue

| Command | Aliases | Opens / does |
|---|---|---|
| `/models` | `/model` | Models modal (above). Switch for session, set default (`[model].default`), set `small` model, refresh discovery, set unknown context window |
| `/providers` | | Provider list: state, protocol, base URL, auth status. Actions: enable/disable, test connection, set API key (keyring/env/cmd wizard), edit file, **add provider** (pick a template from the defaults dir, or start blank; writes a new `providers.d/<id>.toml`) |
| `/profiles` | `/profile` | Profile list with match rules. Actions: switch for session, pin for model (writes `profile =` into the model entry), preview the rendered prompt and its token cost |
| `/resume` | `/sessions` | Session browser for this project (Tab switches to all projects): title, age, turns, model, preview pane with the transcript head. Actions: resume, fork, rename, export, delete (with confirmation) |
| `/new` | `/clear` | Start a fresh session (the current one stays saved) |
| `/rewind` | (Esc Esc) | Pick an earlier user message. Choose conversation / files / both |
| `/undo` | | Revert the last mutating tool call's file changes |
| `/fork` | | Branch the session from here |
| `/title <text>` | | Rename the session |
| `/export [md\|json]` | | Write transcript to a file |
| `/compact [focus]` | | Compact now, with optional focus instructions |
| `/context` | | Context breakdown modal: segments, tokens, prunable items, cache state; actions: prune now, drop an item |
| `/config` | `/settings` | **Settings editor**: tabs mirror `config.toml` tables (Model, UI, Context, Tools, Permissions, Memory, Sessions, Network). Each row shows the effective value, its source layer and a description from the JSON Schema. Edit inline with type-aware widgets (toggle, enum picker, number, string, list) |
| `/permissions` | | Mode picker plus allow/ask/deny rule lists per layer; add/remove rules; shows the compiled-in deny floor (read-only) |
| `/mcp` | | MCP servers: state, tool count, token cost, deferred flag, logs. Actions: restart, enable/disable (session or persistent), view tools, add server (template or blank) |
| `/lsp` | | LSP servers: state, indexing progress, diagnostics count; restart, enable/disable |
| `/skills` | | Skill list by layer; view SKILL.md; enable/disable; invoke |
| `/commands` | | Custom commands by layer; view; create a new one in `$EDITOR` |
| `/memory` | | Memory browser: global/project tabs, view, edit, delete, open folder |
| `/hooks` | | Hooks list (read-only for system layer), last run results |
| `/instructions` | | Loaded AGENTS.md chain, with sizes; open in editor; `/init` to generate one |
| `/init` | | Generate AGENTS.md for this repo |
| `/todo` | | Toggle the todo side pane |
| `/tasks` | | Sub-agent views (M5) |
| `/theme` | | Theme picker with live preview |
| `/keys` | | Keybinding viewer/editor |
| `/doctor` | | Runs `cutoffharness doctor` checks, shown as a modal report |
| `/stats` | | Session usage: tokens, cache hits, TTFT, cost (hosted services) |
| `/reload` | | Reload config, instructions, skills |
| `/cd <path>` | | Change the workspace directory (updates roots, LSP, instructions) |
| `/copy` | | Copy the last response (or pick a code block) to the clipboard |
| `/help` | `?` | Commands and keys |
| `/quit` | `/exit` | Quit |

Built-in commands are implemented in code. Custom commands with the same name are rejected, with a warning naming the file.

### Other modals

The **approval dialog** shows a diff view for edits (side-by-side at 160 columns or wider), a command preview for shell calls with the parsed sub-commands highlighted, and the actions *once / session / always (choose scope) / deny / deny with feedback*. The **startup detail** view opens when you press Enter on a startup panel row.

## Input editor

- Multi-line with Shift-Enter / Alt-Enter for newlines. Ctrl-J also works, for terminals that can't distinguish Shift-Enter. The kitty keyboard protocol is enabled when supported.
- History (↑/↓, Ctrl-R search), persisted per project.
- `@` fuzzy file completion (via `ignore` walker + `nucleo` matcher), `/` command completion (built-in + custom commands + skills), `!` shell escape.
- Bracketed paste. Large pastes collapse into a `[pasted 340 lines]` chip that expands on demand. Image paste and drag-drop paths attach images when the model supports vision.
- An external editor (Ctrl-G opens `$EDITOR` / `%EDITOR%` / `notepad`) for long prompts.

## Rendering performance

- **Redraw coalescing**: engine events mark the UI dirty. A frame ticker redraws at most every 16 ms (60 fps), and only when dirty. Streaming deltas append into a buffer and never trigger an immediate draw.
- **Virtualized transcript**: each message caches its rendered `Vec<Line>` keyed by width. Only visible messages are laid out. A resize invalidates layout lazily.
- **Incremental markdown**: during streaming, only the trailing open block is re-parsed. Earlier blocks are frozen.
- **Highlighting**: `tree-sitter-highlight`, reusing the compiled grammars. Highlighting runs off-thread for large code blocks, which render plain first and are highlighted when ready.
- **No blocking I/O on the render path.** Everything goes through the engine or a background task.

## Keybindings

Defaults are listed in `cutoffharness keys --print`. Users override them in `config.toml [keys]`. Notable defaults: Esc interrupts the turn (double Esc opens the rewind picker), Ctrl-C clears input or interrupts, and a second Ctrl-C quits. Shift-Tab cycles permission mode, and Ctrl-T toggles the todo pane.

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
- Mouse: optional (`ui.mouse = false` default), because mouse capture breaks native text selection in most terminals.

## Accessibility

- `ui.reduced_motion` replaces spinners with static indicators.
- A screen-reader-friendly mode (`--plain`) disables the alt screen and box drawing, and prints linear output, close to headless text mode with input.

## Testing

- `ratatui::backend::TestBackend` + `insta` snapshot tests for every widget and overlay at 80×24 and 200×50.
- Scripted UI tests: feed a recorded Event stream and keystrokes, then snapshot the frames.
- A startup time check in CI ([13](13-testing-and-evals.md)).

## Open questions

- Text selection in full-screen mode: most terminals let users bypass mouse capture with Shift-drag, and mouse capture is off by default anyway. Is in-app selection with a "copy selection" mode worth building? Proposed: `/copy` plus code-block picking in M1, and in-app selection later only if users ask for it.
- Should we support a split-pane diff view (side-by-side) for approvals on wide terminals? Proposed: yes, automatically at ≥160 columns.
