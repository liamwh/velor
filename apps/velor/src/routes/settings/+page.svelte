<script lang="ts">
	import { onMount } from 'svelte';
	import { configStore, configLoading, configError } from '$lib/stores';
	import ConfigEditor from '$lib/components/settings/ConfigEditor.svelte';
	import PromptEditor from '$lib/components/settings/PromptEditor.svelte';
	import NotificationSettings from '$lib/components/settings/NotificationSettings.svelte';
	import { Settings } from 'lucide-svelte';

	type Tab = 'config' | 'prompts' | 'notifications';

	let activeTab: Tab = $state('config');

	const tabs = [
		{ id: 'config' as Tab, label: 'Configuration', icon: Settings },
		{ id: 'prompts' as Tab, label: 'Prompts', icon: Settings },
		{ id: 'notifications' as Tab, label: 'Notifications', icon: Settings }
	];

	onMount(() => {
		configStore.load();
	});

	function setTab(tab: Tab) {
		activeTab = tab;
	}
</script>

<div class="max-w-6xl mx-auto">
	<header class="mb-6">
		<div class="flex items-center gap-4">
			<Settings size={28} class="text-primary" />
			<div>
				<h1 class="text-2xl font-bold text-foreground">Settings</h1>
				<p class="text-sm text-muted-foreground mt-0.5">Manage your Velor configuration</p>
			</div>
		</div>
	</header>

	<!-- Tab Navigation -->
	<nav class="flex gap-2 border-b border-border mb-6" aria-label="Settings tabs">
		{#each tabs as tab (tab.id)}
			{@const Icon = tab.icon}
			<button
				class="flex items-center gap-2 px-4 py-3 text-sm font-medium text-muted-foreground border-b-2 border-transparent hover:text-foreground transition-all duration-200 {activeTab ===
				tab.id
					? 'text-primary border-b-primary'
					: ''}"
				onclick={() => setTab(tab.id)}
				aria-label={tab.label}
				aria-selected={activeTab === tab.id}
				role="tab"
			>
				<Icon size={18} />
				<span>{tab.label}</span>
			</button>
		{/each}
	</nav>

	<!-- Tab Content -->
	<main class="min-h-[400px]">
		{#if $configLoading}
			<div class="flex flex-col items-center justify-center py-16 text-muted-foreground gap-4">
				<div class="w-8 h-8 rounded-full border-2 border-border border-t-primary animate-spin"></div>
				<p>Loading configuration...</p>
			</div>
		{:else if $configError}
			<div class="flex flex-col items-center justify-center py-16 text-muted-foreground gap-4">
				<span class="text-4xl">⚠</span>
				<h3 class="text-lg font-semibold text-foreground">Error Loading Configuration</h3>
				<p>{$configError}</p>
				<button
					onclick={() => configStore.load()}
					class="px-4 py-2 rounded-lg bg-primary text-white hover:bg-[var(--color-accent-hover)] transition-all duration-200"
				>
					Retry
				</button>
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
