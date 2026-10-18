# defaults/providers.d

One provider per file. Delete a file to remove that provider.

A provider file needs only a `base_url`, plus an `api_key` for hosted services:

```toml
base_url = "https://api.example.com/v1"
api_key = { env = "EXAMPLE_API_KEY" }
```

Models are discovered from the server's `/models` endpoint. To pin or describe
specific models, add `models = ["model-id", ...]`. Every other option has a working
default; the full list is in `../schemas/provider.json` (editors with taplo complete it).

When `[model].default` is not set, the first provider that answers is used. Your own
files (user and project layers) are tried before these, and hosted services are skipped
when their key variable is not set.
