# 01 — Configuration

## Goals

- All settings, providers, profiles, MCP servers and LSP servers are TOML.
- **One file per removable thing.** Deleting a file cleanly removes that capability, and nothing else refers to it implicitly.
- Layered: a system admin can set policy that users can't override, and projects can add settings only after the user trusts them.
- Errors are precise: file, line, key, and the reason. A broken file disables only that item, never the whole application.
- Edits made from the UI (e.g. `/models` saves a default, `/config` changes a setting) preserve the user's comments and formatting.

## Directory layout

The directory is resolved per OS. Environment overrides, useful for testing and portable installs: `QUACK_CONFIG_DIR` (user layer), `QUACK_SYSTEM_DIR` (system layer), `QUACK_DATA_DIR`, `QUACK_CACHE_DIR`, and `QUACK_DEFAULTS_DIR` (`none` disables the defaults bundle). `quack config paths` prints the resolved directories.

| Layer | Linux | macOS | Windows |
|---|---|---|---|
| System (admin) | `/etc/quack/` | `/Library/Application Support/quack/` | `%ProgramData%\quack\` |
| User config | `$XDG_CONFIG_HOME/quack/` (`~/.config/quack/`) | `~/.config/quack/` (XDG honoured; see note) | `%APPDATA%\quack\` |
| User data | `$XDG_DATA_HOME/quack/` | `~/Library/Application Support/quack/` | `%LOCALAPPDATA%\quack\` |
| User cache | `$XDG_CACHE_HOME/quack/` | `~/Library/Caches/quack/` | `%LOCALAPPDATA%\quack\cache\` |
| Project | `<repo>/.quack/` | same | same |

Note: on macOS, terminal tools conventionally use `~/.config`, so we default to it there. `~/Library/Application Support` is used for data only.

Each config layer has the same structure:

```
<layer>/
├── config.toml           # settings
├── providers.d/*.toml    # one provider per file           → 02-providers
├── profiles.d/           # model profiles (toml + prompt md) → 03-model-profiles
├── mcp.d/*.toml          # one MCP server per file         → 09-mcp
├── lsp.d/*.toml          # one LSP server per file         → 10-lsp
├── hooks.d/*.toml        # one hook per file               → 11-security
├── skills/<name>/SKILL.md                                  → 08-instructions
├── commands/*.md         # custom slash commands           → 08-instructions
├── grammars/             # optional extra tree-sitter queries / dynamic grammars
└── AGENTS.md             # global instructions (user layer only)
```

The project layer also accepts `.agents/skills/` and the root `AGENTS.md` (see [08](08-instructions-skills-commands.md)).

## Precedence

From lowest to highest:

1. Compiled defaults (settings values only, never providers/MCP/LSP definitions)
2. System layer
3. User layer
4. Project layer (only once the project is trusted)
5. Environment variables `QUACK_*` (e.g. `QUACK_MODEL`)
6. CLI flags and `-c key=value` overrides

Then policy is applied: any key the system layer locks is reset to the system value, and the attempted override is reported as a warning.

### Merge rules

- **Settings (`config.toml`)** are deep-merged by table. Scalars and arrays from higher layers replace lower ones. An array can be extended instead of replaced with `key.append = [...]`, which keeps the common "add one allowed command" case short.
- **Item directories (`providers.d`, `mcp.d`, …)** are merged by the item's `id`, which defaults to the file stem. A higher layer's item **replaces** a lower one with the same id entirely, with no field-level merge (this keeps behaviour predictable). To disable a lower-layer item without deleting its file, write `<id>.toml` containing only `enabled = false`, or list it under `[providers] disabled = ["id"]`.
- An item whose file fails to parse is shown as **Failed** in the startup panel with its miette diagnostic. Every other item still loads.

## `config.toml` reference (initial)

```toml
#:schema schemas/config.json   # optional; local file, enables taplo completion

[model]
default = "local-llama/qwen3-coder-30b"     # "<provider-id>/<model-id>"
small = "local-llama/qwen3-4b"              # titles, summaries, cheap subtasks (optional)
remember_last = true                        # start with the model last switched to

[ui]
theme = "auto"                 # auto | dark | light | <name in themes/>
show_reasoning = "collapsed"   # hidden | collapsed | expanded
startup_panel = "auto"         # auto (hide when all ready) | always | never

[context]
budget_ratio = 0.85            # fraction of model context window we plan to use
cache_strategy = "auto"        # preserve | aggressive | auto   → 05-context
compaction_threshold = 0.80

[tools]
edit_format = "auto"           # auto (from profile/model) | str_replace | apply_patch | whole_file | hashline
shell = "auto"                 # auto | bash | zsh | pwsh | powershell | cmd | <path>
default_timeout_secs = 120

[permissions]
mode = "ask"                   # ask | auto-edit | full-auto | read-only
allow = ["shell:git status*", "shell:cargo check*", "edit:src/**", "mcp:github/get_*"]
deny  = ["shell:rm -rf /*", "mcp:github/delete_*"]   # kinds: shell, edit, read, mcp (<server>/<tool>)

[memory]
path = "~/notes/quack-memory"   # default: <user data>/memory
project_scope = "data-dir"        # data-dir | repo (store under .quack/memory and commit it)
writes = "auto"                   # auto | ask: approve the model's memory writes
index_max_tokens = 1500           # cap on the note index in the system prompt

[skills]
compat_paths = []                 # more skill folders, e.g. ".claude/skills" once accepted
disabled = []                     # skill names to skip
index_max_tokens = 1500           # cap on the skill list in the system prompt

[sessions]
path = ""                         # default: <user data>/sessions
retention_days = 0                # 0 = keep forever

[network]
offline = false                   # true: block all egress except provider base_urls → 11-security
egress_allow = []                 # extra hosts allowed when offline
proxy = ""                        # default: HTTPS_PROXY / system
ca_bundle = ""                    # extra PEM roots in addition to OS store

[instructions]
files = ["AGENTS.md"]             # others ("CLAUDE.md", …) are detected and offered; see 08
max_bytes = 32768

[telemetry]
otel_endpoint = ""                # empty = disabled (default). Never enabled implicitly.

[policy]                          # only honoured in the SYSTEM layer
locked = ["network.offline", "permissions.deny", "providers.allowed"]
providers.allowed = ["corp-vllm"] # restrict selectable providers by id
providers.hosts = ["llm.corp.example"]  # and by base_url host: nothing else can be contacted
mcp.allowed = ["corp-*"]
mcp.hosts = ["mcp.corp.example"]  # remote MCP servers' url hosts
project_config = "trusted-only"   # trusted-only | never | always
```

## Project trust

Loading a project's `.quack/` can run code: MCP `command`s, LSP `command`s, hooks, and shell permissions in `allow`. So:

- On first launch in a repo that contains `.quack/`, the startup panel shows **"Project config found (N MCP servers, M hooks). Trust?"** with options: *Trust*, *Trust settings only* (no executables), *Ignore*.
- Decisions are stored in `<user data>/trust.toml`, keyed by canonical repo path plus a hash of every executable-bearing file. When any of those files changes, the user is asked again.
- AGENTS.md, skills and commands are text and load without trust. Skills that bundle scripts still go through the normal shell permissions when executed.
- Policy `project_config = "never"` disables the project layer entirely, for locked-down enterprises.

### As implemented (2026-10-18)

- When the project needs a decision, the engine sends `Event::ProjectTrust` with what the folder holds (for example "settings, 2 MCP servers, 1 hook"). The TUI opens the **Trust** modal with *Trust*, *Trust settings only*, *Ignore* and *Decide later*. `/trust` reopens it, and `/trust trusted|settings-only|ignored` decides directly. Outside the TUI, `quack trust` shows the state and `quack trust <decision>` records it.
- A decision reloads the session (`Op::TrustProject`, then the same path as `/reload`). Files that a trusted project adds take effect at once, and the conversation and model are kept.
- The fingerprint is SHA-256 over the relative path and contents of every file in `.quack/`, except `skills/`, `commands/` and `memory/`, which are text. Symlinks are hashed by their target and never followed. If the fingerprint differs from the stored one, the project shows as *changed since you trusted it* and is skipped until the user decides again. An *Ignore* decision stays in force when files change. A record with no `hash`, such as one written by an administrator, trusts any content.
- A `.quack/` that holds only text folders needs no decision.
- `trust.toml` is edited with `toml_edit`, so comments survive, and the file is replaced atomically. There is one record per canonical path.
- The hash is hand-written (about 100 lines, checked against the FIPS 180-4 vectors) rather than a new crate, per the dependency policy. The TLS stack's hash isn't used because builds without TLS must still verify trust.

## Remembered state

`<user data>/state.toml` holds what quack remembers between runs. It is not configuration: it is never a config layer, and deleting it only forgets. Today it holds `last_model`, written whenever the model is switched (`/models`, `/setup`).

At startup the remembered model is tried before `model.default`, but only when the default is a personal one (defaults bundle, system or user layer). A trusted project's `model.default`, `QUACK_MODEL`, `-m` and a policy lock on `model.default` all win over it. If it can't be used (for example, its provider file was removed), a warning says so and startup continues with `model.default`. `model.remember_last = false` turns this off, so only `model.default` is used. `quack doctor` shows which model will be used and why.

## Secrets

Secrets never have to be written into TOML. Supported value forms for any secret field:

```toml
api_key = { env = "CORP_LLM_KEY" }
api_key = { cmd = ["pass", "show", "llm/corp"] }          # stdout, trimmed, cached for session
api_key = { keyring = "corp-vllm" }                       # OS keyring (`<service>/<user>` or `<user>`)
api_key = { file = "~/.secrets/llm-key" }
api_key = "sk-..."                                        # allowed, but warns if file is world-readable / in a git repo
```

`cmd` forms run only from system/user layers, or from trusted project layers. `quack keyring set <name>` stores a key (typed hidden, or piped on stdin) and `quack keyring delete <name>` removes it. A bare name lives under the `quack` service. The stores are Credential Manager on Windows, Keychain on macOS, and the Secret Service elsewhere. The Secret Service is reached through the `secret-tool` program (package `libsecret-tools` on Debian/Ubuntu, `libsecret` on Fedora/Arch) rather than a compiled-in D-Bus stack, which would add about 130 crates; when `secret-tool` is missing the error says which package to install. Keyring support is the `keyring` build feature, on by default; builds without it report keyring secrets as unsupported.

## Tooling

- `quack config check` validates every layer and prints miette diagnostics. Exits non-zero on errors, so it can run in CI or from an enterprise image build.
- `quack config show [--layer] [--origin]` prints the effective config, with the source file of each value.
- `quack config schema [--dir DIR]` writes JSON Schema files (via `schemars`) for config, providers, profiles, MCP, LSP, hooks and `compat.toml` into `<user config>/schemas/` (or `DIR`). Kinds not yet implemented get permissive stubs. Default TOML files carry a **relative** `#:schema ../schemas/<kind>.json` line, so taplo and VS Code offer completion without fetching anything from the network. The defaults bundle ships the schema files too.
- `quack doctor` checks config, provider reachability, tokenizer availability, MCP/LSP binaries on PATH, terminal capabilities, and Windows-specific items (VT support, PowerShell version, long paths).

## Editing from the UI

`/config` lists every setting with its effective value, its origin (default, a layer file, env or CLI) and its schema description. Enter flips a boolean, or puts `/config set <key> <value>` in the input to edit. `/models` saves a default model with Ctrl-S. Both write to the user layer's `config.toml` through `toml_edit`, so comments and layout survive. The edit is validated against the schema before anything is written, and the file is replaced atomically. Keys locked by policy are refused. If a higher layer, an environment variable or a CLI flag still overrides the key, the confirmation says so. `ui.*` changes apply at once; the rest apply on the next start.

## Hot reload

As implemented (2026-10-18): `/reload` (`Op::ReloadConfig`) re-reads every layer with the same command-line options. It waits for a running turn and holds new turns until it finishes. It then stops and restarts the MCP servers, runs startup again (the startup panel shows its progress), and keeps the conversation and the model in use, falling back to the normal model choice if that model has gone. The permission mode carries over, capped by policy. Approvals given for the session are dropped, because the rules they were given under may have changed. The TUI rebuilds its `/config` view from the new settings, and `ui.*` changes apply at once. Still to do: watching the folders with `notify`, and restarting only the MCP servers whose files changed.

`notify` watches all layer directories. On change, a debounced `Op::ReloadConfig`:
- reloads settings and item files,
- adds, removes or restarts only the MCP/LSP servers whose files changed,
- never changes the model mid-turn; changes apply from the next turn.

## Windows notes

- Paths in TOML accept `/` and `\`. `~` expands to `%USERPROFILE%`.
- `%VAR%` and `$VAR`/`${VAR}` expansion are both supported in path and command fields.
- Store files as UTF-8. Accept and strip a BOM (Notepad adds one).

## Open questions

- Should `config.toml` support `include = ["..."]` for splitting large configs? Proposed: no. Item directories already split the config into files.
- Is `profiles.d/` worth a user-facing override layer, or are system and user enough? Proposed: all four layers, for consistency.
