<script lang="ts">
	import { onMount, onDestroy, tick } from 'svelte';
	import { currentExecution, executionError } from '$lib/stores';
	import { EVENT_SERVICE } from '$lib/services/events';
	import { Scroll } from 'lucide-svelte';
	import ChatMessage from './ChatMessage.svelte';
	import type { ExecutionRecord, ExecutionEvent } from '$lib/types';
	import { ExecutionEventType } from '$lib/types';

	interface Props {
		executionId?: string;
		autoScroll?: boolean;
		showMetrics?: boolean;
	}

	let { executionId, autoScroll = true, showMetrics = true }: Props = $props();

	let messagesContainer: HTMLElement;
	let isScrolledToBottom = $state(true);
	let isStreaming = $state(false);
	let aggregatedOutput = $state<Map<number, string>>(new Map());

	// Track the execution we're listening to
	let currentExecutionId = $state<string | null>(null);

	// Computed messages from events
	const displayEvents = $derived(() => {
		if (!$currentExecution) return [];
		return $currentExecution.events;
	});

	// Get the state badge class
	function getStateClass(state: string): string {
		const stateMap: Record<string, string> = {
			pending: 'status-pending',
			rendering: 'status-rendering',
			running: 'status-running',
			retrying: 'status-retrying',
			completed: 'status-completed',
			failed: 'status-failed',
			cancelled: 'status-cancelled'
		};
		return stateMap[state] || 'status-pending';
	}

	// Check if we're at the bottom of the scroll
	function checkScrollPosition() {
		if (!messagesContainer) return;
		const threshold = 50;
		const position = messagesContainer.scrollTop + messagesContainer.clientHeight;
		isScrolledToBottom = messagesContainer.scrollHeight - position < threshold;
	}

	// Scroll to bottom of container
	async function scrollToBottom(force = false) {
		if (!messagesContainer) return;
		if (force || isScrolledToBottom || autoScroll) {
			await tick();
			messagesContainer.scrollTop = messagesContainer.scrollHeight;
		}
	}

	// Handle scroll events
	function handleScroll() {
		checkScrollPosition();
	}

	// Manual scroll to bottom
	async function scrollToBottomClick() {
		await scrollToBottom(true);
	}

	// Aggregate output chunks per iteration for better display
	function aggregateOutput(events: ExecutionEvent[]): Map<number, string> {
		const output = new Map<number, string>();
		for (const event of events) {
			if (event.event_type === ExecutionEventType.OutputChunk && event.iteration !== undefined) {
				const existing = output.get(event.iteration) || '';
				output.set(event.iteration, existing + (event.output || ''));
			}
		}
		return output;
	}

	// Process events and group output chunks
	const processedEvents = $derived(() => {
		const events = displayEvents();
		const result: ExecutionEvent[] = [];
		let currentOutput = '';
		let lastIteration: number | undefined = undefined;

		for (let i = 0; i < events.length; i++) {
			const event = events[i];

			if (event.event_type === ExecutionEventType.OutputChunk) {
				// Aggregate consecutive output chunks
				if (lastIteration !== event.iteration) {
					if (currentOutput) {
						result.push({
							event_type: ExecutionEventType.OutputChunk,
							timestamp: events[i - 1]?.timestamp || event.timestamp,
							output: currentOutput,
							iteration: lastIteration
						});
					}
					currentOutput = event.output || '';
					lastIteration = event.iteration;
				} else {
					currentOutput += event.output || '';
				}
			} else {
				// Flush any pending output
				if (currentOutput) {
					result.push({
						event_type: ExecutionEventType.OutputChunk,
						timestamp: event.timestamp,
						output: currentOutput,
						iteration: lastIteration
					});
					currentOutput = '';
					lastIteration = undefined;
				}
				result.push(event);
			}
		}

		// Don't forget the last chunk
		if (currentOutput) {
			result.push({
				event_type: ExecutionEventType.OutputChunk,
				timestamp: events[events.length - 1]?.timestamp || new Date().toISOString(),
				output: currentOutput,
				iteration: lastIteration
			});
		}

		return result;
	});

	// Subscribe to execution updates
	onMount(() => {
		// Listen for execution updates
		const setupListeners = async () => {
			await EVENT_SERVICE.onExecutionUpdated(({ execution }) => {
				if (executionId && execution.id === executionId) {
					isStreaming = execution.state === 'running' || execution.state === 'rendering';
					scrollToBottom();
				}
			});

			await EVENT_SERVICE.onExecutionCompleted(({ execution }) => {
				if (executionId && execution.id === executionId) {
					isStreaming = false;
					scrollToBottom();
				}
			});

			await EVENT_SERVICE.onExecutionFailed(({ execution }) => {
				if (executionId && execution.id === executionId) {
					isStreaming = false;
					scrollToBottom();
				}
			});
		};

		setupListeners();

		// Initial scroll
		scrollToBottom();
	});

	onDestroy(() => {
		// EventService handles cleanup internally
	});

	// Reactive: update current execution ID and streaming state
	$effect(() => {
		if ($currentExecution) {
			currentExecutionId = $currentExecution.id;
			isStreaming =
				$currentExecution.state === 'running' ||
				$currentExecution.state === 'rendering' ||
				$currentExecution.state === 'retrying';
		} else {
			currentExecutionId = null;
			isStreaming = false;
		}
	});

	// Reactive: auto-scroll when new events arrive
	$effect(() => {
		const events = displayEvents();
		if (events.length > 0) {
			scrollToBottom();
		}
	});
</script>

<div class="chat-stream">
	{#if $currentExecution}
		<!-- Status Bar -->
		{#if showMetrics}
			<div class="status-bar">
				<div class="status-info">
					<span class="status-badge {getStateClass($currentExecution.state)}"
						>{$currentExecution.state}</span
					>
					<span class="metrics">
						Iteration {$currentExecution.iteration}
						{#if $currentExecution.metrics.retries > 0}
							<span class="retry-count">({$currentExecution.metrics.retries} retries)</span>
						{/if}
					</span>
				</div>
				<div class="metrics">
					<span>{$currentExecution.metrics.output_chars} chars</span>
					<span>{($currentExecution.metrics.duration_ms / 1000).toFixed(1)}s</span>
				</div>
			</div>
		{/if}

		<!-- Messages Container -->
		<div
			bind:this={messagesContainer}
			class="messages-container"
			onscroll={handleScroll}
			role="log"
			aria-live="polite"
			aria-atomic="false"
		>
			{#if processedEvents().length === 0}
				<div class="empty-state">
					<Scroll size={32} />
					<p>Waiting for output...</p>
				</div>
			{:else}
				{#each processedEvents() as event (event.timestamp + event.event_type + (event.output?.length || 0))}
					<ChatMessage {event} {isStreaming} />
				{/each}
			{/if}
		</div>

		<!-- Scroll to Bottom Button -->
		{#if !isScrolledToBottom}
			<button
				class="scroll-to-bottom"
				onclick={scrollToBottomClick}
				title="Scroll to bottom"
				aria-label="Scroll to bottom"
			>
				<Scroll size={16} />
			</button>
		{/if}
	{:else}
		<div class="no-execution">
			<Scroll size={48} />
			<h3>No Execution Active</h3>
			<p>Start an execution to see live output here.</p>
		</div>
	{/if}

	{#if $executionError}
		<div class="error-banner">
			<span class="error-icon">⚠</span>
			<span class="error-text">{$executionError}</span>
		</div>
	{/if}
</div>

<style>
	.chat-stream {
		@apply flex flex-col h-full bg-[var(--color-bg-primary)] overflow-hidden;
	}

	.status-bar {
		@apply flex items-center justify-between px-4 py-2 border-b border-[var(--color-border)] bg-[var(--color-bg-secondary)];
	}

	.status-info {
		@apply flex items-center gap-2;
	}

	.status-badge {
		@apply px-2 py-0.5 rounded text-xs font-medium uppercase;
	}

	.status-badge.status-pending {
		@apply bg-gray-700 text-gray-300;
	}

	.status-badge.status-rendering {
		@apply bg-blue-900/50 text-blue-300;
	}

	.status-badge.status-running {
		@apply bg-[var(--color-accent-primary)] text-white animate-pulse;
	}

	.status-badge.status-retrying {
		@apply bg-orange-900/50 text-orange-300;
	}

	.status-badge.status-completed {
		@apply bg-green-900/50 text-green-300;
	}

	.status-badge.status-failed {
		@apply bg-red-900/50 text-red-300;
	}

	.status-badge.status-cancelled {
		@apply bg-gray-700 text-gray-300;
	}

	.metrics {
		@apply text-sm text-[var(--color-text-secondary)];
	}

	.retry-count {
		@apply text-orange-400;
	}

	.messages-container {
		@apply flex-1 overflow-y-auto px-4 py-4 space-y-1;
	}

	.empty-state,
	.no-execution {
		@apply flex flex-col items-center justify-center h-full text-[var(--color-text-muted)] gap-4;
	}

	.no-execution h3 {
		@apply text-lg font-semibold text-[var(--color-text-secondary)] mt-2;
	}

	.no-execution p {
		@apply text-sm;
	}

	.scroll-to-bottom {
		@apply absolute bottom-4 right-4 p-2 rounded-full bg-[var(--color-bg-secondary)] border border-[var(--color-border)] text-[var(--color-text-secondary)] hover:text-[var(--color-text-primary)] hover:bg-[var(--color-bg-tertiary)] transition-all shadow-lg;
	}

	.error-banner {
		@apply flex items-center gap-2 px-4 py-3 bg-red-950/50 border-t border-red-900/50 text-red-300;
	}

	.error-icon {
		@apply text-lg;
	}

	.error-text {
		@apply text-sm flex-1;
	}
</style>
