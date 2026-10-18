# 08 — Instructions, Skills, Custom Commands

## AGENTS.md

[AGENTS.md](https://agents.md) is the cross-harness standard for project instructions.

### Discovery

1. **Global**: `<user config>/AGENTS.md`, plus the system layer `AGENTS.md` (enterprise-wide guidance).
2. **Project chain**: walk from the workspace root (git root, or cwd if not in a repo) **down to the cwd**, loading every instruction file found along the way. Files closer to the cwd come later, so they take precedence.
3. **Nested, on demand**: when the agent first reads or edits a file in a subdirectory that has its own instruction file (not already loaded), that file is attached to the tool result as `[instructions for path/: …]`. This keeps monorepo instructions out of context until they are relevant.

### File names

`[instructions] files = ["AGENTS.md"]` by default. Other harnesses' instruction files are **not** loaded silently. They are detected and offered to the user (see [Other harnesses' files](#other-harnesses-files)), and accepting one adds it to this list. The first name in the list that exists in a directory wins for that directory, which prevents loading duplicate content when a repo has both `AGENTS.md` and a `CLAUDE.md` symlink to it.

### Rules

- Each file is capped (`max_bytes`, default 32 KiB). Truncation is shown in the startup panel.
- `@path/to/file.md` import lines are supported, one level deep, workspace-relative, with cycle detection. This matches the convention several harnesses use.
- Rendered through `{{instructions}}` in the profile system prompt. Loaded once at session start for cache stability. If the files change mid-session, `/reload` applies the change and the UI notes that the cache will be invalidated.
- Imports resolve relative to the importing file and must stay inside the workspace (or inside the config directory, for global files). Otherwise a cloned repository could import `~/.ssh/id_rsa` into the prompt. A skipped import leaves a note in its place. An `@word` that isn't a file is left as text.
- Nested files are attached to the first successful tool result whose path is in or below their directory: `read`, `write`, `edit`, and also `glob`/`grep`/`list_dir` on that directory. After a resume, files already attached in the history are not attached again.
- `/init` generates an `AGENTS.md` for the current repo through a normal agent turn with a built-in prompt template (overridable as `commands/init.md`).

## Skills

### Format

This follows the open **Agent Skills** format (SKILL.md with YAML frontmatter), so skills written for other harnesses work unchanged.

```
skills/
└── db-migrations/
    ├── SKILL.md
    ├── scripts/new_migration.sh
    └── reference/conventions.md
```

```markdown
---
name: db-migrations
description: Create and review SQL migrations for this repo's Postgres schema. Use when adding tables, columns, or indexes.
---

# Steps
1. Run `scripts/new_migration.sh <name>` ...
```

### Discovery paths (low → high precedence; same name → higher wins)

1. System layer: `<system>/skills/`
2. User: `<user config>/skills/`, `~/.agents/skills/`
3. Project: `<repo>/.quack/skills/`, `<repo>/.agents/skills/`
4. Compatibility paths (`[skills] compat_paths`): empty by default. Paths are added when the user accepts a detection prompt (below), or manually.

`[skills] disabled = ["name", …]` turns individual skills off without deleting them.

### Progressive disclosure

- Only `name` + `description` go into the system prompt via `{{skills}}`, capped by `skills.index_max_tokens`. If a cap applies, skill descriptions are truncated before skills are dropped.
- The model calls the `skill` tool with a name to load the full SKILL.md body. Referenced files (`reference/*.md`, scripts) are read with normal tools, and paths are given relative to the skill directory.
- Users can invoke a skill directly with `/<skill-name> [args]`, which injects its body with the arguments.
- Scripts bundled in a skill run through the normal `shell` tool and permission engine. Skills get no extra privileges.

### Validation

At load time, frontmatter is validated (`name` matches the directory, `description` is present and ≤ 1,024 chars). Invalid skills show as Failed in the startup panel, and the others still load.

### As implemented (2026-10-18, ahead of M3)

- **Where skills are found:** system `skills/`, user `skills/`, `~/.agents/skills`, the project's `.quack/skills` (only when the project is trusted), the repository's `.agents/skills`, then `[skills] compat_paths`. A later skill with the same name replaces an earlier one.
- **Front matter** is parsed without a YAML library. It reads `key: value`, quoted values and `>`/`|` blocks, and skips nested maps.
- **Invalid skills** show as warnings and a NeedsAttention/Failed startup row. Valid ones still load.
- **Prompt:** `{{ skills }}` holds one line per skill, capped by `index_max_tokens`. Descriptions are halved, down to 80 characters, before skills are dropped.
- **`skill` tool:** returns the body in a `<skill name=… directory=…>` block. It is hidden when there are no skills. Loading a skill is always allowed (its own permission subject), so skills in the user's folder don't prompt.
- **`/<name> [request]`** works in the TUI (also in the slash popup) and in `exec`. The engine replaces it with the skill block plus the request. The session title keeps what the user typed.
- **`/skills`** lists them, with where each came from.
- **Not done yet:** the detection prompt for other harnesses' folders, and custom commands.

## Other harnesses' files

By default only the open standards are loaded: `AGENTS.md` and `.agents/skills/` (project and `~/.agents/skills/`). Files belonging to other harnesses are **detected, never loaded silently**.

### Detected locations

| Kind | Project | User |
|---|---|---|
| Instructions | `CLAUDE.md`, `GEMINI.md`, `.cursorrules`, `.cursor/rules/`, `.github/copilot-instructions.md`, `CONVENTIONS.md`, `.windsurfrules` | `~/.claude/CLAUDE.md`, `~/.gemini/GEMINI.md` |
| Skills | `.claude/skills/`, `.codex/skills/`, `.goose/skills/`, `.maki/skills/` | `~/.claude/skills/`, `~/.codex/skills/`, … |
| Commands | `.claude/commands/` | `~/.claude/commands/` |
| MCP configs (info only) | `.mcp.json`, `.cursor/mcp.json`, `.vscode/mcp.json` | `~/.claude.json`, … |

The detection list is data (`defaults/compat.toml`), so it can be extended or emptied. Detection is just a few `stat` calls run during the async startup phase, so it adds nothing to the first frame.

### Prompt

When something new is detected, a startup panel row appears and stays until the user answers. It never blocks input.

```
 ? compat        found CLAUDE.md, .claude/skills (4 skills) · [L]oad · [I]gnore · [D]etails
```

- **Load** adds the paths to `[instructions] files` / `[skills] compat_paths`. Scope is chosen like any modal write (*project* by default for project files, *user* for user-level files).
- **Ignore** records the decision, so the prompt doesn't come back for those paths until new ones appear.
- **Details** opens a modal listing each file with its size and token cost, so items can be loaded or ignored individually.
- Decisions are stored next to trust records (`<user data>/trust.toml`), keyed by path.
- **Duplicate guard**: if a detected file has the same content as, or is a symlink to, an already-loaded `AGENTS.md`, it is marked "same as AGENTS.md" and never offered.
- **MCP config files** are reported only ("found .mcp.json with 3 servers"), with an action to **convert** them into `mcp.d/*.toml` files that the user reviews. They are never executed in place.
- Headless mode never prompts. It loads only what config says, and `-v` lists the detected-but-ignored files.

## Custom slash commands

Markdown prompt templates, the same mechanism as several harnesses:

```
commands/
├── review.md          → /review
└── test/
    └── flaky.md       → /test:flaky
```

```markdown
---
description: Review the staged diff for bugs
args: "[focus]"
model: ""               # optional override
allowed_tools: ["read", "grep", "shell:git diff*"]   # optional narrowing
---

Review the output of `git diff --staged`. Focus: {{args | default("correctness")}}.
```

- Paths are the same layers as skills (system/user/project).
- MCP prompts appear as `/mcp:<server>:<prompt>` ([09](09-mcp.md)).
- Built-in commands (`/models`, `/resume`, `/compact`, …) are code. A custom command with the same name is rejected, with a warning naming the file.

## Bundles

A bundle is a folder that groups skills, custom commands, `mcp.d`, `hooks.d` and `profiles.d` files under one name, so a team can ship them together:

```
corp-tools/
  bundle.toml        # name, version, description; nothing else required
  skills/  commands/  mcp.d/  hooks.d/  profiles.d/
```

- Installed by copying the folder into `<layer>/bundles/`; removed by deleting it. `/bundles` lists them and turns them on or off. There is no marketplace and no download command, which keeps the no-egress promise; companies distribute bundles the way they distribute other files.
- Bundle contents load as if they were in the layer that holds the bundle, so trust rules apply unchanged: a bundle in an untrusted project does nothing until the project is trusted.
- Name clashes: a file outside any bundle wins over a bundle's, and two bundles with the same item is a warning that names both.
- Planned for M3, with skills and hooks.

## Open questions

- Should skills declare `allowed-tools` that narrow permissions while they are active? Proposed: honour it if present (it is in the spec as experimental), and never let it *widen* permissions.
- When skills loaded from compat paths have unknown frontmatter keys, should we warn? Proposed: no. Ignore them silently for compatibility.
