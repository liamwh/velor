<script lang="ts">
	import { onMount } from 'svelte';
	import { config, executionStore, currentExecution, executionError } from '$lib/stores';
	import { EVENT_SERVICE } from '$lib/services/events';
	import { ChatStream, ChatInput } from '$lib/components/chat';
	import { ExecutionStatus, ExecutionControls } from '$lib/components/execution';
	import { AlertCircle, X } from 'lucide-svelte';
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
	// TODO: Create a proper UI type for prompt selectors to avoid type casting
	const availablePrompts = $derived(
		$config?.prompts
			? (Object.entries($config.prompts).map(([name, prompt]) => ({
					name,
					vars: {},
					...prompt,
					is_template: name.startsWith('_') || false
				})) as any)
			: []
	);

	// Loading state based on execution state
	const isLoading = $derived(
		$currentExecution?.state === 'running' || $currentExecution?.state === 'rendering'
	);
</script>

<div class="executions-page">
	<div class="chat-container">
		<!-- Chat Stream -->
		<ChatStream showMetrics={true} autoScroll={true} />

		<!-- Chat Input -->
		<div class="input-section">
			{#if $currentExecution}
				<!-- Active or Terminal Execution Display -->
				<div class="execution-display">
					<ExecutionStatus execution={$currentExecution} showMetrics={false} compact={true} />
					<ExecutionControls
						execution={$currentExecution}
						onCancel={handleCancel}
						onRetry={handleRetry}
						onClear={handleClear}
						compact={true}
					/>
				</div>
			{:else}
				<!-- Normal Input -->
				<ChatInput
					prompts={availablePrompts}
					loading={isLoading}
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

	.execution-display {
		@apply flex items-center justify-between px-4 py-3 bg-[var(--color-bg-secondary)] gap-4;
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
