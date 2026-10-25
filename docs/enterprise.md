# Rolling quack out in a company

quack has no built-in providers, servers or telemetry. Everything it talks to comes from
TOML files, so a company decides what it can reach by deciding which files it ships and
locking the rest with a system policy. This page covers the parts an administrator needs:
the packages, the system policy, your own defaults bundle, and building from source without
network access.

## Packages

Each release has two flavours of every package. They conflict, so a machine has one:

| | `quack` | `quack-plucked` |
|---|---|---|
| Contents | binary + defaults bundle (provider, profile, MCP and language server files) | binary only |
| Use it when | people pick their own providers | you ship your own bundle, or none |

- **Archives**: `quack[-plucked]-<target>.tar.gz` (Linux, macOS) and `.zip` (Windows), with
  `install.sh` / `install.ps1`. `--archive FILE` installs from a downloaded file with no
  network, and `--prefix` picks the folder.
- **Linux**: `quack[-plucked]_<version>_<amd64|arm64>.deb` and `.rpm`, which install
  `/usr/bin/quack` and, for `quack`, `/usr/share/quack/defaults`. The binaries are static
  (musl) and depend on nothing.
- **macOS and Linux with Homebrew**: each release carries `quack.rb` and `quack-plucked.rb`
  for a tap.
- **SBOM**: `quack.cdx.json` (CycloneDX 1.5) lists every crate in the binary, for every
  target.
- `SHA256SUMS` covers every file.

An MSI for Windows is not built yet; use the zip and `install.ps1`, or your own packaging.

The binary finds its defaults bundle next to itself (`<exe>/../share/quack/defaults`,
`<exe dir>/defaults`), in the system share folders, or where `QUACK_DEFAULTS_DIR` points.
`quack doctor` shows which bundle it found.

## The system policy

The system layer sits under the user and project layers, but its `[policy]` table can lock
any setting so nothing above it changes it: not a user or project file, an environment
variable, a command-line flag, or `/config` in the UI. `quack config paths` prints where it
is:

| OS | System folder |
|---|---|
| Linux | `/etc/quack/` |
| macOS | `/Library/Application Support/quack/` |
| Windows | `%ProgramData%\quack\` |

Deploy `config.toml` there (plus `providers.d/`, `mcp.d/`, `hooks.d/` and so on, if you
ship items) with your usual tooling (MDM, Group Policy file deployment, Ansible). An example
that keeps everything on an internal model server:

```toml
[model]
default = "corp/qwen3.8-coder"

[network]
offline = true                     # nothing but the providers' base URLs and egress_allow

[policy]
locked = ["model.default", "network", "permissions.mode", "sandbox.mode"]
max_permission_mode = "auto-edit"  # caps /permissions, --yolo and every layer
project_config = "trusted-only"    # or "never": ignore repositories' .quack/ entirely

[policy.providers]
allowed = ["corp"]                 # provider ids that may load
hosts = ["llm.corp.example"]       # a provider's base_url must be one of these

[policy.mcp]
allowed = ["github-enterprise", "jira"]
hosts = ["*.corp.example"]
```

- A lock covers everything under it: `locked = ["permissions"]` locks the mode and every
  rule list. A locked key that is unset stays unset.
- `[policy.providers] hosts` holds even when the defaults bundle is installed: a provider
  pointing anywhere else does not load. The same goes for remote MCP servers.
- Hooks in the system layer's `hooks.d/` always run; users cannot turn them off. Use them
  for logging or extra checks on tool calls.
- On Windows, `%ProgramData%` is an environment variable a user can change, so there a
  policy that must hold against the user also needs the folder protected by other means.
  quack has no registry location for policy yet.
- Check the result on a client with `quack doctor` and `quack config show --origin`, which
  shows each setting's layer.

## Your own defaults bundle

The defaults bundle is plain files, so a company bundle is a folder you build and ship:

1. Start from `defaults/` in the source (or the bundle of a release archive).
2. Delete the providers, MCP servers and language servers you don't want, and add your own
   (`providers.d/corp.toml` with `base_url` and `models` is enough).
3. Ship it with `quack-plucked` at `<prefix>/share/quack/defaults`, or point
   `QUACK_DEFAULTS_DIR` at it.

Startup works with any file removed, so a smaller bundle needs no other changes. Teams can
also share skills, commands and server files as **bundles**: a folder with a `bundle.toml`
copied into `bundles/` in the system or user config folder. `/bundles` lists them.

## Building without network access

The recipe below was run with an empty `CARGO_HOME` and `--offline`, so every crate comes
from the vendored folder. It needs the pinned Rust toolchain (`rust-toolchain.toml`, with
your targets) installed on the build machine beforehand.

On a machine with network access:

```sh
git clone https://github.com/paulhovey/quack && cd quack
mkdir -p .cargo
cargo vendor --locked > .cargo/config.toml   # about 600 MB in vendor/
```

Copy the whole folder, `vendor/` and `.cargo/` included, to the build machine. Then, from
inside it (cargo reads `.cargo/config.toml` from the current folder, not the manifest's):

```sh
cargo build --release --locked --offline -p quack-cli \
  --no-default-features --features proto-openai-chat,lang-common,tokenizers-hf
scripts/package.sh x86_64-unknown-linux-gnu target/release/quack   # archives, if wanted
```

- The features keep what an internal OpenAI-compatible server (vLLM, llama.cpp, SGLang)
  needs. Add `proto-anthropic`, `proto-ollama`, `proto-openai-responses`, `acp` (editor
  integration) or `keyring` (OS credential stores) as required. A provider file that names
  a protocol left out of the build fails to load on its own, with a clear error.
- `--locked` and the pinned toolchain make builds repeatable. Set `SOURCE_DATE_EPOCH` for
  reproducible archive timestamps.

## Checking that nothing leaves the network

quack contacts only the providers and MCP servers in its files. The test suite checks this
with a recording proxy (`no_egress_beyond_the_configured_provider`), and on Linux
`scripts/no-egress.sh` runs the startup and turn tests in a network namespace with only
loopback, tracing every `connect()`. To check a deployed setup, run a session with a
network capture and `[network] offline = true`. You should see connections only to your
model server's host and to the remote MCP servers you configured.
