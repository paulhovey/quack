# 14 — Packaging & Distribution

## Principle

The **binary** and the **defaults** stay separate on disk (nothing is compiled in), but the
normal install ships both. Most people install `quack` and get everything; organisations
that ship their own files install or update with `quack-plucked`, which never touches the
defaults.

| Part | Contains | Who removes what |
|---|---|---|
| binary | Engine, adapters (per features), grammars (per features), two themes, empty fallback profile, minimal deny-list floor | Recompile with fewer features |
| defaults bundle | `providers.d/`, `profiles.d/`, `mcp.d/` (examples, disabled), `lsp.d/`, `themes/`, `grammars/` queries, `schemas/`, `compat.toml` (other-harness detection list), `tokenizers/` (optional) | Delete files, or install with `--plucked` |

| Release flavour | Contains |
|---|---|
| `quack` (default) | binary + defaults bundle |
| `quack-plucked` (`--plucked`) | binary only; an existing defaults folder is left as it is |

## Where defaults live after install

Defaults are installed into a **read-only shared data directory**, which is loaded as an extra layer *beneath* the system layer:

| OS | Defaults dir |
|---|---|
| Linux (package) | `/usr/share/quack/defaults/` |
| Linux/macOS (tarball / install script) | `<prefix>/share/quack/defaults/` beside `<prefix>/bin/quack` (default prefix `~/.local`) |
| macOS (Homebrew) | `$(brew --prefix)/share/quack/defaults/` |
| Windows (MSI) | `%ProgramFiles%\quack\defaults\` |
| Windows (zip / install script) | `defaults\` beside `quack.exe` (default `%LOCALAPPDATA%\Programs\quack\`) |
| Fallback | `<user data>/defaults/` |
| Override | `QUACK_DEFAULTS_DIR`, or `[defaults] dir = ""` in system config, or `dir = "none"` to disable |

So the full precedence becomes: **compiled settings defaults → defaults bundle → system → user → project → env → CLI**.

Users or companies remove a capability in one of three ways:
1. Delete the file from the defaults directory (or don't install the bundle at all).
2. Override it with `enabled = false` in user/system `providers.d/<id>.toml`.
3. Set `[defaults] dir = "none"` and ship only their own files in the system layer.

`quack defaults list` shows which defaults are active and which are overridden or disabled, and where each comes from.

## First-run experience

When no provider is configured:
- The startup panel shows **"No providers configured"** with the paths that were searched.
- `quack setup` (also reachable from the panel) is an interactive wizard:
  1. Lists provider files found in the defaults dir, and lets the user enable one by copying it to the user layer (`toml_edit`, comments preserved).
  2. Offers **local server detection**, but only when the user explicitly picks it: it probes `localhost:11434` (Ollama), `:8080` (llama.cpp), `:8000` (vLLM), `:1234` (LM Studio), `:30000` (SGLang). These are loopback only.
  3. Asks for the API key, using the secret form of the user's choice (env / keyring / cmd).
  4. Runs model discovery and picks a default model.
- No wizard step needs internet access.
- Goal: from install to a first working turn **in under a minute** on a machine already running a local server. Detection is also offered from the startup panel's "No providers configured" state, so users don't need to know about `quack setup`. Detected servers are written as minimal provider files (just `base_url`, plus `api = "ollama"` for Ollama), following [01](01-configuration.md).

### As implemented (2026-10-18)

- `quack setup` asks one question at a time and reads plain lines, so it can also be scripted through stdin. It offers three choices:
  1. **Look for servers on this machine.** It probes 127.0.0.1 on ports 11434, 8080, 8000, 1234 and 30000, waiting 1.5 s on each. A server counts only when it answers with a model list.
  2. **Use a hosted service** from the configured ones (out of the box, the bundle's open-model services and model makers' APIs and coding plans), grouped by each file's `group`. A service whose key is set is used as is. Otherwise the key goes into the OS credential store and the file is copied to the user layer with `api_key = { keyring = "<id>" }`, or the wizard says which variable to set.
  3. **Add a server by URL.** An optional key is stored in the credential store.

  It then lists the provider's models and saves the chosen one as `model.default`.
- `/setup` in the TUI runs the same local detection and shows one row per server and model. Enter writes the provider file and switches to that model, which becomes `model.default` only if none is set. A server that a configured provider already points at is reused rather than written again.
- Files written are minimal: a comment and `base_url`, plus `api_key` when a key was given. If the id is already taken, a number is added (`server-2`). A server that a configured provider already points at is reused. A provider the system policy would disable is refused.
- The wizard and `/setup` only offer the loaded providers; the binary contains no server addresses ([02](02-providers.md#servers-not-engines-2026-10-18)). Option 1 probes the loopback ones; option 3 takes any URL.
- The "no providers configured" startup message names `/setup` and `quack setup`.
- Code: `quack-core/src/setup.rs` (shared), `quack-cli/src/setup.rs` (wizard), `Op::DetectServers` / `Op::AddProvider`.

### Setup in the TUI (2026-10-20)

Someone who installs `quack-plucked` and runs `quack` should not have to know about `quack setup`. So the TUI now covers all three of the wizard's choices:
- **It opens on its own.** When startup ends with no model ready and no `[model] default` configured, `/setup` opens once, unless a trust prompt is waiting. That offer does not probe anything: "find servers on this machine" waits for Enter, so detection still runs only when asked. Typing `/setup` yourself probes at once, as before.
- **Three tabs:** *This machine* (detection, as before), *Hosted services* (the configured services that take a key, from `Op::ListServices`; with no defaults bundle this tab says so and points to the next one), and *Any server* (by URL, plus how to write a provider file by hand).
- **Keys are typed into a prompt box** that shows them as dots. Entering a key was earlier left to `quack setup` so that keys never went through the TUI. It is allowed now with these limits: the key never enters the input, its history or the transcript. It travels in `Op::SetProviderKey` / `Op::AddServer` as a `SecretText`, whose `Debug` output is redacted. The engine stores it in the OS credential store under the provider id (on `spawn_blocking`, because Linux shells out to `secret-tool`) and writes only `api_key = { keyring = "<id>" }`. If the credential store is unavailable, the error says so, and the service's row explains how to use its variable instead.
- **A URL** is checked for `http(s)://`, and the id comes from the host (`setup::id_from_url`, shared with the wizard). It asks no name question.
- **Then the model:** once the engine replies with `Event::ProviderAdded`, it sends `Event::ModelCatalog` for that provider alone, and the TUI opens `/models` on it. A model picked there is remembered as the last used one, so the next start uses it. A ready hosted service goes straight to `/models`, filtered to its id.
- Tests: `core/tests/setup.rs` (adding a server by URL, listing services, refusals), and the TUI tests `first_run_offers_setup_and_adds_a_server_by_url` (the key is never drawn, never in `Debug`, never in the input) and `setup_is_not_offered_when_a_model_is_ready_and_esc_backs_out_of_a_prompt`.

## Release artifacts

### As implemented (2026-10-18)

Pushing a `v*` tag runs `.github/workflows/release.yml`. It builds each target, packs it with
`scripts/package.sh`, and publishes a GitHub release. cargo-dist was dropped: it has no
notion of two flavours of one binary, and the hand-written workflow is about 60 lines.

| Target | Archives |
|---|---|
| `x86_64`/`aarch64-unknown-linux-musl` (static) | `quack-<target>.tar.gz`, `quack-plucked-<target>.tar.gz` |
| `x86_64`/`aarch64-apple-darwin` | same |
| `x86_64`/`aarch64-pc-windows-msvc` | `quack-<target>.zip`, `quack-plucked-<target>.zip` |
| All | `install.sh`, `install.ps1`, `SHA256SUMS` |

Asset names carry no version, so `releases/latest/download/<name>` always works; the
version is the release tag. Unix archives are a prefix (`bin/quack`,
`share/quack/defaults/`), Windows archives are flat (`quack.exe`, `defaults\`), matching
where the binary looks for its bundle.

The install scripts (`scripts/install.sh` for Linux and macOS, `scripts/install.ps1` for
Windows) download the archive for this machine, check it against `SHA256SUMS`, and install
the binary and the defaults:

- `--plucked` (`-Plucked`): the binary only. An existing defaults folder is left
  alone; a fresh plucked install has none.
- `--prefix`, `--version`, `--archive FILE` (offline installs), `--from-source [DIR]` (build
  a checkout with cargo), `--uninstall` (keeps config, sessions and memory).
- The binary is replaced by renaming, so a running quack is never left with a half-written
  file (Windows renames the running exe out of the way).
- A full install replaces the defaults folder as a whole, so files removed upstream go away
  and files the user deleted come back. Turning a default off for good is done in the user
  layer (`enabled = false`), or by updating with `--plucked`.
- Windows adds the install folder to the user `PATH`; Unix prints a hint if `bin` is not on it.

### Still to do

| Platform | Artifacts |
|---|---|
| Linux | `.deb`, `.rpm`: `quack` (binary + `/usr/share/quack/defaults`) and `quack-plucked`, which conflict with each other |
| macOS | Homebrew tap (`quack`, `quack-plucked`), notarization |
| Windows | **MSI** (per-machine and per-user), signed (Authenticode) |
| All | signatures, **SBOM** (CycloneDX via `cargo-cyclonedx`) |

**No self-updater** is built in. `quack version --check` exists but is **opt-in** and blocked by `offline`. Updates come through package managers or the enterprise's own distribution.

## Enterprise builds

Documented recipe (`docs/enterprise.md`, to be written in M5):

```bash
git clone https://github.com/<org>/quack && cd quack
rm -rf defaults/providers.d/* defaults/mcp.d/*
cp ~/corp/providers/corp-vllm.toml defaults/providers.d/
cargo vendor > .cargo/config.toml           # air-gapped builds
cargo build --release --locked --offline \
  --no-default-features --features proto-openai-chat,lang-common,tokenizers-hf
```

- A system `config.toml` with `[policy]` locks (offline, allowed providers and provider hosts, max permission mode, audit path), distributed through MDM/GPO/Ansible. `[policy.providers] hosts = ["llm.corp.example"]` guarantees no other model server is contacted, even if the bundle's files are left in place.
- Windows: an MSI with transforms (MST) can drop the system config into `%ProgramData%\quack\`. Group Policy is not supported natively; the config file is the policy surface.
- Reproducible builds: `--locked`, pinned toolchain via `rust-toolchain.toml`, `SOURCE_DATE_EPOCH`.

## Licensing hygiene

- `LICENSE` (Apache-2.0) and a `NOTICE` that lists ported code: codex (Apache-2.0) for apply_patch, sandbox and execpolicy pieces; maki (MIT) for whatever we port; goose (Apache-2.0).
- Ported files keep their original copyright header and add ours.
- `cargo-deny` allowlist: Apache-2.0, MIT, BSD-2/3, ISC, Zlib, Unicode-3.0, MPL-2.0 (file-level, review case by case). **No** GPL/AGPL in the dependency graph, so enterprises can redistribute.
- Default profiles: provenance field required, permissive sources only ([03](03-model-profiles.md)).
- Tokenizer files bundled in defaults: each one keeps its model's licence file alongside it. If redistribution isn't allowed, it is not bundled and `quack tokenizer fetch` is documented instead.

## Open questions

- Should we publish to crates.io (`cargo install quack`)? That gives a plucked binary. Defaults
  would then come from `install.sh` or `quack setup --download-defaults` (opt-in, blocked
  offline).
