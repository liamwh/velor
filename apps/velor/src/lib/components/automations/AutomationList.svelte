<script lang="ts">
	import { onMount } from 'svelte';
	import { Plus, Search, Calendar as CalendarIcon } from 'lucide-svelte';
	import { automationsStore, automations, automationsLoading, automationsError } from '$lib/stores';
	import AutomationCard from './AutomationCard.svelte';
	import type { Automation } from '$lib/types';

	interface Props {
		onCreate?: () => void;
		onEdit?: (automation: Automation) => void;
		onViewRuns?: (name: string) => void;
		onDelete?: (name: string) => Promise<void>;
	}

	let { onCreate, onEdit, onViewRuns, onDelete }: Props = $props();

	let searchQuery = $state('');
	let filterEnabled: 'all' | 'enabled' | 'disabled' = $state('all');
	let runningAutomations = $state<Set<string>>(new Set());
	let deletingAutomations = $state<Set<string>>(new Set());

	// Use the derived stores directly
	const automationsList = $derived($automations);
	const loading = $derived($automationsLoading);
	const error = $derived($automationsError);

	// Filter automations based on search and filter
	const filteredAutomations = $derived(() => {
		let result = automationsList;

		// Filter by enabled state
		if (filterEnabled === 'enabled') {
			result = result.filter((a: Automation) => a.enabled);
		} else if (filterEnabled === 'disabled') {
			result = result.filter((a: Automation) => !a.enabled);
		}

		// Filter by search query
		if (searchQuery.trim()) {
			const query = searchQuery.toLowerCase();
			result = result.filter(
				(a: Automation) =>
					a.name.toLowerCase().includes(query) ||
					a.description?.toLowerCase().includes(query) ||
					a.prompt.toLowerCase().includes(query)
			);
		}

		// Sort: enabled first, then by name
		return result.sort((a: Automation, b: Automation) => {
			if (a.enabled !== b.enabled) {
				return a.enabled ? -1 : 1;
			}
			return a.name.localeCompare(b.name);
		});
	});

	onMount(() => {
		automationsStore.load();
	});

	async function handleToggle(name: string, enabled: boolean) {
		try {
			await automationsStore.toggle({ name, enabled });
		} catch (e) {
			console.error('Failed to toggle automation:', e);
		}
	}

	async function handleRun(name: string) {
		try {
			// eslint-disable-next-line svelte/prefer-svelte-reactivity -- Local copy for mutation
			runningAutomations = new Set(runningAutomations).add(name);
			await automationsStore.runNow(name);
			// Clear running state after a delay
			setTimeout(() => {
				// eslint-disable-next-line svelte/prefer-svelte-reactivity -- Local copy for mutation
				const newSet = new Set(runningAutomations);
				newSet.delete(name);
				runningAutomations = newSet;
			}, 5000);
		} catch (e) {
			console.error('Failed to run automation:', e);
			// eslint-disable-next-line svelte/prefer-svelte-reactivity -- Local copy for mutation
			const newSet = new Set(runningAutomations);
			newSet.delete(name);
			runningAutomations = newSet;
		}
	}

	function setFilter(filter: typeof filterEnabled) {
		filterEnabled = filter;
	}

	async function handleDelete(name: string) {
		if (onDelete) {
			// eslint-disable-next-line svelte/prefer-svelte-reactivity -- Local copy for mutation
			deletingAutomations = new Set(deletingAutomations).add(name);
			try {
				await onDelete(name);
			} catch (e) {
				console.error('Failed to delete automation:', e);
			} finally {
				// eslint-disable-next-line svelte/prefer-svelte-reactivity -- Local copy for mutation
				const newSet = new Set(deletingAutomations);
				newSet.delete(name);
				deletingAutomations = newSet;
			}
		} else {
			// Default behavior: use the store
			// eslint-disable-next-line svelte/prefer-svelte-reactivity -- Local copy for mutation
			deletingAutomations = new Set(deletingAutomations).add(name);
			try {
				await automationsStore.delete(name);
			} catch (e) {
				console.error('Failed to delete automation:', e);
			} finally {
				// eslint-disable-next-line svelte/prefer-svelte-reactivity -- Local copy for mutation
				const newSet = new Set(deletingAutomations);
				newSet.delete(name);
				deletingAutomations = newSet;
			}
		}
	}
</script>

<div class="flex flex-col h-full">
	<!-- Header -->
	<div class="flex items-center justify-between mb-4">
		<div class="flex items-center gap-3">
			<h2 class="flex items-center gap-2 text-lg font-semibold text-foreground">
				<CalendarIcon size={20} />
				<span>Automations</span>
			</h2>
			<span class="px-2 py-0.5 rounded-full bg-muted text-sm text-muted-foreground"
				>{filteredAutomations().length}</span
			>
		</div>
		<div class="flex items-center gap-2">
			{#if onCreate}
				<button
					class="flex items-center gap-2 px-3 py-2 rounded-lg text-sm font-medium transition-all bg-primary text-white hover:bg-[var(--color-accent-hover)]"
					onclick={onCreate}
				>
					<Plus size={16} />
					<span>New Automation</span>
				</button>
			{/if}
		</div>
	</div>

	<!-- Search and Filter -->
	<div class="flex items-center gap-3 mb-4">
		<div
			class="flex-1 flex items-center gap-2 px-3 py-2 rounded-lg bg-card border border-border focus-within:border-primary"
		>
			<Search size={16} class="text-muted-foreground" />
			<input
				type="text"
				placeholder="Search automations..."
				bind:value={searchQuery}
				class="flex-1 bg-transparent border-none outline-none text-foreground placeholder-muted-foreground"
				aria-label="Search automations"
			/>
		</div>
		<div class="flex items-center gap-1 p-1 rounded-lg bg-card border border-border">
			<button
				class="px-3 py-1.5 rounded text-sm text-muted-foreground hover:text-foreground hover:bg-muted transition-all {filterEnabled ===
				'all'
					? 'bg-muted text-foreground font-medium'
					: ''}"
				onclick={() => setFilter('all')}
			>
				All
			</button>
			<button
				class="px-3 py-1.5 rounded text-sm text-muted-foreground hover:text-foreground hover:bg-muted transition-all {filterEnabled ===
				'enabled'
					? 'bg-muted text-foreground font-medium'
					: ''}"
				onclick={() => setFilter('enabled')}
			>
				Enabled
			</button>
			<button
				class="px-3 py-1.5 rounded text-sm text-muted-foreground hover:text-foreground hover:bg-muted transition-all {filterEnabled ===
				'disabled'
					? 'bg-muted text-foreground font-medium'
					: ''}"
				onclick={() => setFilter('disabled')}
			>
				Disabled
			</button>
		</div>
	</div>

	<!-- Error State -->
	{#if error}
		<div class="flex flex-col items-center justify-center flex-1 gap-4 py-12 text-red-400">
			<span class="text-2xl">⚠</span>
			<p class="text-sm">{error}</p>
			<button
				class="flex items-center gap-2 px-3 py-2 rounded-lg text-sm font-medium transition-all bg-card border border-border text-foreground hover:bg-muted"
				onclick={() => automationsStore.load()}
			>
				Retry
			</button>
		</div>
	{/if}

	<!-- Loading State -->
	{#if loading}
		<div class="flex flex-col items-center justify-center flex-1 gap-4 py-12 text-muted-foreground">
			<div class="w-8 h-8 border-2 border-border border-t-primary rounded-full animate-spin"></div>
			<p>Loading automations...</p>
		</div>
	{:else if filteredAutomations().length === 0}
		<!-- Empty State -->
		<div class="flex flex-col items-center justify-center flex-1 gap-4 py-12 text-muted-foreground">
			<CalendarIcon size={48} />
			<h3 class="text-lg font-semibold text-muted-foreground mt-2">No Automations Found</h3>
			{#if searchQuery || filterEnabled !== 'all'}
				<p class="text-sm mb-2">Try adjusting your search or filter criteria.</p>
				<button
					class="flex items-center gap-2 px-3 py-2 rounded-lg text-sm font-medium transition-all bg-card border border-border text-foreground hover:bg-muted"
					onclick={() => (searchQuery = '', (filterEnabled = 'all'))}
				>
					Clear Filters
				</button>
			{:else}
				<p class="text-sm mb-2">Create your first automation to get started.</p>
				{#if onCreate}
					<button
						class="flex items-center gap-2 px-3 py-2 rounded-lg text-sm font-medium transition-all bg-primary text-white hover:bg-[var(--color-accent-hover)]"
						onclick={onCreate}
					>
						<Plus size={16} />
						<span>Create Automation</span>
					</button>
				{/if}
			{/if}
		</div>
	{:else}
		<!-- Automation Grid -->
		<div class="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-4 overflow-y-auto">
			{#each filteredAutomations() as automation (automation.name)}
				<AutomationCard
					{automation}
					onToggle={handleToggle}
					onRun={handleRun}
					onEdit={onEdit}
					onViewRuns={onViewRuns}
					onDelete={handleDelete}
					isToggling={loading}
					isRunning={runningAutomations.has(automation.name)}
					isDeleting={deletingAutomations.has(automation.name)}
				/>
			{/each}
		</div>
	{/if}
</div>
