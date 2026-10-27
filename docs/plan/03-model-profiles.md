# 03 — Model Profiles

## Why profiles exist

Open-weight coding models are post-trained (often with RL) inside a specific agent harness. Each one learned a particular system prompt shape, set of tool names, parameter names, edit tool and message layout:

- Qwen (Qwen3-Coder, Qwen3.5/3.6) ↔ Qwen Code (a Gemini-CLI-derived tool set: `read_file`, `edit`, `run_shell_command`, …)
- Kimi (K2 → K3) ↔ Kimi Code / Kimi CLI (`ReadFile`, `StrReplaceFile`, `Shell`, …)
- GLM (4.x → 5.x) ↔ ZCode, Z.ai's Claude-Code-shaped harness (`Read`, `Edit`, `Bash`, …)
- DeepSeek (V4.x) ↔ DeepSeek Harness (`read`, `write`, `edit`, `bash`, `todo_write`, …; bash-first) and, officially, Claude Code through their Anthropic-compatible endpoint
- MiniMax, MiMo ↔ Claude-Code-style tools (`Read`, `Edit`, `Bash`, …)
- Mistral (Large 4, Codestral) ↔ Mistral Vibe (`read_file`, `write_file`, `edit`, `bash`, `grep`, `todo`, …)
- Meta Muse Spark ↔ no open harness (Muse Code is closed); OpenAI-style native tool calls and their prompting guide's structure
- gpt-oss ↔ Codex-style tools (`shell`, `apply_patch`) in the harmony format
- Small or weak models ↔ SWE-agent-style plain-text commands, with no tool schemas at all

These pairings were written in 2026-10 and must be re-checked against each family's current release before a profile is built.

**Scope: current coding models only (decided 2026-10-18).** Profiles, text tool-call parsers and fixtures are built only for the model families programmers are using now for coding. As of 2026-10 that is Qwen, Kimi, GLM, DeepSeek, MiniMax and Xiaomi MiMo, plus gpt-oss. Older generations and other families get `generic` and the text fallbacks, nothing more. When a family ships a new generation, its profile follows the new one; the old generation is not kept working at extra cost. The eval ([13](13-testing-and-evals.md)) decides which families make the list.

A model performs measurably better when its harness looks like the one it was trained in. Open Interpreter puts this into practice with per-model "harnesses" that change the system prompt, tool definitions, message formatting, reasoning configuration and headers ([docs](https://www.openinterpreter.com/docs/terminal/harness#what-each-quack-changes)). Its harnesses are compiled into the binary. **Ours are data files**, in keeping with the "nothing built in" principle: a profile can be added, edited or deleted without a rebuild.

## What a profile controls

| Area | Examples |
|---|---|
| System prompt | Template file, environment block placement, tone, length |
| Tool surface | Which internal tools are exposed, under what names, descriptions, parameter names and schemas |
| Tool result formatting | Line-numbered vs. raw file output, error wording, truncation markers |
| Edit format | `str_replace`, `apply_patch`, `whole_file`, `search_replace_blocks` |
| Message shaping | System role name, tool-result role, alternation rules, synthetic setup exchange |
| Tool-call mode defaults | native / text / grammar, text parser |
| Reasoning defaults | Format, preserve-in-history, effort mapping |
| Sampling defaults | Vendor-recommended temperature/top_p/top_k |
| Extras | Prompt-cache key, title generation, headers |

A profile never contains endpoints or credentials (those belong to providers). Provider/model TOML can override any profile default.

## On-disk format

```
profiles.d/
└── qwen-code/
    ├── profile.toml
    ├── system.md               # minijinja template
    ├── context.md              # optional: per-session context for the first user message
    └── tools/
        ├── read_file.md        # tool description shown to the model
        ├── edit.md
        └── run_shell_command.md
```

A profile is a directory, so deleting the directory removes the profile.

### `profile.toml`

```toml
#:schema ../schemas/profile.json
id = "qwen-code"
description = "Qwen Code–shaped prompt and tools; for Qwen3-Coder family"
source = "Derived from QwenLM/qwen-code (Apache-2.0)"     # REQUIRED provenance field
extends = "base"                                         # optional inheritance (one level)

[match]                                                  # used when model.profile = "auto"
model = ["*qwen3-coder*", "*Qwen3-Coder*"]
priority = 50

[prompt]
system = "system.md"
system_role = "system"            # system | developer | user-prefix (for templates without system role)
env_placement = "synthetic"       # system | first-user | synthetic
# synthetic = insert a fake user/assistant exchange holding date, OS, cwd, folder listing
#             (Qwen Code does this) so the system prompt stays byte-stable.

[messages]
tool_result_role = "tool"         # tool | user (wrap as <tool_result>)
merge_consecutive = true          # merge adjacent same-role messages for strict templates
prompt_cache_key = false          # send session-stable cache key (Kimi / OpenAI-style)

[defaults]
edit_format = "str_replace"
tool_calling = { mode = "native", text_format = "qwen3-xml" }
reasoning = { format = "none", preserve_in_history = false }
sampling = { temperature = 0.7, top_p = 0.8, top_k = 20, repetition_penalty = 1.05 }

# ---- tool surface ----
[[tools]]
name = "read_file"                # name the model sees
maps_to = "read"                  # internal tool (04-agent-loop-and-tools)
description = "tools/read_file.md"
result_format = "plain"           # plain | numbered (cat -n style)
[tools.params]
absolute_path = { to = "path", transform = "abs_to_workspace" }
offset        = { to = "start_line", transform = "zero_to_one_based" }
limit         = { to = "line_count" }

[[tools]]
name = "edit"
maps_to = "edit.str_replace"
description = "tools/edit.md"
[tools.params]
file_path  = { to = "path", transform = "abs_to_workspace" }
old_string = { to = "old" }
new_string = { to = "new" }
expected_replacements = { to = "count" }

[[tools]]
name = "run_shell_command"
maps_to = "shell"
description = "tools/run_shell_command.md"
[tools.params]
command = { to = "command" }
directory = { to = "cwd" }

[[tools]]
name = "index"                    # tools with no trained equivalent can still be added
maps_to = "index"
description = "tools/index.md"

# Tools the profile does not list are NOT exposed, except tools added at runtime
# (MCP, skills) which are appended using the profile's naming rule:
[runtime_tools]
mcp_name = "mcp__{server}__{tool}"
expose_skill_tool = true
```

### Parameter transforms

There is a small, closed set of declarative transforms. Anything more complex requires code (a new transform in `quack-profiles`), which keeps profiles safe to load without trusting them.

| Transform | Effect |
|---|---|
| `abs_to_workspace` / `workspace_to_abs` | Absolute ↔ workspace-relative paths (separator-normalized, Windows-aware) |
| `zero_to_one_based` / `one_to_zero_based` | Line-number base conversion |
| `split_lines` / `join_lines` | String ↔ array of lines |
| `default(<value>)` | Fill in when missing |
| `const(<value>)` | Fixed internal value; parameter hidden from model |
| `rename_enum({..})` | Map enum values |

Parameter schemas shown to the model are generated from the internal tool's schema plus the mapping. A profile can also give a full `schema = { ... }` override, which is validated at load time to cover every required internal parameter.

### Templates

The system prompt and context templates use `minijinja` and these variables:

| Variable | Notes |
|---|---|
| `os`, `shell`, `cwd`, `workspace_root`, `date` | `date` is frozen at session start, to keep the cache stable |
| `instructions` | Merged AGENTS.md content ([08](08-instructions-skills-commands.md)) |
| `skills` | Name + description list ([08](08-instructions-skills-commands.md)) |
| `memory_index` | Memory index content ([07](07-sessions-memory-checkpoints.md)) |
| `tools_text` | Rendered tool docs. Used only in text/grammar tool-calling modes |
| `folder_listing` | Shallow tree of the workspace, capped |
| `git` | Branch and dirty flag *at session start* |
| `model`, `context_window` | |

**Cache stability rule.** Everything that renders into the system prompt and tool list must be byte-identical for the whole session. Volatile data (fresh git status, time, diagnostics) goes into the latest user message, never into the prefix. A test enforces this by rendering twice across simulated turns and diffing ([13](13-testing-and-evals.md)).

## Selection

1. If the model entry has an explicit `profile = "<id>"`, that profile is used.
2. Otherwise, with `profile = "auto"` (the default): take the highest-`priority` profile whose `[match].model` glob matches the model id or alias.
3. Otherwise, use the profile named in `config.toml [model] fallback_profile`, which defaults to `generic`.
4. If no layer has a `generic` profile, use the compiled-in one: the defaults bundle's own `profiles.d/generic/` files, built in with `include_str!` (2026-10-26), so a binary without the bundle (`quack-plucked`) still has a prompt. A `generic` folder in any layer replaces it.
5. If `generic` is turned off (`enabled = false`), use the compiled-in **empty profile**. It has an empty system prompt (only the rendered `{{instructions}}`, if any), exposes the internal tools under their internal names with no added descriptions, uses `native` tool calling, and sets no sampling overrides. Like every profile it contains no endpoints or connections. It exists only so the binary works with zero files installed, and the status bar shows `profile: empty` as a hint to install one.

`/profiles` shows the active profile and lets the user switch it for the session. `quack profile render <id> --model <m>` prints the full rendered prompt and tool schemas, for debugging and token accounting.

## Default profiles (shipped separately, in `defaults/profiles.d/`)

| Profile | Target | Basis / provenance |
|---|---|---|
| `generic` | Unknown models; our own house style | Original. Short prompt, our native tool names |
| `qwen-code` | Qwen3-Coder, Qwen3.5/3.6 | QwenLM/qwen-code (Apache-2.0); verify licence at time of derivation |
| `kimi-code` | Kimi K3 (and K2) | MoonshotAI kimi-cli; verify licence |
| `codex` | gpt-oss | openai/codex prompts and `apply_patch` (Apache-2.0) |
| `cc-style` | MiniMax, MiMo (GLM and DeepSeek moved to their own profiles) | Original prompt text written by us. Only the tool *names and shapes* follow Claude-Code conventions. Claude Code's prompt is not openly licensed, so it is **not** copied |
| `zcode` | GLM-5.x | Original prompt text. Tool names and shapes follow zai-org/ZCode (Apache-2.0), Z.ai's own Claude-Code-shaped harness |
| `dsh` | DeepSeek V4.x | Original prompt text. Tool names and shapes follow deepseek-ai/deepseek-harness (MIT): lowercase, literal-replacement `edit`, `todo_write`, bash-first |
| `vibe` | Mistral Large 4, Codestral | Original prompt text. Tool names and shapes follow mistralai/mistral-vibe (Apache-2.0), Mistral's own CLI coding agent |
| `swe-agent` | Small/weak models (≤14B) | SWE-agent (MIT). Text-only commands, observation loop |

Licensing rule: every profile's `source` field states where the prompt and tool descriptions come from, and the shipped defaults must only derive from permissively licensed material. CI checks that `source` is present.

## Evaluating profiles

Profiles are judged by measurement, not opinion. The eval suite ([13](13-testing-and-evals.md)) can run the same task suite with one model under several profiles. It reports pass rate, tokens per task, tool-call parse failures and edit failures. Default `[match]` priorities are set from those results.

## As implemented (2026-10-18)

- **Shipped**: `generic`, plus `qwen-code`, `cc-style`, `kimi-code`, `codex`, `zcode`, `dsh` and `vibe`. `zcode` (`*glm-5*`) and `dsh` (`*deepseek-v4*`, `deepseek-flash`) are auto-selected for their families since the 2026-10-20 eval; the other family profiles have empty `[match]` lists, so they are used only when chosen: `profile = "<id>"` on a model, `--profile <id>`, or `[model] profile` for every model in the session. Each unchosen profile's `profile.toml` has the proposed globs in a comment, to enable once `quack eval` shows the profile at least matches `generic` for that family. `vibe` came close on 2026-10-21 — one hard pass on mistral-large-4-0: 14/14 like `generic`, steps and cost clearly lower, parse 0 vs 12 — and waits on a `--repeat 3` run ([roadmap](roadmap.md)).
- **Prompts** are original text written for this project; only tool names and parameter shapes follow each family's harness, and each `source` says which. `swe-agent` is not built: it needs a text-only tool mode that does not exist yet.
- **Tool surface**: `[[tools]]` renames tools and parameters and may replace descriptions (inline, or a `.md` file in the profile directory). Internal tools a profile does not list are still offered under their internal names, unless the profile sets `exclusive = true` (the `codex` profile does: shell, apply_patch, update_plan, memory, skill). A mapping onto a tool that is merely off this session (`lsp` without servers, `skill` without skills, another edit format's tool) is skipped without a warning.
- **Parameters**: `transform` is `abs_to_workspace`, `zero_to_one_based`, `one_to_zero_based`, `join_lines` (an array of lines becomes one string; shown to the model as an array) or `argv` (an argument list becomes one command line; `["bash", "-lc", script]` gives the script). `default` fills a parameter the model left out and makes it optional in the schema; `[tools.fixed]` sends internal parameters with a fixed value and hides them.
- **Defaults**: `[defaults] edit_format` picks the edit tool when `[tools] edit_format = "auto"` (the default). The shipped profiles set **no sampling**: a profile covers a whole model family, and in the first eval (qwen3.8-flash on `qwen-code`, 2026-10-19) Qwen's coder-model values (temperature 0.7, top_k 20, ...) cost extra steps and a failed edit compared with the server's defaults. Sampling belongs on a model entry (`models = [{ id = ..., sampling = { ... } }]`), which overrides a profile's. `[defaults] sampling` stays available for user profiles.
- **`context.md`** (2026-10-19): the per-session part of the prompt, which is the workspace root, git branch, date, top-level files, AGENTS.md instructions, skills and memory index. It is rendered once at session start and put at the start of the first user message of every request, in a `<context>` block (`turn::with_session_context`). The history and the session log keep what the user typed. `system.md` keeps only what is the same in every project on a machine: the guidance, the tool names, the OS and the shell. Chat templates render the tool definitions after the system prompt, so a workspace path in the system prompt used to end the server's cached prefix early (about 256 of 1,907 tokens cached on a new session's first request in the 2026-10-19 eval), and the tools were sent at full price every session. Now the system prompt and tools are byte-identical across sessions and projects (`core/tests/prefix.rs`, `system_prompt_and_tools_are_the_same_in_every_workspace`). A profile without `context.md` renders everything in `system.md`, as before; this replaces the proposed `env.md` and `env_placement`.
- **Template variables**: `edit_tool` and `write_tool` give the names the model sees for the session's edit tool and for `write` (the same name under `whole_file`), so a prompt stays right whatever the edit format.
- **`/profiles`**: every installed profile rendered for the current model (system prompt, tool names, edit format), with the one `auto` would pick marked. Enter switches the session (`Op::SetProfile`; the same model, rebuilt like a model switch, and kept through `/reload`), `auto` goes back to each model's own choice, Ctrl-S saves `[model] profile`.
- Not built yet: `system_role`, `[messages]` shaping, `result_format`, `extends`, `quack profile render`.
- **Tests**: `core/tests/profiles.rs` starts every shipped profile, checks that it loads without warnings, names its source, offers its tools with their parameters and renders without leftover template syntax; it also runs a Codex `shell` call given as an argument list. `core/tests/prefix.rs` runs a whole session for each.

## Open questions

- Should profiles allow a Lua/Wasm hook for arbitrary message shaping? Proposed: no. Keep the transform set closed, and add a new transform in code when one is needed.
- How deep should inheritance go? Proposed: one level (`extends`), with tables merged and `[[tools]]` replaced wholesale unless `tools_append = true`.
