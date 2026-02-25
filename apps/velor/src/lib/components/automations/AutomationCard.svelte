<script lang="ts">
	import {
		Calendar,
		Play,
		Power,
		PowerOff,
		MoreVertical,
		CheckCircle,
		XCircle,
		Clock,
		AlertTriangle
	} from 'lucide-svelte';
	import type { Automation } from '$lib/types';

	interface Props {
		automation: Automation;
		onToggle?: (name: string, enabled: boolean) => Promise<void>;
		onRun?: (name: string) => Promise<void>;
		onEdit?: (automation: Automation) => void;
		onViewRuns?: (name: string) => void;
		isToggling?: boolean;
		isRunning?: boolean;
	}

	let {
		automation,
		onToggle,
		onRun,
		onEdit,
		onViewRuns,
		isToggling = false,
		isRunning = false
	}: Props = $props();

	let showMenu = $state(false);

	function formatSchedule(cron: string): string {
		// Simple cron formatter for 6-field cron (seconds minutes hours day month weekday)
		const parts = cron.split(' ');
		if (parts.length >= 6) {
			const [seconds, minute, hour] = parts;
			if (seconds === '*' && minute === '*' && hour === '*') return 'Every minute';
			if (minute === '*' && hour === '*') return `Every ${seconds} seconds`;
			if (hour === '*') return `At ${seconds}:${minute.padStart(2, '0')} every hour`;
			if (minute !== '*' && hour !== '*') return `At ${hour}:${minute.padStart(2, '0')}`;
		}
		return cron;
	}

	function getStatusIcon(automation: Automation) {
		if (isRunning) {
			return { icon: Clock, class: 'status-running', label: 'Running' };
		}
		if (automation.enabled) {
			return { icon: CheckCircle, class: 'status-enabled', label: 'Enabled' };
		}
		return { icon: XCircle, class: 'status-disabled', label: 'Disabled' };
	}

	async function handleToggle(e: Event) {
		e.stopPropagation();
		if (onToggle) {
			await onToggle(automation.name, !automation.enabled);
		}
	}

	async function handleRun(e: Event) {
		e.stopPropagation();
		if (onRun) {
			await onRun(automation.name);
		}
	}

	function handleEdit(e: Event) {
		e.stopPropagation();
		showMenu = false;
		if (onEdit) {
			onEdit(automation);
		}
	}

	function handleViewRuns(e: Event) {
		e.stopPropagation();
		showMenu = false;
		if (onViewRuns) {
			onViewRuns(automation.name);
		}
	}

	const status = $derived(getStatusIcon(automation));
</script>

<div class="automation-card" class:running={isRunning} role="button" tabindex="0">
	<div class="card-header">
		<div class="status-indicator">
			<svelte:component this={status.icon} size={20} class={status.class} />
		</div>
		<h3 class="card-title">{automation.name}</h3>
		<div class="card-actions">
			{#if onToggle}
				<button
					class="toggle-btn"
					class:enabled={automation.enabled}
					class:toggling={isToggling}
					onclick={handleToggle}
					disabled={isToggling}
					aria-label={automation.enabled ? 'Disable automation' : 'Enable automation'}
					title={automation.enabled ? 'Disable' : 'Enable'}
				>
					{#if isToggling}
						<Clock size={16} class="spinning" />
					{:else}
						{#if automation.enabled}
							<PowerOff size={16} />
						{:else}
							<Power size={16} />
						{/if}
					{/if}
				</button>
			{/if}
			{#if onRun}
				<button
					class="run-btn"
					onclick={handleRun}
					disabled={!automation.enabled || isRunning}
					aria-label="Run automation now"
					title="Run now"
				>
					<Play size={16} />
				</button>
			{/if}
			<button
				class="menu-btn"
				onclick={(e) => {
					e.stopPropagation();
					showMenu = !showMenu;
				}}
				aria-label="More options"
				title="More options"
			>
				<MoreVertical size={16} />
			</button>
		</div>
	</div>

	{#if automation.description}
		<p class="card-description">{automation.description}</p>
	{/if}

	<div class="card-schedule">
		<Calendar size={14} />
		<span>{formatSchedule(automation.schedule)}</span>
		<span class="timezone">{automation.timezone}</span>
	</div>

	<div class="card-prompt">
		<span class="prompt-label">Prompt:</span>
		<code class="prompt-name">{automation.prompt}</code>
	</div>

	{#if showMenu}
		<div class="dropdown-menu">
			<button class="menu-item" onclick={handleEdit}>
				<span>Edit</span>
			</button>
			<button class="menu-item" onclick={handleViewRuns}>
				<span>View Runs</span>
			</button>
		</div>
	{/if}

	{#if isRunning}
		<div class="running-indicator">
			<span class="running-dot"></span>
			<span>Running...</span>
		</div>
	{/if}
</div>

<style>
	.automation-card {
		@apply relative p-4 rounded-lg bg-[var(--color-bg-secondary)] border border-[var(--color-border)] hover:border-[var(--color-border-hover)] transition-all cursor-pointer;
	}

	.automation-card.running {
		@apply border-[var(--color-accent-primary)] shadow-[0_0_0_1px_var(--color-accent-primary)];
	}

	.card-header {
		@apply flex items-center gap-3 mb-2;
	}

	.status-indicator {
		@apply flex-shrink-0;
	}

	.status-enabled {
		@apply text-[var(--color-success)];
	}

	.status-disabled {
		@apply text-[var(--color-text-muted)];
	}

	.status-running {
		@apply text-[var(--color-accent-primary)] animate-pulse;
	}

	.card-title {
		@apply flex-1 text-lg font-semibold text-[var(--color-text-primary)] truncate;
	}

	.card-actions {
		@apply flex items-center gap-1;
	}

	.toggle-btn,
	.run-btn,
	.menu-btn {
		@apply p-1.5 rounded text-[var(--color-text-secondary)] hover:text-[var(--color-text-primary)] hover:bg-[var(--color-bg-tertiary)] transition-all disabled:opacity-50 disabled:cursor-not-allowed;
	}

	.toggle-btn.enabled {
		@apply text-[var(--color-success)];
	}

	.toggle-btn.toggling {
		@apply animate-spin;
	}

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

	.card-description {
		@apply text-sm text-[var(--color-text-secondary)] mb-3 line-clamp-2;
	}

	.card-schedule {
		@apply flex items-center gap-2 text-sm text-[var(--color-text-muted)] mb-2;
	}

	.timezone {
		@apply text-xs text-[var(--color-text-muted)] opacity-70;
	}

	.card-prompt {
		@apply flex items-center gap-2 text-sm;
	}

	.prompt-label {
		@apply text-[var(--color-text-muted)];
	}

	.prompt-name {
		@apply px-1.5 py-0.5 rounded bg-[var(--color-bg-tertiary)] text-[var(--color-accent-primary)] text-xs font-mono;
	}

	.dropdown-menu {
		@apply absolute top-12 right-2 z-10 min-w-[120px] py-1 bg-[var(--color-bg-secondary)] border border-[var(--color-border)] rounded-lg shadow-lg;
	}

	.menu-item {
		@apply w-full px-3 py-2 text-left text-sm text-[var(--color-text-secondary)] hover:text-[var(--color-text-primary)] hover:bg-[var(--color-bg-tertiary)] transition-all;
	}

	.running-indicator {
		@apply absolute top-2 right-2 flex items-center gap-1.5 px-2 py-1 rounded-full bg-[var(--color-accent-light)] text-[var(--color-accent-primary)] text-xs font-medium;
	}

	.running-dot {
		@apply w-1.5 h-1.5 rounded-full bg-[var(--color-accent-primary)] animate-pulse;
	}
</style>
