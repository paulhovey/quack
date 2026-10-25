<script lang="ts">
	// One plate of the guide: a hairline-framed page with its tab colour on the corner.
	import type { Snippet } from 'svelte';

	let { id, color, children }: { id: string; color: string; children: Snippet } = $props();
</script>

<section {id} class="scroll-mt-4 px-3 sm:px-6 xl:pr-40 py-6 md:py-10">
	<div class="relative max-w-7xl mx-auto rounded-3xl border border-rule bg-paper">
		<span class="absolute -top-px right-10 w-14 h-2 rounded-b-md" style="background:{color}" aria-hidden="true"></span>
		<div class="px-4 sm:px-8 lg:px-12 pt-14 md:pt-20 pb-14 md:pb-20">
			{@render children()}
		</div>
	</div>
</section>
