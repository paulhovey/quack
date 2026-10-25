<script lang="ts">
	// The field guide's thumb index: one coloured tab per plate down the right edge, labels
	// set level so they read at a glance. The tab for the plate in view pulls out.
	import { plates } from '#lib/plates.js';

	let active = $state<string>(plates[0].id);

	$effect(() => {
		const seen = new Map<string, number>();
		const io = new IntersectionObserver(
			(entries) => {
				for (const e of entries) seen.set(e.target.id, e.intersectionRatio);
				let best: string = active;
				let most = 0;
				for (const p of plates) {
					const r = seen.get(p.id) ?? 0;
					if (r > most) [best, most] = [p.id, r];
				}
				active = best;
			},
			{ threshold: [0, 0.15, 0.3, 0.5, 0.7, 1], rootMargin: '-20% 0px -40% 0px' }
		);
		for (const p of plates) {
			const el = document.getElementById(p.id);
			if (el) io.observe(el);
		}
		return () => io.disconnect();
	});
</script>

<nav aria-label="Sections" class="hidden xl:flex fixed right-0 top-1/2 -translate-y-1/2 z-30 flex-col items-end gap-1.5">
	{#each plates as p}
		{@const on = active === p.id}
		<a
			href="#{p.id}"
			aria-current={on ? 'true' : undefined}
			class="tab flex items-center gap-2.5 w-40 rounded-l-xl py-3 pl-4 pr-3 font-display font-bold text-base leading-none text-on-tint no-underline transition-transform duration-300 ease-out {on
				? 'translate-x-0'
				: 'translate-x-4 hover:translate-x-1 focus-visible:translate-x-1'}"
			style="background:{p.color}"
		>
			<span class="w-2 h-2 shrink-0 rounded-full bg-on-tint transition-opacity {on ? 'opacity-100' : 'opacity-0'}" aria-hidden="true"></span>
			{p.tab}
		</a>
	{/each}
</nav>

<style>
	.tab {
		box-shadow: -2px 2px 8px -3px rgba(10, 17, 25, 0.35);
	}
</style>
