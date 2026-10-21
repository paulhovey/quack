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

Not built yet: `oauth` (M5) and a per-server `defer` override. Prompts and resources need no settings ([below](#prompts-and-resources-2026-10-21)).

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
- **Not yet.** Progress display, OAuth, saving full output to a file, a Windows run of `.cmd` servers. (Sampling and elicitation landed 2026-10-21; see below.)

## Prompts and resources (2026-10-21)

- **Listing.** When a server advertises `prompts` or `resources`, the manager lists them (and resource templates) after its tools, and lists them again on `list_changed`. A server whose prompts or resources fail to list still serves its tools. Nothing is listed for servers that don't advertise the capability.
- **Prompts as commands.** Each prompt is `/<server>:<prompt>`, sent to clients as `Event::McpPrompts` (the same `CommandInfo` as custom commands) whenever the list changes. The TUI puts them in the slash popup (hint `mcp`) and in `/commands`. A skill or custom command with the same name wins. Arguments are positional, with the last argument taking the rest of the line (`/gh:review 12 error handling only`), or named (`/gh:review focus=the tests pr=12`; a value runs to the next pair). Missing required arguments stop the turn with the usage before anything reaches the model. The engine fetches `prompts/get` inside the turn task and sends the messages as the user's message. A single user message reads as typed; other roles are marked `[assistant]`, since everything goes in one user turn. Notes and the plan-mode request wrap it as they wrap typed text.
- **Resources.** When any ready server lists resources, two tools join the MCP tools (after them, so the order stays stable): `list_mcp_resources` (optional `server`; it reads the stored lists and contacts nothing, so it is always allowed) and `read_mcp_resource` (`server`, `uri`; a URI that one server lists, or the only server with resources, picks the server when it's left out). Reads are `Subject::Mcp { tool: "resources/read", read_only: true, auto_approve: true }`: allowed in every mode, including plan mode, unless a rule like `mcp:<server>/resources/read` says otherwise. MCP tool names can't contain `/`, so the rule can't clash with a real tool. Text contents are shown under their URI, binary ones as a note, and the output is cut to `max_output_tokens`.
- **`/mcp`** lists each server's prompts (as commands) and resource URIs.
- **2026-07-28 `input_required`.** Answered since elicitation landed (below).
- **Tests.** The stdio fixture server has a prompt, a resource and a template (`quack-mcp/tests/stdio.rs`); the engine test runs `/fx:review`, a missing argument, and a `read_mcp_resource` call against the in-process HTTP server (`quack-core/tests/mcp.rs`).

## Elicitation and sampling (2026-10-21)

- **Both ways of asking.** Older servers send `elicitation/create` or `sampling/createMessage` mid-call; 2026-07-28 servers answer `tools/call`, `prompts/get` or `resources/read` with `input_required`, and the client retries with the answers. Calls go through rmcp's `RunningService` helpers, which drive those rounds (up to 10) through the same `ClientHandler` methods, so one implementation serves both. The connection is shared as an `Arc<RunningService>`; stopping a server that a request still holds cancels it instead of waiting.
- **The asker.** The manager passes requests to an `mpsc::Sender<ServerAsk>` the engine sets before servers start (`McpManager::set_asker`), with a oneshot for the answer. Only then does quack advertise `elicitation.form` and `sampling`; without one (the manager on its own), servers are told neither, and forms that arrive anyway are declined. URL-mode elicitation isn't offered and is declined.
- **Forms.** The `requestedSchema` becomes `Form`/`Field`s: text, number or integer, boolean (Yes/No), one of an `enum` or titled `oneOf` (with the older `enumNames`), and any of an array's `items.enum` (typed comma-separated). Each field is one `Event::Questions` question, headed by the server id, with the server's message before the first; answers come back on `Op::AnswerQuestions`, so the TUI's `ask_user` dialog shows them and `quack exec` declines them (it stops the questions). Typed answers are converted and checked against the field. A skipped required field, a bad value, or stopping the questions declines the form, with a warning saying why when it wasn't the user's choice; interrupting the turn cancels it. The planned `--allow-elicitation` flag isn't built: a headless run has nobody to fill the form in.
- **Sampling.** `Subject::Mcp { tool: "sampling", read_only: true, auto_approve: false }`, so rules `mcp:<server>/sampling` decide it, `full-auto` allows it, it may run while planning, and otherwise it asks with `ApprovalRequest::Tool` named `mcp:<server>/sampling`, showing the model, the token cap and the last message. Headless runs deny it like any approval. The request goes to the session's current model with no tools: the server's system prompt and text messages, `max_tokens` capped by the model's `max_output`, and its temperature if given. Images, audio and tool use are refused, as are `tools`. Usage and cost go to `/stats` and the session log. The plan's "or `small`" option is not built: there is no small-model setting yet.
- **Timeouts.** Time spent on a form or an approval doesn't count toward `call_timeout_secs`: when the window runs out while the server is waiting on the user, or after it was answered, the call gets a new window.
- **Tests.** Unit tests for schema reading, value conversion, questions and answers. The stdio fixture's `deploy` and `summarize` tools ask the old way (a 1 s `call_timeout_secs` against a 1.5 s answer checks the paused timeout; without an asker the form is declined and sampling refused). The engine test's HTTP server asks the 2026-07-28 way: a form answered through `Op::AnswerQuestions` and a sampling request approved through `Op::ApprovalResponse` and answered by the mock model.

## Open questions

- Should quack also act as an **MCP server** (exposing its tools to other agents)? Proposed: later, low priority.
- Legacy HTTP+SSE transport: support only if rmcp provides it cheaply. Streamable HTTP is the current standard.
- Which of the features deprecated in 2026-07-28 still need client support for older servers? Decide from what the default example servers use.
