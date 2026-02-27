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
				return 'bg-[var(--color-state-pending-bg)] text-[var(--color-state-pending-text)]';
			case ExecutionState.Rendering:
			case ExecutionState.Running:
				return 'bg-[var(--color-state-running-bg)] text-[var(--color-state-running-text)]';
			case ExecutionState.Retrying:
				return 'bg-[var(--color-state-retrying-bg)] text-[var(--color-state-retrying-text)]';
			case ExecutionState.Completed:
				return 'bg-[var(--color-state-completed-bg)] text-[var(--color-state-completed-text)]';
			case ExecutionState.Failed:
				return 'bg-[var(--color-state-failed-bg)] text-[var(--color-state-failed-text)]';
			case ExecutionState.Cancelled:
				return 'bg-[var(--color-state-cancelled-bg)] text-[var(--color-state-cancelled-text)]';
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
	<div
		class="flex {compact
			? 'flex-row items-center gap-2 px-3 py-2'
			: 'flex-col gap-3 p-4'} rounded-xl bg-card border border-border"
	>
		<div class="flex items-center {compact ? 'gap-2' : 'gap-3'}">
			<div
				class="flex items-center justify-center {compact
					? 'w-5 h-5'
					: 'w-8 h-8'} rounded-full {stateClass}"
			>
				{#if StateIcon}
					<StateIcon
						size={compact ? 14 : 16}
						class="flex-shrink-0 {execution.state === ExecutionState.Running || execution.state === ExecutionState.Rendering
							? 'animate-spin'
							: ''}"
					/>
				{/if}
			</div>
			<div class="{compact ? 'flex-row items-center gap-2' : 'flex-col gap-0.5'}">
				{#if compact}
					<span class="text-sm font-medium text-foreground">{stateLabel}</span>
					<span class="text-muted-foreground font-mono">{execution.id.slice(0, 8)}</span>
				{:else}
					<span class="text-sm font-medium text-foreground">{stateLabel}</span>
					<span class="flex items-center gap-2 text-xs text-muted-foreground">
						Execution <code
							class="px-1.5 py-0.5 rounded bg-muted text-primary font-mono text-xs">{execution.id.slice(0, 8)}</code
						>
						{#if execution.prompt_name}
							<span class="text-muted-foreground">using {execution.prompt_name}</span>
						{/if}
					</span>
				{/if}
			</div>
		</div>

		{#if showMetrics && !compact}
			<div class="flex flex-wrap items-center gap-4 px-3 py-2 rounded-lg bg-muted">
				<div class="flex items-center gap-2">
					<span class="text-xs text-muted-foreground uppercase tracking-wide">Iteration</span>
					<span class="text-sm font-medium text-foreground">{execution.iteration}</span>
				</div>
				<div class="flex items-center gap-2">
					<span class="text-xs text-muted-foreground uppercase tracking-wide">Duration</span>
					<span class="text-sm font-medium text-foreground">{duration}</span>
				</div>
				{#if execution.metrics.retries > 0}
					<div class="flex items-center gap-2">
						<span class="text-xs text-muted-foreground uppercase tracking-wide">Retries</span>
						<span class="text-sm font-medium text-foreground">{execution.metrics.retries}</span>
					</div>
				{/if}
				{#if execution.metrics.output_chars > 0}
					<div class="flex items-center gap-2">
						<span class="text-xs text-muted-foreground uppercase tracking-wide">Output</span>
						<span class="text-sm font-medium text-foreground"
							>{execution.metrics.output_chars} chars</span
						>
					</div>
				{/if}
			</div>
		{/if}

		{#if execution.error && execution.state === ExecutionState.Failed}
			<div
				class="flex items-start gap-2 px-3 py-2 rounded-lg bg-[var(--color-state-failed-bg)] border border-[var(--color-state-failed-border)] text-[var(--color-state-failed-text)] text-sm"
			>
				<AlertCircle size={14} />
				<span>{execution.error}</span>
			</div>
		{/if}
	</div>
{:else}
	<div class="flex items-center justify-center min-h-[60px] {compact ? 'px-3 py-2' : 'p-4'} rounded-xl bg-card border border-border">
		<span class="text-sm text-muted-foreground">No active execution</span>
	</div>
{/if}
