<script lang="ts">
	import type { ExecutionEvent } from '$lib/types';
	import { ExecutionEventType } from '$lib/types';
	import { Check, AlertCircle, Loader2, Copy } from 'lucide-svelte';

	interface Props {
		event: ExecutionEvent;
		isStreaming?: boolean;
	}

	let { event, isStreaming = false }: Props = $props();

	// Determine message type based on event
	const messageType = $derived(
		event.event_type === ExecutionEventType.OutputChunk
			? 'output'
			: event.event_type === ExecutionEventType.Error
				? 'error'
				: event.event_type === ExecutionEventType.StateChanged
					? 'status'
					: 'info'
	);


	// Format timestamp
	const formattedTime = $derived(() => {
		const date = new Date(event.timestamp);
		return date.toLocaleTimeString('en-US', {
			hour: '2-digit',
			minute: '2-digit',
			second: '2-digit'
		});
	});

	// Copy output to clipboard
	async function copyOutput() {
		if (event.output) {
			await navigator.clipboard.writeText(event.output);
		}
	}

	// Get status badge class based on state
	function getStatusBadgeClass(state: string): string {
		switch (state) {
			case 'pending':
				return 'px-2 py-0.5 rounded text-xs font-medium uppercase bg-gray-700 text-gray-300';
			case 'rendering':
				return 'px-2 py-0.5 rounded text-xs font-medium uppercase bg-blue-900/50 text-blue-300';
			case 'running':
				return 'px-2 py-0.5 rounded text-xs font-medium uppercase bg-primary text-white';
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
</script>

{#if messageType === 'output'}
	<div class="flex gap-3 py-2 px-4 rounded-lg bg-card">
		<div class="flex-1 min-w-0">
			<div class="text-foreground whitespace-pre-wrap break-words font-mono text-sm leading-relaxed"
			>
				{event.output}
			</div>
			<div class="flex items-center justify-between mt-2 gap-4">
				<span class="text-xs text-muted-foreground">{formattedTime()}</span>
				{#if event.output}
					<button
						class="p-1 rounded hover:bg-muted text-muted-foreground hover:text-muted-foreground transition-colors"
						onclick={copyOutput}
						title="Copy to clipboard"
						aria-label="Copy to clipboard"
					>
						<Copy size={14} />
					</button>
				{/if}
			</div>
		</div>
	</div>
{:else if messageType === 'error'}
	<div class="flex gap-3 py-2 px-4 rounded-lg bg-red-950/50 border border-red-900/50">
		<div class="flex-shrink-0 w-5 h-5 rounded-full flex items-center justify-center mt-0.5 bg-red-900/50 text-red-400">
			<AlertCircle size={16} />
		</div>
		<div class="flex-1 min-w-0">
			<div class="text-red-300 whitespace-pre-wrap break-words font-mono text-sm"
			>
				{event.error || 'An error occurred'}
			</div>
			<div class="flex items-center justify-between mt-2 gap-4">
				<span class="text-xs text-muted-foreground">{formattedTime()}</span>
			</div>
		</div>
	</div>
{:else if messageType === 'status'}
	<div class="flex gap-3 py-2 px-4 rounded-lg bg-muted opacity-75">
		<div
			class="flex-shrink-0 w-5 h-5 rounded-full flex items-center justify-center mt-0.5 bg-[var(--color-accent-light)] text-primary {isStreaming
				? 'animate-spin'
				: ''}"
		>
			{#if isStreaming}
				<Loader2 size={16} />
			{:else}
				<Check size={16} />
			{/if}
		</div>
		<div class="flex-1 min-w-0">
			{#if event.state}
				<div class="text-sm text-muted-foreground">
					Status: <span class={getStatusBadgeClass(event.state)}>{event.state}</span>
				</div>
			{/if}
			{#if event.iteration !== undefined}
				<div class="text-sm text-muted-foreground">Iteration {event.iteration}</div>
			{/if}
			{#if event.metrics}
				<div class="text-sm text-muted-foreground flex gap-3 mt-1">
					<span class="text-xs text-muted-foreground">Retry: {event.metrics.retries}</span>
					<span class="text-xs text-muted-foreground">{event.metrics.output_chars} chars</span>
					<span class="text-xs text-muted-foreground">{(event.metrics.duration_ms / 1000).toFixed(1)}s</span>
				</div>
			{/if}
			<div class="flex items-center justify-between mt-2 gap-4">
				<span class="text-xs text-muted-foreground">{formattedTime()}</span>
			</div>
		</div>
	</div>
{:else}
	<div class="text-center text-sm text-muted-foreground py-2 px-4">
		<div class="flex-1 min-w-0">
			<div class="text-sm text-muted-foreground">
				{#if event.event_type === ExecutionEventType.IterationCompleted}
					Iteration {event.iteration} completed
				{:else}
					{event.event_type}
				{/if}
			</div>
			<div class="flex items-center justify-between mt-2 gap-4">
				<span class="text-xs text-muted-foreground">{formattedTime()}</span>
			</div>
		</div>
	</div>
{/if}
