<script lang="ts">
	import { onMount } from 'svelte';
	import {
		History,
		Search,
		CheckCircle,
		XCircle,
		Clock,
		Loader2,
		Trash2,
		ChevronDown,
		AlertCircle
	} from 'lucide-svelte';
	import {
		sessionsStore,
		sessions,
		sessionStats,
		sessionsLoading,
		sessionsError,
		sessionsHasMore
	} from '$lib/stores';
	import type { ExecutionRecord, ExecutionState } from '$lib/types';

	interface Props {
		onSelect?: (session: ExecutionRecord) => void;
	}

	let { onSelect }: Props = $props();

	let searchQuery = $state('');
	let filterState: 'all' | 'completed' | 'failed' | 'cancelled' | 'active' = $state('all');
	let deletingId = $state<string | null>(null);
	let showDeleteConfirm = $state<string | null>(null);

	// Use the derived stores directly
	const sessionsList = $derived($sessions);
	const stats = $derived($sessionStats);
	const loading = $derived($sessionsLoading);
	const error = $derived($sessionsError);
	const hasMore = $derived($sessionsHasMore);

	// Filter sessions based on search and filter
	const filteredSessions = $derived(() => {
		let result = sessionsList;

		// Filter by state
		if (filterState === 'completed') {
			result = result.filter((s: ExecutionRecord) => s.state === 'completed');
		} else if (filterState === 'failed') {
			result = result.filter((s: ExecutionRecord) => s.state === 'failed');
		} else if (filterState === 'cancelled') {
			result = result.filter((s: ExecutionRecord) => s.state === 'cancelled');
		} else if (filterState === 'active') {
			result = result.filter(
				(s: ExecutionRecord) =>
					s.state === 'running' || s.state === 'rendering' || s.state === 'pending'
			);
		}

		// Filter by search query
		if (searchQuery.trim()) {
			const query = searchQuery.toLowerCase();
			result = result.filter(
				(s: ExecutionRecord) =>
					s.id.toLowerCase().includes(query) || s.prompt_name.toLowerCase().includes(query)
			);
		}

		return result;
	});

	onMount(() => {
		sessionsStore.load(20);
	});

	function getStateClass(state: ExecutionState): string {
		switch (state) {
			case 'completed':
				return 'bg-[var(--color-success)]/20 text-[var(--color-success)]';
			case 'failed':
				return 'bg-red-500/20 text-red-400';
			case 'cancelled':
				return 'bg-gray-500/20 text-gray-400';
			case 'running':
			case 'rendering':
				return 'bg-[var(--color-accent-primary)]/20 text-[var(--color-accent-primary)]';
			case 'retrying':
				return 'bg-yellow-500/20 text-yellow-400';
			default:
				return 'bg-muted text-muted-foreground';
		}
	}

	function formatDuration(ms: number): string {
		if (ms < 1000) return `${ms}ms`;
		if (ms < 60000) return `${(ms / 1000).toFixed(1)}s`;
		return `${(ms / 60000).toFixed(1)}m`;
	}

	function formatDate(isoString: string): string {
		const date = new Date(isoString);
		return date.toLocaleString(undefined, {
			month: 'short',
			day: 'numeric',
			hour: '2-digit',
			minute: '2-digit'
		});
	}

	function setFilter(filter: typeof filterState) {
		filterState = filter;
	}

	async function handleLoadMore() {
		await sessionsStore.loadMore(20);
	}

	function handleSelect(session: ExecutionRecord) {
		if (onSelect) {
			onSelect(session);
		}
	}

	function confirmDelete(id: string) {
		showDeleteConfirm = id;
	}

	function cancelDelete() {
		showDeleteConfirm = null;
	}

	async function handleDelete(id: string) {
		deletingId = id;
		showDeleteConfirm = null;
		try {
			await sessionsStore.delete(id);
		} catch (e) {
			console.error('Failed to delete session:', e);
		} finally {
			deletingId = null;
		}
	}
</script>

<div class="flex flex-col h-full">
	<!-- Header -->
	<div class="flex items-center justify-between mb-4">
		<div class="flex items-center gap-3">
			<h2 class="flex items-center gap-2 text-lg font-semibold text-foreground">
				<History size={20} />
				<span>Execution History</span>
			</h2>
			{#if stats}
				<span class="px-2 py-0.5 rounded-full bg-muted text-sm text-muted-foreground">
					{stats.total} sessions
				</span>
			{/if}
		</div>
	</div>

	<!-- Stats Bar -->
	{#if stats}
		<div class="flex items-center gap-4 mb-4 text-sm">
			<div class="flex items-center gap-1.5 text-[var(--color-success)]">
				<CheckCircle size={14} />
				<span>{stats.completed} completed</span>
			</div>
			<div class="flex items-center gap-1.5 text-red-400">
				<XCircle size={14} />
				<span>{stats.failed} failed</span>
			</div>
			{#if stats.cancelled > 0}
				<div class="flex items-center gap-1.5 text-gray-400">
					<XCircle size={14} />
					<span>{stats.cancelled} cancelled</span>
				</div>
			{/if}
			{#if stats.active > 0}
				<div class="flex items-center gap-1.5 text-[var(--color-accent-primary)]">
					<Loader2 size={14} class="animate-spin" />
					<span>{stats.active} active</span>
				</div>
			{/if}
		</div>
	{/if}

	<!-- Search and Filter -->
	<div class="flex items-center gap-3 mb-4">
		<div
			class="flex-1 flex items-center gap-2 px-3 py-2 rounded-lg bg-card border border-border focus-within:border-primary"
		>
			<Search size={16} class="text-muted-foreground" />
			<input
				type="text"
				placeholder="Search by ID or prompt name..."
				bind:value={searchQuery}
				class="flex-1 bg-transparent border-none outline-none text-foreground placeholder-muted-foreground"
				aria-label="Search sessions"
			/>
		</div>
		<div class="flex items-center gap-1 p-1 rounded-lg bg-card border border-border">
			<button
				class="px-3 py-1.5 rounded text-sm text-muted-foreground hover:text-foreground hover:bg-muted transition-all {filterState ===
				'all'
					? 'bg-muted text-foreground font-medium'
					: ''}"
				onclick={() => setFilter('all')}
			>
				All
			</button>
			<button
				class="px-3 py-1.5 rounded text-sm text-muted-foreground hover:text-foreground hover:bg-muted transition-all {filterState ===
				'active'
					? 'bg-muted text-foreground font-medium'
					: ''}"
				onclick={() => setFilter('active')}
			>
				Active
			</button>
			<button
				class="px-3 py-1.5 rounded text-sm text-muted-foreground hover:text-foreground hover:bg-muted transition-all {filterState ===
				'completed'
					? 'bg-muted text-foreground font-medium'
					: ''}"
				onclick={() => setFilter('completed')}
			>
				Completed
			</button>
			<button
				class="px-3 py-1.5 rounded text-sm text-muted-foreground hover:text-foreground hover:bg-muted transition-all {filterState ===
				'failed'
					? 'bg-muted text-foreground font-medium'
					: ''}"
				onclick={() => setFilter('failed')}
			>
				Failed
			</button>
		</div>
	</div>

	<!-- Error State -->
	{#if error}
		<div class="flex flex-col items-center justify-center flex-1 gap-4 py-12 text-red-400">
			<AlertCircle size={32} />
			<p class="text-sm">{error}</p>
			<button
				class="flex items-center gap-2 px-3 py-2 rounded-lg text-sm font-medium transition-all bg-card border border-border text-foreground hover:bg-muted"
				onclick={() => sessionsStore.load(20)}
			>
				Retry
			</button>
		</div>
	{:else}
		<!-- Sessions Table -->
		<div class="flex-1 overflow-hidden">
			<div class="h-full overflow-y-auto rounded-lg border border-border">
				<table class="w-full">
					<thead class="sticky top-0 bg-card border-b border-border">
						<tr>
							<th class="px-4 py-3 text-left text-sm font-medium text-muted-foreground">State</th>
							<th class="px-4 py-3 text-left text-sm font-medium text-muted-foreground">Prompt</th>
							<th class="px-4 py-3 text-left text-sm font-medium text-muted-foreground">Started</th>
							<th class="px-4 py-3 text-left text-sm font-medium text-muted-foreground">Duration</th>
							<th class="px-4 py-3 text-left text-sm font-medium text-muted-foreground">Iterations</th>
							<th class="px-4 py-3 text-right text-sm font-medium text-muted-foreground">Actions</th>
						</tr>
					</thead>
					<tbody class="divide-y divide-border">
						{#if loading && sessionsList.length === 0}
							<tr>
								<td colspan="6" class="px-4 py-12 text-center text-muted-foreground">
									<div class="flex flex-col items-center gap-2">
										<Loader2 size={24} class="animate-spin" />
										<span>Loading sessions...</span>
									</div>
								</td>
							</tr>
						{:else if filteredSessions().length === 0}
							<tr>
								<td colspan="6" class="px-4 py-12 text-center text-muted-foreground">
									<div class="flex flex-col items-center gap-2">
										<History size={32} />
										<span>{searchQuery || filterState !== 'all' ? 'No sessions match your filters' : 'No execution history yet'}</span>
									</div>
								</td>
							</tr>
						{:else}
							{#each filteredSessions() as session (session.id)}
								<tr
									class="hover:bg-muted/50 cursor-pointer transition-colors"
									onclick={() => handleSelect(session)}
									onkeydown={(e) => e.key === 'Enter' && handleSelect(session)}
									role="button"
									tabindex="0"
								>
									<td class="px-4 py-3">
										<div class="flex items-center gap-2">
											{#if session.state === 'completed'}
												<CheckCircle size={14} class="text-[var(--color-success)]" />
											{:else if session.state === 'failed' || session.state === 'cancelled'}
												<XCircle size={14} class="text-red-400" />
											{:else if session.state === 'running' || session.state === 'rendering' || session.state === 'retrying'}
												<Loader2 size={14} class="text-[var(--color-accent-primary)] animate-spin" />
											{:else}
												<Clock size={14} class="text-muted-foreground" />
											{/if}
											<span
												class="px-2 py-0.5 rounded text-xs font-medium {getStateClass(session.state)}"
											>
												{session.state}
											</span>
										</div>
									</td>
									<td class="px-4 py-3">
										<code class="px-1.5 py-0.5 rounded bg-muted text-xs font-mono">
											{session.prompt_name}
										</code>
									</td>
									<td class="px-4 py-3 text-sm text-muted-foreground">
										{formatDate(session.started_at)}
									</td>
									<td class="px-4 py-3 text-sm text-muted-foreground">
										{formatDuration(session.metrics.duration_ms)}
									</td>
									<td class="px-4 py-3 text-sm text-muted-foreground">
										{session.iteration}
									</td>
									<td class="px-4 py-3 text-right">
										{#if showDeleteConfirm === session.id}
											<div class="flex items-center justify-end gap-1">
												<button
													class="px-2 py-1 rounded text-xs bg-red-500/20 text-red-400 hover:bg-red-500/30 transition-all"
													onclick={(e) => { e.stopPropagation(); handleDelete(session.id); }}
													disabled={deletingId === session.id}
												>
													{deletingId === session.id ? 'Deleting...' : 'Confirm'}
												</button>
												<button
													class="px-2 py-1 rounded text-xs bg-muted text-muted-foreground hover:bg-muted/80 transition-all"
													onclick={(e) => { e.stopPropagation(); cancelDelete(); }}
												>
													Cancel
												</button>
											</div>
										{:else}
											<button
												class="p-1.5 rounded text-muted-foreground hover:text-red-400 hover:bg-muted transition-all disabled:opacity-50"
												onclick={(e) => { e.stopPropagation(); confirmDelete(session.id); }}
												disabled={deletingId === session.id}
												aria-label="Delete session"
												title="Delete"
											>
												{#if deletingId === session.id}
													<Loader2 size={14} class="animate-spin" />
												{:else}
													<Trash2 size={14} />
												{/if}
											</button>
										{/if}
									</td>
								</tr>
							{/each}
						{/if}
					</tbody>
				</table>
			</div>

			<!-- Load More -->
			{#if hasMore}
				<div class="flex justify-center py-4">
					<button
						class="flex items-center gap-2 px-4 py-2 rounded-lg text-sm font-medium transition-all bg-card border border-border text-foreground hover:bg-muted disabled:opacity-50"
						onclick={handleLoadMore}
						disabled={loading}
					>
						{#if loading}
							<Loader2 size={16} class="animate-spin" />
						{:else}
							<ChevronDown size={16} />
						{/if}
						<span>Load More</span>
					</button>
				</div>
			{/if}
		</div>
	{/if}
</div>
