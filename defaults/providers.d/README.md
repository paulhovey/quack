# defaults/providers.d

The providers that work out of the box: servers on this machine on their usual ports,
and hosted services that need only a key. They are plain data. The binary contains no
server address, so deleting a file here removes that provider completely.

| Files | Contacted when |
|---|---|
| `ollama`, `llama-cpp`, `vllm`, `lmstudio`, `sglang` (127.0.0.1) | Listing models, `doctor`, or startup with no default model. Only this machine |
| Open-model services: `openrouter`, `synthetic`, `charm-hyper`, `together`, `fireworks`, `deepinfra`, `novita` | Only once the key is set (the variable in the file, or `quack setup` stores one) |
| Model makers: `mistral`, `kimi-code`, `moonshot`, `zai-coding`, `zai`, `deepseek` | Same |

A hosted service from this folder is never picked by itself, even with its key set. The
user picks a model in `/models` or `quack setup`, since it may cost money.

A file is short:

```toml
name = "OpenRouter"                 # shown by setup
group = "Open-model services"       # heading setup lists it under
base_url = "https://openrouter.ai/api/v1"
api_key = { env = "OPENROUTER_API_KEY" }
```

Only `base_url` is required. Models are discovered from the server's `/models`
endpoint. Every other option has a working default; the full list is in
`../schemas/provider.json` (editors with taplo complete it).

## For a company

- **Remove what you don't want.** Delete files here, or ship the bundle without
  `providers.d`. A user can switch one off with `[providers] disabled = ["openrouter"]`,
  or with a `providers.d/<id>.toml` containing only `enabled = false`.
- **Ship your own.** Add `corp.toml` here, or in the system config folder's `providers.d`.
- **Guarantee nothing else is called.** In the system layer's `config.toml` (`[policy]`
  is only read there, so users cannot override it):

  ```toml
  [policy.providers]
  hosts = ["llm.corp.example"]   # base_url hosts allowed; everything else is disabled
  allowed = ["corp*"]            # optional: provider ids too
  ```

  Any provider whose `base_url` host doesn't match is disabled, including one a user
  writes later under an allowed id. `quack setup` refuses to add one.
  `[policy.mcp] hosts` does the same for remote MCP servers. `quack doctor`
  lists every host the configuration can reach on its `network` line.
