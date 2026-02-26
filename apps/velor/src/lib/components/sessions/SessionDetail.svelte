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
		@apply fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm p-4;
	}

	.detail-dialog {
		@apply w-full max-w-4xl max-h-[90vh] flex flex-col bg-[var(--color-bg-secondary)] border border-[var(--color-border)] rounded-lg shadow-xl;
	}

	.detail-header {
		@apply flex items-center justify-between px-6 py-4 border-b border-[var(--color-border)];
	}

	.header-left {
		@apply flex items-center gap-3;
	}

	.header-left h2 {
		@apply text-lg font-semibold text-[var(--color-text-primary)];
	}

	.state-badge {
		@apply px-2 py-0.5 rounded text-xs font-medium capitalize;
	}

	.header-actions {
		@apply flex items-center gap-1;
	}

	.btn-icon {
		@apply p-2 rounded text-[var(--color-text-secondary)] hover:text-[var(--color-text-primary)] hover:bg-[var(--color-bg-tertiary)] transition-all disabled:opacity-50;
	}

	.btn-icon.delete {
		@apply hover:text-red-400 hover:bg-red-500/10;
	}

	.close-btn {
		@apply p-1 rounded text-[var(--color-text-secondary)] hover:text-[var(--color-text-primary)] hover:bg-[var(--color-bg-tertiary)] transition-all;
	}

	.detail-body {
		@apply flex-1 overflow-y-auto px-6 py-4 space-y-6;
	}

	.metadata-section {
		@apply space-y-4;
	}

	.metadata-grid {
		@apply grid grid-cols-2 gap-4;
	}

	.metadata-item {
		@apply flex flex-col gap-1;
	}

	.metadata-label {
		@apply text-xs text-[var(--color-text-muted)];
	}

	.metadata-value {
		@apply text-sm text-[var(--color-text-primary)];
	}

	.metrics-section {
		@apply space-y-3;
	}

	.metrics-section h3 {
		@apply text-sm font-medium text-[var(--color-text-secondary)];
	}

	.metrics-grid {
		@apply grid grid-cols-4 gap-3;
	}

	.metric-card {
		@apply flex items-center gap-3 p-3 rounded-lg bg-[var(--color-bg-tertiary)] border border-[var(--color-border)];
	}

	.metric-icon {
		@apply text-[var(--color-text-muted)];
	}

	.metric-content {
		@apply flex flex-col;
	}

	.metric-value {
		@apply text-lg font-semibold text-[var(--color-text-primary)];
	}

	.metric-label {
		@apply text-xs text-[var(--color-text-muted)];
	}

	.error-section {
		@apply p-4 rounded-lg bg-red-950/50 border border-red-900/50;
	}

	.error-header {
		@apply flex items-center gap-2 text-red-400 font-medium mb-2;
	}

	.error-message {
		@apply text-sm text-red-300 whitespace-pre-wrap font-mono overflow-x-auto;
	}

	.output-section {
		@apply space-y-3;
	}

	.output-section h3 {
		@apply text-sm font-medium text-[var(--color-text-secondary)];
	}

	.output-content {
		@apply p-4 rounded-lg bg-[var(--color-bg-tertiary)] border border-[var(--color-border)] text-sm text-[var(--color-text-primary)] font-mono whitespace-pre-wrap overflow-x-auto max-h-[300px] overflow-y-auto;
	}

	.empty-output {
		@apply flex items-center justify-center py-8 rounded-lg bg-[var(--color-bg-tertiary)] border border-[var(--color-border)] text-[var(--color-text-muted)];
	}

	.events-section {
		@apply space-y-3;
	}

	.events-header {
		@apply flex items-center justify-between;
	}

	.events-header h3 {
		@apply text-sm font-medium text-[var(--color-text-secondary)];
	}

	.events-count {
		@apply text-xs text-[var(--color-text-muted)];
	}

	.events-list {
		@apply p-4 rounded-lg bg-[var(--color-bg-tertiary)] border border-[var(--color-border)] space-y-2 max-h-[200px] overflow-y-auto;
	}

	.event-item {
		@apply flex items-center gap-3 text-sm;
	}

	.event-icon {
		@apply w-4 text-center;
	}

	.event-type {
		@apply flex-1 text-[var(--color-text-secondary)] capitalize;
	}

	.event-time {
		@apply text-xs text-[var(--color-text-muted)] font-mono;
	}

	.event-state {
		@apply px-1.5 py-0.5 rounded text-xs bg-[var(--color-bg-secondary)] text-[var(--color-text-muted)];
	}

	.event-iteration {
		@apply px-1.5 py-0.5 rounded text-xs bg-[var(--color-accent-primary)]/20 text-[var(--color-accent-primary)];
	}

	.empty-events {
		@apply flex items-center justify-center py-8 rounded-lg bg-[var(--color-bg-tertiary)] border border-[var(--color-border)] text-[var(--color-text-muted)];
	}

	.show-more-btn {
		@apply flex items-center justify-center gap-1 w-full py-2 text-xs text-[var(--color-text-muted)] hover:text-[var(--color-text-secondary)] transition-all;
	}
</style>
