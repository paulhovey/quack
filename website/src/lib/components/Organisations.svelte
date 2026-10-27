<script lang="ts">
	import Plate from '#lib/components/Plate.svelte';
	import PlateHeading from '#lib/components/PlateHeading.svelte';

	const points = [
		['Nothing built in that reaches out', 'Every provider and MCP server is a small TOML file. Delete one and that capability is gone; a test checks startup stays clean for every default file. Only the core prompt and sub-agents are compiled in.'],
		['Hosts you allow', 'On top of contacting only what’s configured, `[policy.providers] hosts` and `[policy.mcp] hosts` narrow where providers and MCP servers may point, and `quack doctor` lists every host it can reach.'],
		['Settings you can lock', 'Config layers from system to user, project, environment and command line. An administrator’s locks hold in `/config`, and `quack config show --origin` says where each value came from.'],
		['Hooks users can’t remove', 'Hooks in the system config folder are enforced: block a tool call, add context, or check a prompt, in any language.'],
		['Ship your own files', '`quack-plucked` is the duck without its feathers: the whole agent minus the hosted-service providers, for teams with their own model server. The Config plate below walks through it.'],
		['Apache-2.0, few crates', 'Every dependency is counted per OS before it’s added. The keyring adds no crates on Linux.']
	];
	const parts = (t: string) => t.split(/(`[^`]+`)/).filter(Boolean);
</script>

<Plate id="organisations" color="var(--color-duck)">
		<PlateHeading title="For teams rolling it out">
			The same agent, with the controls an administrator needs to say yes to it.
		</PlateHeading>

		<div class="grid grid-cols-1 lg:grid-cols-[minmax(0,3fr)_minmax(0,2fr)] gap-12 items-start">
			<dl class="grid sm:grid-cols-2 gap-x-10 gap-y-8">
				{#each points as [term, text]}
					<div class="border-t-2 border-ink pt-4">
						<dt class="font-display text-xl font-extrabold">{term}</dt>
						<dd class="mt-2 text-ink-2 leading-relaxed">
							{#each parts(text) as p}{#if p.startsWith('`')}<code class="text-[0.9em] text-ink">{p.slice(1, -1)}</code>{:else}{p}{/if}{/each}
						</dd>
					</div>
				{/each}
			</dl>

			<figure class="lg:sticky lg:top-8 space-y-4">
				<div class="rounded-xl border-2 border-ink bg-term text-term-text overflow-hidden">
					<p class="px-4 py-2 text-[13px] font-mono text-term-dim border-b border-white/10">providers.d/lab.toml</p>
					<pre class="px-4 py-4 text-[13px] sm:text-[14px] leading-relaxed overflow-x-auto"><span class="text-sky">base_url</span> = <span class="text-duck">"http://gpu-01.lab:8000/v1"</span></pre>
				</div>
				<div class="rounded-xl border-2 border-ink bg-term text-term-text overflow-hidden">
					<p class="px-4 py-2 text-[13px] font-mono text-term-dim border-b border-white/10">/etc/quack/config.toml</p>
					<pre class="px-4 py-4 text-[13px] sm:text-[14px] leading-relaxed overflow-x-auto"><span class="text-term-dim">[policy.providers]</span>
<span class="text-sky">hosts</span> = [<span class="text-duck">"gpu-01.lab"</span>]</pre>
				</div>
				<figcaption class="text-sm text-ink-2">
					A provider needs only its URL; models are discovered. The policy keeps every provider to the lab’s own
					server.
				</figcaption>
			</figure>
		</div>
	</Plate>
