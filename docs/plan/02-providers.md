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
| `proto-openai-chat` | `openai-chat` | vLLM, SGLang, llama.cpp `llama-server`, LM Studio, TGI, Novita, Fireworks, Together, DeepInfra, OpenRouter, LiteLLM gateways, Ollama `/v1` |
| `proto-openai-responses` | `openai-responses` | OpenAI Responses API and compatible gateways (vLLM has partial support) |
| `proto-anthropic` | `anthropic-messages` | Anthropic-compatible endpoints (several open-model hosts, e.g. Z.AI and Moonshot, expose one) |
| `proto-ollama` | `ollama` | Ollama native `/api/chat`, which allows per-request `num_ctx` and `keep_alive` |

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

## Provider file schema

```toml
#:schema ../schemas/provider.json
id = "corp-vllm"                       # default: file stem
name = "Corp GPU cluster"
api = "openai-chat"
base_url = "https://llm.corp.internal/v1"
enabled = true

# auth — any secret form from 01-configuration
api_key = { env = "CORP_LLM_KEY" }
auth_header = "Authorization"          # default; value "Bearer <key>"
# auth_scheme = "Bearer" | "" (raw) | custom
headers = { "X-Team" = "platform" }

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
vision = false

[models.tool_calling]
mode = "native"                        # native | text | grammar
text_format = "auto"                   # used when mode=text or native fails: hermes | qwen3-xml | mistral | glm | json-block
parallel = true

[models.reasoning]
format = "reasoning_content"           # none | reasoning_content | reasoning | think-tags
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

[models.extra_body]                    # merged verbatim into request JSON
chat_template_kwargs = { enable_thinking = false }

[models.cost]                          # optional; per million tokens, for hosted services
input = 0.0
output = 0.0
cached_input = 0.0

[models.serving]
prefill_tps = 3000                     # hint for cache-aware pruning (05-context); "auto" = measure
prefix_cache = true                    # does the server reuse KV prefix?
```

Unknown keys are an error (`deny_unknown_fields`), except inside `extra_body` and `headers`.

### Discovered vs. declared models

When discovery is enabled, `/models` results are merged with `[[models]]`. Declared entries win per field. Discovered-only models get `context_window` from the response when the server reports it (vLLM reports `max_model_len`, llama.cpp reports `n_ctx` via `/props`). Otherwise they are marked *unknown context*, and the UI asks for a value the first time the model is used, then writes it back into the TOML with `toml_edit`.

## Tool-call normalization

This is the most important robustness work in the provider layer.

### Modes

1. **native**: send `tools` in the request and read structured `tool_calls` deltas. This is the preferred mode.
2. **text**: send no `tools`. The profile describes the tools in the system prompt, and we parse the model's output with the selected `text_format` parser. This works with any server, including ones whose tool-call parser is misconfigured.
3. **grammar**: like text mode, but the output is constrained (llama.cpp `grammar`/`json_schema`, vLLM `guided_json`/structured outputs) so tool calls are always parseable. This mode is opt-in, because constrained decoding can hurt quality and speed.

### Fallback chain

Even in native mode, a model sometimes emits a tool call as plain text in `content`. This happens when the server's `--tool-call-parser` doesn't match the model, or when the call is inside `<think>`. So the stream is always passed through a **text tool-call detector** for the configured `text_format`. If it finds a complete, schema-valid call in content, that call is promoted to a real tool call and a debug note is logged. There is a toggle to disable this.

### Parsers (in `cutoffharness-providers::toolparse`)

| Format | Shape |
|---|---|
| `hermes` | `<tool_call>{"name":..,"arguments":{..}}</tool_call>` (Qwen2.5, Hermes, many fine-tunes) |
| `qwen3-xml` | `<tool_call><function=name><parameter=x>..</parameter></function></tool_call>` (Qwen3-Coder) |
| `mistral` | `[TOOL_CALLS][{"name":..,"arguments":..}]` |
| `glm` | GLM-4.x `<tool_call>name<arg_key>..</arg_key><arg_value>..</arg_value></tool_call>` |
| `json-block` | Fenced ```json {"tool": .., "args": ..}``` (generic fallback for weak models) |
| `deepseek` | DeepSeek special-token format, for raw completions |
| `kimi` | Kimi K2 special-token format |

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
| `think-tags` | `<think>…</think>` inline in content. Split out by a streaming tag parser that handles a missing opening tag (some templates prefill `<think>`) |
| `none` | — |

`preserve_in_history` controls whether earlier reasoning is sent back in later turns. Most models want it stripped. Some interleaved-thinking models (e.g. some Kimi/GLM/MiniMax releases) work better when it is kept for the current tool-use chain. The default value comes from the profile.

Reasoning tokens count towards the context budget only while they are kept.

## Tokenizers

Goal: count tokens locally, quickly, and without downloads.

| `source` | How |
|---|---|
| `hf-file` | `tokenizers` crate loads a `tokenizer.json` that ships next to the provider TOML or sits in `<data>/tokenizers/`. Never downloaded unless `cutoffharness tokenizer fetch <model>` is run explicitly |
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

## Default provider files (shipped separately)

`defaults/providers/`: `ollama.toml`, `llama-cpp.toml`, `vllm.toml`, `lmstudio.toml`, `sglang.toml`, `novita.toml`, `fireworks.toml`, `together.toml`, `openrouter.toml`, `deepinfra.toml`. Each file is fully commented, with every model entry tagged `# verified: <date>`. See [14](14-packaging-and-distribution.md) for how they are installed and removed.

## Open questions

- Should we support `api = "openai-completions"` (raw prompt with our own chat template) for servers with broken templates? It would give full control but means maintaining Jinja templates. Proposed: later, behind a feature, using `minijinja` with each model's own template.
- Should multiple base URLs per provider (load balancing/failover across GPU nodes) be supported? Proposed: `base_urls = [...]` with round-robin plus health checks, in M5.
