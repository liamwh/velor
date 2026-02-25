<script lang="ts">
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { page } from '$app/stores';
	import { automationsStore, daemonRunning } from '$lib/stores';
	import { EVENT_SERVICE } from '$lib/services/events';
	import {
		Home,
		Calendar,
		Plus,
		Settings,
		Power,
		PowerOff,
		History,
		Play
	} from 'lucide-svelte';

	// Listen for daemon events from backend
	onMount(async () => {
		await EVENT_SERVICE.onDaemonStarted(({ running }) => {
			automationsStore.setDaemonRunning(running);
		});
		await EVENT_SERVICE.onDaemonStopped(({ running }) => {
			automationsStore.setDaemonRunning(running);
		});
	});

	async function toggleDaemon() {
		if ($daemonRunning) {
			await automationsStore.stopDaemon();
		} else {
			await automationsStore.startDaemon();
		}
	}

	async function navigate(route: string) {
		await goto(route);
	}

	const navItems = [
		{ id: '/', label: 'Home', icon: Home },
		{ id: '/executions', label: 'Executions', icon: History },
		{ id: '/automations', label: 'Automations', icon: Calendar },
		{ id: '/settings', label: 'Settings', icon: Settings },
	];

	const quickActions = [
		{ id: 'new-prompt', label: 'New Prompt', icon: Plus },
		{ id: 'run-now', label: 'Run Now', icon: Play },
	];
</script>

<aside class="sidebar">
	<div class="sidebar-top">
		<div class="logo">
			<span class="logo-text">Velor</span>
		</div>

		<nav class="nav">
			{#each navItems as item}
				<button
					class="nav-item"
					class:active={$page.url.pathname === item.id}
					onclick={() => navigate(item.id)}
					aria-label={item.label}
					title={item.label}
				>
					<svelte:component this={item.icon} size={20} />
					<span class="nav-label">{item.label}</span>
				</button>
			{/each}
		</nav>

		<div class="quick-actions">
			{#each quickActions as action}
				<button
					class="quick-action-btn"
					onclick={() => navigate(action.id)}
					aria-label={action.label}
					title={action.label}
				>
					<svelte:component this={action.icon} size={18} />
					<span>{action.label}</span>
				</button>
			{/each}
		</div>
	</div>

	<div class="sidebar-bottom">
		<button
			class="daemon-toggle"
			class:running={$daemonRunning}
			onclick={toggleDaemon}
			aria-label={$daemonRunning ? 'Stop daemon' : 'Start daemon'}
			title={$daemonRunning ? 'Stop daemon' : 'Start daemon'}
		>
			{#if $daemonRunning}
				<PowerOff size={18} />
				<span>Stop</span>
			{:else}
				<Power size={18} />
				<span>Start</span>
			{/if}
			<span class="daemon-indicator" class:active={$daemonRunning}></span>
		</button>
	</div>
</aside>

<style>
	.sidebar {
		@apply flex flex-col h-full w-64 bg-[var(--color-bg-secondary)] border-r border-[var(--color-border)];
	}

	.sidebar-top {
		@apply flex flex-col flex-1 overflow-hidden;
	}

	.logo {
		@apply flex items-center justify-center h-16 border-b border-[var(--color-border)];
	}

	.logo-text {
		@apply text-xl font-bold text-[var(--color-text-primary)];
	}

	.nav {
		@apply flex flex-col gap-1 p-3;
	}

	.nav-item {
		@apply flex items-center gap-3 px-3 py-2.5 rounded-lg text-[var(--color-text-secondary)] hover:text-[var(--color-text-primary)] hover:bg-[var(--color-bg-tertiary)] transition-all duration-200;
	}

	.nav-item.active {
		@apply bg-[var(--color-accent-primary)] text-[var(--color-text-primary)];
	}

	.nav-label {
		@apply text-sm font-medium;
	}

	.quick-actions {
		@apply flex flex-col gap-2 p-3 border-t border-[var(--color-border)] mt-auto;
	}

	.quick-action-btn {
		@apply flex items-center gap-2 px-3 py-2 rounded-lg text-sm text-[var(--color-text-secondary)] hover:text-[var(--color-text-primary)] hover:bg-[var(--color-bg-tertiary)] transition-all duration-200;
	}

	.sidebar-bottom {
		@apply flex flex-col gap-2 p-3 border-t border-[var(--color-border)];
	}

	.daemon-toggle {
		@apply flex items-center justify-between gap-2 px-3 py-2.5 rounded-lg text-sm text-[var(--color-text-secondary)] hover:bg-[var(--color-bg-tertiary)] transition-all duration-200;
	}

	.daemon-toggle.running {
		@apply text-[var(--color-success)];
	}

	.daemon-indicator {
		@apply w-2 h-2 rounded-full bg-[var(--color-text-muted)];
	}

	.daemon-indicator.active {
		@apply bg-[var(--color-success)] shadow-[0_0_8px_var(--color-success)];
	}

	.settings-btn {
		@apply flex items-center justify-center p-2.5 rounded-lg text-[var(--color-text-secondary)] hover:text-[var(--color-text-primary)] hover:bg-[var(--color-bg-tertiary)] transition-all duration-200;
	}
</style>
