# 14 — Packaging & Distribution

## Principle

The **binary** and the **defaults** are separate artifacts:

| Artifact | Contains | Who removes what |
|---|---|---|
| `cutoffharness` binary | Engine, adapters (per features), grammars (per features), two themes, empty fallback profile, minimal deny-list floor | Recompile with fewer features |
| `cutoffharness-defaults` bundle | `providers.d/`, `profiles.d/`, `mcp.d/` (examples, disabled), `lsp.d/`, `themes/`, `grammars/` queries, `schemas/`, `compat.toml` (other-harness detection list), `tokenizers/` (optional) | Delete files |

## Where defaults live after install

Defaults are installed into a **read-only shared data directory**, which is loaded as an extra layer *beneath* the system layer:

| OS | Defaults dir |
|---|---|
| Linux (package) | `/usr/share/cutoffharness/defaults/` |
| Linux/macOS (tarball / user install) | `<user data>/defaults/` |
| macOS (Homebrew) | `$(brew --prefix)/share/cutoffharness/defaults/` |
| Windows (MSI) | `%ProgramFiles%\cutoffharness\defaults\` |
| Windows (zip / user install) | `%LOCALAPPDATA%\cutoffharness\defaults\` |
| Override | `CUTOFFHARNESS_DEFAULTS_DIR`, or `[defaults] dir = ""` in system config, or `dir = "none"` to disable |

So the full precedence becomes: **compiled settings defaults → defaults bundle → system → user → project → env → CLI**.

Users or companies remove a capability in one of three ways:
1. Delete the file from the defaults directory (or don't install the bundle at all).
2. Override it with `enabled = false` in user/system `providers.d/<id>.toml`.
3. Set `[defaults] dir = "none"` and ship only their own files in the system layer.

`cutoffharness defaults list` shows which defaults are active and which are overridden or disabled, and where each comes from.

## First-run experience

When no provider is configured:
- The startup panel shows **"No providers configured"** with the paths that were searched.
- `cutoffharness setup` (also reachable from the panel) is an interactive wizard:
  1. Lists provider files found in the defaults dir, and lets the user enable one by copying it to the user layer (`toml_edit`, comments preserved).
  2. Offers **local server detection**, but only when the user explicitly picks it: it probes `localhost:11434` (Ollama), `:8080` (llama.cpp), `:8000` (vLLM), `:1234` (LM Studio), `:30000` (SGLang). These are loopback only.
  3. Asks for the API key, using the secret form of the user's choice (env / keyring / cmd).
  4. Runs model discovery and picks a default model.
- No wizard step needs internet access.
- Goal: from install to a first working turn **in under a minute** on a machine already running a local server. Detection is also offered from the startup panel's "No providers configured" state, so users don't need to know about `cutoffharness setup`. Detected servers are written as minimal provider files (just `base_url`, plus `api = "ollama"` for Ollama), following [01](01-configuration.md).

### As implemented (2026-10-18)

- `cutoffharness setup` asks one question at a time and reads plain lines, so it can also be scripted through stdin. It offers three choices:
  1. **Look for servers on this machine.** It probes 127.0.0.1 on ports 11434, 8080, 8000, 1234 and 30000, waiting 1.5 s on each. A server counts only when it answers with a model list.
  2. **Use a hosted service** from the wizard's list, grouped into open-model services (OpenRouter, Synthetic, Charm Hyper, Together, Fireworks, DeepInfra, Novita) and the model makers' APIs and coding plans (Mistral, Kimi Code, Moonshot, Z.ai Coding Plan, Z.ai, DeepSeek). If the service's variable is set, the file reads the key from it. Otherwise the key goes into the OS credential store (`api_key = { keyring = "<id>" }`), or the file names the variable and the wizard says how to set it. A service that a configured provider already points at is reused.
  3. **Add a server by URL.** An optional key is stored in the credential store.

  It then lists the provider's models and saves the chosen one as `model.default`.
- `/setup` in the TUI runs the same local detection and shows one row per server and model. Enter writes the provider file and switches to that model, which becomes `model.default` only if none is set. A server that a configured provider already points at is reused rather than written again. Hosted services and other addresses are pointed to `cutoffharness setup`, so API keys are never typed into the TUI.
- Files written are minimal: a comment, `base_url`, then `api = "ollama"` for Ollama or `api_key` for a hosted service. The comment for a local engine says what it needs for tool calls (`--jinja`, `--tool-call-parser`); the wizard and `/setup` show the same note. If the id is already taken, a number is added (`ollama-2`).
- The defaults bundle ships no provider files (since 2026-10-18; see [02](02-providers.md#servers-not-engines-2026-10-18)). Engine ports and hosted services live in the setup code instead.
- The "no providers configured" startup message names `/setup` and `cutoffharness setup`.
- Code: `cutoffharness-core/src/setup.rs` (shared), `cutoffharness-cli/src/setup.rs` (wizard), `Op::DetectServers` / `Op::AddProvider`.

## Release artifacts

Built with **cargo-dist** (`dist`):

| Platform | Artifacts |
|---|---|
| Linux x86_64 / aarch64 | static **musl** tarball, `.deb`, `.rpm` |
| macOS x86_64 / aarch64 | tarball, Homebrew tap, notarized |
| Windows x86_64 / aarch64 | zip, **MSI** (per-machine and per-user), signed (Authenticode) |
| All | `cutoffharness-defaults-<ver>.{tar.gz,zip}`, `SHA256SUMS`, signatures, **SBOM** (CycloneDX via `cargo-cyclonedx`) |

Shell and PowerShell installer scripts install the binary, and the defaults *only if asked* (`--with-defaults`).

**No self-updater** is built in. `cutoffharness version --check` exists but is **opt-in** and blocked by `offline`. Updates come through package managers or the enterprise's own distribution.

## Enterprise builds

Documented recipe (`docs/enterprise.md`, to be written in M5):

```bash
git clone https://github.com/<org>/cutoffharness && cd cutoffharness
rm -rf defaults/providers.d/* defaults/mcp.d/*
cp ~/corp/providers/corp-vllm.toml defaults/providers.d/
cargo vendor > .cargo/config.toml           # air-gapped builds
cargo build --release --locked --offline \
  --no-default-features --features proto-openai-chat,lang-common,sandbox,tokenizers-hf
```

- A system `config.toml` with `[policy]` locks (offline, allowed providers, max permission mode, audit path), distributed through MDM/GPO/Ansible.
- Windows: an MSI with transforms (MST) can drop the system config into `%ProgramData%\cutoffharness\`. Group Policy is not supported natively; the config file is the policy surface.
- Reproducible builds: `--locked`, pinned toolchain via `rust-toolchain.toml`, `SOURCE_DATE_EPOCH`.

## Licensing hygiene

- `LICENSE` (Apache-2.0) and a `NOTICE` that lists ported code: codex (Apache-2.0) for apply_patch, sandbox and execpolicy pieces; maki (MIT) for whatever we port; goose (Apache-2.0).
- Ported files keep their original copyright header and add ours.
- `cargo-deny` allowlist: Apache-2.0, MIT, BSD-2/3, ISC, Zlib, Unicode-3.0, MPL-2.0 (file-level, review case by case). **No** GPL/AGPL in the dependency graph, so enterprises can redistribute.
- Default profiles: provenance field required, permissive sources only ([03](03-model-profiles.md)).
- Tokenizer files bundled in defaults: each one keeps its model's licence file alongside it. If redistribution isn't allowed, it is not bundled and `cutoffharness tokenizer fetch` is documented instead.

## Open questions

- Should we publish to crates.io (`cargo install cutoffharness`)? Proposed: yes for the binary. Defaults then come from the separate bundle or `cutoffharness setup --download-defaults`, which is opt-in and blocked offline.
- Package-manager defaults packages: `cutoffharness` and `cutoffharness-defaults` as separate `.deb`/`.rpm`/brew formulae? Proposed: yes. This mirrors the separation exactly.
