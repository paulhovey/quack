# defaults/providers.d

Empty on purpose. A provider is just a server, so nothing ships here: `cutoffharness
setup` (or `/setup` in the TUI) finds servers on this machine, offers the hosted
services, or takes any URL, and writes a small file to your own config folder.

A company bundle can put its own providers here, one per file:

```toml
base_url = "https://llm.corp.example/v1"
api_key = { env = "CORP_LLM_KEY" }
```

Models are discovered from the server's `/models` endpoint. To pin or describe
specific models, add `models = ["model-id", ...]`. Every other option has a working
default; the full list is in `../schemas/provider.json` (editors with taplo complete it).
