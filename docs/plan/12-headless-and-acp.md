# 12 — Headless Mode & ACP

Both are clients of the engine protocol ([00](00-architecture.md#engine-protocol)), so the agent loop is shared.

## Headless (`quack exec`)

For CI, scripts, git hooks, and wrapping by other tools.

```
quack exec "fix the failing test in crates/core" \
  --model corp-vllm/qwen3-coder \
  --permission-mode auto-edit \
  --allow "shell:cargo test*" \
  --output-format stream-json \
  --max-steps 50 \
  --resume <id> | --continue
echo "summarize this diff" | git diff | quack exec -    # prompt + stdin
```

Aliases: `quack -p "<prompt>"` / `--print`.

### Output formats

| Format | Content |
|---|---|
| `text` (default) | Final assistant message only, to stdout. Progress goes to stderr if `-v` |
| `json` | One object at the end: final message, session id, usage, tool calls summary, exit reason |
| `stream-json` | NDJSON of engine events (a stable, documented subset). The schema is versioned |

We'll evaluate making `stream-json` compatible with Claude Code's stream-json format, as maki does, so existing wrappers work unchanged. Check its licence and spec stability first. If not, publish our own JSON Schema.

### Behaviour

- Approvals: anything that would `ask` is **denied** in headless mode unless `--permission-mode` or `--allow` covers it. The denial reason goes to the model, so it can adapt.
- `ask_user` and MCP elicitation are unavailable unless `--allow-elicitation` is given, in which case they read from the TTY if one is present.
- Exit codes: `0` success, `1` agent error / max steps, `2` config error, `3` provider unreachable, `4` denied by policy, `130` interrupted.
- Sessions are saved like interactive ones (`--no-session` to skip).
- Startup output appears only with `-v`, as structured lines.

## ACP (Zed Agent Client Protocol) — low priority, M6

[ACP](https://agentclientprotocol.com) lets editors (Zed, and others adopting it) drive an agent over stdio JSON-RPC. The editor owns the UI, and the agent streams updates, tool calls and permission requests.

### Plan

- Crate `quack-acp`, cargo feature `acp`, built on the `agent-client-protocol` Rust crate.
- `quack acp` runs as an ACP agent on stdio.
- Mapping:

| ACP | Engine |
|---|---|
| `initialize` | Capabilities: load session, prompt content types (text, image, resource links), MCP passthrough |
| `session/new` (with cwd, MCP servers from the editor) | Start session; editor-supplied MCP servers merged with `mcp.d/` |
| `session/load` | `Op::ResumeSession` |
| `session/prompt` | `Op::UserTurn` |
| `session/cancel` | `Op::Interrupt` |
| `session/update` notifications | `AssistantDelta`, `ReasoningDelta`, `ToolCallBegin/End`, plans (todo) |
| `session/request_permission` | `ApprovalRequest` ↔ `ApprovalResponse` |
| Client `fs/read_text_file`, `fs/write_text_file` | Optional: route file I/O through the editor so unsaved buffers are seen. Behind a setting |
| Client terminal methods | Optional: run `shell` in the editor's terminal |

- Model and profile selection is exposed through ACP session modes/config options where the spec allows. Otherwise it comes from config.
- Test with Zed, plus the reference client from the ACP repo.

maki (`maki-acp`) and goose both implement ACP and are the main references.

## Open questions

- Should a **local socket server** (daemon) be offered so the TUI can attach to a headless-started session? This is deferred; the protocol allows it.
- Should we also support the **A2A** protocol for agent-to-agent use? Proposed: not planned.
