# 02 — Providers

## Goals

- A provider is **one TOML file** in `providers.d/`. Deleting the file removes the provider.
- The binary contains **wire-protocol adapters only**, each behind a cargo feature. It contains no endpoints, model lists or keys.
- Tool calling and reasoning work reliably across the quirks of open-weight serving stacks (llama.cpp, vLLM, SGLang, Ollama, TGI, hosted gateways).
- Exact or close-to-exact token counts are available for context budgeting, without network access.

## Concepts

| Term | Meaning | Lives in |
|---|---|---|
| **Adapter** | Code that speaks one wire protocol (request building, SSE parsing, errors) | Binary, cargo feature |
| **Provider** | An endpoint: base URL, auth, headers, TLS, models it serves | `providers.d/<id>.toml` |
| **Model** | A model on a provider, with its capabilities and parameters | `[[models]]` inside the provider file |
| **Profile** | The prompt, tool surface and message shaping used with a model family | `profiles.d/` → [03](03-model-profiles.md) |

## Adapters

| Feature | Adapter `api =` | Covers |
|---|---|---|
| (always built) | `openai-chat` | vLLM, SGLang, llama.cpp `llama-server`, LM Studio, TGI, Novita, Fireworks, Together, DeepInfra, OpenRouter, LiteLLM gateways, Ollama `/v1` |
| `proto-openai-responses` | `openai-responses` | OpenAI Responses API and compatible gateways (vLLM has partial support) |
| `proto-anthropic` | `anthropic-messages` | Anthropic-compatible endpoints (several open-model hosts, e.g. Z.AI and Moonshot, expose one) |
| `proto-ollama` | `ollama` | Ollama native `/api/chat`, which allows per-request `num_ctx` and `keep_alive` |

`openai-chat` is the baseline and is always compiled in: every build needs at least one adapter, and it covers nearly every server. The `proto-openai-chat` feature remains as a no-op for compatibility. The other adapters are the optional ones.

Why a native Ollama adapter: Ollama's default context length is small, and the OpenAI-compatible endpoint does not reliably let us set `num_ctx` per request. Long sessions can then be **silently truncated**. The native adapter always sends `options.num_ctx` equal to the model's configured `context_window`. (Verify current Ollama behaviour when implementing.)

Adapter trait (sketch):

```rust
#[async_trait]
pub trait Adapter: Send + Sync {
    fn id(&self) -> &'static str;                         // "openai-chat"
    async fn stream(&self, req: ChatRequest, cancel: CancellationToken)
        -> Result<BoxStream<'static, Result<StreamEvent, ProviderError>>>;
    async fn list_models(&self) -> Result<Vec<DiscoveredModel>> { Ok(vec![]) }
    async fn tokenize(&self, _text: &str) -> Option<Result<Vec<u32>>> { None } // llama.cpp /tokenize
}

pub enum StreamEvent {
    TextDelta(String),
    ReasoningDelta(String),
    ToolCallStart { index: u32, id: Option<String>, name: String },
    ToolCallArgsDelta { index: u32, json_fragment: String },
    ToolCallEnd { index: u32 },
    Usage(Usage),                 // prompt, completion, cached_prompt tokens when reported
    Finish(FinishReason),
}
```

`ChatRequest` is our canonical, provider-neutral message list, tool list and sampling parameters. The profile layer ([03](03-model-profiles.md)) shapes it *before* the adapter serializes it.

## anthropic-messages and openai-responses (2026-10-18)

- **`anthropic-messages`** (`proto-anthropic`): `POST <base>/v1/messages`, or `<base>/messages` when `base_url` already ends in `/v1`. With no `auth_header`, the key goes in `x-api-key` and `anthropic-version: 2023-06-01` is sent. The history is folded into alternating turns: tool results are `tool_result` blocks in the next user turn, a system message after the first becomes user text, and the conversation always opens with the user. Cache breakpoints sit on the system prompt, the newest block, and the end of the user turn before the last reply (where the previous request put its own, so a step that adds many blocks still reads the cache). `max_tokens` is required, so 8,192 is sent when the model sets none. `reasoning.effort` becomes `output_config.effort`. Thinking is turned on by the provider file (`extra_body.thinking`), not by the adapter, since compatible hosts take other shapes. Replies with thinking are kept sealed and sent back unchanged (below). Usage counts cache reads and writes in the prompt, reads as cached and writes as `cache_write_tokens`. `refusal` ends a reply as a content filter. Models are listed from `/v1/models`.
- **`openai-responses`** (`proto-openai-responses`): `POST <base>/responses`, stateless (`store: false`, the whole history in `input` each time, as with chat). The first system message is `instructions`, later ones `developer` messages. Tool calls and results are `function_call` / `function_call_output` items. Streams of text, reasoning (`reasoning_text` and `reasoning_summary_text`) and function-call arguments are handled, including servers that send arguments only at `.done`. `reasoning.effort` comes from the model's `reasoning.effort`; the request's `cache_key` (the session id) is sent as `prompt_cache_key`. Replies with encrypted reasoning or an assistant `phase` are kept sealed and sent back in order. `input_tokens_details.cache_write_tokens` is counted.
- Both are tested against mock servers only (`quack-providers/tests/mock_server.rs`, plus unit tests of the request bodies and stream parsing).

## Claude, OpenAI and ChatGPT plans (2026-10-19)

Optional, like every provider: three files in `defaults/providers.d`, contacted only once set up, never picked on their own, and removed by deleting the file or by `[policy.providers] hosts`.

| File | Billing | Set up with | Contacts |
|---|---|---|---|
| `anthropic.toml` | Claude API key, per token | `ANTHROPIC_API_KEY` or `quack setup` | `api.anthropic.com` |
| `openai.toml` | OpenAI API key, per token | `OPENAI_API_KEY` or `quack setup` | `api.openai.com` |
| `chatgpt.toml` | The ChatGPT plan's allowance (Plus, Pro) | `quack login chatgpt` | `api.openai.com`, and `auth.openai.com` for sign-in and hourly renewal |

**Claude subscriptions are not supported.** Anthropic's terms ([legal and compliance](https://code.claude.com/docs/en/legal-and-compliance)) allow Claude Free/Pro/Max sign-in only in Anthropic's own apps and forbid third-party tools from offering it or routing requests through those credentials. OpenAI, by contrast, documents an approval-free path for open-source apps that run on the user's machine ([Sign in with ChatGPT](https://developers.openai.com/siwc/token-sharing-open-source)).

### Sealed replies

Both APIs return reasoning that must go back unchanged for the model to keep it: Anthropic's thinking blocks carry signatures, OpenAI's reasoning items carry `encrypted_content`. The adapters keep the reply as the server sent it in `Message::Assistant.sealed` (`quack_providers::Sealed`): the blocks or items in order, text verbatim, tool calls by position (the calls themselves stay in `tool_calls`). Only replies that have reasoning (or an OpenAI `phase`) are sealed. `sealed` is logged with the message, so a resumed session sends the same request.

- A sealed reply goes back only to the provider that made it, under the same system prompt and tools (`fingerprint`, FNV-1a of both), and for OpenAI the same model. Otherwise it is sent rebuilt from `content` and `tool_calls`. Anthropic's API itself drops thinking another Claude model can't read.
- A reply whose text or calls the engine changed (a text tool call promoted to a real one) loses its seal.
- Pruning and compaction (`prune::apply`, `compact::apply`) remove every seal: signed thinking after an edited message no longer verifies, and dropping all of it at that one point is what Anthropic recommends.
- **Append-only history for `anthropic-messages`** (`Provider::append_only`). Claude's newer models check that a thinking block's history is unchanged (preserved thinking). The plan reminder, which rides on the newest message of each request for other providers, is instead written into the step's last tool result and stays there. `core/tests/anthropic.rs` checks that every request extends the one before it, across a resume.
- `anthropic.toml` sends `thinking.block_binding.prefix_mismatch_behavior = "drop_block"` (beta `thinking-binding-controls-2026-08-01`), so an edit we missed costs that reasoning rather than failing the request.

### The provider files

- `anthropic.toml`: `claude-opus-5-5` (`opus`), `claude-sonnet-5-5` (`sonnet`), `claude-haiku-5-5` (`haiku`), `claude-fable-5-1` (`fable`), plus `claude-opus-5`, `claude-sonnet-5` and `claude-opus-4-8`, with 1M windows, `max_output = 64000`, prices (cache reads and 5-minute writes) and an effort each. A file-level `extra_body` asks for adaptive thinking with summaries; models with refusal classifiers get `fallbacks = "default"` (beta `server-side-fallback-2026-07-01`), billed at the fallback model's price.
- `openai.toml` and `chatgpt.toml`: `gpt-6.1-sol` (`sol`), `gpt-6-astra` (`astra`), `gpt-6-luna` (`luna`), with `reasoning = { summary = "auto", context = "all_turns" }`. Prices in `openai.toml` are for prompts up to 272k tokens. The ChatGPT plan's own list (`models[].slug`, only `visibility = "list"`) is merged in `/models`.
- Provider files gained `extra_body` (merged under each model's; objects now merge key by key in every adapter), `[auth]`, and `cost.cache_write`.
- The family profiles (`cc-style` for Claude, `codex` for GPT) stay opt-in until `quack eval` shows they beat `generic` on these models.

### Sign in with ChatGPT

`[auth] type = "chatgpt"` with an `issuer` (feature `auth-chatgpt`, on by default, needs `openai-responses`). The binary names no host: the issuer comes from the file, and policy must allow its host as well as `base_url`'s; `doctor`'s network line lists it.

- `quack login [provider] [--new] [--no-browser]` (`quack_providers::login`): PKCE (S256), a callback on `127.0.0.1:1455` (any free port if taken), `ext_agent_host_id` (a `urn:uuid:` made once, kept in `<data>/auth/host-id`), `client_id = dynamic_agent_client` plus `agent_name_hint = "quack"` on first sign-in, then the issued client id, `id_token_hint` and `login_hint`. The ID token's RS256 signature is checked against the issuer's JWKS (endpoints on another host are refused), then `iss`, `aud`, `exp` and `nonce`. A sign-in without `chatgpt.tokens.use.direct` is saved but not used. `--new` registers again to switch accounts.
- Credentials: `<data>/auth/<provider>.json`, written whole with mode 0600 on Unix. The provider sends `Authorization: Bearer` and renews the token two minutes before it expires; a lock file serializes renewals across processes, because each refresh replaces the refresh token. Terminal refresh errors (`invalid_grant`, `refresh_token_reused`, ...) clear the tokens and ask for `quack login`; network failures keep them.
- The plan route (`responses::Route::ChatgptPlan`): no `max_output_tokens`, `temperature` or `top_p`; function tools go in one `namespace` (`workspace`), and calls and results carry it. `subscription_sharing_usage_limit_exceeded` is reported as the plan's limit and not retried.
- `quack logout` revokes the refresh token at the issuer's `revocation_endpoint` and clears the tokens, keeping the registration.
- Until signed in, `/models` and `doctor` show "run `quack login chatgpt`", and nothing is sent: the token is looked up before any request (`tests/login.rs`).
- No new crates: the SHA-256, RSA and random number generator come from `aws-lc-rs`, already built for reqwest's TLS on every OS. Base64url and form encoding are a few lines each.

### Not done yet

- None of this has been run against the live APIs; everything is checked against mock servers and the published docs. A first live run should confirm the beta headers, `reasoning.context`, and that the plan route accepts `prompt_cache_key` and `reasoning.summary`.
- `quack setup` lists `anthropic` and `openai` (they have keys) but not `chatgpt`; it says nothing about `quack login`.
- Server tools (web search) and Anthropic's mid-conversation system messages are not used. Images are: see [Vision](#vision-2026-10-24).

## Provider file schema

### Minimal form (what most users write)

User-facing files stay small: every option except `base_url` has a working default, and
the documentation and shipped defaults lead with this form.

```toml
base_url = "https://api.example.com/v1"
api_key = { env = "EXAMPLE_API_KEY" }   # omit for local servers
models = ["qwen3-coder"]                # optional; omitted = discovered from /models
```

- `api` defaults to `openai-chat`.
- `models` accepts plain ids. Use a table per model (`{ id = "...", context_window = 131072 }` or `[[models]]`) only to set per-model options.
- With no `models`, the list is discovered from `GET /models` at startup. Discovery also runs when the selected model has no `context_window`, since most servers report it (`context_window`, vLLM `max_model_len`, OpenRouter `context_length`).
- With no `[model].default`, startup picks the first usable provider: the user's own layers before the defaults bundle, skipping providers whose `api_key` environment variable is unset, and probing local servers with a short timeout. The startup panel says the model was auto-selected.

### Full reference

Everything below is optional.

```toml
#:schema ../schemas/provider.json
id = "corp-vllm"                       # default: file stem
name = "Corp GPU cluster"
api = "openai-chat"                    # default
base_url = "https://llm.corp.internal/v1"
enabled = true

# auth — any secret form from 01-configuration
api_key = { env = "CORP_LLM_KEY" }
# or a sign-in instead of a key: `quack login <id>` (see "Sign in with ChatGPT")
# auth = { type = "chatgpt", issuer = "https://auth.openai.com" }
auth_header = "Authorization"          # default; value "Bearer <key>"
# auth_scheme = "Bearer" | "" (raw) | custom
headers = { "X-Team" = "platform" }
extra_body = { }                       # merged into every request, under each model's

[tls]
ca_file = "/etc/ssl/corp-root.pem"     # in addition to OS store
client_cert = "~/.certs/me.pem"        # mTLS for internal gateways
client_key  = "~/.certs/me.key"
# insecure_skip_verify = false         # exists, warns loudly, blockable by policy

[http]
timeout_connect_secs = 10
timeout_idle_secs = 300                # long prefill on big contexts can be slow
max_retries = 3                        # 429/5xx/connection reset, exponential backoff + jitter
proxy = ""                             # override [network].proxy

[discovery]
enabled = true                         # GET /models at startup (async), merge with [[models]]
include = ["Qwen/*", "zai-org/*"]
default_profile = "auto"

[[models]]
id = "Qwen/Qwen3-Coder-480B-A35B-Instruct"
alias = "qwen3-coder"                  # what the user types / shows in UI
context_window = 262144
max_output = 32768
profile = "auto"                       # or explicit "qwen-code" → 03-model-profiles
vision = false                         # image support: the view_image tool and attached images (see Vision)

[models.tool_calling]
mode = "native"                        # native | text | grammar
text_format = "auto"                   # used when mode=text or native fails: hermes | qwen3-xml | glm | json-block
parallel = true

[models.reasoning]
format = "auto"                        # auto (default) | none | reasoning_content | reasoning | think-tags
preserve_in_history = false            # some models (interleaved thinking) need true
effort = ""                            # passed through if supported

[models.tokenizer]
source = "hf-file"                     # hf-file | tiktoken | server | estimate
path = "tokenizers/qwen3-coder.json"   # relative to provider file dir, or absolute

[models.sampling]
temperature = 0.7
top_p = 0.8
top_k = 20
min_p = 0.0
repetition_penalty = 1.05

[models.extra_body]                    # merged into the request JSON (objects key by key)
chat_template_kwargs = { enable_thinking = false }

[models.cost]                          # optional; US$ per million tokens, for /stats
input = 0.0
output = 0.0
cached_input = 0.0                     # unset: the input price
cache_write = 0.0                      # where writes are billed apart; unset: the input price

[models.serving]
prefill_tps = 3000                     # hint for cache-aware pruning (05-context); "auto" = measure
prefix_cache = true                    # does the server reuse KV prefix?
```

Unknown keys are an error (`deny_unknown_fields`), except inside `extra_body` and `headers`.

### Discovered vs. declared models

Startup lists models when `enabled` is set, when no models are listed, or when the model to use is not listed or lacks `context_window` or `cost` (2026-10-24: before, an unlisted model in a provider with any `[[models]]` got neither, so evals on it went unpriced). A failed listing warns unless only the price was missing. The `/models` picker always lists. When discovery is enabled, `/models` results are merged with `[[models]]`. Declared entries win per field. Discovered-only models get `context_window` from the response when the server reports it (vLLM reports `max_model_len`, llama.cpp reports `n_ctx` via `/props`). Otherwise they are marked *unknown context*, and the UI asks for a value the first time the model is used, then writes it back into the TOML with `toml_edit`.

## Tool-call normalization

This is the most important robustness work in the provider layer.

### Modes

1. **native**: send `tools` in the request and read structured `tool_calls` deltas. This is the preferred mode.
2. **text**: send no `tools`. The profile describes the tools in the system prompt, and we parse the model's output with the selected `text_format` parser. This works with any server, including ones whose tool-call parser is misconfigured.
3. **grammar**: like text mode, but the output is constrained (llama.cpp `grammar`/`json_schema`, vLLM `guided_json`/structured outputs) so tool calls are always parseable. This mode is opt-in, because constrained decoding can hurt quality and speed.

### Fallback chain

Even in native mode, a model sometimes emits a tool call as plain text in `content`. This happens when the server's `--tool-call-parser` doesn't match the model, or when the call is inside `<think>`. So the stream is always passed through a **text tool-call detector** for the configured `text_format`. If it finds a complete, schema-valid call in content, that call is promoted to a real tool call and a debug note is logged. There is a toggle to disable this.

### Parsers (in `quack-providers::toolparse`)

| Format | Shape |
|---|---|
| `hermes` | `<tool_call>{"name":..,"arguments":{..}}</tool_call>` (Qwen2.5, Hermes, many fine-tunes) |
| `qwen3-xml` | `<tool_call><function=name><parameter=x>..</parameter></function></tool_call>` (Qwen3-Coder) |
| `glm` | GLM `<tool_call>name<arg_key>..</arg_key><arg_value>..</arg_value></tool_call>` |
| `json-block` | Fenced ```json {"tool": .., "args": ..}``` (generic fallback for weak models) |
| `deepseek` | DeepSeek special-token format, for raw completions |
| `kimi` | Kimi special-token format |

(Exact token formats must be verified against each model's chat template when implementing. The templates themselves are the source of truth.)

Every parser:
- is incremental and works on streaming deltas, so the UI can show "calling `edit` on src/main.rs…" before the call finishes,
- repairs JSON leniently (trailing commas, single quotes, unescaped newlines in strings, missing closing braces at EOS),
- validates arguments against the tool's JSON Schema. On failure the model gets a short error result naming the offending field, not a crash,
- coerces types the way the model obviously intended (`"5"` → `5`, `"true"` → `true`) when the schema demands it.

### Fixture corpus

`tests/fixtures/providers/<server>/<model>/*.sse` contains recorded raw streams from real servers. Parser and adapter tests replay them. Adding a model to the compatibility matrix means adding fixtures (see [13](13-testing-and-evals.md)).

## Reasoning handling

| `format` | Source |
|---|---|
| `reasoning_content` | `delta.reasoning_content` (vLLM reasoning parsers, DeepSeek, SGLang) |
| `reasoning` | `delta.reasoning` (OpenRouter, Ollama `think`) |
| `auto` (default) | Both fields above, plus `<think>` splitting when the reply opens with `<think>` |
| `think-tags` | `<think>…</think>` inline in content. Split out by a streaming tag parser that handles a missing opening tag (some templates prefill `<think>`) |
| `none` | — |

`preserve_in_history` controls whether earlier reasoning is sent back in later turns. Most models want it stripped. Some interleaved-thinking models (e.g. some Kimi/GLM/MiniMax releases) work better when it is kept for the current tool-use chain. The default value comes from the profile.

Reasoning tokens count towards the context budget only while they are kept.

## Tokenizers

Goal: count tokens locally, quickly, and without downloads.

| `source` | How |
|---|---|
| `hf-file` | `tokenizers` crate loads a `tokenizer.json` that ships next to the provider TOML or sits in `<data>/tokenizers/`. Never downloaded unless `quack tokenizer fetch <model>` is run explicitly |
| `tiktoken` | `tiktoken-rs` encodings (gpt-oss uses o200k) |
| `server` | llama.cpp `/tokenize`, batched and cached by content hash. Used for verification, never on the hot path |
| `estimate` | bytes/3.5 heuristic, corrected per session using `usage.prompt_tokens` reported by the server (an EWMA ratio) |

When the server reports real `usage`, its numbers always replace our estimate for already-sent history. Local counting is only needed for content we have not sent yet.

The `tokenizers` crate is heavy, so it is behind the `tokenizers-hf` feature. Builds without it fall back to `estimate` with server correction.

## Model switching

- `/models` opens the models modal ([06](06-tui.md#slash-commands--modals)): grouped by provider, with context window, profile, readiness and cost. It can switch for the session or save as default.
- Switching mid-session re-shapes history for the new profile/adapter on the next request. Tool-call IDs are re-mapped and reasoning is stripped when the new model's `preserve_in_history = false`. If the history exceeds the new budget, the user is offered compaction first.

## Errors

Errors are normalized into `ProviderError { kind, status, retryable, message, raw }`:
`Auth`, `RateLimited { retry_after }`, `ContextLengthExceeded`, `ServerOverloaded`, `BadRequest`, `Network`, `Timeout`, `Protocol`.

`ContextLengthExceeded` triggers an automatic compaction and a single retry ([05](05-context-management.md)). These messages are recognised by pattern for servers that return them as 400s with different texts.

## Vision (2026-10-24)

One model flag, `vision = true`, turns image support on; without it the tool is not offered, attached paths warn with the fix, and no pixels are ever sent.

- **`view_image`** (`quack-tools/src/image.rs`) is offered only when `ctx.vision`: it probes the file's header (`quack_protocol::Image`: png, jpeg, gif, webp; nothing is decoded, no image crate ships), base64-encodes it, and returns a caption — `[image: path 32KB 1024x768]` — in the tool result plus the pixels as `ToolOutput.images`. Files over 3 MB raw are refused with the limit in the message, because base64 would exceed what providers accept; nothing is resized.
- **Pasted and dropped images** attach to the turn ([06](06-tui.md)); the engine loads them (`engine.rs`, relative to the working directory) and a model without `vision` gets a warning that names the flag.
- **On the wire**, each adapter puts them where its API takes them:

| Adapter | User message | Tool result |
|---|---|---|
| `openai-chat` | `image_url` content parts before the text | the caption stays the tool message's string; the pixels follow the whole run of tool results as one `user` message of image parts (chat-completions servers take image parts only on user content, so the run of tool results stays contiguous). Mistral's API takes `image_url` chunks in the tool message, so there they go inline, and since it rejects a `user` message right after a `tool` one (an interrupted turn leaves one), an empty assistant reply goes between |
| `anthropic-messages` | `image` blocks (base64 `source`) before the text | `image` blocks inside the `tool_result` content |
| `openai-responses` | `input_image` parts before `input_text` | `input_image` parts inside `function_call_output.output` |
| `ollama` | `images: [<base64>...]` on the message | the same `images` field (the API takes it on every role) |

- **Tokens are estimated by pixels, not base64 length** — `(w*h)/750`, floor 256, 1000 when the header gave no size — so a 1 MB PNG does not read as hundreds of thousands of tokens and trigger compaction for nothing. Pruning that collapses a tool result to its caption drops the pixels (the model can call `view_image` again).
- **A model without vision never gets pixels**: `turn::request` swaps each message's images for a note (`[1 image not sent: this model takes no images]`), so a `/models` switch, a resume on another model or a planner without vision still works. The history keeps the images.
- **History keeps them inline**: images ride inside the messages, so the session log replays them exactly on resume with no sidecar files.

## Servers, not engines (2026-10-18)

vLLM, SGLang, llama.cpp, LM Studio and the hosted services are all OpenAI-compatible servers. Their provider files would differ only in `base_url`, and in the key for hosted ones. Engine quirks are detected from responses, not from file names: llama.cpp by `owned_by: "llamacpp"` in `/models` (then `/props` for the served window), and vLLM by `max_model_len`. Ollama is the one exception, because its native API (`api = "ollama"`) is used to set the context window per request.

The defaults bundle ships one small file per engine and service (`name`, `base_url`, and `api_key = { env = ... }` for hosted ones, plus an optional `group` heading and `note`). The binary names no server: setup, `/setup` and auto-selection work from whatever providers are loaded, so a company that deletes the files, ships the bundle without them, or sets `[defaults] dir = "none"` gets a binary that only knows its own providers. (A first version kept this list in `setup.rs` with no files shipped. It was moved back the same day: compiled-in addresses could not be removed.)

| Engine | Default address | Needed for tool calls |
|---|---|---|
| Ollama | `127.0.0.1:11434` (native API) | — |
| llama.cpp `llama-server` | `127.0.0.1:8080/v1` | `--jinja` |
| vLLM | `127.0.0.1:8000/v1` | `--enable-auto-tool-choice --tool-call-parser <parser>` |
| LM Studio | `127.0.0.1:1234/v1` | — |
| SGLang | `127.0.0.1:30000/v1` | `--tool-call-parser <parser>` |

The hosted services come in two groups: open-model services (OpenRouter, Synthetic, Charm Hyper, Together, Fireworks, DeepInfra, Novita) and the model makers' own APIs and coding plans (Mistral, including Vibe plan keys; Kimi Code plan; Moonshot; Z.ai API; DeepSeek). Coding-plan keys only work on their plan's endpoint, so Kimi appears twice. The Z.ai GLM Coding Plan is left out: Z.ai's terms allow it only in their listed tools (https://docs.z.ai/devpack/tool/others). DeepSeek has no separate coding plan.

How the bundled providers behave:

- **Hosted, no key:** never contacted. `/models` leaves them out, and `/providers`, `doctor` and the wizard show them as "not set up" or "offered" rather than as errors.
- **Hosted, key set:** listed and usable, but never auto-selected. The user picks a model, since a paid service shouldn't start by itself. When setup stores a key in the credential store, it copies the file to the user layer with `api_key = { keyring = "<id>" }`, keeping its comments.
- **Local:** probed only on loopback, and auto-selected (after the user's own providers) when no default model is set. So a running llama.cpp or Ollama works with no setup at all.

Restricting them is covered in `defaults/providers.d/README.md`. `[policy.providers] hosts = [...]` in the system layer disables every provider whose `base_url` host doesn't match, which an id allow-list can't do, since a user could reuse an allowed id. `[policy.mcp] hosts` does the same for remote MCP servers. `doctor`'s `network` line lists every host the configuration can reach.

A server elsewhere needs only its URL, plus a key if it was started with one (for example vLLM's `--api-key`):

```toml
base_url = "https://gpu-box.example.com/v1"
api_key = { env = "GPU_BOX_KEY" }
```

A company's bundle ships its own providers in the same folder ([14](14-packaging-and-distribution.md#enterprise-builds)).

## Mistral Large 4 (2026-10-18)

Mistral Large 4 ("Le Chonk") has been in API preview since 2026-10-06 as `mistral-large-4` (alias `mistral-large-4-0`, and `mistral-large` now points to it). It has about 1T total parameters (around 50B active), takes text and images, has a 1M window per Mistral's model card, and supports `reasoning_effort`. Open weights are promised for the end of October 2026, under a license not yet announced. The main paths are Mistral's API (pay-as-you-go or Vibe plan keys, both `mistral` in setup's list) and hosted services such as OpenRouter. Only Large 4 is targeted; other Mistral models get the same API handling but were not looked at.

Sources: Mistral's own harness (`mistralai/mistral-vibe`, commit `4ae5c59`, 2026-10-09) and Mistral's platform docs (model card `mistral-large-4-0`, the reasoning guide, and the chat API reference).

### As implemented

`openai-chat` has a **Mistral dialect**, used when the provider's host is `api.mistral.ai`. Nothing needs to be set in the provider file.

| Mistral's API | What we do |
|---|---|
| Rejects any field it doesn't know ("Extra inputs are not permitted") | No `stream_options` (usage comes in the last chunk anyway). `top_k`, `min_p` and `repetition_penalty` are dropped. `extra_body` is still sent as written |
| `reasoning_effort` is `none` or `high` | `none`, `off`, `minimal`, `low` → `none`; anything else → `high`, as Vibe maps them. Unset sends nothing (server default) |
| Earlier reasoning must be sent back: the docs warn that stripping `ThinkChunk` "significantly degrades output quality" | Assistant messages carry their reasoning as a `thinking` part before the text, unless the effort is `none` or the model sets `reasoning.preserve_in_history = false` |
| Streamed `content` may be a list of `text` and `thinking` parts | Read for every server, not only Mistral |
| `/models` reports `max_context_length` | Read during discovery, so the 1M window is found without configuration |
| Tool-call ids are 9 letters or digits | Ids that aren't (our text-parsed calls, or history from another model) are replaced by a stable 9-character id derived from them, for the call and its result alike. This also applies to any model named `mistral-large*` on other servers (vLLM with the Mistral tokenizer) |
| Tool results carry the tool `name`; an empty answer next to tool calls is `""` | Both sent in the Mistral dialect only (other servers keep `null`, which some require) |
| `prompt_cache_key` raises cache hits; cached input costs 10% | A key derived from the system prompt and the first user message, so it stays the same for the whole session |
| Tool calls arrive in one piece once fully generated, so a long call is silent; Vibe has no idle timeout by default | Our idle timeout stays at 300 s |
| Retries 429, 500, 502, 503, 504, 520 | 520 added |
| `cached_tokens` under `usage.prompt_tokens_details` | Already read |

The `vibe` profile (2026-10-20) follows Vibe's tool names and keeps the exact-string `edit`. Measured live through OpenRouter on 2026-10-21 ([roadmap](roadmap.md)): both pass the hard suite 14/14 on mistral-large-4-0, but `generic`, which then defaulted to the first, per-line hashline, hit 12 parse and 23 edit failures where `vibe` hit 0 and 1. `generic` is back on `str_replace` since the same day. Temperature is left to the server, whose default "varies depending on the model" per Mistral's docs; Vibe uses 1.0 with thinking for its default model, but Mistral has published no value for Large 4. To set it anyway, list the model with `sampling.temperature` in the provider file.

### Not done yet

- Not run against Mistral's own API: there is no Mistral key in the test environment. The request shape is covered by unit tests against what Vibe sends and the API reference. The model itself ran live through OpenRouter on 2026-10-21, but that path is plain `openai-chat` and does not exercise this dialect.
- Images: Large 4 accepts them, but we send no images yet.
- Self-hosting the open weights, when they ship: vLLM with `--tokenizer-mode mistral --config-format mistral --load-format mistral --enable-auto-tool-choice --tool-call-parser mistral`. At about 1T parameters this needs several GPUs. Check these flags against the release and add them to setup's vLLM note.
- A budget-capped eval subset through the API ([13](13-testing-and-evals.md)) before calling it verified.

## Open questions

- Should we support `api = "openai-completions"` (raw prompt with our own chat template) for servers with broken templates? It would give full control but means maintaining Jinja templates. Proposed: later, behind a feature, using `minijinja` with each model's own template.
- Should multiple base URLs per provider (load balancing/failover across GPU nodes) be supported? Proposed: `base_urls = [...]` with round-robin plus health checks, in M5.

### Reasoning in history (2026-10-18)

Earlier assistant reasoning is sent back by default: `reasoning_content` on OpenAI-compatible servers, `thinking` on Ollama, `thinking` parts on Mistral. DeepSeek's thinking mode returns HTTP 400 for a request with `tools` that omits it ([docs](https://api-docs.deepseek.com/guides/thinking_mode)); MiniMax-M2, Kimi K2 thinking and GLM document it as needed across tool calls. Local chat templates drop the reasoning they don't render. `[reasoning] preserve_in_history = false` on a model turns it off, and `effort = "none"` sends none. Checked live on 2026-10-18 that an OpenAI-compatible router accepts the field.
