<script lang="ts">
	import Plate from '#lib/components/Plate.svelte';
	import { ArrowUpRight, ChevronRight } from '@lucide/svelte';
	import PlateHeading from '#lib/components/PlateHeading.svelte';

	// From docs/plan/roadmap.md, newest first; every row is a live run on a paid key.
	const runs = [
		{
			date: '2026-10-21',
			what: 'mistral-large-4, vibe vs generic, one hard pass',
			result: '14/14 each · 0 vs 12 bad calls · 1 vs 23 failed edits · $0.94 vs $1.70'
		},
		{
			date: '2026-10-20',
			what: 'glm-5.3-flash, zcode vs generic, hard suite ×3',
			result: '37/42 vs 35/42 · 482 vs 539 steps · half the tool errors'
		},
		{
			date: '2026-10-20',
			what: 'deepseek-v4.1-flash, dsh vs generic, hard suite ×3',
			result: '42/42 each · 367 vs 424 steps · $0.36 vs $0.45'
		},
		{
			date: '2026-10-19',
			what: 'Per-session context moved out of the system prompt',
			result: 'Uncached prompt tokens down 45% on deepseek-v4.1-flash'
		},
		{
			date: '2026-10-19',
			what: '8 models × 2 profiles, 21 small tasks (Charm Hyper)',
			result: 'All 21/21, no unparsable calls, $2.42 for the lot'
		}
	];

	// FrontierHarness Eval 1.0: Kimi K3 on 30 tasks, harness varied.
	const field = [
		['Codex', '66.7%', '$3.47', '88.0%'],
		['DSH Creator', '63.3%', '$3.28', '84.3%'],
		['Claude Code', '63.3%', '$18.34', '67.8%'],
		['Pi', '60.0%', '$2.43', '79.4%'],
		['DSH PTC', '60.0%', '$4.58', '87.2%'],
		['DSH Standard', '60.0%', '$3.46', '86.5%'],
		['Oh My Pi', '56.7%', '$4.75', '82.2%'],
		['Kimi Code', '56.7%', '$3.65', '88.0%'],
		['DSH Minimal', '56.7%', '$4.72', '84.6%'],
		['Exo Harness', '53.3%', '$1.05', '70.3%'],
		['OpenCode', '50.0%', '$3.24', '78.4%'],
		['Hermes', '50.0%', '$2.90', '85.9%']
	];
</script>

<Plate id="measured" color="var(--color-pink)">
		<PlateHeading title="Measured, not guessed">
			<code class="text-ink">quack eval</code> runs a task suite against any model and profile and counts passes, steps,
			bad calls, failed edits, tokens and cost. A family profile is switched on only when it clearly beats
			<code class="text-ink">generic</code>.
		</PlateHeading>

		<div class="rounded-2xl border-2 border-ink overflow-hidden">
			<table class="w-full text-left">
				<caption class="sr-only">quack's own eval runs</caption>
				<thead class="hidden sm:table-header-group bg-duck-soft">
					<tr class="text-sm">
						<th scope="col" class="px-5 py-3 font-semibold w-32">Date</th>
						<th scope="col" class="px-5 py-3 font-semibold">Run</th>
						<th scope="col" class="px-5 py-3 font-semibold">Result</th>
					</tr>
				</thead>
				<tbody class="divide-y divide-rule">
					{#each runs as r}
						<tr class="block sm:table-row align-top py-3 sm:py-0">
							<td class="block sm:table-cell px-5 pt-1 sm:py-4 font-mono text-sm text-ink-2 whitespace-nowrap tabular-nums">{r.date}</td>
							<td class="block sm:table-cell px-5 py-1 sm:py-4">{r.what}</td>
							<td class="block sm:table-cell px-5 pb-1 sm:py-4 font-semibold tabular-nums">{r.result}</td>
						</tr>
					{/each}
				</tbody>
			</table>
		</div>
		<p class="mt-3 text-sm text-ink-2">
			From <a href="https://github.com/paulhovey/quack/blob/main/docs/plan/roadmap.md" class="text-sky-ink font-semibold" rel="noreferrer">the roadmap</a>. Results files are in <code>evals/results/</code>.
		</p>

		<div class="mt-20 grid lg:grid-cols-[minmax(0,2fr)_minmax(0,3fr)] gap-10 items-start">
			<div>
				<h3 class="text-3xl font-extrabold leading-tight">Why the harness matters</h3>
				<p class="mt-4 text-lg text-ink-2 leading-relaxed">
					FrontierHarness Eval held one model, Kimi K3, fixed across 30 tasks and changed only the harness. Pass
					rates ran from 50% to 67%, and the median cost of a task from $1.05 to $18.34.
				</p>
				<p class="mt-4 text-lg text-ink-2 leading-relaxed">
					quack hasn’t been run on it yet. When it has, the result goes here, whatever it is.
				</p>
				<a
					href="https://frontierharness.org"
					class="mt-6 inline-flex items-center gap-1 font-semibold text-sky-ink"
					rel="noreferrer">frontierharness.org <ArrowUpRight class="w-4 h-4" aria-hidden="true" /></a
				>
			</div>
			<figure>
				<!-- The chart's labels are too small to read on a phone; a tap opens it full size. -->
				<a href="/assets/eval/frontier-harness-chart.svg" class="block">
					<img
						src="/assets/eval/frontier-harness-chart.svg"
						alt="FrontierHarness Eval: pass rate against median cost per task for 12 harness configurations running Kimi K3"
						class="w-full rounded-xl border-2 border-ink"
						loading="lazy"
						width="1200"
						height="630"
					/>
				</a>
				<details class="mt-4 group">
					<summary class="list-none [&::-webkit-details-marker]:hidden cursor-pointer inline-flex items-center gap-1.5 font-semibold text-ink hover:text-sky-ink"><ChevronRight class="w-4 h-4 text-beak-ink transition-transform group-open:rotate-90" aria-hidden="true" />The numbers behind the chart</summary>
					<div class="mt-3 overflow-x-auto">
						<table class="w-full text-left text-sm tabular-nums">
							<thead class="text-ink-2">
								<tr>
									<th scope="col" class="py-2 pr-4 font-semibold">Harness</th>
									<th scope="col" class="py-2 pr-4 font-semibold">Pass rate</th>
									<th scope="col" class="py-2 pr-4 font-semibold">Median cost</th>
									<th scope="col" class="py-2 font-semibold">Median cache hit</th>
								</tr>
							</thead>
							<tbody class="divide-y divide-rule">
								{#each field as [name, pass, cost, cache]}
									<tr>
										<td class="py-2 pr-4">{name}</td>
										<td class="py-2 pr-4">{pass}</td>
										<td class="py-2 pr-4">{cost}</td>
										<td class="py-2">{cache}</td>
									</tr>
								{/each}
							</tbody>
						</table>
					</div>
				</details>
				<figcaption class="mt-3 text-sm text-ink-2">Chart and figures: FrontierHarness Eval 1.0.</figcaption>
			</figure>
		</div>
	</Plate>
