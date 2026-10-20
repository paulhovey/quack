# defaults/profiles.d

Model profiles, one directory each: `profile.toml` plus a `system.md` prompt template
(minijinja), and optionally a `context.md`. `context.md` is the per-session part (workspace,
date, files, AGENTS.md, skills, memory), sent at the start of the first user message so the
system prompt and tools stay the same in every project and the server's prompt cache reuses
them. Without one, everything goes in `system.md`. Delete a directory to remove that profile; with none installed the binary
uses its compiled-in empty profile.

- `generic`: a short original prompt for any model, using the internal tool names.
- `qwen-code`: Qwen Code tool names (`read_file`, `edit`, `run_shell_command`, ...).
- `cc-style`: Claude Code-style tool names (`Read`, `Edit`, `Bash`, ...), for GLM, MiniMax,
  DeepSeek and MiMo.
- `kimi-code`: Kimi CLI tool names (`ReadFile`, `StrReplaceFile`, `Shell`, ...).
- `codex`: Codex tools for gpt-oss: `shell` with an argument list, `apply_patch`,
  `update_plan`.

The family profiles have empty `[match]` lists: they are used only when a model names
them (`profile = "qwen-code"`) until `quack eval` shows they beat `generic` for that
family. Each file's comment has the proposed globs. Every prompt is original text; only
tool names and parameter shapes follow the family's own harness (see each `source`).
