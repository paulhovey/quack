# quack

A terminal coding agent for open-weight models: self-hosted (llama.cpp, vLLM, Ollama,
SGLang) or hosted open-model services. Written in Rust with a full-screen ratatui UI.

> **Status: scaffold.** The workspace, config loading, engine protocol and TUI shell exist.
> Turns are answered by a stub until the provider adapters land in M1.
> See [docs/plan/roadmap.md](docs/plan/roadmap.md).

`quack` is a working name, chosen so it is easy to find and replace.

## Principles

- **Nothing built in.** Providers, profiles, MCP servers and LSP servers are TOML files, one
  per item. Defaults ship as a separate bundle; delete a file to remove a capability.
- **No implicit egress.** The binary contacts only endpoints you configure.
- **Linux, macOS and Windows** (x64 + ARM64) from the start.

## Build and run

```sh
cargo run                                   # full-screen TUI
cargo run -- exec "explain src/main.rs"     # headless, one turn
cargo run -- config check                   # validate every config layer
cargo run -- config show --origin           # effective settings and where each came from
cargo run -- config paths                   # resolved directories
QUACK_DEFAULTS_DIR=defaults cargo run   # use the defaults bundle from this checkout
```

## Layout

| Path | What |
|---|---|
| `crates/` | 17 crates; see [docs/plan/00-architecture.md](docs/plan/00-architecture.md) |
| `defaults/` | The separately shipped defaults bundle (providers, profiles, schemas, …) |
| `docs/plan/` | The implementation plan, one document per area |

## License

Apache-2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE).
