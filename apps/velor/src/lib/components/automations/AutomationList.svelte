<script lang="ts">
	import { onMount } from 'svelte';
	import { Plus, Search, Filter, Calendar as CalendarIcon } from 'lucide-svelte';
	import { automationsStore, automationsLoading, automationsError } from '$lib/stores';
	import AutomationCard from './AutomationCard.svelte';
	import type { Automation } from '$lib/types';

	interface Props {
		onCreate?: () => void;
		onEdit?: (automation: Automation) => void;
		onViewRuns?: (name: string) => void;
	}

	let { onCreate, onEdit, onViewRuns }: Props = $props();

	let searchQuery = $state('');
	let filterEnabled: 'all' | 'enabled' | 'disabled' = $state('all');
	let runningAutomations = $state<Set<string>>(new Set());

	// Subscribe to store
	let storeState = $state(automationsStore.get());
	automationsStore.subscribe((state) => {
		storeState = state;
	});

	const automations = $derived(storeState.automations);
	const loading = $derived(storeState.loading);
	const error = $derived(storeState.error);

	// Filter automations based on search and filter
	const filteredAutomations = $derived(() => {
		let result = automations;

		// Filter by enabled state
		if (filterEnabled === 'enabled') {
			result = result.filter((a) => a.enabled);
		} else if (filterEnabled === 'disabled') {
			result = result.filter((a) => !a.enabled);
		}

		// Filter by search query
		if (searchQuery.trim()) {
			const query = searchQuery.toLowerCase();
			result = result.filter(
				(a) =>
					a.name.toLowerCase().includes(query) ||
					a.description?.toLowerCase().includes(query) ||
					a.prompt.toLowerCase().includes(query)
			);
		}

		// Sort: enabled first, then by name
		return result.sort((a, b) => {
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
			runningAutomations = new Set(runningAutomations).add(name);
			await automationsStore.runNow(name);
			// Clear running state after a delay
			setTimeout(() => {
				runningAutomations = new Set(runningAutomations).filter((x) => x !== name);
			}, 5000);
		} catch (e) {
			console.error('Failed to run automation:', e);
			runningAutomations = new Set(runningAutomations).filter((x) => x !== name);
		}
	}

	function setFilter(filter: typeof filterEnabled) {
		filterEnabled = filter;
	}
</script>

<div class="automation-list">
	<!-- Header -->
	<div class="list-header">
		<div class="header-left">
			<h2 class="header-title">
				<CalendarIcon size={20} />
				<span>Automations</span>
			</h2>
			<span class="count-badge">{filteredAutomations().length}</span>
		</div>
		<div class="header-actions">
			{#if onCreate}
				<button class="btn-primary" onclick={onCreate}>
					<Plus size={16} />
					<span>New Automation</span>
				</button>
			{/if}
		</div>
	</div>

	<!-- Search and Filter -->
	<div class="list-controls">
		<div class="search-box">
			<Search size={16} class="search-icon" />
			<input
				type="text"
				placeholder="Search automations..."
				bind:value={searchQuery}
				class="search-input"
				aria-label="Search automations"
			/>
		</div>
		<div class="filter-group">
			<button
				class="filter-btn"
				class:active={filterEnabled === 'all'}
				onclick={() => setFilter('all')}
			>
				All
			</button>
			<button
				class="filter-btn"
				class:active={filterEnabled === 'enabled'}
				onclick={() => setFilter('enabled')}
			>
				Enabled
			</button>
			<button
				class="filter-btn"
				class:active={filterEnabled === 'disabled'}
				onclick={() => setFilter('disabled')}
			>
				Disabled
			</button>
		</div>
	</div>

	<!-- Error State -->
	{#if error}
		<div class="error-state">
			<span class="error-icon">⚠</span>
			<p class="error-message">{error}</p>
			<button class="btn-secondary" onclick={() => automationsStore.load()}>Retry</button>
		</div>
	{/if}

	<!-- Loading State -->
	{#if loading}
		<div class="loading-state">
			<div class="spinner"></div>
			<p>Loading automations...</p>
		</div>
	{:else if filteredAutomations().length === 0}
		<!-- Empty State -->
		<div class="empty-state">
			<CalendarIcon size={48} />
			<h3>No Automations Found</h3>
			{#if searchQuery || filterEnabled !== 'all'}
				<p>Try adjusting your search or filter criteria.</p>
				<button class="btn-secondary" onclick={() => (searchQuery = '', (filterEnabled = 'all'))}>
					Clear Filters
				</button>
			{:else}
				<p>Create your first automation to get started.</p>
				{#if onCreate}
					<button class="btn-primary" onclick={onCreate}>
						<Plus size={16} />
						<span>Create Automation</span>
					</button>
				{/if}
			{/if}
		</div>
	{:else}
		<!-- Automation Grid -->
		<div class="automation-grid">
			{#each filteredAutomations() as automation (automation.name)}
				<AutomationCard
					{automation}
					onToggle={handleToggle}
					onRun={handleRun}
					onEdit={onEdit}
					onViewRuns={onViewRuns}
					isToggling={loading}
					isRunning={runningAutomations.has(automation.name)}
				/>
			{/each}
		</div>
	{/if}
</div>

<style>
	.automation-list {
		@apply flex flex-col h-full;
	}

	.list-header {
		@apply flex items-center justify-between mb-4;
	}

	.header-left {
		@apply flex items-center gap-3;
	}

	.header-title {
		@apply flex items-center gap-2 text-lg font-semibold text-[var(--color-text-primary)];
	}

	.count-badge {
		@apply px-2 py-0.5 rounded-full bg-[var(--color-bg-tertiary)] text-sm text-[var(--color-text-secondary)];
	}

	.header-actions {
		@apply flex items-center gap-2;
	}

	.list-controls {
		@apply flex items-center gap-3 mb-4;
	}

	.search-box {
		@apply flex-1 flex items-center gap-2 px-3 py-2 rounded-lg bg-[var(--color-bg-secondary)] border border-[var(--color-border)] focus-within:border-[var(--color-accent-primary)];
	}

	.search-icon {
		@apply text-[var(--color-text-muted)];
	}

	.search-input {
		@apply flex-1 bg-transparent border-none outline-none text-[var(--color-text-primary)] placeholder-[var(--color-text-muted)];
	}

	.filter-group {
		@apply flex items-center gap-1 p-1 rounded-lg bg-[var(--color-bg-secondary)] border border-[var(--color-border)];
	}

	.filter-btn {
		@apply px-3 py-1.5 rounded text-sm text-[var(--color-text-secondary)] hover:text-[var(--color-text-primary)] hover:bg-[var(--color-bg-tertiary)] transition-all;
	}

	.filter-btn.active {
		@apply bg-[var(--color-bg-tertiary)] text-[var(--color-text-primary)] font-medium;
	}

	.btn-primary,
	.btn-secondary {
		@apply flex items-center gap-2 px-3 py-2 rounded-lg text-sm font-medium transition-all;
	}

	.btn-primary {
		@apply bg-[var(--color-accent-primary)] text-white hover:bg-[var(--color-accent-hover)];
	}

	.btn-secondary {
		@apply bg-[var(--color-bg-secondary)] border border-[var(--color-border)] text-[var(--color-text-primary)] hover:bg-[var(--color-bg-tertiary)];
	}

	.error-state,
	.loading-state,
	.empty-state {
		@apply flex flex-col items-center justify-center flex-1 gap-4 py-12;
	}

	.error-state {
		@apply text-red-400;
	}

	.error-icon {
		@apply text-2xl;
	}

	.error-message {
		@apply text-sm;
	}

	.loading-state {
		@apply text-[var(--color-text-secondary)];
	}

	.spinner {
		@apply w-8 h-8 border-2 border-[var(--color-border)] border-t-[var(--color-accent-primary)] rounded-full animate-spin;
	}

	.empty-state {
		@apply text-[var(--color-text-muted)];
	}

	.empty-state h3 {
		@apply text-lg font-semibold text-[var(--color-text-secondary)] mt-2;
	}

	.empty-state p {
		@apply text-sm mb-2;
	}

	.automation-grid {
		@apply grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-4 overflow-y-auto;
	}
</style>
