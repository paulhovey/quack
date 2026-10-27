// The Config plate's content. It mirrors the README's Configuration section, which in turn
// follows crates/quack-config (settings.rs, provider.rs, mcp.rs, lsp.rs, hooks.rs, agent.rs).
// Text in backticks renders as code.

export const providerFile = `# /etc/quack/providers.d/corp.toml
name = "Corp LLM"
base_url = "https://llm.internal.example:8000/v1"
api_key = { env = "CORP_LLM_KEY" }   # leave out if no key
models = [
  { id = "qwen3-coder-480b", alias = "coder" },
  { id = "qwen3-coder-30b", alias = "fast" },
]`;

export const policyFile = `# /etc/quack/config.toml
[model]
default = "corp/coder"
small = "corp/fast"

[network]
offline = true

[sandbox]
mode = "workspace-write"     # commands: no network

[permissions]
deny = ["shell:curl *", "shell:wget *"]

[policy]
locked = [
  "model.default", "network",
  "sandbox.mode", "permissions.deny",
]
max_permission_mode = "full-auto"   # no yolo
project_config = "trusted-only"

[policy.providers]
allowed = ["corp"]
hosts = ["llm.internal.example"]

[policy.mcp]
hosts = ["*.internal.example"]`;

export const installCmd = `curl -fsSL https://github.com/paulhovey/quack/releases/latest/download/install.sh \\
  | sh -s -- --plucked`;

export const doctorOutput = `$ quack doctor
ok    defaults   /usr/share/quack/defaults
warn  providers  1 of 2 usable
ok      corp     2 declared models
warn    rogue    not allowed by policy
ok    model      corp/coder
ok    agents     explore (on corp/fast), general
ok    sandbox    workspace-write · Landlock + seccomp
info  network    llm.internal.example · policy allows only
                 llm.internal.example, *.internal.example`;

export const guarantees: [string, string][] = [
	['Only what the files name', 'No telemetry, update checks or model downloads. A recording-proxy test and a CI run in a loopback-only network namespace check every build.'],
	['Hosts are a policy, not a hope', 'With `[policy.providers] hosts` set, a provider pointing anywhere else doesn’t load, whoever wrote it: a user, a project or a bundle.'],
	['Locks hold everywhere', 'User files, a repo’s `.quack/`, environment variables, `-c` and `/config` can’t change a locked key. `QUACK_SYSTEM_DIR` is ignored while the real system folder holds a policy.'],
	['Commands stay offline too', 'The model’s shell commands run in the OS sandbox with no network on Linux and macOS. Windows has no sandbox yet, so there, `deny` rules are the only control on commands.']
];

export const layers: [string, string, string][] = [
	['Defaults bundle', '`share/quack/defaults/` beside the binary, or `QUACK_DEFAULTS_DIR`', '`quack-plucked` leaves out hosted services'],
	['System', '`/etc/quack/` · `/Library/Application Support/quack/` · `%ProgramData%\\quack\\`', 'The only layer whose `[policy]` counts'],
	['User', '`~/.config/quack/` · `%APPDATA%\\quack\\`', 'Your own files'],
	['Project', '`.quack/` at the repository root', 'Loads only once you trust the repo'],
	['Environment', '`QUACK_MODEL`', 'Picks the model, like `-m`'],
	['Command line', '`-m`, `--profile`, `--permission-mode`, `-c key=value`', 'This run only']
];

export const folder = `config.toml          settings
providers.d/*.toml   model servers and services
mcp.d/*.toml         MCP servers
lsp.d/*.toml         language servers
hooks.d/*.toml       programs that run on agent events
agents.d/*.toml      sub-agents for the task tool
profiles.d/<id>/     prompts and tool shapes per model family
commands/*.md        your slash commands
skills/<name>/       SKILL.md skills
bundles/<name>/      groups of the above
AGENTS.md            instructions for every project`;

export type Group = { title: string; file: string; rows: [string, string, string][] };

export const reference: Group[] = [
	{
		title: 'Models',
		file: 'config.toml [model]',
		rows: [
			['default', '""', 'Model to start with: `<provider>/<model>` or `<provider>/<alias>`'],
			['small', '""', 'Titles, summaries and the explore sub-agent. Empty: `default`'],
			['planner', '""', 'Writes the plan in plan mode; your usual model carries it out'],
			['remember_last', 'true', 'Start with the model you last switched to'],
			['profile', '""', 'One profile for every model, e.g. `generic` or `qwen-code`']
		]
	},
	{
		title: 'Permissions',
		file: 'config.toml [permissions]',
		rows: [
			['mode', '"ask"', '`read-only`, `ask`, `auto-edit`, `full-auto` (sandbox and deny rules still apply) or `yolo`'],
			['allow', '[]', 'Rules that run without asking: `shell:cargo test*`, `edit:src/**`, `mcp:github/get_*`'],
			['ask', '[]', 'Rules that always ask, even in `full-auto`'],
			['deny', '[]', 'Rules that never run, in any mode. A built-in deny list always applies too']
		]
	},
	{
		title: 'Sandbox',
		file: 'config.toml [sandbox]',
		rows: [
			['mode', '"auto"', '`auto`, `workspace-write` (no network), `workspace-write-net`, `read-only` or `off`. A mode you set fails closed'],
			['writable', '[]', 'More folders commands may write to, e.g. `["~/.cargo"]`']
		]
	},
	{
		title: 'Network',
		file: 'config.toml [network]',
		rows: [
			['offline', 'false', 'Remote MCP servers must be on this machine or in `egress_allow`'],
			['egress_allow', '[]', 'Host globs allowed while offline'],
			['proxy', '""', 'Proxy for model requests. Empty: `HTTPS_PROXY` or the system’s'],
			['ca_bundle', '""', 'PEM file of extra root certificates, for an internal CA']
		]
	},
	{
		title: 'Policy',
		file: 'config.toml [policy], system layer only',
		rows: [
			['locked', '[]', 'Dotted keys no other layer can change; a table locks all of it'],
			['max_permission_mode', 'unset', 'The most permissive mode allowed, `--yolo` included'],
			['project_config', '"trusted-only"', '`trusted-only`, `never` or `always`: whether a repo’s `.quack/` loads'],
			['providers.allowed', '[]', 'Provider id globs that may load. Empty: all'],
			['providers.hosts', '[]', 'Host globs a provider’s `base_url` must match. Empty: any'],
			['mcp.allowed', '[]', 'MCP server id globs that may load'],
			['mcp.hosts', '[]', 'Host globs a remote MCP server’s `url` must match']
		]
	},
	{
		title: 'Context',
		file: 'config.toml [context]',
		rows: [
			['window', 'unset', 'Use at most this many tokens of the model’s window'],
			['budget_ratio', '0.85', 'Share of the window quack plans to use'],
			['compaction_threshold', '0.80', 'Share of the budget at which pruning, then compaction, starts'],
			['cache_strategy', '"auto"', '`auto` or `preserve` keep the prompt cache warm; `aggressive` prunes every request']
		]
	},
	{
		title: 'Tools',
		file: 'config.toml [tools]',
		rows: [
			['edit_format', '"auto"', '`str_replace`, `hashline`, `apply_patch`, `search_replace_blocks` or `whole_file`'],
			['shell', '"auto"', '`bash`, `zsh`, `pwsh`, `powershell`, `cmd`, or a path'],
			['default_timeout_secs', '120', 'When a shell command’s process tree is killed'],
			['read_outline_tokens', '2000', 'Larger source files are read as an outline first. `0`: off']
		]
	},
	{
		title: 'Interface',
		file: 'config.toml [ui]',
		rows: [
			['theme', '"auto"', '`dark` or `light`'],
			['show_reasoning', '"collapsed"', '`collapsed`, `expanded` or `hidden`'],
			['startup_panel', '"auto"', '`auto`, `always` or `never`'],
			['ascii', 'false', 'ASCII instead of box-drawing glyphs'],
			['mouse', 'true', '`false` leaves the mouse to the terminal'],
			['reduced_motion', 'false', 'Static indicators instead of spinners'],
			['terminal_title', 'true', 'Window title shows the duck and the session title. `false`: leave it alone']
		]
	},
	{
		title: 'Instructions, skills, memory, sessions',
		file: 'config.toml',
		rows: [
			['instructions.files', '["AGENTS.md"]', 'Instruction file names, in order of preference'],
			['instructions.other_agents', 'true', 'Read CLAUDE.md, GEMINI.md, `.cursorrules` where there’s no AGENTS.md'],
			['instructions.max_bytes', '32768', 'Cap on each instruction file'],
			['skills.compat_paths', '[]', 'More skill folders, e.g. `.claude/skills`'],
			['skills.disabled', '[]', 'Skill names to skip'],
			['memory.path', '""', 'Where notes live, e.g. a synced folder'],
			['memory.project_scope', '"data-dir"', '`repo` keeps project notes in `.quack/memory`'],
			['memory.writes', '"auto"', '`ask` makes each note ask first'],
			['memory.nudge', 'true', 'Remind the model to save lasting preferences'],
			['sessions.path', '""', 'Where sessions are saved'],
			['defaults.dir', '""', 'The defaults bundle’s location; `"none"` turns it off']
		]
	},
	{
		title: 'A provider',
		file: 'providers.d/<id>.toml',
		rows: [
			['base_url', 'required', 'The API root, e.g. `https://llm.internal.example:8000/v1`'],
			['api', '"openai-chat"', '`openai-chat`, `openai-responses`, `anthropic-messages` or `ollama`'],
			['api_key', 'none', 'A secret: `{ env = … }`, `{ file = … }`, `{ cmd = […] }` or `{ keyring = … }`'],
			['models', '[]', 'Ids or tables. Empty: listed from the server'],
			['headers', '{}', 'Extra headers; values may be secrets'],
			['extra_body', '{}', 'Merged into every request'],
			['tls.ca_file', 'none', 'An extra CA for this server'],
			['tls.client_cert / client_key', 'none', 'Mutual TLS'],
			['http.timeout_idle_secs', '300', 'How long the stream may go quiet'],
			['http.max_retries', '3', 'Retries, honouring `Retry-After`'],
			['http.proxy', '""', 'A proxy for this provider only']
		]
	},
	{
		title: 'A model',
		file: 'models = [{ id = …, … }]',
		rows: [
			['alias', 'none', 'A short name: `corp/coder`'],
			['context_window / max_output', 'from the server', 'Token limits'],
			['profile', '"auto"', 'A profile id, or `auto` to pick by its `[match]` globs'],
			['vision', 'false', 'Offer images to the model'],
			['tool_calling.mode', '"native"', '`native`, `text` or `grammar` (schema-guided decoding)'],
			['tool_calling.text_format', '"auto"', '`hermes`, `qwen3-xml`, `glm`, `json-block`, `deepseek` or `kimi`'],
			['reasoning.effort', '""', 'e.g. `low`, `medium`, `high`'],
			['sampling', 'from the profile', '`temperature`, `top_p`, `top_k`, `min_p`, `repetition_penalty`'],
			['cost', 'from the server', 'Dollars per million tokens, for `/stats`']
		]
	},
	{
		title: 'An MCP server',
		file: 'mcp.d/<id>.toml',
		rows: [
			['command / args', '', 'A local server, over stdio'],
			['url', '', 'A remote streamable HTTP server, instead'],
			['headers / env', '{}', 'Values may be secrets'],
			['sandbox', 'false', 'Run a local server in the OS sandbox'],
			['tools.allow / deny', '[]', 'Tool name globs to offer or hide'],
			['tools.auto_approve', '[]', 'Tool name globs that run without asking']
		]
	},
	{
		title: 'A language server',
		file: 'lsp.d/<id>.toml',
		rows: [
			['command / args', 'required', 'Used only when it’s on `PATH`; nothing is installed'],
			['extensions', 'required', 'File extensions it covers, without the dot'],
			['root_markers', '[]', 'Files that mark a project root'],
			['settings', '{}', 'Answers to `workspace/configuration`'],
			['diagnostics.min_severity', '"warning"', '`error`, `warning`, `info` or `hint`']
		]
	},
	{
		title: 'A hook',
		file: 'hooks.d/<id>.toml',
		rows: [
			['event', 'required', '`session_start`, `user_prompt`, `pre_tool`, `post_tool`, `turn_end`, `session_end`, `pre_compact`'],
			['command', 'required', 'Program and arguments; `{dir}` is the file’s folder, `{python}` the Python on `PATH`'],
			['match', '{}', '`tool` and `pattern` globs for tool events, a `prompt` regex for prompts'],
			['timeout_secs', '10', 'How long it may run'],
			['on_error', '"warn"', '`warn`, `allow` or `deny`']
		]
	},
	{
		title: 'A sub-agent',
		file: 'agents.d/<id>.toml',
		rows: [
			['description', 'required', 'What it’s for; the model reads it to choose'],
			['prompt', '""', 'Added to the system prompt for its run'],
			['tools', 'all', 'Tool names it may use'],
			['read_only', 'false', 'Change nothing; may run in plan mode and in parallel'],
			['model', 'the caller’s', '`small` or `<provider>/<model>`'],
			['max_steps', 'none', 'Model calls before it reports']
		]
	}
];
