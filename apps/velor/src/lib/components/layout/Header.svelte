<script lang="ts">
	import * as Sidebar from "$lib/components/ui/sidebar/index.js";
	import { gitRoot, config } from "$lib/stores";
	import { FolderGit2, Activity } from "lucide-svelte";

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
	<div class="header-left flex items-center gap-2">
		<Sidebar.Trigger />
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
