<script lang="ts">
	import { X, Clock, Hash, AlertCircle, Play, Trash2, RefreshCw, ChevronDown, ChevronUp } from 'lucide-svelte';
	import type { ExecutionRecord, ExecutionEvent, ExecutionState } from '$lib/types';
	import { sessionsStore } from '$lib/stores';

	interface Props {
		session: ExecutionRecord;
		onClose?: () => void;
		onRetry?: (promptName: string) => void;
	}

	let { session, onClose, onRetry }: Props = $props();

	let isDeleting = $state(false);
	let showAllEvents = $state(false);

	// Show only last 20 events by default
	const displayEvents = $derived(
		showAllEvents ? session.events : session.events.slice(-20)
	);

	function getStateBadge(state: ExecutionState): { class: string; label: string } {
		switch (state) {
			case 'completed':
				return { class: 'bg-[var(--color-success)]/20 text-[var(--color-success)]', label: 'Completed' };
			case 'failed':
				return { class: 'bg-red-500/20 text-red-400', label: 'Failed' };
			case 'cancelled':
				return { class: 'bg-gray-500/20 text-gray-400', label: 'Cancelled' };
			case 'running':
				return { class: 'bg-[var(--color-accent-primary)]/20 text-[var(--color-accent-primary)]', label: 'Running' };
			case 'rendering':
				return { class: 'bg-blue-500/20 text-blue-400', label: 'Rendering' };
			case 'retrying':
				return { class: 'bg-yellow-500/20 text-yellow-400', label: 'Retrying' };
			default:
				return { class: 'bg-muted text-muted-foreground', label: 'Pending' };
		}
	}

	function formatDuration(ms: number): string {
		if (ms < 1000) return `${ms}ms`;
		if (ms < 60000) return `${(ms / 1000).toFixed(1)}s`;
		const minutes = Math.floor(ms / 60000);
		const seconds = Math.floor((ms % 60000) / 1000);
		return `${minutes}m ${seconds}s`;
	}

	function formatDate(isoString: string): string {
		return new Date(isoString).toLocaleString(undefined, {
			weekday: 'short',
			month: 'short',
			day: 'numeric',
			hour: '2-digit',
			minute: '2-digit',
			second: '2-digit'
		});
	}

	function formatEventTime(isoString: string): string {
		return new Date(isoString).toLocaleTimeString(undefined, {
			hour: '2-digit',
			minute: '2-digit',
			second: '2-digit'
		});
	}

	function getEventIcon(event: ExecutionEvent): string {
		switch (event.event_type) {
			case 'state_changed':
				return '🔄';
			case 'output_chunk':
				return '📝';
			case 'error':
				return '❌';
			case 'iteration_completed':
				return '✓';
			case 'metrics_updated':
				return '📊';
			default:
				return '•';
		}
	}

	function getOutputText(): string {
		// Extract output chunks from events
		return session.events
			.filter((e) => e.event_type === 'output_chunk' && e.output)
			.map((e) => e.output)
			.join('');
	}

	async function handleDelete() {
		isDeleting = true;
		try {
			await sessionsStore.delete(session.id);
			if (onClose) onClose();
		} catch (e) {
			console.error('Failed to delete session:', e);
		} finally {
			isDeleting = false;
		}
	}

	function handleRetry() {
		if (onRetry) {
			onRetry(session.prompt_name);
		}
	}

	const badge = $derived(getStateBadge(session.state));
	const outputText = $derived(getOutputText());
	const hasMoreEvents = $derived(session.events.length > 20);
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="detail-overlay" onclick={onClose} onkeydown={(e) => e.key === 'Escape' && onClose?.()}>
	<!-- svelte-ignore a11y_no_static_element_interactions -->
	<div class="detail-dialog" onclick={(e) => e.stopPropagation()} onkeydown={(e) => e.stopPropagation()}>
		<!-- Header -->
		<div class="detail-header">
			<div class="header-left">
				<h2>Session Details</h2>
				<span class="state-badge {badge.class}">{badge.label}</span>
			</div>
			<div class="header-actions">
				{#if session.state === 'failed' || session.state === 'completed'}
					<button
						class="btn-icon"
						onclick={handleRetry}
						aria-label="Retry execution"
						title="Retry"
					>
						<RefreshCw size={16} />
					</button>
				{/if}
				<button
					class="btn-icon delete"
					onclick={handleDelete}
					disabled={isDeleting}
					aria-label="Delete session"
					title="Delete"
				>
					{#if isDeleting}
						<Play size={16} class="animate-spin" />
					{:else}
						<Trash2 size={16} />
					{/if}
				</button>
				<button class="close-btn" onclick={onClose} aria-label="Close">
					<X size={20} />
				</button>
			</div>
		</div>

		<!-- Content -->
		<div class="detail-body">
			<!-- Metadata Section -->
			<div class="metadata-section">
				<div class="metadata-grid">
					<div class="metadata-item">
						<span class="metadata-label">Session ID</span>
						<code class="metadata-value font-mono text-xs">{session.id}</code>
					</div>
					<div class="metadata-item">
						<span class="metadata-label">Prompt</span>
						<code class="metadata-value font-mono text-sm">{session.prompt_name}</code>
					</div>
					<div class="metadata-item">
						<span class="metadata-label">Started</span>
						<span class="metadata-value">{formatDate(session.started_at)}</span>
					</div>
					{#if session.completed_at}
						<div class="metadata-item">
							<span class="metadata-label">Completed</span>
							<span class="metadata-value">{formatDate(session.completed_at)}</span>
						</div>
					{/if}
				</div>

				<!-- Metrics -->
				<div class="metrics-section">
					<h3>Metrics</h3>
					<div class="metrics-grid">
						<div class="metric-card">
							<Clock size={16} class="metric-icon" />
							<div class="metric-content">
								<span class="metric-value">{formatDuration(session.metrics.duration_ms)}</span>
								<span class="metric-label">Duration</span>
							</div>
						</div>
						<div class="metric-card">
							<Hash size={16} class="metric-icon" />
							<div class="metric-content">
								<span class="metric-value">{session.iteration}</span>
								<span class="metric-label">Iterations</span>
							</div>
						</div>
						<div class="metric-card">
							<RefreshCw size={16} class="metric-icon" />
							<div class="metric-content">
								<span class="metric-value">{session.metrics.retries}</span>
								<span class="metric-label">Retries</span>
							</div>
						</div>
						<div class="metric-card">
							<Play size={16} class="metric-icon" />
							<div class="metric-content">
								<span class="metric-value">{(session.metrics.output_chars / 1000).toFixed(1)}k</span>
								<span class="metric-label">Output chars</span>
							</div>
						</div>
					</div>
				</div>
			</div>

			<!-- Error Section (if failed) -->
			{#if session.error}
				<div class="error-section">
					<div class="error-header">
						<AlertCircle size={16} />
						<span>Error</span>
					</div>
					<pre class="error-message">{session.error}</pre>
				</div>
			{/if}

			<!-- Output Section -->
			<div class="output-section">
				<h3>Output</h3>
				{#if outputText}
					<pre class="output-content">{outputText}</pre>
				{:else}
					<div class="empty-output">
						<span>No output captured</span>
					</div>
				{/if}
			</div>

			<!-- Events Timeline -->
			<div class="events-section">
				<div class="events-header">
					<h3>Event Timeline</h3>
					<span class="events-count">{session.events.length} events</span>
				</div>
				{#if session.events.length > 0}
					<div class="events-list">
						{#if hasMoreEvents && !showAllEvents}
							<button
								class="show-more-btn"
								onclick={() => showAllEvents = true}
							>
								<ChevronUp size={14} />
								<span>Show earlier events ({session.events.length - 20} more)</span>
							</button>
						{/if}
						{#each displayEvents as event, i (i)}
							<div class="event-item">
								<span class="event-icon">{getEventIcon(event)}</span>
								<span class="event-type">{event.event_type.replace(/_/g, ' ')}</span>
								<span class="event-time">{formatEventTime(event.timestamp)}</span>
								{#if event.state}
									<span class="event-state">{event.state}</span>
								{/if}
								{#if event.iteration !== undefined}
									<span class="event-iteration">#{event.iteration}</span>
								{/if}
							</div>
						{/each}
						{#if hasMoreEvents && showAllEvents}
							<button
								class="show-more-btn"
								onclick={() => showAllEvents = false}
							>
								<ChevronDown size={14} />
								<span>Show less</span>
							</button>
						{/if}
					</div>
				{:else}
					<div class="empty-events">
						<span>No events recorded</span>
					</div>
				{/if}
			</div>
		</div>
	</div>
</div>

<style>
	.detail-overlay {
		position: fixed;
		inset: 0;
		z-index: 50;
		display: flex;
		align-items: center;
		justify-content: center;
		background-color: rgb(0 0 0 / 0.6);
		backdrop-filter: blur(4px);
		padding: 1rem;
	}

	.detail-dialog {
		width: 100%;
		max-width: 56rem;
		max-height: 90vh;
		display: flex;
		flex-direction: column;
		background-color: var(--color-bg-secondary);
		border: 1px solid var(--color-border);
		border-radius: 0.5rem;
		box-shadow: 0 20px 25px -5px rgb(0 0 0 / 0.1), 0 8px 10px -6px rgb(0 0 0 / 0.1);
	}

	.detail-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		padding-left: 1.5rem;
		padding-right: 1.5rem;
		padding-top: 1rem;
		padding-bottom: 1rem;
		border-bottom: 1px solid var(--color-border);
	}

	.header-left {
		display: flex;
		align-items: center;
		gap: 0.75rem;
	}

	.header-left h2 {
		font-size: 1.125rem;
		font-weight: 600;
		color: var(--color-text-primary);
	}

	.state-badge {
		padding-left: 0.5rem;
		padding-right: 0.5rem;
		padding-top: 0.125rem;
		padding-bottom: 0.125rem;
		border-radius: 0.25rem;
		font-size: 0.75rem;
		font-weight: 500;
		text-transform: capitalize;
	}

	.header-actions {
		display: flex;
		align-items: center;
		gap: 0.25rem;
	}

	.btn-icon {
		padding: 0.5rem;
		border-radius: 0.25rem;
		color: var(--color-text-secondary);
		transition: all 0.15s ease-in-out;
	}

	.btn-icon:hover {
		color: var(--color-text-primary);
		background-color: var(--color-bg-tertiary);
	}

	.btn-icon:disabled {
		opacity: 0.5;
	}

	.btn-icon.delete:hover {
		color: rgb(248 113 113);
		background-color: rgb(239 68 68 / 0.1);
	}

	.close-btn {
		padding: 0.25rem;
		border-radius: 0.25rem;
		color: var(--color-text-secondary);
		transition: all 0.15s ease-in-out;
	}

	.close-btn:hover {
		color: var(--color-text-primary);
		background-color: var(--color-bg-tertiary);
	}

	.detail-body {
		flex: 1;
		overflow-y: auto;
		padding-left: 1.5rem;
		padding-right: 1.5rem;
		padding-top: 1rem;
		padding-bottom: 1rem;
		display: flex;
		flex-direction: column;
		gap: 1.5rem;
	}

	.metadata-section {
		display: flex;
		flex-direction: column;
		gap: 1rem;
	}

	.metadata-grid {
		display: grid;
		grid-template-columns: repeat(2, minmax(0, 1fr));
		gap: 1rem;
	}

	.metadata-item {
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
	}

	.metadata-label {
		font-size: 0.75rem;
		color: var(--color-text-muted);
	}

	.metadata-value {
		font-size: 0.875rem;
		color: var(--color-text-primary);
	}

	.metrics-section {
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
	}

	.metrics-section h3 {
		font-size: 0.875rem;
		font-weight: 500;
		color: var(--color-text-secondary);
	}

	.metrics-grid {
		display: grid;
		grid-template-columns: repeat(4, minmax(0, 1fr));
		gap: 0.75rem;
	}

	.metric-card {
		display: flex;
		align-items: center;
		gap: 0.75rem;
		padding: 0.75rem;
		border-radius: 0.5rem;
		background-color: var(--color-bg-tertiary);
		border: 1px solid var(--color-border);
	}

	.metric-icon {
		color: var(--color-text-muted);
	}

	.metric-content {
		display: flex;
		flex-direction: column;
	}

	.metric-value {
		font-size: 1.125rem;
		font-weight: 600;
		color: var(--color-text-primary);
	}

	.metric-label {
		font-size: 0.75rem;
		color: var(--color-text-muted);
	}

	.error-section {
		padding: 1rem;
		border-radius: 0.5rem;
		background-color: rgb(127 29 29 / 0.5);
		border: 1px solid rgb(185 28 28 / 0.5);
	}

	.error-header {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		color: rgb(248 113 113);
		font-weight: 500;
		margin-bottom: 0.5rem;
	}

	.error-message {
		font-size: 0.875rem;
		color: rgb(253 186 116);
		white-space: pre-wrap;
		font-family: ui-monospace, monospace;
		overflow-x: auto;
	}

	.output-section {
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
	}

	.output-section h3 {
		font-size: 0.875rem;
		font-weight: 500;
		color: var(--color-text-secondary);
	}

	.output-content {
		padding: 1rem;
		border-radius: 0.5rem;
		background-color: var(--color-bg-tertiary);
		border: 1px solid var(--color-border);
		font-size: 0.875rem;
		color: var(--color-text-primary);
		font-family: ui-monospace, monospace;
		white-space: pre-wrap;
		overflow-x: auto;
		max-height: 300px;
		overflow-y: auto;
	}

	.empty-output {
		display: flex;
		align-items: center;
		justify-content: center;
		padding-top: 2rem;
		padding-bottom: 2rem;
		border-radius: 0.5rem;
		background-color: var(--color-bg-tertiary);
		border: 1px solid var(--color-border);
		color: var(--color-text-muted);
	}

	.events-section {
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
	}

	.events-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
	}

	.events-header h3 {
		font-size: 0.875rem;
		font-weight: 500;
		color: var(--color-text-secondary);
	}

	.events-count {
		font-size: 0.75rem;
		color: var(--color-text-muted);
	}

	.events-list {
		padding: 1rem;
		border-radius: 0.5rem;
		background-color: var(--color-bg-tertiary);
		border: 1px solid var(--color-border);
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
		max-height: 200px;
		overflow-y: auto;
	}

	.event-item {
		display: flex;
		align-items: center;
		gap: 0.75rem;
		font-size: 0.875rem;
	}

	.event-icon {
		width: 1rem;
		text-align: center;
	}

	.event-type {
		flex: 1;
		color: var(--color-text-secondary);
		text-transform: capitalize;
	}

	.event-time {
		font-size: 0.75rem;
		color: var(--color-text-muted);
		font-family: ui-monospace, monospace;
	}

	.event-state {
		padding-left: 0.375rem;
		padding-right: 0.375rem;
		padding-top: 0.125rem;
		padding-bottom: 0.125rem;
		border-radius: 0.25rem;
		font-size: 0.75rem;
		background-color: var(--color-bg-secondary);
		color: var(--color-text-muted);
	}

	.event-iteration {
		padding-left: 0.375rem;
		padding-right: 0.375rem;
		padding-top: 0.125rem;
		padding-bottom: 0.125rem;
		border-radius: 0.25rem;
		font-size: 0.75rem;
		background-color: rgb(var(--color-accent-primary) / 0.2);
		color: var(--color-accent-primary);
	}

	.empty-events {
		display: flex;
		align-items: center;
		justify-content: center;
		padding-top: 2rem;
		padding-bottom: 2rem;
		border-radius: 0.5rem;
		background-color: var(--color-bg-tertiary);
		border: 1px solid var(--color-border);
		color: var(--color-text-muted);
	}

	.show-more-btn {
		display: flex;
		align-items: center;
		justify-content: center;
		gap: 0.25rem;
		width: 100%;
		padding-top: 0.5rem;
		padding-bottom: 0.5rem;
		font-size: 0.75rem;
		color: var(--color-text-muted);
		transition: all 0.15s ease-in-out;
	}

	.show-more-btn:hover {
		color: var(--color-text-secondary);
	}
</style>
