<script lang="ts">
	import { ExecutionState } from '$lib/types';
	import type { ExecutionRecord } from '$lib/types';
	import { Activity, CheckCircle, XCircle, Loader2, PauseCircle, AlertCircle } from 'lucide-svelte';

	interface Props {
		execution: ExecutionRecord | null;
		showMetrics?: boolean;
		compact?: boolean;
	}

	let { execution, showMetrics = true, compact = false }: Props = $props();

	/**
	 * Get the icon for the current execution state
	 */
	function getStateIcon() {
		if (!execution) return null;

		switch (execution.state) {
			case ExecutionState.Pending:
				return PauseCircle;
			case ExecutionState.Rendering:
			case ExecutionState.Running:
				return Loader2;
			case ExecutionState.Retrying:
				return Activity;
			case ExecutionState.Completed:
				return CheckCircle;
			case ExecutionState.Failed:
				return XCircle;
			case ExecutionState.Cancelled:
				return AlertCircle;
			default:
				return Activity;
		}
	}

	/**
	 * Get the CSS class for the current execution state
	 */
	function getStateClass(): string {
		if (!execution) return '';

		switch (execution.state) {
			case ExecutionState.Pending:
				return 'state-pending';
			case ExecutionState.Rendering:
			case ExecutionState.Running:
				return 'state-running';
			case ExecutionState.Retrying:
				return 'state-retrying';
			case ExecutionState.Completed:
				return 'state-completed';
			case ExecutionState.Failed:
				return 'state-failed';
			case ExecutionState.Cancelled:
				return 'state-cancelled';
			default:
				return '';
		}
	}

	/**
	 * Get the human-readable state label
	 */
	function getStateLabel(): string {
		if (!execution) return 'No execution';

		switch (execution.state) {
			case ExecutionState.Pending:
				return 'Pending';
			case ExecutionState.Rendering:
				return 'Rendering';
			case ExecutionState.Running:
				return 'Running';
			case ExecutionState.Retrying:
				return 'Retrying';
			case ExecutionState.Completed:
				return 'Completed';
			case ExecutionState.Failed:
				return 'Failed';
			case ExecutionState.Cancelled:
				return 'Cancelled';
			default:
				return 'Unknown';
		}
	}

	/**
	 * Get the execution duration text
	 */
	function getDuration(): string {
		if (!execution) return '--';

		const start = new Date(execution.started_at).getTime();
		const end = execution.completed_at
			? new Date(execution.completed_at).getTime()
			: Date.now();
		const durationMs = end - start;

		if (durationMs < 1000) return `${durationMs}ms`;
		if (durationMs < 60000) return `${(durationMs / 1000).toFixed(1)}s`;
		const minutes = Math.floor(durationMs / 60000);
		const seconds = Math.floor((durationMs % 60000) / 1000);
		return `${minutes}m ${seconds}s`;
	}

	const StateIcon = $derived(getStateIcon());
	const stateClass = $derived(getStateClass());
	const stateLabel = $derived(getStateLabel());
	const duration = $derived(getDuration());
</script>

{#if execution}
	<div class="execution-status {compact ? 'compact' : ''}">
		<div class="status-main">
			<div class="status-indicator {stateClass}">
				{#if StateIcon}
					<StateIcon size={compact ? 14 : 16} class="state-icon {execution.state === ExecutionState.Running || execution.state === ExecutionState.Rendering ? 'animate-spin' : ''}" />
				{/if}
			</div>
			<div class="status-info">
				{#if compact}
					<span class="status-label">{stateLabel}</span>
					<span class="execution-id">{execution.id.slice(0, 8)}</span>
				{:else}
					<span class="status-label">{stateLabel}</span>
					<span class="execution-details">
						Execution <code class="id">{execution.id.slice(0, 8)}</code>
						{#if execution.prompt_name}
							<span class="prompt-name">using {execution.prompt_name}</span>
						{/if}
					</span>
				{/if}
			</div>
		</div>

		{#if showMetrics && !compact}
			<div class="status-metrics">
				<div class="metric">
					<span class="metric-label">Iteration</span>
					<span class="metric-value">{execution.iteration}</span>
				</div>
				<div class="metric">
					<span class="metric-label">Duration</span>
					<span class="metric-value">{duration}</span>
				</div>
				{#if execution.metrics.retries > 0}
					<div class="metric">
						<span class="metric-label">Retries</span>
						<span class="metric-value">{execution.metrics.retries}</span>
					</div>
				{/if}
				{#if execution.metrics.output_chars > 0}
					<div class="metric">
						<span class="metric-label">Output</span>
						<span class="metric-value">{execution.metrics.output_chars} chars</span>
					</div>
				{/if}
			</div>
		{/if}

		{#if execution.error && execution.state === ExecutionState.Failed}
			<div class="error-message">
				<AlertCircle size={14} />
				<span>{execution.error}</span>
			</div>
		{/if}
	</div>
{:else}
	<div class="execution-status empty {compact ? 'compact' : ''}">
		<span class="no-execution">No active execution</span>
	</div>
{/if}

<style>
	.execution-status {
		@apply flex flex-col gap-3 p-4 rounded-xl bg-[var(--color-bg-secondary)] border border-[var(--color-border)];
	}

	.execution-status.compact {
		@apply flex-row items-center gap-2 px-3 py-2;
	}

	.execution-status.empty {
		@apply items-center justify-center min-h-[60px];
	}

	.no-execution {
		@apply text-sm text-[var(--color-text-tertiary)];
	}

	.status-main {
		@apply flex items-center gap-3;
	}

	.status-indicator {
		@apply flex items-center justify-center w-8 h-8 rounded-full;
	}

	.execution-status.compact .status-indicator {
		@apply w-5 h-5;
	}

	.status-indicator.state-pending {
		@apply bg-yellow-900/30 text-yellow-500;
	}

	.status-indicator.state-running {
		@apply bg-[var(--color-accent-light)] text-[var(--color-accent-primary)];
	}

	.status-indicator.state-retrying {
		@apply bg-orange-900/30 text-orange-400;
	}

	.status-indicator.state-completed {
		@apply bg-green-900/30 text-green-400;
	}

	.status-indicator.state-failed {
		@apply bg-red-900/30 text-red-400;
	}

	.status-indicator.state-cancelled {
		@apply bg-gray-700/50 text-gray-400;
	}

	.state-icon {
		@apply flex-shrink-0;
	}

	.status-info {
		@apply flex flex-col gap-0.5;
	}

	.execution-status.compact .status-info {
		@apply flex-row items-center gap-2;
	}

	.status-label {
		@apply text-sm font-medium text-[var(--color-text-primary)];
	}

	.execution-details {
		@apply flex items-center gap-2 text-xs text-[var(--color-text-secondary)];
	}

	.execution-id {
		@apply text-[var(--color-text-tertiary)] font-mono;
	}

	.id {
		@apply px-1.5 py-0.5 rounded bg-[var(--color-bg-tertiary)] text-[var(--color-accent-primary)] font-mono text-xs;
	}

	.prompt-name {
		@apply text-[var(--color-text-tertiary)];
	}

	.status-metrics {
		@apply flex flex-wrap items-center gap-4 px-3 py-2 rounded-lg bg-[var(--color-bg-tertiary)];
	}

	.metric {
		@apply flex items-center gap-2;
	}

	.metric-label {
		@apply text-xs text-[var(--color-text-tertiary)] uppercase tracking-wide;
	}

	.metric-value {
		@apply text-sm font-medium text-[var(--color-text-primary)];
	}

	.error-message {
		@apply flex items-start gap-2 px-3 py-2 rounded-lg bg-red-950/30 border border-red-900/50 text-red-300 text-sm;
	}
</style>
