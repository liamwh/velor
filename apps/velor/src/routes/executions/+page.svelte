<script lang="ts">
	import { onMount } from 'svelte';
	import { config, executionStore, currentExecution, executionError, sessionsStore } from '$lib/stores';
	import { EVENT_SERVICE } from '$lib/services/events';
	import { ChatInput } from '$lib/components/chat';
	import { ExecutionStatus, ExecutionControls } from '$lib/components/execution';
	import { SessionsList, SessionDetail } from '$lib/components/sessions';
	import { AlertCircle, X } from 'lucide-svelte';
	import type { ExecutionConfig, ExecutionRecord } from '$lib/types';

	let showSessionDetail = $state(false);
	let sessionToView = $state<ExecutionRecord | null>(null);

	// Load execution history on mount
	onMount(async () => {
		await executionStore.loadHistory(20);
		await sessionsStore.load(20);

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
			sessionsStore.refresh(20);
		});

		await EVENT_SERVICE.onExecutionFailed(({ execution }) => {
			executionStore.updateCurrent(execution);
			executionStore.loadHistory(20);
			sessionsStore.refresh(20);
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

	// View a session from the history
	function handleViewSession(session: ExecutionRecord) {
		sessionToView = session;
		showSessionDetail = true;
	}

	// Close session detail
	function handleCloseSessionDetail() {
		showSessionDetail = false;
		sessionToView = null;
	}

	// Retry from session detail
	function handleRetryFromSession(promptName: string) {
		const config: ExecutionConfig = {
			prompt_name: promptName,
			vars: {}
		};
		executionStore.start(config);
		handleCloseSessionDetail();
	}

	// Available prompts for ChatInput
	const availablePrompts = $derived(
		(function() {
			if (!$config?.prompts) return [];

			return Object.entries($config.prompts).map(([name, prompt]) => {
				// Handle both string and object prompt formats
				let template: string;
				let completeToken: string | undefined;

				if (typeof prompt === 'string') {
					template = prompt;
				} else if (prompt && typeof prompt === 'object' && 'template' in prompt) {
					const promptObj = prompt as { template: string; complete_token?: string };
					template = promptObj.template;
					completeToken = promptObj.complete_token;
				} else {
					template = '';
				}

				return {
					name,
					vars: {},
					template,
					complete_token: completeToken,
					is_template: name.startsWith('_') || false
				};
			});
		})()
	);

	// Loading state based on execution state
	const isLoading = $derived(
		$currentExecution?.state === 'running' || $currentExecution?.state === 'rendering'
	);
</script>

<div class="h-full flex flex-col">
	<!-- Session History Section -->
	<div class="flex-1 overflow-hidden">
		<SessionsList onSelect={handleViewSession} />
	</div>

	<!-- Active Execution Section -->
	{#if $currentExecution}
		<div class="border-t border-border">
			<div class="p-4">
				<h3 class="text-sm font-medium text-muted-foreground mb-2">Current Execution</h3>
				<div class="flex items-center justify-between bg-card rounded-lg p-3 gap-4">
					<ExecutionStatus execution={$currentExecution} showMetrics={false} compact={true} />
					<ExecutionControls
						execution={$currentExecution}
						onCancel={handleCancel}
						onRetry={handleRetry}
						onClear={handleClear}
						compact={true}
					/>
				</div>
			</div>
		</div>
	{:else}
		<!-- Chat Input for starting new executions -->
		<div class="border-t border-border">
			<ChatInput prompts={availablePrompts} loading={isLoading} onStart={handleStart} />
		</div>
	{/if}

	{#if $executionError}
		<div class="flex items-center gap-3 px-4 py-3 bg-red-950/50 border-t border-red-900/50 text-red-300">
			<AlertCircle size={16} />
			<span class="flex-1 text-sm">{$executionError}</span>
			<button
				class="p-1 rounded hover:bg-red-900/50 transition-colors"
				onclick={clearError}
				aria-label="Dismiss error"
			>
				<X size={14} />
			</button>
		</div>
	{/if}
</div>

<!-- Session Detail Modal -->
{#if showSessionDetail && sessionToView}
	<SessionDetail
		session={sessionToView}
		onClose={handleCloseSessionDetail}
		onRetry={handleRetryFromSession}
	/>
{/if}
