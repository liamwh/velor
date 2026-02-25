<script lang="ts">
	import { gitRoot, config } from '$lib/stores';
	import { FolderGit2, Activity } from 'lucide-svelte';

	let currentGitRoot = $state<string | null>(null);
	let promptCount = $state(0);

	// Subscribe to config changes
	$effect(() => {
		const unsubscribe = gitRoot.subscribe((root) => {
			currentGitRoot = root;
		});
		return unsubscribe;
	});

	$effect(() => {
		const unsubscribe = config.subscribe((cfg) => {
			if (cfg?.prompts) {
				promptCount = Object.keys(cfg.prompts).length;
			}
		});
		return unsubscribe;
	});
</script>

<header class="header">
	<div class="header-left">
		<h1 class="title">Velor Agent CLI</h1>
		{#if currentGitRoot}
			<div class="git-root" title="Project root">
				<FolderGit2 size={14} />
				<span class="git-path">{currentGitRoot}</span>
			</div>
		{/if}
	</div>

	<div class="header-right">
		<div class="stats">
			<div class="stat-item" title="Available prompts">
				<Activity size={14} />
				<span class="stat-label">{promptCount} Prompts</span>
			</div>
		</div>
	</div>
</header>

<style>
	.header {
		@apply flex items-center justify-between h-16 px-6 border-b border-[var(--color-border)] bg-[var(--color-bg-secondary)];
	}

	.header-left {
		@apply flex items-center gap-4;
	}

	.title {
		@apply text-lg font-semibold text-[var(--color-text-primary)];
	}

	.git-root {
		@apply flex items-center gap-1.5 px-2.5 py-1 rounded text-xs text-[var(--color-text-secondary)] bg-[var(--color-bg-tertiary)];
	}

	.git-path {
		@apply max-w-[300px] truncate;
	}

	.header-right {
		@apply flex items-center gap-4;
	}

	.stats {
		@apply flex items-center gap-3;
	}

	.stat-item {
		@apply flex items-center gap-1.5 text-sm text-[var(--color-text-secondary)];
	}

	.stat-label {
		@apply text-xs;
	}
</style>
