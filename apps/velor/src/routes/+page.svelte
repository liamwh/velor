<script lang="ts">
	import { config, gitRoot } from '$lib/stores';
	import { Activity, Zap, FolderOpen } from 'lucide-svelte';
</script>

<div class="welcome">
	<div class="hero">
		<h1>Welcome to Velor</h1>
		<p class="subtitle">
			Autonomous AI agents powered by Claude, now with a beautiful GUI.
		</p>
	</div>

	<div class="cards">
		<div class="card">
			<div class="card-icon">
				<Activity size={24} />
			</div>
			<h3>Run Agents</h3>
			<p>Execute automated AI agents with your configured prompts and variables.</p>
		</div>

		<div class="card">
			<div class="card-icon">
				<Zap size={24} />
			</div>
			<h3>Schedule Automations</h3>
			<p>Set up cron-based automations that run your agents on a schedule.</p>
		</div>

		<div class="card">
			<div class="card-icon">
				<FolderOpen size={24} />
			</div>
			<h3>Manage Configuration</h3>
			<p>Edit global and project-level settings through the interface.</p>
		</div>
	</div>

	{#if $config}
		<div class="config-status">
			<h2>Configuration Status</h2>
			<div class="status-item">
				<span class="label">Git Root:</span>
				<span class="value">{$gitRoot || 'Not detected'}</span>
			</div>
			<div class="status-item">
				<span class="label">Prompts:</span>
				<span class="value">{$config.prompts ? Object.keys($config.prompts).length : 0} configured</span>
			</div>
			<div class="status-item">
				<span class="label">Claude Binary:</span>
				<span class="value">{$config.binary || 'claude-glm'}</span>
			</div>
		</div>
	{/if}
</div>

<style>
	.welcome {
		@apply max-w-4xl mx-auto;
	}

	.hero {
		@apply text-center mb-12;
	}

	.hero h1 {
		@apply text-4xl font-bold text-[var(--color-text-primary)] mb-3;
	}

	.subtitle {
		@apply text-lg text-[var(--color-text-secondary)];
	}

	.cards {
		@apply grid grid-cols-1 md:grid-cols-3 gap-6 mb-12;
	}

	.card {
		@apply p-6 rounded-xl bg-[var(--color-bg-secondary)] border border-[var(--color-border)] hover:border-[var(--color-accent-primary)] transition-all duration-200;
	}

	.card-icon {
		@apply w-12 h-12 rounded-lg bg-[var(--color-accent-light)] flex items-center justify-center text-[var(--color-accent-primary)] mb-4;
	}

	.card h3 {
		@apply text-lg font-semibold text-[var(--color-text-primary)] mb-2;
	}

	.card p {
		@apply text-sm text-[var(--color-text-secondary)];
	}

	.config-status {
		@apply p-6 rounded-xl bg-[var(--color-bg-secondary)] border border-[var(--color-border)];
	}

	.config-status h2 {
		@apply text-lg font-semibold text-[var(--color-text-primary)] mb-4;
	}

	.status-item {
		@apply flex justify-between py-2 border-b border-[var(--color-border)] last:border-0;
	}

	.status-item .label {
		@apply text-sm text-[var(--color-text-secondary)];
	}

	.status-item .value {
		@apply text-sm text-[var(--color-text-primary)] font-medium;
	}
</style>

