<script lang="ts">
	// A plate's title and the paragraph under it. The plate's number goes in its foot
	// caption (PlateCaption), as in a field guide.
	import type { Snippet } from 'svelte';

	let { title, children }: { title: string; children?: Snippet } = $props();
</script>

<header class="mb-10 md:mb-14">
	<h2 class="text-4xl sm:text-5xl lg:text-6xl font-extrabold tracking-[-0.03em] leading-[1.02] max-w-4xl">
		{title}
	</h2>
	{#if children}
		<div class="mt-5 text-lg sm:text-xl text-ink-2 leading-relaxed max-w-[65ch]">
			{@render children()}
		</div>
	{/if}
</header>
