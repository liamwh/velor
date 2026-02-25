<script lang="ts">
	import { onMount } from 'svelte';
	import { X, CheckCircle, XCircle, Clock, RefreshCw, AlertTriangle } from 'lucide-svelte';
	import { automationsStore, automationRuns } from '$lib/stores';
	import type { AutomationRun } from '$lib/types';

	interface Props {
		automationName: string;
		onClose?: () => void;
	}

	let { automationName, onClose }: Props = $props();

	let loading = $state(false);
	let error = $state<string | null>(null);

	// Get runs from store
	const runs = $derived($automationRuns);

	onMount(() => {
		loadRuns();
	});

	async function loadRuns() {
		loading = true;
		error = null;
		try {
			await automationsStore.loadRuns(automationName, 50);
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load runs';
		} finally {
			loading = false;
		}
	}

	function getStatusIcon(status: AutomationRun['status']) {
		switch (status) {
			case 'Completed':
				return { icon: CheckCircle, class: 'status-completed', label: 'Completed' };
			case 'Failed':
				return { icon: XCircle, class: 'status-failed', label: 'Failed' };
			case 'Running':
				return { icon: RefreshCw, class: 'status-running', label: 'Running' };
			case 'Pending':
				return { icon: Clock, class: 'status-pending', label: 'Pending' };
			case 'Cancelled':
				return { icon: XCircle, class: 'status-cancelled', label: 'Cancelled' };
			default:
				return { icon: AlertTriangle, class: 'status-unknown', label: 'Unknown' };
		}
	}

	function formatTimestamp(timestamp: string | undefined): string {
		if (!timestamp) return '—';
		return new Date(timestamp).toLocaleString();
	}

	function formatDuration(started: string, completed: string | undefined): string {
		if (!completed) return '—';
		const start = new Date(started).getTime();
		const end = new Date(completed).getTime();
		const duration = end - start;
		if (duration < 1000) return `${duration}ms`;
		if (duration < 60000) return `${(duration / 1000).toFixed(1)}s`;
		return `${(duration / 60000).toFixed(1)}m`;
	}

	function getRelativeTime(timestamp: string): string {
		const now = Date.now();
		const then = new Date(timestamp).getTime();
		const diff = now - then;

		if (diff < 60000) return 'just now';
		if (diff < 3600000) return `${Math.floor(diff / 60000)}m ago`;
		if (diff < 86400000) return `${Math.floor(diff / 3600000)}h ago`;
		return `${Math.floor(diff / 86400000)}d ago`;
	}
</script>

<div class="runs-overlay" onclick={onClose}>
	<div class="runs-dialog" onclick={(e) => e.stopPropagation()}>
		<!-- Header -->
		<div class="runs-header">
			<div class="header-content">
				<h2>Run History</h2>
				<span class="automation-name">{automationName}</span>
			</div>
			<div class="header-actions">
				<button class="icon-btn" onclick={loadRuns} aria-label="Refresh" title="Refresh">
					<RefreshCw size={18} class:spinning={loading} />
				</button>
				<button class="icon-btn" onclick={onClose} aria-label="Close">
					<X size={20} />
				</button>
			</div>
		</div>

		<!-- Runs List -->
		<div class="runs-body">
			{#if error}
				<div class="error-state">
					<AlertTriangle size={32} />
					<p>{error}</p>
					<button class="btn-secondary" onclick={loadRuns}>Retry</button>
				</div>
			{:else if loading}
				<div class="loading-state">
					<div class="spinner"></div>
					<p>Loading runs...</p>
				</div>
			{:else if runs.length === 0}
				<div class="empty-state">
					<Clock size={48} />
					<h3>No Runs Yet</h3>
					<p>This automation hasn't been run yet.</p>
				</div>
			{:else}
				<div class="runs-list">
					{#each runs as run (run.id)}
						<div class="run-card" class:failed={run.status === 'Failed'}>
							<div class="run-header">
								<div class="run-status">
									<svelte:component this={getStatusIcon(run.status).icon} size={18} class={getStatusIcon(run.status).class} />
									<span class="status-label">{getStatusIcon(run.status).label}</span>
								</div>
								<span class="run-time">{getRelativeTime(run.scheduled_for)}</span>
							</div>

							<div class="run-details">
								<div class="detail-row">
									<span class="detail-label">Scheduled</span>
									<span class="detail-value">{formatTimestamp(run.scheduled_for)}</span>
								</div>
								<div class="detail-row">
									<span class="detail-label">Started</span>
									<span class="detail-value">{formatTimestamp(run.started_at)}</span>
								</div>
								{#if run.completed_at}
									<div class="detail-row">
										<span class="detail-label">Completed</span>
										<span class="detail-value">{formatTimestamp(run.completed_at)}</span>
									</div>
								{/if}
								<div class="detail-row">
									<span class="detail-label">Duration</span>
									<span class="detail-value">{run.duration_ms ? `${(run.duration_ms / 1000).toFixed(1)}s` : '—'}</span>
								</div>
								<div class="detail-row">
									<span class="detail-label">Iterations</span>
									<span class="detail-value">{run.iterations_completed}</span>
								</div>
							</div>

							{#if run.error}
								<div class="run-error">
									<AlertTriangle size={14} />
									<span class="error-message">{run.error}</span>
								</div>
							{/if}

							{#if run.output}
								<div class="run-output">
									<span class="output-label">Output Preview</span>
									<pre class="output-content">{run.output}</pre>
								</div>
							{/if}
						</div>
					{/each}
				</div>
			{/if}
		</div>
	</div>
</div>

<style>
	.runs-overlay {
		@apply fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm p-4;
	}

	.runs-dialog {
		@apply w-full max-w-2xl max-h-[80vh] flex flex-col bg-[var(--color-bg-secondary)] border border-[var(--color-border)] rounded-lg shadow-xl;
	}

	.runs-header {
		@apply flex items-center justify-between px-6 py-4 border-b border-[var(--color-border)];
	}

	.header-content h2 {
		@apply text-lg font-semibold text-[var(--color-text-primary)];
	}

	.automation-name {
		@apply text-sm text-[var(--color-text-muted)];
	}

	.header-actions {
		@apply flex items-center gap-1;
	}

	.icon-btn {
		@apply p-2 rounded text-[var(--color-text-secondary)] hover:text-[var(--color-text-primary)] hover:bg-[var(--color-bg-tertiary)] transition-all;
	}

	.runs-body {
		@apply flex-1 overflow-y-auto px-6 py-4;
	}

	.error-state,
	.loading-state,
	.empty-state {
		@apply flex flex-col items-center justify-center gap-4 py-12 text-[var(--color-text-muted)];
	}

	.error-state {
		@apply text-red-400;
	}

	.spinner {
		@apply w-8 h-8 border-2 border-[var(--color-border)] border-t-[var(--color-accent-primary)] rounded-full animate-spin;
	}

	.empty-state h3 {
		@apply text-lg font-semibold text-[var(--color-text-secondary)];
	}

	.empty-state p {
		@apply text-sm;
	}

	.runs-list {
		@apply space-y-3;
	}

	.run-card {
		@apply p-4 rounded-lg bg-[var(--color-bg-primary)] border border-[var(--color-border)] hover:border-[var(--color-border-hover)] transition-all;
	}

	.run-card.failed {
		@apply border-red-900/50 bg-red-950/20;
	}

	.run-header {
		@apply flex items-center justify-between mb-3;
	}

	.run-status {
		@apply flex items-center gap-2;
	}

	.status-label {
		@apply text-sm font-medium;
	}

	.status-completed {
		@apply text-[var(--color-success)];
	}

	.status-failed,
	.status-cancelled {
		@apply text-red-400;
	}

	.status-running {
		@apply text-[var(--color-accent-primary)] animate-pulse;
	}

	.status-pending {
		@apply text-[var(--color-text-muted)];
	}

	.status-unknown {
		@apply text-[var(--color-warning)];
	}

	.run-time {
		@apply text-xs text-[var(--color-text-muted)];
	}

	.run-details {
		@apply grid grid-cols-2 gap-1.5 mb-3;
	}

	.detail-row {
		@apply flex items-center justify-between text-sm;
	}

	.detail-label {
		@apply text-[var(--color-text-muted)];
	}

	.detail-value {
		@apply text-[var(--color-text-secondary)] font-mono text-xs;
	}

	.run-error {
		@apply flex items-start gap-2 p-2 rounded bg-red-950/30 border border-red-900/30 text-red-300 text-sm;
	}

	.error-message {
		@apply flex-1 break-words;
	}

	.run-output {
		@apply space-y-1;
	}

	.output-label {
		@apply text-xs font-medium text-[var(--color-text-muted)];
	}

	.output-content {
		@apply p-2 rounded bg-[var(--color-bg-tertiary)] text-xs text-[var(--color-text-secondary)] font-mono whitespace-pre-wrap break-words overflow-x-auto;
	}

	.btn-secondary {
		@apply px-4 py-2 rounded-lg text-sm font-medium bg-[var(--color-bg-tertiary)] border border-[var(--color-border)] text-[var(--color-text-primary)] hover:bg-[var(--color-border)] transition-all;
	}

	.spinning {
		animation: spin 1s linear infinite;
	}

	@keyframes spin {
		from {
			transform: rotate(0deg);
		}
		to {
			transform: rotate(360deg);
		}
	}
</style>
