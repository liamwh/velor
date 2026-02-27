<script lang="ts">
	import { onMount, onDestroy, tick } from 'svelte';
	import { currentExecution, executionError } from '$lib/stores';
	import { EVENT_SERVICE } from '$lib/services/events';
	import { Scroll } from 'lucide-svelte';
	import ChatMessage from './ChatMessage.svelte';
	import type { ExecutionEvent } from '$lib/types';
	import { ExecutionEventType } from '$lib/types';

	interface Props {
		executionId?: string;
		autoScroll?: boolean;
		showMetrics?: boolean;
	}

	let { executionId, autoScroll = true, showMetrics = true }: Props = $props();

	let messagesContainer = $state<HTMLElement | undefined>(undefined);
	let isScrolledToBottom = $state(true);
	let isStreaming = $state(false);

	// Track the execution we're listening to
	let _currentExecutionId = $state<string | null>(null);

	// Computed messages from events
	const displayEvents = $derived(() => {
		if (!$currentExecution) return [];
		return $currentExecution.events;
	});

	// Get the state badge class
	function getStateClass(state: string): string {
		switch (state) {
			case 'pending':
				return 'px-2 py-0.5 rounded text-xs font-medium uppercase bg-gray-700 text-gray-300';
			case 'rendering':
				return 'px-2 py-0.5 rounded text-xs font-medium uppercase bg-blue-900/50 text-blue-300';
			case 'running':
				return 'px-2 py-0.5 rounded text-xs font-medium uppercase bg-primary text-white animate-pulse';
			case 'retrying':
				return 'px-2 py-0.5 rounded text-xs font-medium uppercase bg-orange-900/50 text-orange-300';
			case 'completed':
				return 'px-2 py-0.5 rounded text-xs font-medium uppercase bg-green-900/50 text-green-300';
			case 'failed':
				return 'px-2 py-0.5 rounded text-xs font-medium uppercase bg-red-900/50 text-red-300';
			case 'cancelled':
				return 'px-2 py-0.5 rounded text-xs font-medium uppercase bg-gray-700 text-gray-300';
			default:
				return 'px-2 py-0.5 rounded text-xs font-medium uppercase bg-gray-700 text-gray-300';
		}
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
			if (messagesContainer) {
				messagesContainer.scrollTop = messagesContainer.scrollHeight;
			}
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
			_currentExecutionId = $currentExecution.id;
			isStreaming =
				$currentExecution.state === 'running' ||
				$currentExecution.state === 'rendering' ||
				$currentExecution.state === 'retrying';
		} else {
			_currentExecutionId = null;
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

<div class="flex flex-col h-full bg-background overflow-hidden">
	{#if $currentExecution}
		<!-- Status Bar -->
		{#if showMetrics}
			<div class="flex items-center justify-between px-4 py-2 border-b border-border bg-card">
				<div class="flex items-center gap-2">
					<span class={getStateClass($currentExecution.state)}>{$currentExecution.state}</span>
					<span class="text-sm text-muted-foreground">
						Iteration {$currentExecution.iteration}
						{#if $currentExecution.metrics.retries > 0}
							<span class="text-orange-400">({$currentExecution.metrics.retries} retries)</span>
						{/if}
					</span>
				</div>
				<div class="text-sm text-muted-foreground">
					<span>{$currentExecution.metrics.output_chars} chars</span>
					<span>{($currentExecution.metrics.duration_ms / 1000).toFixed(1)}s</span>
				</div>
			</div>
		{/if}

		<!-- Messages Container -->
		<div
			bind:this={messagesContainer}
			class="flex-1 overflow-y-auto px-4 py-4 space-y-1"
			onscroll={handleScroll}
			role="log"
			aria-live="polite"
			aria-atomic="false"
		>
			{#if processedEvents().length === 0}
				<div class="flex flex-col items-center justify-center h-full text-muted-foreground gap-4">
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
				class="absolute bottom-4 right-4 p-2 rounded-full bg-card border border-border text-muted-foreground hover:text-foreground hover:bg-muted transition-all shadow-lg"
				onclick={scrollToBottomClick}
				title="Scroll to bottom"
				aria-label="Scroll to bottom"
			>
				<Scroll size={16} />
			</button>
		{/if}
	{:else}
		<div class="flex flex-col items-center justify-center h-full text-muted-foreground gap-4">
			<Scroll size={48} />
			<h3 class="text-lg font-semibold text-muted-foreground mt-2">No Execution Active</h3>
			<p class="text-sm">Start an execution to see live output here.</p>
		</div>
	{/if}

	{#if $executionError}
		<div class="flex items-center gap-2 px-4 py-3 bg-red-950/50 border-t border-red-900/50 text-red-300">
			<span class="text-lg">⚠</span>
			<span class="text-sm flex-1">{$executionError}</span>
		</div>
	{/if}
</div>
