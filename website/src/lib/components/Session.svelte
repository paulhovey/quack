<script lang="ts">
	import Plate from '#lib/components/Plate.svelte';
	import PlateHeading from '#lib/components/PlateHeading.svelte';

	// Each line is a list of [style, text] runs, laid out like quack-tui's own snapshots.
	type Run = [string, string];
	type Scene = { id: string; label: string; caption: string; status: Run[]; lines: Run[][] };

	const duck: Run[] = [['', ' '], ['duck', '_(•)'], ['beak', '>'], ['', ' ']];

	const scenes: Scene[] = [
		{
			id: 'fix',
			label: 'Fix a failing test',
			caption: 'Every change shows its diff, and commands ask first unless you allow them.',
			status: [['badge-ask', ' ask '], ['dim', '  charm/glm-5.3 · 12.3k/104k'], ['spacer', ''], ['warn', 'waiting for approval']],
			lines: [
				[['dim', ' › '], ['', 'the date parser test fails on leap years, fix it']],
				[],
				[['dim', ' ✻ thought for 41 words · ctrl-t shows']],
				[['ok', ' ✓ '], ['tool-read', 'grep'], ['', ' fn days_in_month '], ['dim', '· 2 matches in 1 file']],
				[['ok', ' ✓ '], ['tool-read', 'read'], ['', ' src/date.rs:40-72']],
				[['ok', ' ✓ '], ['tool-edit', 'edit'], ['', ' src/date.rs '], ['dim', '· +3 -1']],
				[['gutter', '    ▎'], ['dim', '@@ -51,7 +51,9 @@']],
				[['gutter', '    ▎'], ['del', '-    2 => 28,']],
				[['gutter', '    ▎'], ['add', '+    2 if is_leap(year) => 29,']],
				[['gutter', '    ▎'], ['add', '+    2 => 28,']],
				[],
				[['box', ' ╭ Run this command? ─────────────────────────────────╮']],
				[['box', ' │ '], ['', '$ cargo test date::'], ['box', '                                 │']],
				[['box', ' │ '], ['accent', 'y'], ['dim', ' allow  '], ['accent', 'a'], ['dim', ' allow for this session  '], ['accent', 'n'], ['dim', ' deny'], ['box', '      │']],
				[['box', ' ╰─────────────────────────────────────────────────────╯']]
			]
		},
		{
			id: 'plan',
			label: 'Plan, then build',
			caption: 'Plan mode looks around, asks about the big decisions, then keeps a checklist the model sees at every step.',
			status: [['badge-auto', ' auto-edit '], ['dim', '  charm/qwen3.8-flash · 70k/104k · 48 t/s']],
			lines: [
				[['dim', ' › '], ['', 'make the parser faster']],
				[],
				[['ok', ' ✓ '], ['', 'plan 1/3 done '], ['dim', '· profile the hot loop']],
				[['ok', '     ✓ '], ['dim', 'read the parser']],
				[['accent', '     ▶ '], ['', 'profile the hot loop']],
				[['dim', '     · cache token lookups']],
				[['err', ' ✗ '], ['tool-read', 'shell'], ['', ' cargo bench '], ['dim', '· exit 101']],
				[['dim', '     error: no bench target']],
				[],
				[...duck, ['', 'There is no bench target yet; adding one.']],
				[],
				[['dim', '   ── 14s · 2 tools · 1.2k tokens out · 48 t/s ────────']],
				[],
				[['dim', '  plan  '], ['warn', '▰▱▱'], ['dim', ' 1/3  '], ['accent', '▶'], ['', ' profile the hot loop']]
			]
		},
		{
			id: 'models',
			label: 'Switch models',
			caption: 'Every model of every provider in one picker. Switch mid-conversation and keep the history.',
			status: [['badge-ask', ' ask '], ['dim', '  ollama/qwen3-coder:30b · 8.1k/32k']],
			lines: [
				[['box', ' ╭ models ─────────────────────────────────── 24 ╮']],
				[['box', ' │ '], ['accent', '› '], ['', 'ollama/qwen3-coder:30b       '], ['dim', 'local · 32k'], ['box', '   │']],
				[['box', ' │   '], ['', 'vllm/glm-5.3-air             '], ['dim', 'lab · 128k'], ['box', '   │']],
				[['box', ' │   '], ['', 'charm/kimi-k3                '], ['dim', 'hosted    '], ['box', '   │']],
				[['box', ' │   '], ['', 'deepseek/deepseek-v4-pro     '], ['dim', 'hosted    '], ['box', '   │']],
				[['box', ' │   '], ['', 'anthropic/claude-sonnet-5-5  '], ['dim', 'key set   '], ['box', '   │']],
				[['box', ' │   '], ['', 'lmstudio/devstral-small      '], ['dim', 'local · 64k'], ['box', '  │']],
				[['box', ' │──────────────────────────────────────────────│']],
				[['box', ' │ '], ...duck.slice(1), ['dim', 'profile qwen-code · edit hashline'], ['box', '      │']],
				[['box', ' ╰──────────────── ↑↓ move · ⏎ switch · esc ────╯']]
			]
		}
	];

	// Close each box row on the same column as its top border.
	for (const sc of scenes) {
		const w = sc.lines.find((l) => l[0]?.[1].includes('╭'))?.reduce((n, [, t]) => n + t.length, 0) ?? 0;
		for (const l of sc.lines) {
			const last = l[l.length - 1];
			if (l[0]?.[1].startsWith(' │') && last?.[1].trimStart().startsWith('│')) {
				const used = l.slice(0, -1).reduce((n, [, t]) => n + t.length, 0);
				last[1] = ' '.repeat(Math.max(0, w - used - 1)) + '│';
			}
		}
	}

	let current = $state(scenes[0]);
</script>

<Plate id="session" color="var(--color-sky)">
		<PlateHeading title="Seen in the wild">
			A full-screen terminal UI that works over SSH, in any terminal, on Linux, macOS and Windows. These
			are drawn the way quack draws them; the model names and output are examples.
		</PlateHeading>

		<div class="grid grid-cols-1 lg:grid-cols-[minmax(0,1fr)_minmax(0,2.2fr)] gap-8 items-start">
			<div class="flex lg:flex-col gap-2 overflow-x-auto pb-1" role="tablist" aria-label="Example sessions">
				{#each scenes as s}
					<button
						type="button"
						role="tab"
						aria-selected={current.id === s.id}
						onclick={() => (current = s)}
						class="shrink-0 text-left rounded-xl px-4 py-3 border-2 transition-colors {current.id === s.id
							? 'border-ink bg-sky-soft'
							: 'border-transparent hover:border-rule'}"
					>
						<span class="block font-display font-bold text-lg">{s.label}</span>
						<span class="hidden lg:block mt-1 text-ink-2 leading-snug">{s.caption}</span>
					</button>
				{/each}
			</div>

			<figure>
				<div class="term rounded-xl overflow-hidden border-2 border-ink shadow-[0_18px_40px_-18px_rgba(29,42,58,0.55)]">
					<div class="flex justify-between px-3 py-1.5 text-term-dim text-[11px] sm:text-[13px]">
						<span><span class="text-term-text">quack 0.1.0</span> · ~/src/calendar</span>
						<span>⎇ main</span>
					</div>
					<div class="px-1 pt-2 pb-4 min-h-[23rem] overflow-x-auto" role="tabpanel">
						{#each current.lines as line}
							<div class="whitespace-pre leading-[1.55] text-[9.5px] min-[420px]:text-[11px] sm:text-[13px] lg:text-[14px]">{#each line as [cls, text]}<span class={cls}>{text}</span>{/each}&nbsp;</div>
						{/each}
					</div>
					<div class="mx-2 mb-1 rounded-md border border-term-dim/50 px-2 py-1 text-[14px]"><span class="text-sky">›</span><span class="caret">▏</span></div>
					<div class="flex flex-wrap gap-x-2 gap-y-1 px-2 py-1.5 text-[11px] sm:text-[13px] whitespace-pre">
						{#each current.status as [cls, text]}<span class={cls}>{text}</span>{/each}
						<span class="ml-auto text-term-dim">/help</span>
					</div>
				</div>
				<figcaption class="mt-3 text-ink-2 lg:hidden">{current.caption}</figcaption>
			</figure>
		</div>
	</Plate>

<style>
	.term {
		background: var(--color-term);
		color: var(--color-term-text);
		font-family: var(--font-mono);
		font-variant-ligatures: none;
	}
	.term :global(.dim) { color: var(--color-term-dim); }
	.term :global(.duck) { color: var(--color-duck); }
	.term :global(.beak) { color: var(--color-beak); }
	.term :global(.ok) { color: #7ee08a; }
	.term :global(.err) { color: #ff7a93; }
	.term :global(.accent) { color: var(--color-sky); }
	.term :global(.warn) { color: var(--color-duck); }
	.term :global(.tool-read) { color: var(--color-sky); }
	.term :global(.tool-edit) { color: var(--color-duck); }
	.term :global(.add) { color: #7ee08a; }
	.term :global(.del) { color: #ff7a93; }
	.term :global(.gutter) { color: var(--color-term-dim); }
	.term :global(.box) { color: #6f7b8f; }
	.term :global(.badge-ask) { background: var(--color-sky); color: #0b1a24; font-weight: 700; }
	.term :global(.badge-auto) { background: var(--color-duck); color: #1d1a05; font-weight: 700; }
	.term :global(.spacer) { flex: 1; }
	.caret {
		color: var(--color-term-text);
		animation: blink 1.1s steps(1) infinite;
	}
	@keyframes blink {
		50% { opacity: 0; }
	}
</style>
