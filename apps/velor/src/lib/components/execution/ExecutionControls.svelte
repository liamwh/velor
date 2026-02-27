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
	<div
		class="flex items-center {compact
			? 'gap-1.5'
			: 'gap-2'} {failed
			? 'bg-[var(--color-state-failed-bg)] px-3 py-2 rounded-lg border border-[var(--color-state-failed-border)]'
			: ''}"
	>
		{#if active && onCancel}
			<button
				class="flex items-center justify-center gap-1.5 {compact
					? 'px-2 py-1.5'
					: 'px-3 py-2'} rounded-lg text-sm font-medium transition-all duration-200 disabled:opacity-50 disabled:cursor-not-allowed bg-[var(--color-btn-cancel-bg)] text-[var(--color-btn-cancel-text)] hover:bg-[var(--color-btn-cancel-hover)] active:bg-[var(--color-btn-cancel-active)] border border-[var(--color-btn-cancel-border)]"
				onclick={handleCancel}
				disabled={loading}
				title="Cancel execution"
				aria-label="Cancel execution"
			>
				<X size={compact ? 14 : 16} />
				{#if !compact}
					<span class="hidden sm:inline">Cancel</span>
				{/if}
			</button>
		{/if}

		{#if terminal}
			{#if onRetry}
				<button
					class="flex items-center justify-center gap-1.5 {compact
						? 'px-2 py-1.5'
						: 'px-3 py-2'} rounded-lg text-sm font-medium transition-all duration-200 disabled:opacity-50 disabled:cursor-not-allowed bg-primary text-white hover:bg-[var(--color-accent-hover)] active:bg-[var(--color-accent-active)]"
					onclick={handleRetry}
					disabled={loading}
					title="Retry execution"
					aria-label="Retry execution"
				>
					<RefreshCw size={compact ? 14 : 16} />
					{#if !compact}
						<span class="hidden sm:inline">Retry</span>
					{/if}
				</button>
			{/if}

			{#if onClear}
				<button
					class="flex items-center justify-center gap-1.5 {compact
						? 'px-2 py-1.5'
						: 'px-3 py-2'} rounded-lg text-sm font-medium transition-all duration-200 disabled:opacity-50 disabled:cursor-not-allowed bg-muted text-muted-foreground hover:bg-border active:bg-input border border-border"
					onclick={handleClear}
					disabled={loading}
					title="Clear execution"
					aria-label="Clear execution"
				>
					<Trash2 size={compact ? 14 : 16} />
					{#if !compact}
						<span class="hidden sm:inline">Clear</span>
					{/if}
				</button>
			{/if}
		{/if}

		{#if showStart && onStart && !execution}
			<button
				class="flex items-center justify-center gap-1.5 {compact
					? 'px-2 py-1.5'
					: 'px-3 py-2'} rounded-lg text-sm font-medium transition-all duration-200 disabled:opacity-50 disabled:cursor-not-allowed bg-[var(--color-btn-start-bg)] text-[var(--color-btn-start-text)] hover:bg-[var(--color-btn-start-hover)] active:bg-[var(--color-btn-start-active)] border border-[var(--color-btn-start-border)]"
				onclick={handleStart}
				disabled={loading}
				title="Start new execution"
				aria-label="Start new execution"
			>
				<Play size={compact ? 14 : 16} />
				{#if !compact}
					<span class="hidden sm:inline">Start</span>
				{/if}
			</button>
		{/if}

		{#if loading}
			<div class="flex items-center justify-center w-8 h-8">
				<div
					class="w-4 h-4 border-2 border-primary border-t-transparent rounded-full animate-spin"
				></div>
			</div>
		{/if}
	</div>
{/if}
