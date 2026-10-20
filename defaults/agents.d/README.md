# agents.d

Sub-agents the model can start with the `task` tool. Each runs with a fresh context and
its own todo list, and only its final report goes back to the model that started it. One
file per agent; the file stem is its id.

```toml
description = "What it is for (the model reads this to choose one)"
prompt = "Added after the system prompt for the agent's run."
```

Optional fields, all described in `../schemas/agent.json`:

| Field | Default | Meaning |
|---|---|---|
| `tools` | every tool of the session | Internal tool names (`read`, `grep`, `shell`, `edit`, ...); `mcp` adds MCP tools. `task` and `ask_user` are never given |
| `read_only` | `false` | Edits refused and only reading commands run, as in plan mode. Read-only agents run in plan mode, and several started in one reply run in parallel |
| `model` | the model running the turn | `small` for `[model] small`, or `<provider>/<model>` |
| `max_steps` | 50 | Model calls before it stops and reports |

Shipped: `explore` (read-only search, on the `small` model when set) and `general` (any
self-contained change). Disable one with a file holding `enabled = false` in your user
config's `agents.d/`; with no agents at all, the `task` tool is not offered.
