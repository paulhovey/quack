# 05 — Context Management

## Goals

- Fit useful work into 32k–128k context windows.
- Minimize **prefill cost** on self-hosted servers, where re-processing a long prefix can take tens of seconds.
- Make every token visible: the user can always see what is using the context and why.
- Use tree-sitter so the model reads *structure first* and *bodies on demand* (maki's core idea).

## The central tension: pruning vs. prefix cache

llama.cpp (`cache_prompt`), vLLM (automatic prefix caching) and SGLang (RadixAttention) all reuse the KV cache for an identical **prefix**. Hosted services discount cached tokens.

- Appending to history is cheap: only the new tokens are prefilled.
- **Editing anything earlier in history** (pruning an old tool result, for example) invalidates the cache from that point on. Everything after it is re-prefilled.

maki-style aggressive pruning saves tokens per turn but can cost a full re-prefill each time. At 3,000 tok/s prefill, re-processing 60k tokens takes about 20 s. So pruning is a **cost decision**, not a default reflex.

### Strategy: prune in epochs

History is only rewritten at **epoch boundaries**, never on every turn. An epoch boundary happens when:

1. projected usage for the next request exceeds `compaction_threshold × budget`, or
2. reclaimable tokens are large enough that `reclaimable_tokens × remaining_expected_turns` beats `re_prefill_tokens` (estimated from `prefill_tps` and measured cache-hit stats), or
3. the user runs `/compact` or `/prune`, or
4. the server returns `ContextLengthExceeded`.

Between boundaries, history is append-only, so every request is a cache hit except for its new tail.

`[context] cache_strategy`:
- `preserve`: only prune or compact at threshold or on overflow. Best for slow local GPUs.
- `aggressive`: prune on every turn (maki-like). Best for fast hosted services without a cache discount, or for very small windows.
- `auto` (default): uses the cost model above, with `prefill_tps` taken from the model TOML, or measured as `prompt_tokens / time_to_first_token` on uncached requests.

### Stable prefix layout

```
[system prompt]          ← byte-stable for the whole session (03 cache stability rule)
[tool definitions]       ← byte-stable; deferred tools are activated by appending, not reordering
[synthetic env exchange] ← optional, stable
[compaction summary]     ← only changes at epoch boundaries
[history …]              ← append-only between epochs
[latest user message + volatile context: git status delta, new diagnostics, queued notes]
```

Activating a deferred MCP tool mid-session would change the tool list. In native mode this means appending the tool at the **end** of the tool array (llama.cpp and vLLM templates render tools in order). In text mode the definition goes into the next user message. Either way, the prefix before the new tool stays cached.

## Budget accounting

```
budget = floor(context_window × budget_ratio) − max_output_reserve
```

The `ContextLedger` tracks tokens per segment: system, tools, instructions, memory, summary, and each history item. It uses local token counts ([02](02-providers.md#tokenizers)), corrected by the server's reported usage.

`/context` shows a breakdown: one band with each segment in its own fill and the compaction point marked, tokens per segment, the room left before compaction in turns at the recent pace, and a suggestion based on what takes the most room. The status bar shows the room left (`34k left · ~4 turns`) rather than a gauge, because "how long can I keep going" is the question a user has.

## Tree-sitter: structure first

### `index` tool (maki-style skeleton)

Input is a file path, a directory, or a glob. Output is a compact outline with **exact line ranges**:

```
src/session.rs (412 lines, rust)
  1-12     use …
  14-31    pub struct Session { id, events, … }
  33-180   impl Session
    35-60    pub fn new(cfg: &Config) -> Result<Self>
    62-118   pub async fn run_turn(&mut self, input: Input) -> Result<()>
    120-180  fn apply_prune(&mut self, plan: PrunePlan)
  182-412  mod tests
```

- Implemented with each language's `tags.scm` query (definitions plus doc-comment spans) and a fallback "top-level nodes" mode for languages without one.
- Queries live in `defaults/grammars/<lang>/tags.scm`. They are data, so they can be overridden in `grammars/`.
- Directory mode lists files with one line each (top-level symbols, truncated), capped by tokens.

### `read` integration

- `read` on a file larger than `read.full_threshold_tokens` (default 2,000) **without** a line range returns the skeleton plus "file is N tokens; request a range". This is the main saving.
- `read` with a symbol name (`symbol = "Session::run_turn"`) resolves the range through the index, so the model doesn't need line numbers.
- After an edit, the index for that file is recomputed incrementally (`tree.edit` + reparse).

### Grammars

- Common languages are compiled in behind cargo features (`lang-*`; see [00](00-architecture.md#cargo-features-binary-level)).
- **Dynamic grammars** (M5): load `tree-sitter-<lang>.{so,dylib,dll}` from `grammars/` with `libloading`, together with its queries. This keeps grammars as removable as everything else.
- Parsing is lazy and cached per file by `(path, mtime, size)`. Large trees are evicted LRU.

## Pruning rules (applied at epoch boundaries)

Rules run in order of cheapest information loss first. Each produces a `PruneOp`, which is recorded in the session log so a resume rebuilds exactly the same context.

| # | Rule | Replacement |
|---|---|---|
| 1 | **Superseded reads**: a file region that was re-read later, or edited after being read | `[read of src/x.rs:10-80 superseded at step 42]` |
| 2 | **Stale reads**: file changed on disk since it was read | Stub + "re-read if needed" |
| 3 | **Large tool outputs** older than K steps (shell, grep) | Head/tail kept, middle elided, full log path referenced |
| 4 | **Old full-file reads** older than K steps | Replaced by the file's skeleton (from `index`) |
| 5 | **Resolved error chains**: failed tool calls followed by a success of the same intent | Collapsed into one line: "N failed attempts at X; succeeded at step M" |
| 6 | **Reasoning** from completed tool chains (when the model's profile doesn't preserve it) | Dropped |
| 7 | **Old turns** beyond the budget | Compaction (below) |

User messages and the model's final answers are never pruned by rules 1–6.

## Compaction (summarization)

Compaction runs when pruning alone can't get usage under the threshold.

1. Pick the cut point: keep the last N turns verbatim (default: everything since the last user message, plus 2 earlier turns).
2. Summarize everything before the cut with the `small` model if configured, otherwise the current model. Use a structured prompt that produces: goal, decisions made, files touched (with current state), open problems, and next steps. Pin the todo list and modified-file list verbatim; they are not summarized.
3. Replace the summarized span with a single `[compaction summary]` message. Persist the original events (the session log keeps everything), so `/rewind` and resume can still see them.
4. Emit `Event::Compacted`, and show a divider in the transcript.

Compaction prompts are profile-overridable templates (`compact.md` in a profile directory).

### As implemented (M2, 2026-10-18)

- **When:** before each request, if the corrected estimate is over `compaction_threshold × budget`, rules 1 and 3 run first (rules 2 and 4–6 wait for the index and the eval). If the request is still over, compaction runs. A `ContextLengthExceeded` from the server compacts once and retries; a second overflow ends the turn. `cache_strategy = "aggressive"` prunes before every request. `auto` currently behaves like `preserve`, until the prefill measurements exist.
- **Pruning** never touches the last 8 messages, results under 400 characters, or results already pruned. Rule 1 counts only the `read` tool; `glob`, `grep` and `list_dir` read many files.
- **Cut:** automatic compaction keeps up to 3 recent turns that fit in 35% of the budget. `/compact [focus]` keeps only the current turn. If the current turn alone is too big, the cut falls inside it, and the user's request is carried into the summary verbatim.
- **Summarizer:** the session's model (the `small` model comes later) gets a system prompt (`compact.md`, or a compiled-in default) and the summarized span rendered as plain text.
  - It gets plain text, not the raw messages, so tool-call templates don't matter.
  - Tool results are cut to 1,500 characters each, and the oldest messages are dropped above about 60% of the budget.
  - The reply is capped at `budget / 4` tokens (1,024–8,192). Small thinking models can otherwise loop until the window is full.
- **Fallback:** if the model returns an error or an empty summary, the summary becomes a plain list of the user's requests and the tool calls, built without a model. The session continues either way.
- **Log:** `prune` records hold the replaced contents and `compaction` records hold the cut and the summary message. Replay applies them in order, and repairs dangling tool calls at each `resume` record, as the live session did. Resume therefore rebuilds the same history.
- **Tested** with mock servers (threshold, overflow retry, `/compact`, resume after compaction) and locally with Qwen3.5-0.8B on llama.cpp with a 6k window: three turns, two compactions in one turn, and the facts from the first turn recalled after them.

### As implemented (2026-10-18): index and pruning rules

- **Outlines** come from tree-sitter with each grammar crate's own `tags.scm` (plus `impl` blocks for Rust), for Rust, Python, JavaScript, TypeScript/TSX and Go, behind `lang-common`. C and C++ (added 2026-10-19) use queries of our own in `outline.rs`, since the grammars' `tags.scm` capture only a function's declarator line: whole definitions, structs, unions, enums, typedefs, function-like macros, and for C++ classes, namespaces, methods and prototypes. `.h` is parsed as C++, which reads C headers too. A C++ method defined outside its class keeps its qualified name (`Shape::area`), and `read symbol=` prefers the longest match, so a definition beats its prototype. A signature whose name is not on the definition's first line shows the lines up to it (`static int add(int a, int b)`) or `typedef struct { … } Name;`. Definitions are nested by their line ranges. The queries are compiled in for now; `defaults/grammars/` overrides are still to do, and so are caching and incremental reparsing (files are parsed on demand, which is fast at these sizes).
- **`index`**: a file's outline in the format above, or for a directory one line per source file with its top-level definitions, capped at about 3,000 tokens.
- **`read`**: without a range, a source file over `[tools] read_outline_tokens` (2,000; 0 turns it off) returns its outline and a note to read a range or a `symbol`. `symbol = "Type::method"` (or `Type.method`) reads that definition's lines.
- **Pruning rules** now: 1 superseded reads, 2 stale reads (the file changed on disk since the session last read or wrote it), 4 old whole-file reads replaced by the outline of the text that was read, 5 failed calls (an error, a non-zero exit, a timeout) followed by a successful call with the same tool and target (the same command, or the same file), and 3 large outputs. Rule 5 also takes failures down to 200 characters. A read that returned an outline, or one symbol, does not supersede earlier reads. Rule 6 (reasoning) is left out: reasoning goes back to the server only where it is required, so dropping it saves no request tokens and would break the cache there.
- Tested by unit tests in `quack-context` and `core/tests/pruning.rs` (an old read becomes its outline).

## Code-execution sandbox (deferred)

maki's `code_execution` tool lets the model chain tools inside an interpreter so intermediate data never enters context. This is high value but large in scope (a sandboxed interpreter embedded in the binary). It is deferred to post-M5, with candidates Lua (`mlua`), Rhai, or a Wasm Python. It is tracked here because it changes the context math significantly.

## Sub-agents as context isolation (M3)

The `task` tool runs a sub-agent with a fresh context and a restricted tool set. Only its final report enters the parent context. Sub-agents can use the `small` model. Each one shows up in the UI as its own navigable view (maki `/tasks`).

## Metrics

Logged per request: prompt tokens, cached tokens (when reported), completion tokens and, when the model has a `cost`, the price. `/stats` shows per-session totals, rebuilt from the log on resume. Timings (TTFT, speed, time in tools and approvals) are kept in memory only, so after a resume they cover the time since. These feed the `auto` strategy and the eval reports.

## Open questions

- Should rule 4 (old reads → skeleton) also apply between epochs in `auto` mode on hosted APIs with no cache discount? Proposed: yes. In that case `auto` behaves like `aggressive` when the provider sets `prefix_cache = false`.
- Repo map (aider-style ranked symbol graph) for the first turn? Proposed: optional in M5, off by default. `index` on demand is usually enough.
