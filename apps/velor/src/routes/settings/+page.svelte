<script lang="ts">
	import { onMount } from 'svelte';
	import { configStore, homeConfig, repoConfig, configLoading, configError } from '$lib/stores';
	import ConfigEditor from '$lib/components/settings/ConfigEditor.svelte';
	import PromptEditor from '$lib/components/settings/PromptEditor.svelte';
	import NotificationSettings from '$lib/components/settings/NotificationSettings.svelte';
	import { Settings } from 'lucide-svelte';

	type Tab = 'config' | 'prompts' | 'notifications';

	let activeTab: Tab = $state('config');
	let mounted = $state(false);

	const tabs = [
		{ id: 'config' as Tab, label: 'Configuration', icon: Settings },
		{ id: 'prompts' as Tab, label: 'Prompts', icon: Settings },
		{ id: 'notifications' as Tab, label: 'Notifications', icon: Settings }
	];

	onMount(() => {
		mounted = true;
		configStore.load();
	});

	function setTab(tab: Tab) {
		activeTab = tab;
	}
</script>

<div class="settings-page">
	<header class="settings-header">
		<div class="header-title">
			<Settings size={28} />
			<div>
				<h1>Settings</h1>
				<p>Manage your Velor configuration</p>
			</div>
		</div>
	</header>

	<!-- Tab Navigation -->
	<nav class="tabs" aria-label="Settings tabs">
		{#each tabs as tab}
			<button
				class="tab"
				class:active={activeTab === tab.id}
				onclick={() => setTab(tab.id)}
				aria-label={tab.label}
				aria-selected={activeTab === tab.id}
				role="tab"
			>
				<svelte:component this={tab.icon} size={18} />
				<span>{tab.label}</span>
			</button>
		{/each}
	</nav>

	<!-- Tab Content -->
	<main class="settings-content">
		{#if configLoading()}
			<div class="loading-state">
				<div class="spinner"></div>
				<p>Loading configuration...</p>
			</div>
		{:else if configError()}
			<div class="error-state">
				<span class="error-icon">⚠</span>
				<h3>Error Loading Configuration</h3>
				<p>{configError()}</p>
				<button onclick={() => configStore.load()} class="retry-btn">Retry</button>
			</div>
		{:else}
			{#if activeTab === 'config'}
				<ConfigEditor />
			{:else if activeTab === 'prompts'}
				<PromptEditor />
			{:else if activeTab === 'notifications'}
				<NotificationSettings />
			{/if}
		{/if}
	</main>
</div>

<style>
	.settings-page {
		@apply max-w-6xl mx-auto;
	}

	.settings-header {
		@apply mb-6;
	}

	.header-title {
		@apply flex items-center gap-4;
	}

	.header-title h1 {
		@apply text-2xl font-bold text-[var(--color-text-primary)];
	}

	.header-title p {
		@apply text-sm text-[var(--color-text-secondary)] mt-0.5;
	}

	.header-title :global(svg) {
		@apply text-[var(--color-accent-primary)];
	}

	.tabs {
		@apply flex gap-2 border-b border-[var(--color-border)] mb-6;
	}

	.tab {
		@apply flex items-center gap-2 px-4 py-3 text-sm font-medium text-[var(--color-text-secondary)] border-b-2 border-transparent hover:text-[var(--color-text-primary)] transition-all duration-200;
	}

	.tab.active {
		@apply text-[var(--color-accent-primary)] border-b-[var(--color-accent-primary)];
	}

	.settings-content {
		@apply min-h-[400px];
	}

	.loading-state,
	.error-state {
		@apply flex flex-col items-center justify-center py-16 text-[var(--color-text-secondary)] gap-4;
	}

	.error-state h3 {
		@apply text-lg font-semibold text-[var(--color-text-primary)];
	}

	.error-icon {
		@apply text-4xl;
	}

	.retry-btn {
		@apply px-4 py-2 rounded-lg bg-[var(--color-accent-primary)] text-white hover:bg-[var(--color-accent-hover)] transition-all duration-200;
	}

	.spinner {
		@apply w-8 h-8 rounded-full border-2 border-[var(--color-border)] border-t-[var(--color-accent-primary)] animate-spin;
	}
</style>
