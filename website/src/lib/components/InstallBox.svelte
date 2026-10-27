<script lang="ts">
	import { Check, Copy, Download } from '@lucide/svelte';

	const repo = 'https://github.com/paulhovey/quack/releases';
	const base = `${repo}/latest/download`;
	type Way = 'sh' | 'ps' | 'src';
	const ways: { id: Way; label: string }[] = [
		{ id: 'sh', label: 'Linux & macOS' },
		{ id: 'ps', label: 'Windows' },
		{ id: 'src', label: 'From source' }
	];
	// The commands from the installers' own headers; plucked leaves out the hosted services.
	const command = (way: Way, plucked: boolean) =>
		({
			sh: plucked ? `curl -fsSL ${base}/install.sh | sh -s -- --plucked` : `curl -fsSL ${base}/install.sh | sh`,
			ps: plucked ? `& ([scriptblock]::Create((irm ${base}/install.ps1))) -Plucked` : `irm ${base}/install.ps1 | iex`,
			src: plucked ? 'scripts/install.sh --from-source --plucked' : 'scripts/install.sh --from-source'
		})[way];

	// The release archives, as .github/workflows/release.yml names them.
	type Os = 'linux' | 'mac' | 'windows';
	type Arch = 'x86_64' | 'aarch64';
	const platforms: Record<Os, { name: string; triple: string; ext: string; arch: Record<Arch, string> }> = {
		linux: { name: 'Linux', triple: 'unknown-linux-musl', ext: 'tar.gz', arch: { x86_64: 'x64', aarch64: 'ARM64' } },
		mac: { name: 'macOS', triple: 'apple-darwin', ext: 'tar.gz', arch: { x86_64: 'Intel', aarch64: 'Apple silicon' } },
		windows: { name: 'Windows', triple: 'pc-windows-msvc', ext: 'zip', arch: { x86_64: 'x64', aarch64: 'ARM64' } }
	};

	let { size = 'lg' }: { size?: 'lg' | 'md' } = $props();
	let way = $state<Way>('sh');
	let plucked = $state(false);
	let os = $state<Os | null>(null);
	// null when the browser won't say; then both archives are offered.
	let arch = $state<Arch | null>(null);
	let copied = $state(false);
	let timer: ReturnType<typeof setTimeout> | undefined;

	const cmd = $derived(command(way, plucked));
	const archives = $derived.by(() => {
		if (!os || way === 'src') return [];
		const p = platforms[os];
		const arches: Arch[] = arch ? [arch] : ['aarch64', 'x86_64'];
		return arches.map((a) => {
			const file = `quack${plucked ? '-plucked' : ''}-${a}-${p.triple}.${p.ext}`;
			return { file, href: `${base}/${file}`, label: `${p.name} ${p.arch[a]}` };
		});
	});

	// Pick the visitor's platform: client hints where the browser has them, else the user
	// agent. Phones and anything unrecognised keep the default tab and get no archive link.
	$effect(() => {
		type Hints = { platform: string; getHighEntropyValues(h: string[]): Promise<{ architecture?: string }> };
		const hints = (navigator as Navigator & { userAgentData?: Hints }).userAgentData;
		const ua = navigator.userAgent;
		const name = hints?.platform || ua;
		if (/android|iphone|ipad|cros/i.test(name) || /android|iphone|ipad/i.test(ua)) return;
		const found: Os | null = /win/i.test(name) ? 'windows' : /mac/i.test(name) ? 'mac' : /linux/i.test(name) ? 'linux' : null;
		if (!found) return;
		os = found;
		way = found === 'windows' ? 'ps' : 'sh';
		// Windows on ARM still reports x64 in its user agent, and Macs always say Intel.
		if (found === 'linux') arch = /aarch64|arm64/i.test(ua) ? 'aarch64' : /x86_64|amd64/i.test(ua) ? 'x86_64' : null;
		hints
			?.getHighEntropyValues(['architecture'])
			.then((v) => {
				if (v.architecture === 'arm') arch = 'aarch64';
				else if (v.architecture === 'x86') arch = 'x86_64';
			})
			.catch(() => {});
	});

	async function copy() {
		try {
			await navigator.clipboard.writeText(cmd);
			copied = true;
			clearTimeout(timer);
			timer = setTimeout(() => (copied = false), 2000);
		} catch {
			copied = false;
		}
	}
</script>

<div class="w-full">
	<div class="flex flex-wrap items-center justify-between gap-x-4 gap-y-2 mb-2">
		<div class="flex flex-wrap gap-1" role="tablist" aria-label="Install method">
			{#each ways as w}
				<button
					type="button"
					role="tab"
					aria-selected={way === w.id}
					onclick={() => (way = w.id)}
					class="px-2 min-[360px]:px-2.5 sm:px-3 py-1 rounded-full text-[13px] min-[360px]:text-sm font-semibold transition-colors {way === w.id
						? 'bg-ink text-paper'
						: 'text-ink-2 hover:bg-wash hover:text-ink'}"
				>
					{w.label}
				</button>
			{/each}
		</div>
		<button
			type="button"
			role="switch"
			aria-checked={plucked}
			onclick={() => (plucked = !plucked)}
			class="group flex items-center gap-2 text-sm font-semibold text-ink"
		>
			<span
				class="relative w-9 h-5 rounded-full border-2 border-ink transition-colors {plucked ? 'bg-beak' : 'bg-paper'}"
				aria-hidden="true"
			>
				<span
					class="absolute top-0.5 left-0.5 w-3 h-3 rounded-full bg-ink transition-transform {plucked ? 'translate-x-4' : ''}"
				></span>
			</span>
			Plucked: no hosted services
		</button>
	</div>
	<div
		class="flex items-stretch rounded-xl border-2 border-ink bg-duck-soft overflow-hidden shadow-[0_6px_18px_-8px_rgba(29,42,58,0.35)]"
	>
		<code
			class="flex-1 min-w-0 whitespace-pre-wrap [overflow-wrap:break-word] leading-relaxed px-4 {size === 'lg'
				? 'py-3 text-[14px] sm:text-[15px]'
				: 'py-3 text-sm'} font-semibold text-ink"
			><span class="select-none text-beak-ink">{way === 'ps' ? '>' : '$'}&nbsp;</span>{#each cmd.split(' ') as word, w}{#if w}{' '}{/if}{#if word.includes('/')}{#each word.split('/') as part, i}{#if i}/<wbr />{/if}{part}{/each}{:else}<span class="whitespace-nowrap">{word}</span>{/if}{/each}</code
		>
		<button
			type="button"
			onclick={copy}
			class="shrink-0 flex items-center gap-1.5 px-3.5 sm:px-4 bg-duck border-l-2 border-ink font-semibold text-on-tint hover:bg-[#ffe347] active:translate-y-px transition-colors"
		>
			{#if copied}
				<Check class="w-4 h-4" aria-hidden="true" /> <span class="max-sm:sr-only">Copied</span>
			{:else}
				<Copy class="w-4 h-4" aria-hidden="true" /> <span class="max-sm:sr-only">Copy</span>
			{/if}
		</button>
	</div>
	{#if archives.length}
		<p class="mt-2.5 flex flex-wrap items-center gap-x-4 gap-y-1 text-sm text-ink-2">
			<span>Or download the archive:</span>
			{#each archives as a}
				<a href={a.href} class="inline-flex items-center gap-1 font-semibold text-sky-ink" title={a.file}>
					<Download class="w-4 h-4" aria-hidden="true" />{a.label}
				</a>
			{/each}
			<a href={repo} class="font-semibold text-sky-ink" rel="noreferrer">other platforms</a>
		</p>
	{/if}
	<p class="sr-only" aria-live="polite">{copied ? 'Install command copied' : ''}</p>
</div>
