<script lang="ts">
	import { untrack } from 'svelte';

	// The welcome duck from quack-tui (`welcome` in ui.rs), frame for frame: 16 steps of
	// 8 ticks at 16 ms. It glides, quacks, blinks and rests; reduced motion holds step 0.
	// Its parts carry `data-part` so a plate can run leader lines to them.
	let {
		animated = true,
		class: cls = '',
		onstep
	}: { animated?: boolean; class?: string; onstep?: () => void } = $props();

	type Pose = [shift: number, eye: string, beak: string, tail: string, wave: number];
	const poses: Pose[] = [
		[0, 'o', '_>', ' /  ', 0],
		[0, 'o', '_>', ' /  ', 1],
		[0, 'o', '_>', ' |  ', 2],
		[0, 'o', '_>', ' /  ', 3],
		[1, 'o', '_>', ' / ', 0],
		[1, 'o', '_>', ' | ', 1],
		[1, 'o', '_>', ' / ', 2],
		[1, 'o', ' >', ' / ', 3],
		[1, 'o', ' >', ' / ', 0],
		[1, 'o', '_>', ' / ', 1],
		[0, 'o', '_>', ' |  ', 2],
		[0, 'o', '_>', ' /  ', 3],
		[0, 'o', '_>', ' /  ', 0],
		[0, '-', '_>', ' /  ', 1],
		[0, 'o', '_>', ' /  ', 2],
		[0, 'o', '_>', ' /  ', 3]
	];
	const waves = ['~~~~~~~~~~', '~≈~≈~≈~≈~≈', '≈~≈~≈~≈~≈~', '~≈~≈~≈~≈~≈'];

	let step = $state(0);

	$effect(() => {
		if (!animated) return;
		const still = window.matchMedia('(prefers-reduced-motion: reduce)');
		let timer: ReturnType<typeof setInterval> | undefined;
		const run = () => {
			clearInterval(timer);
			step = 0;
			if (!still.matches) timer = setInterval(() => (step = (step + 1) % 16), 128);
		};
		run();
		still.addEventListener('change', run);
		return () => {
			clearInterval(timer);
			still.removeEventListener('change', run);
		};
	});

	const pose = $derived(poses[step]);
	const pad = $derived(pose[0] ? ' ' : '');

	$effect(() => {
		void step;
		untrack(() => onstep?.());
	});
</script>

<pre class="duck {cls}" aria-hidden="true">{pad}      __
{pad}    <b data-part="beak">&lt;</b>(<span data-part="eye">{pose[1]}</span> )___
{pad}     ( <span data-part="body">.</span><b>{pose[2]}</b><span data-part="tail">{pose[3]}</span>
{pad}      `---'
   <i data-part="water">{waves[pose[4]]}</i></pre>

<style>
	.duck {
		font-family: var(--font-mono);
		font-weight: 700;
		font-variant-ligatures: none;
		line-height: 1.05;
		color: var(--color-duck-glyph);
		margin: 0;
		white-space: pre;
	}
	.duck b {
		color: var(--color-beak);
		font-weight: 800;
	}
	.duck i {
		font-style: normal;
		color: var(--color-sky);
	}
</style>
