<script lang="ts">
	import Duck from '#lib/components/Duck.svelte';
	import InstallBox from '#lib/components/InstallBox.svelte';
	import Plate from '#lib/components/Plate.svelte';
	import PlateHeading from '#lib/components/PlateHeading.svelte';
</script>

<Plate id="install" color="var(--color-beak)">
		<PlateHeading title="Where to find it">
			One command installs the binary and the defaults: provider files for local servers and hosted
			services, model profiles, language servers and MCP examples. Then run <code class="text-ink">quack</code>.
		</PlateHeading>

		<div class="grid grid-cols-1 lg:grid-cols-[minmax(0,3fr)_minmax(0,2fr)] gap-12 items-center">
			<div class="max-w-2xl">
				<InstallBox />
				<ul class="mt-8 space-y-3 text-ink-2 leading-relaxed">
					<li><strong class="text-ink">Plucked:</strong> the duck without its feathers. Just the binary, no provider files or profiles, for teams that ship their own. It never touches an existing defaults folder, so it’s also how to update just the binary.</li>
					<li><strong class="text-ink">Options:</strong> <code class="text-ink">--prefix</code>, <code class="text-ink">--version</code>, <code class="text-ink">--archive FILE</code> for offline installs, and <code class="text-ink">--uninstall</code>.</li>
					<li><strong class="text-ink">Archives</strong> for every platform are on the <a href="https://github.com/paulhovey/quack/releases" class="text-sky-ink font-semibold" rel="noreferrer">releases page</a>.</li>
				</ul>
				<p class="mt-8 text-sm text-ink-2">
					In development and usable day to day. Not there yet: editor integration (ACP), sandboxing on Windows, LSP
					rename, <code>/fork</code> and <code>/export</code>, config auto-reload and MCP OAuth.
				</p>
			</div>
			<div class="flex justify-center">
				<Duck class="text-[clamp(1.5rem,3.4vw,2.6rem)]" />
			</div>
		</div>
	</Plate>
