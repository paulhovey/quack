# 09 — MCP (Model Context Protocol)

## Goals

- Each MCP server is **one TOML file** in `mcp.d/`, so removing it means deleting the file. No servers are built in.
- MCP servers never slow down startup or the first turn.
- MCP tool definitions don't use up a small context window (lazy/deferred tools).
- Works identically on Windows (command resolution, process cleanup).

## Implementation

Built on [`rmcp`](https://github.com/modelcontextprotocol/rust-sdk), the official Rust SDK, in client mode. `cutoffharness-mcp` owns a `McpManager` that:

- starts every enabled server concurrently at startup, each with `startup_timeout_secs`, reporting `StartupProgress`,
- performs `initialize`, then `tools/list`, `prompts/list` and `resources/list` (whichever the server advertises),
- registers tools in the tool registry as `mcp:<server>:<tool>`, with model-facing names chosen by the profile's naming rule,
- restarts a crashed stdio server with backoff (max N restarts, then Failed),
- applies `notifications/tools/list_changed` by re-listing tools, and adds new tools at the end of the list for cache stability ([05](05-context-management.md#stable-prefix-layout)).

## Server file schema

```toml
#:schema ../schemas/mcp.json
id = "github"                          # default: file stem
enabled = true
description = "GitHub issues/PRs"

transport = "stdio"                    # stdio | http
command = "github-mcp-server"          # resolved via PATH (+PATHEXT on Windows)
args = ["stdio"]
cwd = ""                               # default: workspace root
env = { GITHUB_TOKEN = { env = "GITHUB_TOKEN" } }   # secret forms from 01-configuration
inherit_env = ["PATH", "HOME", "USERPROFILE", "SYSTEMROOT", "TEMP", "TMP"]  # minimal by default

# transport = "http"
# url = "https://mcp.corp.internal/github"
# headers = { Authorization = { env = "CORP_MCP_TOKEN", format = "Bearer {}" } }
# oauth = true                         # M5: OAuth 2.1 + PKCE, tokens in OS keyring

startup_timeout_secs = 20
call_timeout_secs = 120
max_output_tokens = 8000               # results above this are truncated, full output saved

[tools]
allow = ["search_*", "get_*", "list_*"]   # glob; default all
deny  = ["delete_*"]
defer = "auto"                         # auto | always | never   (see Deferred tools)
approval = "ask"                       # ask | auto  (per-tool overrides below)
[tools.overrides]
"get_issue" = { approval = "auto" }

[prompts]
expose_as_commands = true              # /mcp:github:<prompt>

[resources]
expose = true                          # @mcp:github:<uri> mentions in input
```

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
| Logging notifications | Written to the cutoffharness log file, and visible in `/mcp` |

## Security

- Project-layer `mcp.d` files load only for trusted projects ([01](01-configuration.md#project-trust)).
- Policy `mcp.allowed` (glob on ids) and `network.offline`: in offline mode, `http` transports are allowed only to hosts in `egress_allow`. stdio servers can still access the network themselves, which is documented; see [11](11-security.md) for sandboxing stdio servers.
- MCP tool approvals follow the permission engine. The default is `ask` for anything not marked `auto`.

## UI

`/mcp` lists servers with their state, tool count, token cost and deferred status. Actions: restart, enable/disable for the session, view logs, view tools.

## Default MCP files (shipped separately)

`defaults/mcp/` contains **examples only**, and all are disabled (`enabled = false`): `filesystem.example.toml`, `github.example.toml`, `postgres.example.toml`, `playwright.example.toml`. We don't enable any third-party server by default.

## Open questions

- Should cutoffharness also act as an **MCP server** (exposing its tools to other agents)? Proposed: later, low priority.
- Legacy HTTP+SSE transport: support only if rmcp provides it cheaply. Streamable HTTP is the current standard.
