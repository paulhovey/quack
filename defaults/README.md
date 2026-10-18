# Defaults bundle

Default data files that ship **separately** from the binary as the
`cutoffharness-defaults` bundle (docs/plan/14-packaging-and-distribution.md). Nothing
here is compiled in. Delete any file, or the whole bundle, and the binary still starts
cleanly with the empty profile.

The bundle loads as a layer beneath the system layer. Its location is resolved at runtime,
and `CUTOFFHARNESS_DEFAULTS_DIR` overrides it. To run from a checkout:

```sh
CUTOFFHARNESS_DEFAULTS_DIR=defaults cargo run
```

| Directory | Contents | Lands in |
|---|---|---|
| `providers.d/` | Empty: for a company's own providers. `cutoffharness setup` writes users' providers | — |
| `profiles.d/` | Model profiles: `<id>/profile.toml` + prompt templates | M1 (`generic`), M2 (the rest) |
| `mcp.d/` | Example MCP servers, all `enabled = false`: everything, filesystem, time, github | M3 (landed early) |
| `lsp.d/` | Language server files; never auto-installed | M4 |
| `grammars/` | tree-sitter query files (`tags.scm`, …) | M2 |
| `themes/` | Extra themes (dark and light are compiled in) | M3 |
| `schemas/` | JSON Schema for editor completion; regenerate with `cutoffharness config schema --dir defaults/schemas` | — |
| `compat.toml` | Other harnesses' files to detect and offer | M2 |

Every file must name its source if derived from another project, and only permissively
licensed material is shipped (docs/plan/03-model-profiles.md).
