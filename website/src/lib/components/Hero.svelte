<script lang="ts">
	import { ArrowUpRight, Moon, Sun } from '@lucide/svelte';
	import { mode, toggleMode } from 'mode-watcher';
	import Duck from '#lib/components/Duck.svelte';
	import InstallBox from '#lib/components/InstallBox.svelte';

	// The hero's field marks, each with a leader line to a part of the duck. Each links to
	// its full account in the grinder.
	const marks = [
		{ term: 'Repairs messy tool calls', note: 'Qwen3-XML, GLM, DeepSeek, Kimi and more', href: '#grinder-missed_tool_call', part: 'beak', clear: 0.62 },
		{ term: 'Keeps prompt caches warm', note: 'the system prompt never changes', href: '#grinder-cache_miss', part: 'eye', clear: 1.7 },
		{ term: 'Checks every edit', note: 'tree-sitter, then your language server', href: '#grinder-syntax_break', part: 'body', clear: 1.5 },
		{ term: 'Contacts only what you set up', note: 'tested in a loopback-only network', href: '#grinder-silent_egress', part: 'water', clear: 0.62 }
	];

	let plate: HTMLElement | undefined = $state();
	let lines = $state<{ x1: number; y1: number; x2: number; y2: number }[]>([]);
	let size = $state({ w: 0, h: 0 });

	// Leader lines run from each mark's anchor to the centre of its part of the duck.
	function measure() {
		if (!plate) return;
		const box = plate.getBoundingClientRect();
		size = { w: box.width, h: box.height };
		lines = marks.flatMap((m, i) => {
			const a = plate!.querySelector(`[data-anchor="${i}"]`)?.getBoundingClientRect();
			const p = plate!.querySelector(`[data-part="${m.part}"]`)?.getBoundingClientRect();
			if (!a || !p || !a.width) return [];
			const x1 = a.left + a.width / 2 - box.left;
			const y1 = a.top + a.height / 2 - box.top;
			const px = p.left + p.width / 2 - box.left;
			const py = p.top + p.height / 2 - box.top;
			// Stop short of the glyph so the line points at it without covering it; the eye and
			// body sit among other glyphs (the cap, the bill), so their lines stop further out.
			const len = Math.hypot(px - x1, py - y1);
			const gap = Math.min(len / 2, Math.max(p.height * m.clear, 16));
			return [{ x1, y1, x2: px - ((px - x1) / len) * gap, y2: py - ((py - y1) / len) * gap }];
		});
	}

	$effect(() => {
		if (!plate) return;
		const ro = new ResizeObserver(measure);
		ro.observe(plate);
		document.fonts?.ready.then(measure);
		return () => ro.disconnect();
	});
</script>

{#snippet mark(m: (typeof marks)[number], i: number, below: boolean)}
	<li class="relative {below ? 'lg:pt-6' : 'lg:pb-6'}">
		<span
			data-anchor={i}
			class="hidden lg:block absolute left-6 w-2.5 h-2.5 rounded-full bg-beak {below ? 'top-0' : 'bottom-0'}"
			aria-hidden="true"
		></span>
		<span class="lg:hidden absolute left-0 top-[0.7em] w-3 h-[3px] bg-beak rounded-full" aria-hidden="true"></span>
		<a href={m.href} class="group block pl-5 lg:pl-0 no-underline text-ink">
			<span class="block font-display font-bold text-xl leading-tight group-hover:underline decoration-beak">{m.term}</span>
			<span class="block mt-1 text-ink-2">{m.note}</span>
		</a>
	</li>
{/snippet}

<section id="top" class="relative bg-paper">
	<div class="xl:pr-40"><div class="max-w-7xl mx-auto px-4 sm:px-8 pt-6">
		<div class="flex items-center justify-between text-sm font-semibold text-ink-2">
			<a href="#top" class="flex items-center gap-2 no-underline text-ink">
				<span class="font-mono font-extrabold text-base"><span class="text-duck-ink">_(o)</span><span class="text-beak-ink">&gt;</span></span>
				<span class="font-display text-lg font-extrabold">quack</span>
			</a>
			<div class="flex items-center gap-5">
				<a href="https://github.com/paulhovey/quack" class="flex items-center gap-1 no-underline text-ink hover:text-sky-ink" rel="noreferrer">
					GitHub <ArrowUpRight class="w-4 h-4" aria-hidden="true" />
				</a>
				<!-- Icons switch on the html class so the right one shows before hydration -->
				<button
					type="button"
					onclick={toggleMode}
					aria-label="Dark mode"
					aria-pressed={mode.current === 'dark'}
					class="grid place-items-center w-10 h-10 rounded-full border border-rule text-ink hover:bg-wash"
				>
					<Moon class="w-5 h-5 dark:hidden" aria-hidden="true" />
					<Sun class="w-5 h-5 hidden dark:block" aria-hidden="true" />
				</button>
			</div>
		</div>

		<div class="grid grid-cols-1 lg:grid-cols-[minmax(0,5fr)_minmax(0,7fr)] gap-x-10 gap-y-12 pt-10 md:pt-16 items-start">
			<div>
				<div class="flex items-end justify-between gap-4">
					<h1 class="font-display font-extrabold tracking-[-0.045em] leading-[0.86] text-[clamp(4.5rem,13vw,9.5rem)]">quack</h1>
					<Duck class="lg:hidden text-[clamp(1rem,4.2vw,1.4rem)] -mb-1" />
				</div>
				<p class="mt-6 font-display font-bold text-[clamp(1.7rem,3.4vw,2.6rem)] leading-[1.08] tracking-[-0.02em]">
					Any model.<br />Any provider.<br /><span class="bg-duck text-on-tint px-1.5 -mx-1.5 box-decoration-clone">No leaks.</span>
				</p>
				<p class="mt-6 text-lg leading-relaxed text-ink-2 max-w-[46ch]">
					A terminal coding agent that gets the best out of whatever model you point it at: llama.cpp on
					your desk, vLLM in your lab, an open-model API, or Claude and GPT.
				</p>
				<p class="mt-4 text-lg leading-relaxed text-ink max-w-[46ch]">
					It would be worth using if it phoned home. <strong>It doesn't.</strong>
				</p>

				<div class="mt-9 max-w-xl">
					<p class="font-display font-bold text-lg mb-3">Where to find it</p>
					<InstallBox />
					<p class="mt-3 text-sm text-ink-2">
						Then run <code class="font-semibold text-ink">quack</code>; it finds model servers on this machine or walks you
						through adding one.
					</p>
				</div>
			</div>

			<!-- The plate: the welcome duck with its field marks -->
			<figure bind:this={plate} class="relative">
				{#if lines.length}
					<svg class="hidden lg:block absolute inset-0 pointer-events-none overflow-visible" width={size.w} height={size.h} aria-hidden="true">
						{#each lines as l, i}
							<line class="leader-line" style="--delay:{0.15 + i * 0.12}s; --len:{Math.hypot(l.x2 - l.x1, l.y2 - l.y1)}px" x1={l.x1} y1={l.y1} x2={l.x2} y2={l.y2} />
							<circle cx={l.x2} cy={l.y2} r="3.5" class="leader-end" />
						{/each}
					</svg>
				{/if}
				<ol class="grid grid-cols-1 sm:grid-cols-2 gap-x-10 gap-y-6">
					{#each marks.slice(0, 2) as m, i}{@render mark(m, i, false)}{/each}
				</ol>
				<div class="hidden lg:flex justify-center py-12">
					<Duck class="text-[clamp(1.6rem,3.2vw,2.9rem)]" onstep={measure} />
				</div>
				<ol start="3" class="mt-6 lg:mt-0 grid grid-cols-1 sm:grid-cols-2 gap-x-10 gap-y-6">
					{#each marks.slice(2) as m, i}{@render mark(m, i + 2, true)}{/each}
				</ol>
			</figure>
		</div>
	</div></div>
	<div class="water mt-14" aria-hidden="true"></div>
</section>

<style>
	.leader-line {
		stroke: var(--color-beak);
		stroke-width: 2;
		stroke-dasharray: var(--len);
		animation: draw-line 0.9s cubic-bezier(0.16, 1, 0.3, 1) backwards;
		animation-delay: var(--delay);
	}
	.leader-end {
		fill: var(--color-beak);
		stroke: var(--color-beak);
		stroke-width: 2;
	}
	@keyframes draw-line {
		from {
			stroke-dashoffset: var(--len);
		}
	}
</style>
