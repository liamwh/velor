<script lang="ts">
	import { onMount } from 'svelte';
	import { SvelteSet } from 'svelte/reactivity';
	import { discoverSpecs, buildPlanPrompt, generatePlan } from '$lib/services/tauri';
	import { Button } from '$lib/components/ui/button/index.js';
	import { FileText, RefreshCw, Copy, Check, Play, Eye, AlertCircle } from 'lucide-svelte';
	import type { SpecFileInfo } from '$lib/types';

	let specs = $state<SpecFileInfo[]>([]);
	let selectedSpecs = new SvelteSet<string>();
	let generatedPlan = $state<string>('');
	let isLoading = $state(false);
	let isGenerating = $state(false);
	let error = $state<string | null>(null);
	let copied = $state(false);
	let showDryRun = $state(false);
	let dryRunPrompt = $state<string>('');
	let apiKey = $state('');
	let model = $state('gpt-4o');
	let dryRun = $state(false);

	onMount(async () => {
		await loadSpecs();
	});

	async function loadSpecs(): Promise<void> {
		isLoading = true;
		error = null;
		try {
			specs = await discoverSpecs();
			// Select all specs by default
			selectedSpecs = new SvelteSet(specs.map((s) => s.name));
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load specs';
		} finally {
			isLoading = false;
		}
	}

	function toggleSpec(name: string): void {
		if (selectedSpecs.has(name)) {
			selectedSpecs.delete(name);
		} else {
			selectedSpecs.add(name);
		}
	}

	function selectAll(): void {
		selectedSpecs = new SvelteSet(specs.map((s) => s.name));
	}

	function deselectAll(): void {
		selectedSpecs = new SvelteSet();
	}

	async function handleGeneratePlan(): Promise<void> {
		if (selectedSpecs.size === 0) {
			error = 'Please select at least one spec file';
			return;
		}

		isGenerating = true;
		error = null;
		generatedPlan = '';
		dryRunPrompt = '';

		try {
			const selectedSpecList = specs.filter((s) => selectedSpecs.has(s.name));

			if (dryRun) {
				dryRunPrompt = await buildPlanPrompt(selectedSpecList);
				showDryRun = true;
			} else {
				generatedPlan = await generatePlan({
					api_key: apiKey || undefined,
					model: model || undefined,
					dry_run: false
				});
			}
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to generate plan';
		} finally {
			isGenerating = false;
		}
	}

	async function copyToClipboard(text: string): Promise<void> {
		try {
			await navigator.clipboard.writeText(text);
			copied = true;
			setTimeout(() => {
				copied = false;
			}, 2000);
		} catch {
			error = 'Failed to copy to clipboard';
		}
	}
</script>

<div class="flex flex-col h-full gap-4">
	<!-- Header -->
	<div class="flex items-center justify-between">
		<div class="flex items-center gap-3">
			<FileText class="text-primary" size={24} />
			<div>
				<h2 class="text-lg font-semibold">Plan Generator</h2>
				<p class="text-sm text-muted-foreground">Generate implementation plans from spec files</p>
			</div>
		</div>
		<Button variant="outline" size="sm" onclick={loadSpecs} disabled={isLoading}>
			<RefreshCw size={16} class={isLoading ? 'animate-spin' : ''} />
			<span>Refresh</span>
		</Button>
	</div>

	{#if error}
		<div class="flex items-center gap-2 px-4 py-3 bg-destructive/10 border border-destructive/20 rounded-lg text-destructive">
			<AlertCircle size={16} />
			<span class="text-sm">{error}</span>
		</div>
	{/if}

	<!-- Main Content -->
	<div class="flex-1 grid grid-cols-1 lg:grid-cols-2 gap-4 min-h-0">
		<!-- Specs List -->
		<div class="flex flex-col border border-border rounded-lg overflow-hidden">
			<div class="flex items-center justify-between px-4 py-3 bg-muted/50 border-b border-border">
				<h3 class="font-medium">Spec Files ({specs.length})</h3>
				<div class="flex gap-2">
					<Button variant="ghost" size="sm" onclick={selectAll}>Select All</Button>
					<Button variant="ghost" size="sm" onclick={deselectAll}>Deselect All</Button>
				</div>
			</div>
			<div class="flex-1 overflow-auto p-2">
				{#if isLoading}
					<div class="flex items-center justify-center py-8 text-muted-foreground">
						<RefreshCw size={20} class="animate-spin mr-2" />
						<span>Loading specs...</span>
					</div>
				{:else if specs.length === 0}
					<div class="flex flex-col items-center justify-center py-8 text-muted-foreground gap-2">
						<FileText size={32} class="opacity-50" />
						<p>No spec files found</p>
						<p class="text-sm">Create .md files in the specs/ directory</p>
					</div>
				{:else}
					<div class="space-y-1">
						{#each specs as spec (spec.name)}
							<button
								class="w-full flex items-start gap-3 p-3 rounded-lg text-left transition-colors {selectedSpecs.has(spec.name)
									? 'bg-primary/10 border border-primary/30'
									: 'hover:bg-muted/50 border border-transparent'}"
								onclick={() => toggleSpec(spec.name)}
							>
								<input
									type="checkbox"
									checked={selectedSpecs.has(spec.name)}
									class="mt-1"
									onclick={(e) => e.stopPropagation()}
									onchange={() => toggleSpec(spec.name)}
								/>
								<div class="flex-1 min-w-0">
									<div class="font-medium truncate">{spec.name}</div>
									<div class="text-xs text-muted-foreground truncate">{spec.path}</div>
									<div class="text-sm text-muted-foreground mt-1 line-clamp-2">
										{spec.content.slice(0, 100)}...
									</div>
								</div>
							</button>
						{/each}
					</div>
				{/if}
			</div>
		</div>

		<!-- Generated Plan -->
		<div class="flex flex-col border border-border rounded-lg overflow-hidden">
			<div class="flex items-center justify-between px-4 py-3 bg-muted/50 border-b border-border">
				<h3 class="font-medium">
					{#if showDryRun}
						Preview Prompt
					{:else}
						Generated Plan
					{/if}
				</h3>
				{#if (generatedPlan || dryRunPrompt) && !showDryRun}
					<Button
						variant="ghost"
						size="sm"
						onclick={() => copyToClipboard(generatedPlan)}
					>
						{#if copied}
							<Check size={16} class="text-green-500" />
						{:else}
							<Copy size={16} />
						{/if}
						<span>Copy</span>
					</Button>
				{/if}
			</div>
			<div class="flex-1 overflow-auto p-4">
				{#if showDryRun && dryRunPrompt}
					<pre class="text-sm whitespace-pre-wrap font-mono">{dryRunPrompt}</pre>
				{:else if generatedPlan}
					<pre class="text-sm whitespace-pre-wrap font-mono">{generatedPlan}</pre>
				{:else}
					<div class="flex flex-col items-center justify-center py-8 text-muted-foreground gap-2">
						<Play size={32} class="opacity-50" />
						<p>No plan generated yet</p>
						<p class="text-sm">Select specs and click Generate</p>
					</div>
				{/if}
			</div>
		</div>
	</div>

	<!-- Settings and Actions -->
	<div class="flex flex-col sm:flex-row items-start sm:items-end justify-between gap-4 pt-4 border-t border-border">
		<div class="flex flex-wrap items-center gap-4">
			<div class="flex items-center gap-2">
				<label for="model" class="text-sm text-muted-foreground">Model:</label>
				<select
					id="model"
					bind:value={model}
					class="px-3 py-1.5 rounded-md border border-border bg-background text-sm"
				>
					<option value="gpt-4o">gpt-4o</option>
					<option value="gpt-4o-mini">gpt-4o-mini</option>
					<option value="gpt-4-turbo">gpt-4-turbo</option>
					<option value="gpt-3.5-turbo">gpt-3.5-turbo</option>
				</select>
			</div>
			<div class="flex items-center gap-2">
				<label for="apiKey" class="text-sm text-muted-foreground">API Key:</label>
				<input
					id="apiKey"
					type="password"
					bind:value={apiKey}
					placeholder="Uses OPENAI_API_KEY env var"
					class="px-3 py-1.5 rounded-md border border-border bg-background text-sm w-64"
				/>
			</div>
			<label class="flex items-center gap-2 text-sm">
				<input type="checkbox" bind:checked={dryRun} />
				<span>Dry run (preview prompt only)</span>
			</label>
		</div>
		<div class="flex gap-2">
			{#if showDryRun}
				<Button variant="outline" onclick={() => (showDryRun = false)}>
					<Eye size={16} />
					<span>Back to Plan</span>
				</Button>
			{/if}
			<Button
				onclick={handleGeneratePlan}
				disabled={isGenerating || selectedSpecs.size === 0}
			>
				{#if isGenerating}
					<RefreshCw size={16} class="animate-spin" />
				{:else}
					<Play size={16} />
				{/if}
				<span>{dryRun ? 'Preview Prompt' : 'Generate Plan'}</span>
			</Button>
		</div>
	</div>
</div>
