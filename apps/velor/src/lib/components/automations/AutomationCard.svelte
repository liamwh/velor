<script lang="ts">
	import {
		Calendar,
		Play,
		Power,
		PowerOff,
		MoreVertical,
		CheckCircle,
		XCircle,
		Clock
	} from 'lucide-svelte';
	import type { Automation } from '$lib/types';

	interface Props {
		automation: Automation;
		onToggle?: (name: string, enabled: boolean) => Promise<void>;
		onRun?: (name: string) => Promise<void>;
		onEdit?: (automation: Automation) => void;
		onViewRuns?: (name: string) => void;
		onDelete?: (name: string) => Promise<void>;
		isToggling?: boolean;
		isRunning?: boolean;
		isDeleting?: boolean;
	}

	let {
		automation,
		onToggle,
		onRun,
		onEdit,
		onViewRuns,
		onDelete,
		isToggling = false,
		isRunning = false,
		isDeleting = false
	}: Props = $props();

	let showMenu = $state(false);
	let showDeleteConfirm = $state(false);

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
			return { icon: Clock, class: 'text-primary animate-pulse', label: 'Running' };
		}
		if (automation.enabled) {
			return { icon: CheckCircle, class: 'text-[var(--color-success)]', label: 'Enabled' };
		}
		return { icon: XCircle, class: 'text-muted-foreground', label: 'Disabled' };
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

	function handleDeleteClick(e: Event) {
		e.stopPropagation();
		showMenu = false;
		showDeleteConfirm = true;
	}

	function cancelDelete() {
		showDeleteConfirm = false;
	}

	async function confirmDelete(e: Event) {
		e.stopPropagation();
		showDeleteConfirm = false;
		if (onDelete) {
			await onDelete(automation.name);
		}
	}

	const status = $derived(getStatusIcon(automation));
</script>

<div
	class="relative p-4 rounded-lg bg-card border border-border hover:border-input transition-all cursor-pointer {isRunning
		? 'border-primary shadow-[0_0_0_1px_var(--color-accent-primary)]'
		: ''}"
	role="button"
	tabindex="0"
>
	<div class="flex items-center gap-3 mb-2">
		<div class="flex-shrink-0">
			<status.icon size={20} class={status.class} />
		</div>
		<h3 class="flex-1 text-lg font-semibold text-foreground truncate">{automation.name}</h3>
		<div class="flex items-center gap-1">
			{#if onToggle}
				<button
					class="p-1.5 rounded text-muted-foreground hover:text-foreground hover:bg-muted transition-all disabled:opacity-50 disabled:cursor-not-allowed {automation.enabled
						? 'text-[var(--color-success)]'
						: ''} {isToggling
						? 'animate-spin'
						: ''}"
					onclick={handleToggle}
					disabled={isToggling}
					aria-label={automation.enabled ? 'Disable automation' : 'Enable automation'}
					title={automation.enabled ? 'Disable' : 'Enable'}
				>
					{#if isToggling}
						<Clock size={16} />
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
					class="p-1.5 rounded text-muted-foreground hover:text-foreground hover:bg-muted transition-all disabled:opacity-50 disabled:cursor-not-allowed"
					onclick={handleRun}
					disabled={!automation.enabled || isRunning}
					aria-label="Run automation now"
					title="Run now"
				>
					<Play size={16} />
				</button>
			{/if}
			<button
				class="p-1.5 rounded text-muted-foreground hover:text-foreground hover:bg-muted transition-all"
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
		<p class="text-sm text-muted-foreground mb-3 line-clamp-2">{automation.description}</p>
	{/if}

	<div class="flex items-center gap-2 text-sm text-muted-foreground mb-2">
		<Calendar size={14} />
		<span>{formatSchedule(automation.schedule)}</span>
		<span class="text-xs text-muted-foreground opacity-70">{automation.timezone}</span>
	</div>

	<div class="flex items-center gap-2 text-sm">
		<span class="text-muted-foreground">Prompt:</span>
		<code
			class="px-1.5 py-0.5 rounded bg-muted text-primary text-xs font-mono">{automation.prompt}</code
		>
	</div>

	{#if showMenu}
		<div
			class="absolute top-12 right-2 z-10 min-w-[120px] py-1 bg-card border border-border rounded-lg shadow-lg"
		>
			<button
				class="w-full px-3 py-2 text-left text-sm text-muted-foreground hover:text-foreground hover:bg-muted transition-all"
				onclick={handleEdit}
			>
				<span>Edit</span>
			</button>
			<button
				class="w-full px-3 py-2 text-left text-sm text-muted-foreground hover:text-foreground hover:bg-muted transition-all"
				onclick={handleViewRuns}
			>
				<span>View Runs</span>
			</button>
			{#if onDelete}
				<button
					class="w-full px-3 py-2 text-left text-sm text-red-400 hover:text-red-300 hover:bg-red-500/10 transition-all"
					onclick={handleDeleteClick}
					disabled={isDeleting}
				>
					<span>{isDeleting ? 'Deleting...' : 'Delete'}</span>
				</button>
			{/if}
		</div>
	{/if}

	{#if showDeleteConfirm}
		<!-- svelte-ignore a11y_no_static_element_interactions -->
		<div
			class="absolute inset-0 z-20 flex items-center justify-center bg-black/60 rounded-lg"
			onclick={(e) => e.stopPropagation()}
			onkeydown={(e) => e.stopPropagation()}
		>
			<div class="flex flex-col items-center gap-2 p-4 bg-card border border-border rounded-lg shadow-xl">
				<p class="text-sm text-foreground">Delete "{automation.name}"?</p>
				<p class="text-xs text-muted-foreground">This action cannot be undone.</p>
				<div class="flex items-center gap-2 mt-2">
					<button
						class="px-3 py-1.5 rounded text-xs bg-muted text-muted-foreground hover:bg-muted/80 transition-all"
						onclick={cancelDelete}
					>
						Cancel
					</button>
					<button
						class="px-3 py-1.5 rounded text-xs bg-red-500/20 text-red-400 hover:bg-red-500/30 transition-all"
						onclick={confirmDelete}
						disabled={isDeleting}
					>
						{isDeleting ? 'Deleting...' : 'Delete'}
					</button>
				</div>
			</div>
		</div>
	{/if}

	{#if isRunning}
		<div
			class="absolute top-2 right-2 flex items-center gap-1.5 px-2 py-1 rounded-full bg-[var(--color-accent-light)] text-primary text-xs font-medium"
		>
			<span class="w-1.5 h-1.5 rounded-full bg-primary animate-pulse"></span>
			<span>Running...</span>
		</div>
	{/if}
</div>
