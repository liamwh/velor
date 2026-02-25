<script lang="ts">
	import { ExecutionState } from '$lib/types';
	import type { ExecutionRecord } from '$lib/types';
	import { X, RefreshCw, Trash2, Play } from 'lucide-svelte';

	interface Props {
		execution: ExecutionRecord | null;
		loading?: boolean;
		onCancel?: () => void | Promise<void>;
		onRetry?: () => void | Promise<void>;
		onClear?: () => void | Promise<void>;
		onStart?: () => void | Promise<void>;
		showStart?: boolean;
		compact?: boolean;
	}

	let {
		execution,
		loading = false,
		onCancel,
		onRetry,
		onClear,
		onStart,
		showStart = false,
		compact = false
	}: Props = $props();

	/**
	 * Check if execution is in an active state
	 */
	function isActive(): boolean {
		if (!execution) return false;
		return [
			ExecutionState.Running,
			ExecutionState.Rendering,
			ExecutionState.Retrying,
			ExecutionState.Pending
		].includes(execution.state);
	}

	/**
	 * Check if execution is in a terminal state
	 */
	function isTerminal(): boolean {
		if (!execution) return false;
		return [ExecutionState.Completed, ExecutionState.Failed, ExecutionState.Cancelled].includes(
			execution.state
		);
	}

	/**
	 * Check if execution failed
	 */
	function isFailed(): boolean {
		return execution?.state === ExecutionState.Failed;
	}

	/**
	 * Handle cancel action
	 */
	async function handleCancel() {
		if (onCancel && !loading) {
			await onCancel();
		}
	}

	/**
	 * Handle retry action
	 */
	async function handleRetry() {
		if (onRetry && !loading) {
			await onRetry();
		}
	}

	/**
	 * Handle clear action
	 */
	async function handleClear() {
		if (onClear && !loading) {
			await onClear();
		}
	}

	/**
	 * Handle start action
	 */
	async function handleStart() {
		if (onStart && !loading) {
			await onStart();
		}
	}

	const active = $derived(isActive());
	const terminal = $derived(isTerminal());
	const failed = $derived(isFailed());
	const hasActions = $derived(onCancel || onRetry || onClear || onStart);
</script>

{#if hasActions}
	<div class="execution-controls {compact ? 'compact' : ''} {failed ? 'failed' : ''}">
		{#if active && onCancel}
			<button
				class="control-btn cancel-btn"
				onclick={handleCancel}
				disabled={loading}
				title="Cancel execution"
				aria-label="Cancel execution"
			>
				<X size={compact ? 14 : 16} />
				{#if !compact}
					<span>Cancel</span>
				{/if}
			</button>
		{/if}

		{#if terminal}
			{#if onRetry}
				<button
					class="control-btn retry-btn"
					onclick={handleRetry}
					disabled={loading}
					title="Retry execution"
					aria-label="Retry execution"
				>
					<RefreshCw size={compact ? 14 : 16} />
					{#if !compact}
						<span>Retry</span>
					{/if}
				</button>
			{/if}

			{#if onClear}
				<button
					class="control-btn clear-btn"
					onclick={handleClear}
					disabled={loading}
					title="Clear execution"
					aria-label="Clear execution"
				>
					<Trash2 size={compact ? 14 : 16} />
					{#if !compact}
						<span>Clear</span>
					{/if}
				</button>
			{/if}
		{/if}

		{#if showStart && onStart && !execution}
			<button
				class="control-btn start-btn"
				onclick={handleStart}
				disabled={loading}
				title="Start new execution"
				aria-label="Start new execution"
			>
				<Play size={compact ? 14 : 16} />
				{#if !compact}
					<span>Start</span>
				{/if}
			</button>
		{/if}

		{#if loading}
			<div class="loading-indicator">
				<div class="spinner"></div>
			</div>
		{/if}
	</div>
{/if}

<style>
	.execution-controls {
		@apply flex items-center gap-2;
	}

	.execution-controls.compact {
		@apply gap-1.5;
	}

	.execution-controls.failed {
		@apply bg-[var(--color-state-failed-bg)] px-3 py-2 rounded-lg border border-[var(--color-state-failed-border)];
	}

	.control-btn {
		@apply flex items-center justify-center gap-1.5 px-3 py-2 rounded-lg text-sm font-medium transition-all duration-200 disabled:opacity-50 disabled:cursor-not-allowed;
	}

	.execution-controls.compact .control-btn {
		@apply px-2 py-1.5;
	}

	.cancel-btn {
		@apply bg-[var(--color-btn-cancel-bg)] text-[var(--color-btn-cancel-text)] hover:bg-[var(--color-btn-cancel-hover)] active:bg-[var(--color-btn-cancel-active)] border border-[var(--color-btn-cancel-border)];
	}

	.retry-btn {
		@apply bg-[var(--color-accent-primary)] text-white hover:bg-[var(--color-accent-hover)] active:bg-[var(--color-accent-active)];
	}

	.clear-btn {
		@apply bg-[var(--color-bg-tertiary)] text-[var(--color-text-secondary)] hover:bg-[var(--color-border)] active:bg-[var(--color-border-hover)] border border-[var(--color-border)];
	}

	.start-btn {
		@apply bg-[var(--color-btn-start-bg)] text-[var(--color-btn-start-text)] hover:bg-[var(--color-btn-start-hover)] active:bg-[var(--color-btn-start-active)] border border-[var(--color-btn-start-border)];
	}

	.control-btn span {
		@apply hidden sm:inline;
	}

	.execution-controls:not(.compact) .control-btn span {
		@apply inline;
	}

	.loading-indicator {
		@apply flex items-center justify-center w-8 h-8;
	}

	.spinner {
		@apply w-4 h-4 border-2 border-[var(--color-accent-primary)] border-t-transparent rounded-full animate-spin;
	}
</style>
