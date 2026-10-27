<script lang="ts">
	// The administrator's path: the plucked binary, two TOML files, a check. Then where each
	// file lives and every option, grouped and folded. Content is in config.ts.
	import { ChevronRight } from '@lucide/svelte';
	import Plate from '#lib/components/Plate.svelte';
	import PlateHeading from '#lib/components/PlateHeading.svelte';
	import {
		doctorOutput,
		folder,
		guarantees,
		installCmd,
		layers,
		policyFile,
		providerFile,
		reference
	} from '#lib/config.js';

	const parts = (t: string) => t.split(/(`[^`]+`)/).filter(Boolean);

	// Just enough TOML colouring for the two examples: tables, keys, strings, comments.
	type Tok = [cls: string, text: string];
	function toml(line: string): Tok[] {
		if (/^\s*#/.test(line)) return [['text-term-dim', line]];
		if (/^\s*\[[^\]]+\]\s*$/.test(line)) return [['text-term-dim', line]];
		const out: Tok[] = [];
		const key = line.match(/^(\s*)([\w.]+)(\s*=)/);
		let rest = line;
		if (key) {
			out.push(['', key[1]], ['text-sky', key[2]], ['', key[3]]);
			rest = line.slice(key[0].length);
		}
		for (const p of rest.split(/("[^"]*"|#.*$)/).filter(Boolean)) {
			out.push([p.startsWith('"') ? 'text-duck' : p.startsWith('#') ? 'text-term-dim' : '', p]);
		}
		return out;
	}

	const steps = [
		{
			title: 'Install the plucked duck',
			text: 'The whole agent (prompts, sub-agents, MCP, ACP, language servers) minus the hosted-service providers, so nothing in it points off the machine. Offline, use `--archive`, or the `.deb` and `.rpm` packages.',
			file: 'shell',
			code: installCmd,
			lang: 'sh'
		},
		{
			title: 'Describe your server',
			text: 'Anything OpenAI-compatible: vLLM, SGLang, llama.cpp. `base_url` is the only required line; leave out `models` and they’re listed from the server.',
			file: 'providers.d/corp.toml',
			code: providerFile,
			lang: 'toml'
		},
		{
			title: 'Lock it down',
			text: 'The system layer’s policy holds against user files, a repo’s `.quack/`, environment variables and the command line.',
			file: 'config.toml',
			code: policyFile,
			lang: 'toml'
		},
		{
			title: 'Check it',
			text: 'There’s nothing else to install. `quack doctor` makes no model call; a user’s provider pointing elsewhere shows as blocked, and the only host listed is yours.',
			file: 'terminal',
			code: doctorOutput,
			lang: 'text'
		}
	];
</script>

<Plate id="config" color="var(--color-sky)">
	<PlateHeading title="Your server. Nothing else.">
		Install <code class="text-ink">quack-plucked</code>, write two TOML files for
		<code class="text-ink">https://&lt;internal-server&gt;:8000/v1</code>, and you have the whole harness, with that
		server as the only host it will ever contact.
	</PlateHeading>

	<ol class="grid grid-cols-1 lg:grid-cols-2 gap-x-10 gap-y-12">
		{#each steps as s, i}
			<li class="min-w-0 flex flex-col">
				<div class="flex items-baseline gap-3">
					<span
						class="shrink-0 grid place-items-center w-9 h-9 rounded-full bg-sky text-on-tint font-display font-extrabold text-lg"
						aria-hidden="true">{i + 1}</span
					>
					<h3 class="font-display text-2xl font-extrabold">{s.title}</h3>
				</div>
				<p class="mt-3 text-ink-2 leading-relaxed max-w-[60ch]">
					{#each parts(s.text) as p}{#if p.startsWith('`')}<code class="text-[0.9em] text-ink">{p.slice(1, -1)}</code>{:else}{p}{/if}{/each}
				</p>
				<div class="mt-4 rounded-xl border-2 border-ink bg-term text-term-text overflow-hidden">
					<p class="px-4 py-2 text-[13px] font-mono text-term-dim border-b border-white/10">{s.file}</p>
					<pre class="px-4 py-4 text-[13px] sm:text-[14px] leading-relaxed overflow-x-auto">{#if s.lang === 'toml'}{#each s.code.split('\n') as line, n}{#if n > 0}{'\n'}{/if}{#each toml(line) as [cls, text]}<span class={cls}>{text}</span>{/each}{/each}{:else if s.lang === 'sh'}<span class="text-term-dim">$ </span>{s.code}{:else}{s.code}{/if}</pre>
				</div>
			</li>
		{/each}
	</ol>

	<h3 class="mt-20 font-display text-3xl sm:text-4xl font-extrabold tracking-[-0.02em]">What’s guaranteed</h3>
	<dl class="mt-8 grid sm:grid-cols-2 xl:grid-cols-4 gap-x-10 gap-y-8">
		{#each guarantees as [term, text]}
			<div class="border-t-2 border-ink pt-4">
				<dt class="font-display text-xl font-extrabold">{term}</dt>
				<dd class="mt-2 text-ink-2 leading-relaxed">
					{#each parts(text) as p}{#if p.startsWith('`')}<code class="text-[0.9em] text-ink">{p.slice(1, -1)}</code>{:else}{p}{/if}{/each}
				</dd>
			</div>
		{/each}
	</dl>

	<h3 class="mt-20 font-display text-3xl sm:text-4xl font-extrabold tracking-[-0.02em]">Where it lives</h3>
	<p class="mt-4 text-lg text-ink-2 leading-relaxed max-w-[65ch]">
		Each layer overrides the one before. Every layer’s folder has the same shape, one file per item, and a file with
		the same name in a higher layer replaces the lower one. <code class="text-ink">quack config paths</code> prints the
		folders for this machine.
	</p>
	<div class="mt-8 grid grid-cols-1 lg:grid-cols-[minmax(0,3fr)_minmax(0,2fr)] gap-10 items-start">
		<div class="rounded-xl border border-rule overflow-hidden">
			<table class="w-full text-left">
				<caption class="sr-only">Configuration layers, lowest first</caption>
				<thead class="hidden sm:table-header-group bg-sky-soft">
					<tr class="text-sm">
						<th scope="col" class="px-5 py-3 font-semibold">Layer</th>
						<th scope="col" class="px-5 py-3 font-semibold">Where</th>
						<th scope="col" class="px-5 py-3 font-semibold">Notes</th>
					</tr>
				</thead>
				<tbody>
					{#each layers as [layer, where, note]}
						<tr class="block sm:table-row border-t border-rule first:border-t-0 sm:first:border-t py-3 sm:py-0">
							<th scope="row" class="block sm:table-cell px-5 sm:py-4 font-semibold align-top whitespace-nowrap">{layer}</th>
							<td class="block sm:table-cell px-5 py-1 sm:py-4 text-sm align-top break-words">
								{#each parts(where) as p}{#if p.startsWith('`')}<code class="text-ink">{p.slice(1, -1)}</code>{:else}{p}{/if}{/each}
							</td>
							<td class="block sm:table-cell px-5 sm:py-4 text-sm text-ink-2 align-top">
								{#each parts(note) as p}{#if p.startsWith('`')}<code class="text-ink">{p.slice(1, -1)}</code>{:else}{p}{/if}{/each}
							</td>
						</tr>
					{/each}
				</tbody>
			</table>
		</div>
		<div class="rounded-xl border-2 border-ink bg-term text-term-text overflow-hidden">
			<p class="px-4 py-2 text-[13px] font-mono text-term-dim border-b border-white/10">any layer’s folder</p>
			<pre class="px-4 py-4 text-[13px] leading-relaxed overflow-x-auto">{#each folder.split('\n') as line, n}{#if n > 0}{'\n'}{/if}<span class="text-sky">{line.slice(0, 21)}</span><span class="text-term-dim">{line.slice(21)}</span>{/each}</pre>
		</div>
	</div>

	<h3 class="mt-20 font-display text-3xl sm:text-4xl font-extrabold tracking-[-0.02em]">Every option</h3>
	<p class="mt-4 text-lg text-ink-2 leading-relaxed max-w-[65ch]">
		Any subset works; everything left out keeps its default. <code class="text-ink">quack config check</code> validates
		every file against its JSON schema, <code class="text-ink">/config</code> edits settings in the UI, and the
		<a href="https://github.com/paulhovey/quack#configuration-reference" class="text-sky-ink font-semibold" rel="noreferrer">README</a>
		has the full list.
	</p>
	<div class="mt-8 lg:columns-2 gap-10">
		{#each reference as g}
			<details class="group mb-4 break-inside-avoid rounded-xl border border-rule bg-paper">
				<summary
					class="list-none [&::-webkit-details-marker]:hidden cursor-pointer flex items-center gap-2 px-4 py-3 hover:text-sky-ink"
				>
					<ChevronRight class="w-4 h-4 shrink-0 text-sky-ink transition-transform group-open:rotate-90" aria-hidden="true" />
					<span class="font-display text-lg font-bold">{g.title}</span>
					<code class="hidden sm:inline ml-auto text-xs text-ink-2 text-right">{g.file}</code>
				</summary>
				<dl class="px-4 pb-4">
					{#each g.rows as [key, def, what]}
						<div class="border-t border-rule py-2.5">
							<dt class="flex flex-wrap items-baseline gap-x-3">
								<code class="font-semibold text-ink">{key}</code>
								{#if def}<code class="text-xs text-ink-2">{def}</code>{/if}
							</dt>
							<dd class="mt-0.5 text-sm text-ink-2 leading-relaxed [overflow-wrap:anywhere]">
								{#each parts(what) as p}{#if p.startsWith('`')}<code class="text-ink">{p.slice(1, -1)}</code>{:else}{p}{/if}{/each}
							</dd>
						</div>
					{/each}
				</dl>
			</details>
		{/each}
	</div>
</Plate>
