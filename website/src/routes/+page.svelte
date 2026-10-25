<script lang="ts">
	import ThumbTabs from '#lib/components/ThumbTabs.svelte';
	import Hero from '#lib/components/Hero.svelte';
	import GrinderComparison from '#lib/components/GrinderComparison.svelte';
	import Session from '#lib/components/Session.svelte';
	import FieldMarks from '#lib/components/FieldMarks.svelte';
	import Measured from '#lib/components/Measured.svelte';
	import Organisations from '#lib/components/Organisations.svelte';
	import Install from '#lib/components/Install.svelte';
	import Footer from '#lib/components/Footer.svelte';
</script>

<svelte:head>
	<title>quack · any model, any provider, no leaks</title>
	<meta
		name="description"
		content="A terminal coding agent that gets the best out of any model: local servers, open-model APIs, Claude and GPT. Tool-call repair, warm prompt caches, every edit checked, and it contacts only what you configure."
	/>
	<meta property="og:title" content="quack · any model, any provider, no leaks" />
	<meta
		property="og:description"
		content="A terminal coding agent for open-weight models and any provider. Contacts only what you configure."
	/>
	<meta property="og:type" content="website" />
</svelte:head>

<a
	href="#grinder"
	class="sr-only focus:not-sr-only focus:fixed focus:top-3 focus:left-3 focus:z-50 focus:bg-duck focus:text-on-tint focus:px-4 focus:py-2 focus:rounded-lg"
	>Skip to content</a
>
<ThumbTabs />
<main class="bg-wash pb-6">
	<Hero />
	<GrinderComparison />
	<Session />
	<FieldMarks />
	<Measured />
	<Organisations />
	<Install />
</main>
<Footer />
