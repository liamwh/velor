<script lang="ts">
	import type { ExecutionEvent } from '$lib/types';
	import { ExecutionEventType } from '$lib/types';
	import { Check, AlertCircle, Loader2, Copy } from 'lucide-svelte';
	import { cn } from '$lib/utils';

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

	// Get icon based on message type
	const icon = $derived(() => {
		switch (messageType) {
			case 'output':
				return null;
			case 'error':
				return AlertCircle;
			case 'status':
				return isStreaming ? Loader2 : Check;
			default:
				return null;
		}
	});

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
</script>

{#if messageType === 'output'}
	<div class="message message-output">
		<div class="message-content">
			<div class="output-text">{event.output}</div>
			<div class="message-meta">
				<span class="timestamp">{formattedTime()}</span>
				{#if event.output}
					<button
						class="copy-btn"
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
	<div class="message message-error">
		<div class="message-icon error-icon">
			<AlertCircle size={16} />
		</div>
		<div class="message-content">
			<div class="error-text">{event.error || 'An error occurred'}</div>
			<div class="message-meta">
				<span class="timestamp">{formattedTime()}</span>
			</div>
		</div>
	</div>
{:else if messageType === 'status'}
	<div class="message message-status">
		<div class="message-icon status-icon" class:spinning={isStreaming}>
			{#if isStreaming}
				<Loader2 size={16} />
			{:else}
				<Check size={16} />
			{/if}
		</div>
		<div class="message-content">
			{#if event.state}
				<div class="status-text">
					Status: <span class="status-badge status-{event.state}">{event.state}</span>
				</div>
			{/if}
			{#if event.iteration !== undefined}
				<div class="iteration-text">Iteration {event.iteration}</div>
			{/if}
			{#if event.metrics}
				<div class="metrics-text">
					<span class="metric">Retry: {event.metrics.retries}</span>
					<span class="metric">{event.metrics.output_chars} chars</span>
					<span class="metric">{(event.metrics.duration_ms / 1000).toFixed(1)}s</span>
				</div>
			{/if}
			<div class="message-meta">
				<span class="timestamp">{formattedTime()}</span>
			</div>
		</div>
	</div>
{:else}
	<div class="message message-info">
		<div class="message-content">
			<div class="info-text">
				{#if event.event_type === ExecutionEventType.IterationCompleted}
					Iteration {event.iteration} completed
				{:else}
					{event.event_type}
				{/if}
			</div>
			<div class="message-meta">
				<span class="timestamp">{formattedTime()}</span>
			</div>
		</div>
	</div>
{/if}

<style>
	.message {
		@apply flex gap-3 py-2 px-4 rounded-lg;
	}

	.message-output {
		@apply bg-[var(--color-bg-secondary)];
	}

	.message-error {
		@apply bg-red-950/50 border border-red-900/50;
	}

	.message-status {
		@apply bg-[var(--color-bg-tertiary)] opacity-75;
	}

	.message-info {
		@apply text-center text-sm text-[var(--color-text-muted)];
	}

	.message-icon {
		@apply flex-shrink-0 w-5 h-5 rounded-full flex items-center justify-center mt-0.5;
	}

	.error-icon {
		@apply bg-red-900/50 text-red-400;
	}

	.status-icon {
		@apply bg-[var(--color-accent-light)] text-[var(--color-accent-primary)];
	}

	.message-content {
		@apply flex-1 min-w-0;
	}

	.output-text {
		@apply text-[var(--color-text-primary)] whitespace-pre-wrap break-words font-mono text-sm leading-relaxed;
	}

	.error-text {
		@apply text-red-300 whitespace-pre-wrap break-words font-mono text-sm;
	}

	.status-text,
	.iteration-text,
	.metrics-text,
	.info-text {
		@apply text-sm text-[var(--color-text-secondary)];
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
		@apply bg-[var(--color-accent-primary)] text-white;
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

	.metrics-text {
		@apply flex gap-3 mt-1;
	}

	.metric {
		@apply text-xs text-[var(--color-text-muted)];
	}

	.message-meta {
		@apply flex items-center justify-between mt-2 gap-4;
	}

	.timestamp {
		@apply text-xs text-[var(--color-text-muted)];
	}

	.copy-btn {
		@apply p-1 rounded hover:bg-[var(--color-bg-tertiary)] text-[var(--color-text-muted)] hover:text-[var(--color-text-secondary)] transition-colors;
	}

	/* Spinning animation for loading state */
	.spinning {
		animation: spin 1s linear infinite;
	}

	@keyframes spin {
		from {
			transform: rotate(0deg);
		}
		to {
			transform: rotate(360deg);
		}
	}
</style>
