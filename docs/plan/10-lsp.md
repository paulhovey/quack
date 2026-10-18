# 10 — LSP (Language Server Protocol)

## Goals

- Give the model compiler-grade feedback (diagnostics after edits) and precise navigation (definition, references) without it having to run full builds.
- Each language server is **one TOML file** in `lsp.d/`. **Nothing is downloaded automatically.** A server is used only if its binary is already on PATH, or at a configured path.
- Servers start lazily and never block startup or turns.

## Server file schema

```toml
#:schema ../schemas/lsp.json
id = "rust-analyzer"
enabled = true
command = "rust-analyzer"              # PATH lookup (+PATHEXT on Windows)
args = []
languages = ["rust"]                   # tree-sitter / LSP languageId
extensions = ["rs"]
root_markers = ["Cargo.toml", "rust-project.json"]   # nearest ancestor = workspace folder
env = {}

startup = "lazy"                       # lazy (first matching file touched) | eager | manual
idle_shutdown_secs = 900

initialization_options = { check = { command = "clippy" } }
[settings]                             # workspace/configuration responses
"rust-analyzer.cargo.features" = "all"

[diagnostics]
settle_ms = 800                        # wait this long after an edit for publishDiagnostics to settle
max_wait_ms = 5000                     # never wait longer than this
min_severity = "warning"               # error | warning | info | hint
max_items = 20                         # per file, in a tool result
```

`defaults/lsp.d/` ships commented files for: rust-analyzer, gopls, pyright / basedpyright, typescript-language-server, clangd, jdtls, omnisharp / csharp-ls, lua-language-server, bash-language-server, powershell-editor-services, taplo, yaml-language-server. Each file includes a comment with the install command. **We never run that command ourselves.**

## Client manager (`cutoffharness-lsp`)

- One client per `(server id, workspace folder)`.
- Implementation: `lsp-types` + JSON-RPC over stdio. `async-lsp` is to be evaluated in the M4 spike (see [00](00-architecture.md#open-questions)).
- **Document sync**: when a tool reads or edits a file, the client sends `didOpen` with full text, then `didChange` (full sync is enough at this scale). File-watcher changes made by the user or by shell commands are forwarded as `workspace/didChangeWatchedFiles`.
- **Lifecycle**: crash → restart with backoff (max 3), then Failed. Idle shutdown after `idle_shutdown_secs`. Clean `shutdown`/`exit` on quit, and the Job Object / process group catches stragglers.
- **Readiness**: track `$/progress` (rust-analyzer indexing, etc.) and show it in the status bar and `/lsp`. Tools that need a ready server wait up to a timeout, then return "language server still indexing" rather than blocking forever.

## How the model uses LSP

### 1. Automatic diagnostics after edits (the most valuable feature)

After `write`/`edit.*`, if a server covers that file:
1. Send `didChange` / `didSave`.
2. Wait for `publishDiagnostics` to settle (`settle_ms`, capped by `max_wait_ms`).
3. Append **new or changed** diagnostics (compared with before the edit) at or above `min_severity` to the edit tool's result:

```
edit applied: src/session.rs (+4 −1)
diagnostics (2 new):
  src/session.rs:88:17 error[E0308]: mismatched types: expected `u32`, found `usize`
  src/session.rs:91:5  warning: unused variable `plan`
```

Only the delta is reported. Pre-existing errors elsewhere are not repeated on every edit.

### 2. LSP tools (internal names; profiles map them)

| Tool | LSP method | Notes |
|---|---|---|
| `lsp.definition` | `textDocument/definition` | Input is `path` + `symbol` or `line:col`. Output is location + a snippet |
| `lsp.references` | `textDocument/references` | Capped, grouped by file |
| `lsp.hover` | `textDocument/hover` | Type signatures and docs, trimmed |
| `lsp.symbols` | `workspace/symbol` | Fuzzy symbol search across the workspace |
| `lsp.diagnostics` | pull or cached push | For a file or the whole workspace |
| `lsp.rename` | `textDocument/rename` | **Mutating**: the workspace edit becomes a checkpointed multi-file edit with approval |

`symbol`-based positions are resolved through the tree-sitter index ([05](05-context-management.md)), so the model rarely needs exact columns. Weaker models get columns wrong often.

LSP tools are exposed only while a covering server is configured and has been started. Otherwise they are hidden, so they cost nothing.

### 3. Diagnostics in the UI

Status bar `⚠3 ✗1` for the files touched in this session. `/lsp` lists servers, their state and progress, and diagnostics.

## Windows

- File URIs: use `url::Url::from_file_path` / `to_file_path`. Normalize drive-letter case when comparing (`file:///c:/` vs `file:///C:/`), and percent-encode spaces.
- Servers distributed as `.cmd` shims (typescript-language-server via npm) need the same `cmd /C` handling as MCP ([09](09-mcp.md#windows)).
- Some servers emit CRLF in hover text, so strip it.

## Open questions

- Should `lsp.code_action` (quick fixes) be exposed to the model? Proposed: M5, read-only listing first, then applying with approval.
- Pull diagnostics (`textDocument/diagnostic`) vs. push only? Proposed: use pull when the server advertises it, because it gives a definite "done" signal instead of guessing settle times.
