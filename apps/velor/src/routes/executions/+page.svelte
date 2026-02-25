<script lang="ts">
	import { onMount } from 'svelte';
	import { config, prompts, executionStore, currentExecution, executionError } from '$lib/stores';
	import { EVENT_SERVICE } from '$lib/services/events';
	import { ChatStream, ChatInput } from '$lib/components/chat';
	import { X, RefreshCw, Trash2, AlertCircle } from 'lucide-svelte';
	import type { ExecutionConfig } from '$lib/types';

	// Load execution history on mount
	onMount(async () => {
		await executionStore.loadHistory(20);

		// Listen for execution events
		await EVENT_SERVICE.onExecutionStarted(({ execution }) => {
			executionStore.updateCurrent(execution);
		});

		await EVENT_SERVICE.onExecutionUpdated(({ execution }) => {
			executionStore.updateCurrent(execution);
		});

		await EVENT_SERVICE.onExecutionCompleted(({ execution }) => {
			executionStore.updateCurrent(execution);
			// Reload history to include this execution
			executionStore.loadHistory(20);
		});

		await EVENT_SERVICE.onExecutionFailed(({ execution }) => {
			executionStore.updateCurrent(execution);
			executionStore.loadHistory(20);
		});
	});

	// Start execution from ChatInput
	function handleStart(config: ExecutionConfig) {
		executionStore.start(config);
	}

	// Cancel current execution
	async function handleCancel() {
		if ($currentExecution) {
			await executionStore.cancel();
		}
	}

	// Clear current execution
	function handleClear() {
		executionStore.clearCurrent();
	}

	// Retry current execution
	async function handleRetry() {
		if ($currentExecution) {
			const config: ExecutionConfig = {
				prompt_name: $currentExecution.prompt_name,
				vars: {}
			};
			// Extract vars from the original config if available
			await executionStore.start(config);
		}
	}

	// Clear error
	function clearError() {
		executionStore.clearCurrent();
	}

	// Available prompts for ChatInput
	const availablePrompts = $derived(
		$config?.prompts
			? Object.entries($config.prompts).map(([name, prompt]) => ({
					name,
					...prompt,
					is_template: name.startsWith('_') || false
				}))
			: []
	);
</script>

<div class="executions-page">
	<div class="chat-container">
		<!-- Chat Stream -->
		<ChatStream showMetrics={true} autoScroll={true} />

		<!-- Chat Input -->
		<div class="input-section">
			{#if $currentExecution && ($currentExecution.state === 'running' || $currentExecution.state === 'rendering' || $currentExecution.state === 'retrying')}
				<!-- Active Execution Controls -->
				<div class="execution-controls">
					<div class="execution-status">
						<span class="status-indicator running"></span>
						<span class="status-text">
							Execution {$currentExecution.id.slice(0, 8)} is running
						</span>
					</div>
					<div class="control-buttons">
						<button
							class="control-btn cancel-btn"
							onclick={handleCancel}
							title="Cancel execution"
							aria-label="Cancel execution"
						>
							<X size={16} />
							<span>Cancel</span>
						</button>
					</div>
				</div>
			{:else if $currentExecution && $currentExecution.state === 'failed'}
				<!-- Failed Execution Controls -->
				<div class="execution-controls failed">
					<div class="execution-status">
						<AlertCircle size={16} class="text-red-400" />
						<span class="status-text">Execution failed: {$currentExecution.error || 'Unknown error'}</span>
					</div>
					<div class="control-buttons">
						<button
							class="control-btn secondary-btn"
							onclick={handleRetry}
							title="Retry execution"
							aria-label="Retry execution"
						>
							<RefreshCw size={16} />
							<span>Retry</span>
						</button>
						<button
							class="control-btn secondary-btn"
							onclick={handleClear}
							title="Clear execution"
							aria-label="Clear execution"
						>
							<Trash2 size={16} />
							<span>Clear</span>
						</button>
					</div>
				</div>
			{:else}
				<!-- Normal Input -->
				<ChatInput
					prompts={availablePrompts}
					loading={$currentExecution?.state === 'running' || $currentExecution?.state === 'rendering'}
					onStart={handleStart}
				/>
			{/if}
		</div>
	</div>

	{#if $executionError}
		<div class="error-banner">
			<AlertCircle size={16} />
			<span>{$executionError}</span>
			<button class="close-btn" onclick={clearError} aria-label="Dismiss error">
				<X size={14} />
			</button>
		</div>
	{/if}
</div>

<style>
	.executions-page {
		@apply h-full flex flex-col;
	}

	.chat-container {
		@apply flex-1 flex flex-col overflow-hidden;
	}

	.input-section {
		@apply border-t border-[var(--color-border)];
	}

	.execution-controls {
		@apply flex items-center justify-between px-4 py-3 bg-[var(--color-bg-secondary)];
	}

	.execution-controls.failed {
		@apply bg-red-950/30 border-t border-red-900/50;
	}

	.execution-status {
		@apply flex items-center gap-2;
	}

	.status-indicator {
		@apply w-2 h-2 rounded-full;
	}

	.status-indicator.running {
		@apply bg-[var(--color-accent-primary)] animate-pulse;
	}

	.status-text {
		@apply text-sm text-[var(--color-text-secondary)];
	}

	.control-buttons {
		@apply flex items-center gap-2;
	}

	.control-btn {
		@apply flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-sm font-medium transition-all;
	}

	.cancel-btn {
		@apply bg-red-900/50 text-red-300 hover:bg-red-900/70;
	}

	.secondary-btn {
		@apply bg-[var(--color-bg-tertiary)] text-[var(--color-text-secondary)] hover:bg-[var(--color-border-hover)];
	}

	.error-banner {
		@apply flex items-center gap-3 px-4 py-3 bg-red-950/50 border-t border-red-900/50 text-red-300;
	}

	.error-banner span {
		@apply flex-1 text-sm;
	}

	.close-btn {
		@apply p-1 rounded hover:bg-red-900/50 transition-colors;
	}
</style>
