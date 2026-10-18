# Defaults bundle

Default data files that ship **separately** from the binary as the
`quack-defaults` bundle (docs/plan/14-packaging-and-distribution.md). Nothing
here is compiled in. Delete any file, or the whole bundle, and the binary still starts
cleanly with the empty profile.

The bundle loads as a layer beneath the system layer. Its location is resolved at runtime,
and `QUACK_DEFAULTS_DIR` overrides it. To run from a checkout:

```sh
QUACK_DEFAULTS_DIR=defaults cargo run
```

| Directory | Contents | Lands in |
|---|---|---|
| `providers.d/` | Servers on this machine (Ollama, llama.cpp, vLLM, LM Studio, SGLang) and hosted services, each unused until it runs or has a key; see its README for restricting them | M1 |
| `profiles.d/` | Model profiles: `<id>/profile.toml` + prompt templates | M1 (`generic`), M2 (the rest) |
| `mcp.d/` | Example MCP servers, all `enabled = false`: everything, filesystem, time, github | M3 (landed early) |
| `lsp.d/` | Language servers for the main languages (Rust, Go, Python, TypeScript/JavaScript, C/C++, C#, Java, Lua, Bash, TOML, YAML), each used only when its program is installed; never auto-installed | M4 (landed early) |
| `grammars/` | tree-sitter query files (`tags.scm`, …) | M2 |
| `themes/` | Extra themes (dark and light are compiled in) | M3 |
| `schemas/` | JSON Schema for editor completion; regenerate with `quack config schema --dir defaults/schemas` | — |
| `compat.toml` | Other harnesses' files to detect and offer | M2 |

Every file must name its source if derived from another project, and only permissively
licensed material is shipped (docs/plan/03-model-profiles.md).
