<script lang="ts">
	import Plate from '#lib/components/Plate.svelte';
	import PlateHeading from '#lib/components/PlateHeading.svelte';

	// Everything from the README's feature list that the grinder and the teams plate don't
	// already cover, each said once.
	const groups = [
		{
			name: 'Models and providers',
			color: 'var(--color-duck)',
			items: [
				['Local servers', 'llama.cpp, Ollama, vLLM, SGLang and LM Studio, found on this machine by `/setup`.'],
				['Hosted services', '12 open-model APIs, from OpenRouter and DeepSeek to Kimi, Z.ai and Mistral, or any OpenAI-compatible URL.'],
				['Claude and GPT', 'The Anthropic and OpenAI APIs with a key, or a ChatGPT plan through `quack login chatgpt`.'],
				['Reasoning kept', 'Thinking survives tool calls and `/resume`, sent back the way each API expects.'],
				['Switch any time', '`/models` lists every model; switching keeps the history and approvals.'],
				['A planner model', 'Plan with a big model, carry it out with your usual one, and keep its cache warm.']
			]
		},
		{
			name: 'Editing code',
			color: 'var(--color-beak)',
			items: [
				['Five edit formats', 'hashline, str_replace, apply_patch, search/replace blocks or whole files, chosen per model.'],
				['A forgiving edit', 'Matches past stray whitespace, copied line numbers and smart quotes; keeps CRLF and BOMs.'],
				['All or nothing', 'A multi-file change that fails partway rolls every file back.'],
				['Undo and rewind', '`/undo` the last call, or `/rewind` the conversation, the files, or both.'],
				['Code intelligence', 'Language servers for 11 languages, plus an `lsp` tool for definitions, references and hover.'],
				['Outlines', '`index` outlines a file or folder with tree-sitter; `read` a symbol by name.']
			]
		},
		{
			name: 'Long tasks',
			color: 'var(--color-sky)',
			items: [
				['A live todo list', 'The model keeps a checklist and sees it at every step.'],
				['Sub-agents', '`explore` scouts the repo in its own context and hands back paths and line ranges.'],
				['Loop guards', 'Repeated calls, edit-test-edit cycles and empty replies are caught.'],
				['Changed-file reminders', 'The model is told which files it has touched, without rewriting history.'],
				['Small windows', 'Old reads and outputs are pruned first; compaction keeps your request word for word.'],
				['Sessions', 'Saved as they happen, crash-proof, and resumed with exactly the same context.']
			]
		},
		{
			name: 'Safety',
			color: 'var(--color-leaf)',
			items: [
				['Sandboxed shell', 'Commands write only in the workspace and have no network: Landlock + seccomp, or Seatbelt.'],
				['Five permission modes', 'From read-only to yolo, with allow and deny rules over a floor you can’t remove.'],
				['Commands parsed', 'In `git status && rm -rf x` the `rm` still asks, even inside subshells and heredocs.'],
				['Project trust', 'A cloned repo’s `.quack/` runs nothing until you trust it.']
			]
		},
		{
			name: 'Making it yours',
			color: 'var(--color-pink)',
			items: [
				['MCP', 'Local and remote servers, deferred tool search, prompts as slash commands, forms and sampling.'],
				['Hooks', 'Programs in any language on seven agent events; they can add context, block or rewrite calls.'],
				['Skills and commands', 'SKILL.md skills, and any markdown file in `commands/` becomes a slash command.'],
				['Your agent files', 'Reads AGENTS.md, CLAUDE.md, GEMINI.md, `.cursorrules` and Copilot instructions.'],
				['Memory', 'Notes per project and global, with approval before one is saved.'],
				['Your own queries', 'Drop in `tags.scm` or `highlights.scm` to outline or colour a framework’s macros.']
			]
		},
		{
			name: 'Everywhere',
			color: 'var(--color-duck)',
			items: [
				['Three systems', 'Linux, macOS and Windows on x64 and ARM64; PowerShell and Job Objects on Windows.'],
				['Starts at once', 'The first frame draws in about <10 ms on Linux.'],
				['Headless', '`quack exec "…"` runs one turn for scripts and CI.'],
				['Sees itself', '`/context` and `/stats` chart tokens, cache hits, cost and time.'],
				['Over SSH', 'Mouse selection and `/copy` go through the terminal (OSC 52).']
			]
		}
	];

	// `code` spans in the item text.
	const parts = (t: string) => t.split(/(`[^`]+`)/).filter(Boolean);
</script>

<Plate id="field-marks" color="var(--color-leaf)">
		<PlateHeading title="Field marks">
			Everything else it does today, one line each. The full list is in the
			<a href="https://github.com/paulhovey/quack#features" class="text-sky-ink font-semibold" rel="noreferrer">README</a>.
		</PlateHeading>

		<div class="columns-1 md:columns-2 xl:columns-3 gap-x-12">
			{#each groups as g}
				<div class="break-inside-avoid mb-12">
					<h3 class="flex items-center gap-2.5 text-2xl font-extrabold mb-4">
						<span class="w-6 h-2.5 rounded-full" style="background:{g.color}" aria-hidden="true"></span>
						{g.name}
					</h3>
					<dl class="space-y-3">
						{#each g.items as [term, text]}
							<div>
								<dt class="inline font-bold">{term}.</dt>
								<dd class="inline text-ink-2">
									{#each parts(text) as p}{#if p.startsWith('`')}<code class="text-[0.9em] text-ink bg-paper px-1 rounded border border-rule">{p.slice(1, -1)}</code>{:else}{p}{/if}{/each}
								</dd>
							</div>
						{/each}
					</dl>
				</div>
			{/each}
		</div>
	</Plate>
