# quack — Implementation Plan

A fast, context-efficient coding agent for **self-hosted and open-weight models**: Ollama, llama.cpp, vLLM/SGLang clusters, rented GPUs, and hosted open-model services such as Novita and Fireworks. Written in Rust with a ratatui TUI. Licensed Apache-2.0.

## Guiding principles

1. **Nothing built in.** The binary has no provider, MCP server, LSP server, or model definitions compiled into it. All of them are TOML files on disk. We ship a separate set of *default* TOML files that anyone can delete, replace, or never install. A company can fork, delete `defaults/providers.d/*.toml`, add `internal-vllm.toml`, and ship that.
2. **Protocols are code, endpoints are data.** The binary knows how to *speak* a handful of wire protocols (OpenAI chat, Anthropic messages, …). Each one sits behind a cargo feature, so a minimal enterprise build can compile out everything except the one protocol it uses.
3. **No hidden egress.** No telemetry, no update checks, no auto-downloads (LSP servers, tokenizers, grammars) unless explicitly enabled. An `offline` policy hard-blocks every network path except configured providers.
4. **Context is the scarce resource.** Self-hosted models usually run with 32k–128k context and slow prefill. Every feature is judged by its token cost and its effect on the KV/prefix cache.
5. **Fast first frame.** Draw the UI in under 50 ms, then show what is loading. Never block input on MCP, LSP, or model discovery.
6. **Windows is a first-class platform** from the first milestone, not a port.
7. **Engine and UI are separate.** The agent engine is driven by a typed event protocol. The TUI, headless mode, and ACP are all clients of it.

## Documents

| # | Document | Scope |
|---|---|---|
| 00 | [Architecture](00-architecture.md) | Workspace layout, crates, engine protocol, async model, dependency choices |
| 01 | [Configuration](01-configuration.md) | TOML layering, paths per OS, policy locks, project trust, secrets |
| 02 | [Providers](02-providers.md) | Provider TOML schema, wire adapters, tool-call normalization, reasoning, tokenizers |
| 03 | [Model profiles](03-model-profiles.md) | Per-model-family prompt, tool surface and message shaping, as data files |
| 04 | [Agent loop & tools](04-agent-loop-and-tools.md) | Turn lifecycle, built-in tools, edit formats, shell on every OS |
| 05 | [Context management](05-context-management.md) | Token budget, tree-sitter index, pruning, cache-aware compaction |
| 06 | [TUI](06-tui.md) | Startup panel, layout, input, rendering performance, Windows terminals |
| 07 | [Sessions, memory, checkpoints](07-sessions-memory-checkpoints.md) | JSONL sessions, resume/fork/rewind, markdown memory, file undo |
| 08 | [Instructions, skills, commands](08-instructions-skills-commands.md) | AGENTS.md, Agent Skills, custom slash commands |
| 09 | [MCP](09-mcp.md) | `mcp.d/` TOML, rmcp client, deferred tools, lifecycle |
| 10 | [LSP](10-lsp.md) | `lsp.d/` TOML, client manager, diagnostics feedback, LSP tools |
| 11 | [Security](11-security.md) | Permissions, sandboxing per OS, offline/egress policy, hooks, audit log |
| 12 | [Headless & ACP](12-headless-and-acp.md) | `exec` mode, stream-json output, Agent Client Protocol |
| 13 | [Testing & evals](13-testing-and-evals.md) | Unit/snapshot/fixture tests, invariants, model compatibility matrix, perf budgets |
| 14 | [Packaging & distribution](14-packaging-and-distribution.md) | Defaults bundle, release artifacts, enterprise and air-gapped builds |
| — | [Roadmap](roadmap.md) | Milestones M0–M6 with exit criteria |

## Decisions log

| Date | Decision | Notes |
|---|---|---|
| 2026-10-17 | Rust + ratatui (crossterm backend) | Matches maki and codex |
| 2026-10-17 | License Apache-2.0 | Compatible with codex (Apache-2.0), goose (Apache-2.0), maki (MIT) |
| 2026-10-17 | No built-in provider/MCP/LSP definitions | Defaults ship as separate TOML files |
| 2026-10-17 | Windows supported from M1 | Enterprise requirement |
| 2026-10-17 | ACP = Zed Agent Client Protocol, low priority | Planned for after the core is stable |
| 2026-10-17 | One TOML file per provider / MCP server / LSP server / hook; one directory per profile | Removal = delete one file |
| 2026-10-17 | Working name `quack` | Unique string for an easy rename later |
| 2026-10-17 | Full-screen (alternate screen) TUI | Configuration happens through slash commands that open large modals ([06](06-tui.md#slash-commands--modals)) |
| 2026-10-17 | Fallback profile is empty | Used only when no profiles are installed: empty prompt, no connections, no tools beyond the internal set |
| 2026-10-17 | Accepted compiled-in minimum | Empty fallback profile, dark + light themes, a minimal deny-list floor. Nothing that makes or names an external connection |
| 2026-10-17 | Windows targets: x64 + ARM64, Windows 10 1809+ | 1809 is the ConPTY/VT floor |
| 2026-10-17 | Read AGENTS.md + `.agents/skills` by default; detect other harnesses' files and prompt | See [08](08-instructions-skills-commands.md#other-harnesses-files) |
| 2026-10-17 | Primary test target: hosted open-model services | Local servers covered by CPU-only CI containers; see [roadmap](roadmap.md) |
| 2026-10-17 | First scaffold: full workspace, M1 crates real | See [roadmap](roadmap.md#scaffold-m1-kickoff) |
| 2026-10-17 | Model profiles (prompt + tool surface per model family) are data, not code | Inspired by Open Interpreter's per-model harnesses, which are compiled in |
| 2026-10-17 | Edition 2024; toolchain pinned to 1.99.1; MSRV = pinned minus two releases (1.97) | `rust-toolchain.toml`; CI runs a separate MSRV check |
| 2026-10-17 | Scaffold landed | Workspace compiles on Linux, macOS and Windows targets; see [roadmap](roadmap.md#scaffold-m1-kickoff) |

## Open decisions

Each document ends with its own open questions. The cross-cutting ones are:

- **Product name.** Affects the config directory, binary name and crate prefix.
- **Repository URL.** `Cargo.toml` assumes `github.com/paulhovey/quack`; confirm before CI and the docs reference it publicly.

## Reference projects

- **maki** (MIT): https://github.com/tontinton/maki. Tree-sitter `index` tool, context frugality, tree-sitter-bash permissions, ACP, ratatui UI.
- **codex** (Apache-2.0): https://github.com/openai/codex (`codex-rs/`). Crate split (core/protocol/tui/exec), TOML config, rollout sessions, sandboxing (Linux/macOS/Windows), apply_patch, execpolicy, rmcp client.
- **goose** (Apache-2.0): https://github.com/aaif-goose/goose. Provider abstraction across many backends, MCP-first extensions, ACP support.
- **Open Interpreter**: https://www.openinterpreter.com/docs/terminal/harness. Per-model harness profiles (prompt, tools, message format), the reference for [03](03-model-profiles.md).

Code may be ported from all three with attribution kept in file headers and `NOTICE`.
