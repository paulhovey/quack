# 09 — MCP (Model Context Protocol)

## Goals

- Each MCP server is **one TOML file** in `mcp.d/`, so removing it means deleting the file. No servers are built in.
- MCP servers never slow down startup or the first turn.
- MCP tool definitions don't use up a small context window (lazy/deferred tools).
- Works identically on Windows (command resolution, process cleanup).

## Implementation

Built on [`rmcp`](https://github.com/modelcontextprotocol/rust-sdk), the official Rust SDK, in client mode.

**Spec version.** Target the **2026-07-28** revision first. It made the protocol stateless: no transport sessions, no `initialize` handshake, per-request `_meta` and headers, a rewritten authorization model, and three deprecated core features. Older servers (2025-06-18 / 2025-11-25) stay supported as a fallback. As of 2026-10, rmcp's support for 2026-07-28 was reported as beta; check its release status and conformance results before starting M3, and pin the version. Where this document still says `initialize`, read it as "the version-appropriate discovery step". `quack-mcp` owns a `McpManager` that:

- starts every enabled server concurrently at startup, each with `startup_timeout_secs`, reporting `StartupProgress`,
- performs `initialize`, then `tools/list`, `prompts/list` and `resources/list` (whichever the server advertises),
- registers tools in the tool registry as `mcp:<server>:<tool>`, with model-facing names chosen by the profile's naming rule,
- restarts a crashed stdio server with backoff (max N restarts, then Failed),
- applies `notifications/tools/list_changed` by re-listing tools, and adds new tools at the end of the list for cache stability ([05](05-context-management.md#stable-prefix-layout)).

## Server file schema

A local server needs only `command` (and usually `args`); a remote one needs only `url`. The transport follows from which of the two is set.

```toml
#:schema ../schemas/mcp.json
command = "npx"                        # found on PATH (+PATHEXT on Windows); `~/` in args is the home folder
args = ["-y", "@modelcontextprotocol/server-everything", "stdio"]

# Everything below is optional.
enabled = true
description = "GitHub issues/PRs"      # shown in /mcp and in tool_search
env = { GITHUB_TOKEN = { env = "GITHUB_TOKEN" } }   # secret forms from 01-configuration
cwd = ""                               # default: workspace root
inherit_env = ["PATH", "HOME"]         # default: a minimal set (PATH, HOME, temp, locale, proxy, and what Windows needs)

# url = "https://mcp.corp.internal/github"          # instead of command
# headers = { Authorization = { env = "CORP_MCP_TOKEN", format = "Bearer {}" } }

startup_timeout_secs = 20
call_timeout_secs = 120
max_output_tokens = 8000               # longer results keep their start and end

[tools]
allow = ["search_*", "get_*"]          # globs on tool names; default all
deny  = ["delete_*"]
auto_approve = ["get_*"]               # run without asking; ["*"] for all
```

Not built yet: `oauth` (M5), `[prompts]` and `[resources]` (see below), and a per-server `defer` override.

### Windows

- `command` resolution uses the `which` crate with `PATHEXT`, so `npx` finds `npx.cmd`, and `uvx` finds `uvx.exe`.
- Batch-file (`.cmd`/`.bat`) servers are launched through `cmd /C` with careful argument quoting. This is a known source of bugs, so it gets dedicated tests.
- Each stdio server process goes into a Job Object (kill-on-close), so servers are never orphaned.
- The default `inherit_env` includes `SYSTEMROOT`, `TEMP` and `APPDATA`, without which many Node/Python servers fail on Windows.

## Deferred tools (context budget)

Large MCP servers can carry 20–100 tools and over 10k tokens of schemas, which is a large share of a 32k-context model's window.

- `defer = "auto"`: if the total MCP tool schema tokens exceed `mcp.defer_threshold_tokens` (default 10% of the budget), the tools are **deferred**.
- Deferred tools aren't sent in the tool list. Instead, the system prompt gets one line per server ("github: GitHub issues/PRs — 34 tools, use tool_search"), and the `tool_search` tool is exposed.
- `tool_search(query)` returns matching tool names and short descriptions, and **activates** the chosen tools. Activated tools are appended to the tool list (native mode) or described in the next user message (text mode).
- `defer = "never"` always includes the server's tools, and `defer = "always"` always defers them.

## Results

- Text content goes to the model, trimmed to `max_output_tokens`, with the full content saved like shell output.
- Image content goes to the model if it supports vision. Otherwise it is replaced by a placeholder plus a path.
- Resource links can be read on demand through a `read_mcp_resource` internal tool.
- Errors (`isError: true`) are passed through verbatim, because models handle them fine.

## Client capabilities

| Capability | Plan |
|---|---|
| `roots` | Provide the workspace root(s). Updated on `/cd` |
| `sampling` | **Supported, gated.** The server's LLM request is served by the current model (or `small`), with user approval by default. This is useful offline, since MCP servers can use the local model instead of calling out |
| `elicitation` | Shown as an `ask_user`-style dialog in the TUI. Declined automatically in headless mode unless `--allow-elicitation` is passed |
| Progress notifications | Shown on the tool call row |
| Logging notifications | Written to the quack log file, and visible in `/mcp` |

## Security

- Project-layer `mcp.d` files load only for trusted projects ([01](01-configuration.md#project-trust)).
- Policy `mcp.allowed` (glob on ids) and `network.offline`: in offline mode, `http` transports are allowed only to hosts in `egress_allow`. stdio servers can still access the network themselves, which is documented; see [11](11-security.md) for sandboxing stdio servers.
- MCP tool approvals follow the permission engine. The default is `ask` for anything not marked `auto`.

## UI

`/mcp` lists servers with their state, tool count, token cost and deferred status. Actions: restart, enable/disable for the session, view logs, view tools.

## Default MCP files (shipped separately)

`defaults/mcp.d/` contains **examples only**, and all are disabled (`enabled = false`): `filesystem.example.toml`, `github.example.toml`, `postgres.example.toml`, `playwright.example.toml`. We don't enable any third-party server by default.

## As implemented (2026-10-18)

M3 MCP landed early, with these parts:

- **Crate.** `quack-mcp` uses rmcp `=3.5.1` with only `client`, `transport-async-rw` and the reqwest streamable-HTTP client. Stdio servers are started by our own code (`quack-shell`'s program lookup and process tree: process group on Unix, Job Object on Windows), not rmcp's `transport-child-process`. That keeps out `process-wrap` and the `windows` crates: rmcp adds 5 crates on every OS (rmcp, chrono, num-traits, sse-stream, tokio-stream).
- **Protocol.** rmcp's `Auto` lifecycle: `server/discover` (2026-07-28) first, then the `initialize` handshake at 2025-11-25 for servers that answer "method not found" or don't answer in 10 s. Roots and logging are not offered; both are deprecated in 2026-07-28, and stdio servers start in the workspace root.
- **Startup.** The engine reads `mcp.d` and connects every server in the background before provider startup; the MCP startup row updates as servers connect (`2 of 3 ready · 15 tools · connecting github`). A turn never waits. Each request offers the tools of the servers that are ready at that moment, in the order servers became ready. `quack exec` is the exception: a headless run is one turn, so it waits until the MCP row settles (bounded by each server's `startup_timeout_secs`) before sending the prompt.
- **Tool names.** `mcp__<server>__<tool>`, with other characters replaced by `_` and cut to 64. MCP tools bypass the profile's tool surface.
- **Deferred tools.** When the MCP tool definitions are over a tenth of the context budget (32k when the window is unknown), only `tool_search` is offered. Its description lists the ready servers; a search scores tools by words in the name (×3) and description, returns up to 8, and turns them on for the rest of the session.
- **Permissions.** `Subject::Mcp { server, tool, read_only, auto_approve }`. Rules `mcp:<server>/<tool>` (globs) work in `[permissions] allow/ask/deny`. Plan mode and `read-only` mode refuse tools the server doesn't mark `readOnlyHint`; marked tools still ask unless allowed. `tools.auto_approve` and `full-auto` allow without asking. `tool_search` is always allowed.
- **Results.** Text content verbatim; images and audio replaced by a note (no vision yet); embedded text resources inlined; links shown as `[link: uri name]`; `structuredContent` used when there is no content. `isError` marks the call failed. Output over `max_output_tokens × 4` characters keeps 60 % head and 40 % tail.
- **Failures.** A server that fails to start is shown with the reason and its last stderr line; others are unaffected. A stdio server that exits is restarted after 2, 4 and 8 s, then left failed until `/mcp restart <id>` (or Ctrl-S in `/mcp`). `list_changed` re-lists that server's tools.
- **Offline.** With `network.offline`, `url` servers must be on loopback or match `network.egress_allow`.
- **UI.** `/mcp` lists servers with state, error, command or URL, tools, estimated tool tokens, whether tools are deferred, and the last 12 stderr lines. `quack doctor` has an `mcp` row: files parse, name a transport, and local programs are found on `PATH` (nothing is started).
- **Defaults.** `defaults/mcp.d/` ships `everything`, `filesystem`, `time` and `github`, all `enabled = false`.
- **Tests.** A fixture stdio server (`mcp-fixture-server`, a bin in the MCP crate) covers listing, calls, error results, a crash mid-call with restart, and a missing program. An in-process HTTP server in the engine tests covers offering and calling tools and `tool_search`. Checked live with `@modelcontextprotocol/server-everything` (Node) and `mcp-server-time` (Python) and Qwen3.5-0.8B on llama.cpp: `tool_search` → `echo` → `get_current_time`.
- **Not yet.** Prompts as commands, resources and `read_mcp_resource`, sampling, elicitation, progress display, OAuth, saving full output to a file, a Windows run of `.cmd` servers.

## Open questions

- Should quack also act as an **MCP server** (exposing its tools to other agents)? Proposed: later, low priority.
- Legacy HTTP+SSE transport: support only if rmcp provides it cheaply. Streamable HTTP is the current standard.
- Which of the features deprecated in 2026-07-28 still need client support for older servers? Decide from what the default example servers use.
