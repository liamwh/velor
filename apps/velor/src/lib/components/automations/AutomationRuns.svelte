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

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="runs-overlay" onclick={onClose} onkeydown={(e) => e.key === 'Escape' && onClose?.()}>
	<!-- svelte-ignore a11y_no_static_element_interactions -->
	<div class="runs-dialog" onclick={(e) => e.stopPropagation()} onkeydown={(e) => e.stopPropagation()}>
		<!-- Header -->
		<div class="runs-header">
			<div class="header-content">
				<h2>Run History</h2>
				<span class="automation-name">{automationName}</span>
			</div>
			<div class="header-actions">
				<button class="icon-btn" onclick={loadRuns} aria-label="Refresh" title="Refresh">
					<span class:spinning={loading}>
						<RefreshCw size={18} />
					</span>
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
									<!-- svelte-ignore svelte_component_deprecated -->
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
		position: fixed;
		inset: 0;
		z-index: 50;
		display: flex;
		align-items: center;
		justify-content: center;
		background-color: rgb(0 0 0 / 0.6);
		backdrop-filter: blur(4px);
		padding: 1rem;
	}

	.runs-dialog {
		width: 100%;
		max-width: 42rem;
		max-height: 80vh;
		display: flex;
		flex-direction: column;
		background-color: var(--color-bg-secondary);
		border: 1px solid var(--color-border);
		border-radius: 0.5rem;
		box-shadow: 0 20px 25px -5px rgb(0 0 0 / 0.1), 0 8px 10px -6px rgb(0 0 0 / 0.1);
	}

	.runs-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		padding-left: 1.5rem;
		padding-right: 1.5rem;
		padding-top: 1rem;
		padding-bottom: 1rem;
		border-bottom: 1px solid var(--color-border);
	}

	.header-content {
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
	}

	.header-content h2 {
		font-size: 1.125rem;
		font-weight: 600;
		color: var(--color-text-primary);
	}

	.automation-name {
		font-size: 0.875rem;
		color: var(--color-text-muted);
	}

	.header-actions {
		display: flex;
		align-items: center;
		gap: 0.25rem;
	}

	.icon-btn {
		padding: 0.5rem;
		border-radius: 0.25rem;
		color: var(--color-text-secondary);
		transition: all 0.15s ease-in-out;
	}

	.icon-btn:hover {
		color: var(--color-text-primary);
		background-color: var(--color-bg-tertiary);
	}

	.runs-body {
		flex: 1;
		overflow-y: auto;
		padding-left: 1.5rem;
		padding-right: 1.5rem;
		padding-top: 1rem;
		padding-bottom: 1rem;
	}

	.error-state,
	.loading-state,
	.empty-state {
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		gap: 1rem;
		padding-top: 3rem;
		padding-bottom: 3rem;
		color: var(--color-text-muted);
	}

	.error-state {
		color: rgb(248 113 113);
	}

	.spinner {
		width: 2rem;
		height: 2rem;
		border: 2px solid var(--color-border);
		border-top-color: var(--color-accent-primary);
		border-radius: 9999px;
		animation: spin 1s linear infinite;
	}

	.empty-state h3 {
		font-size: 1.125rem;
		font-weight: 600;
		color: var(--color-text-secondary);
	}

	.empty-state p {
		font-size: 0.875rem;
	}

	.runs-list {
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
	}

	.run-card {
		padding: 1rem;
		border-radius: 0.5rem;
		background-color: var(--color-bg-primary);
		border: 1px solid var(--color-border);
		transition: all 0.15s ease-in-out;
	}

	.run-card:hover {
		border-color: var(--color-border-hover);
	}

	.run-card.failed {
		border-color: rgb(127 29 29 / 0.5);
		background-color: rgb(127 29 29 / 0.2);
	}

	.run-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		margin-bottom: 0.75rem;
	}

	.run-status {
		display: flex;
		align-items: center;
		gap: 0.5rem;
	}

	.status-label {
		font-size: 0.875rem;
		font-weight: 500;
	}

	/* svelte-ignore css_unused_selector */
	.status-completed {
		color: var(--color-success);
	}

	/* svelte-ignore css_unused_selector */
	.status-failed,
	.status-cancelled {
		color: rgb(248 113 113);
	}

	/* svelte-ignore css_unused_selector */
	.status-running {
		color: var(--color-accent-primary);
		animation: pulse 2s cubic-bezier(0.4, 0, 0.6, 1) infinite;
	}

	/* svelte-ignore css_unused_selector */
	.status-pending {
		color: var(--color-text-muted);
	}

	/* svelte-ignore css_unused_selector */
	.status-unknown {
		color: var(--color-state-warning-text, oklch(0.75 0.15 45));
	}

	@keyframes pulse {
		0%, 100% {
			opacity: 1;
		}
		50% {
			opacity: 0.5;
		}
	}

	.run-time {
		font-size: 0.75rem;
		color: var(--color-text-muted);
	}

	.run-details {
		display: grid;
		grid-template-columns: repeat(2, minmax(0, 1fr));
		gap: 0.375rem;
		margin-bottom: 0.75rem;
	}

	.detail-row {
		display: flex;
		align-items: center;
		justify-content: space-between;
		font-size: 0.875rem;
	}

	.detail-label {
		color: var(--color-text-muted);
	}

	.detail-value {
		color: var(--color-text-secondary);
		font-family: ui-monospace, monospace;
		font-size: 0.75rem;
	}

	.run-error {
		display: flex;
		align-items: flex-start;
		gap: 0.5rem;
		padding: 0.5rem;
		border-radius: 0.25rem;
		background-color: rgb(127 29 29 / 0.3);
		border: 1px solid rgb(185 28 28 / 0.3);
		color: rgb(253 186 116);
		font-size: 0.875rem;
	}

	.error-message {
		flex: 1;
		overflow-wrap: break-word;
	}

	.run-output {
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
	}

	.output-label {
		font-size: 0.75rem;
		font-weight: 500;
		color: var(--color-text-muted);
	}

	.output-content {
		padding: 0.5rem;
		border-radius: 0.25rem;
		background-color: var(--color-bg-tertiary);
		font-size: 0.75rem;
		color: var(--color-text-secondary);
		font-family: ui-monospace, monospace;
		white-space: pre-wrap;
		overflow-wrap: break-word;
		overflow-x: auto;
	}

	.btn-secondary {
		padding-left: 1rem;
		padding-right: 1rem;
		padding-top: 0.5rem;
		padding-bottom: 0.5rem;
		border-radius: 0.5rem;
		font-size: 0.875rem;
		font-weight: 500;
		background-color: var(--color-bg-tertiary);
		border: 1px solid var(--color-border);
		color: var(--color-text-primary);
		transition: all 0.15s ease-in-out;
	}

	.btn-secondary:hover {
		background-color: var(--color-border);
	}

	/* svelte-ignore css_unused_selector */
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
