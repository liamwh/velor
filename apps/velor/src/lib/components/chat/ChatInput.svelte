<script lang="ts">
	import { executionStore } from '$lib/stores';
	import type { ExecutionConfig, Prompt } from '$lib/types';
	import { Send, Loader2, Settings, FileCode } from 'lucide-svelte';

	interface Props {
		prompts: Prompt[];
		loading?: boolean;
		onStart?: (config: ExecutionConfig) => void;
	}

	let { prompts = [], loading = false, onStart }: Props = $props();

	let selectedPrompt = $state<Prompt | undefined>(
		prompts.find((p) => p.name === 'default') || prompts[0]
	);
	let customVars = $state<Record<string, string>>({});
	let showVarEditor = $state(false);
	let newVarKey = $state('');
	let newVarValue = $state('');

	// Get available prompts (non-template ones)
	const availablePrompts = $derived(() => {
		return prompts.filter((p) => !p.is_template);
	});

	// Get template vars from selected prompt
	const templateVars = $derived(() => {
		if (!selectedPrompt?.vars) return [];
		return Object.entries(selectedPrompt.vars).map(([key, value]) => ({
			key,
			default: value
		}));
	});

	// Combined vars (template defaults + custom overrides)
	const combinedVars = $derived(() => {
		const vars: Record<string, string> = {};
		// Start with template defaults
		for (const [key, value] of Object.entries(selectedPrompt?.vars || {})) {
			vars[key] = String(value);
		}
		// Apply custom overrides
		for (const [key, value] of Object.entries(customVars)) {
			vars[key] = value;
		}
		return vars;
	});

	// Start execution
	async function handleSubmit() {
		if (!selectedPrompt || loading) return;

		const config: ExecutionConfig = {
			prompt_name: selectedPrompt.name,
			vars: combinedVars
		};

		if (onStart) {
			onStart(config);
		} else {
			await executionStore.start(config);
		}
	}

	// Handle Enter key (Shift+Enter for newline)
	function handleKeydown(event: KeyboardEvent) {
		if (event.key === 'Enter' && !event.shiftKey) {
			event.preventDefault();
			handleSubmit();
		}
	}

	// Add a new variable
	function addVariable() {
		if (newVarKey.trim()) {
			customVars = { ...customVars, [newVarKey.trim()]: newVarValue };
			newVarKey = '';
			newVarValue = '';
		}
	}

	// Remove a variable
	function removeVariable(key: string) {
		const updated = { ...customVars };
		delete updated[key];
		customVars = updated;
	}

	// Reset custom vars
	function resetVars() {
		customVars = {};
	}
</script>

<div class="chat-input">
	<div class="input-wrapper">
		<!-- Prompt Selector -->
		<select
			bind:value={selectedPrompt}
			class="prompt-select"
			disabled={loading}
			aria-label="Select prompt template"
		>
			{#each availablePrompts() as prompt}
				<option value={prompt}>{prompt.name}</option>
			{/each}
		</select>

		<!-- Variable Editor Toggle -->
		<button
			class="var-toggle"
			onclick={() => (showVarEditor = !showVarEditor)}
			disabled={loading}
			title="Edit variables"
			aria-label="Edit variables"
			aria-pressed={showVarEditor}
		>
			<Settings size={18} />
		</button>

		<!-- Submit Button -->
		<button
			class="submit-btn"
			onclick={handleSubmit}
			disabled={loading || !selectedPrompt}
			title="Start execution (Enter)"
			aria-label="Start execution"
		>
			{#if loading}
				<Loader2 size={18} class="spinning" />
			{:else}
				<Send size={18} />
			{/if}
		</button>
	</div>

	<!-- Variable Editor Panel -->
	{#if showVarEditor}
		<div class="var-editor">
			<div class="var-header">
				<span class="var-title">Variables</span>
				<button
					class="reset-btn"
					onclick={resetVars}
					disabled={loading || Object.keys(customVars).length === 0}
				>
					Reset
				</button>
			</div>

			<!-- Template Variables (read-only) -->
			{#if templateVars().length > 0}
				<div class="var-section">
					<span class="var-section-title">Template Defaults</span>
					{#each templateVars() as { key, default: defaultValue }}
						<div class="var-item var-item-readonly">
							<span class="var-key">{key}</span>
							<span class="var-value">{String(defaultValue)}</span>
						</div>
					{/each}
				</div>
			{/if}

			<!-- Custom Variables -->
			{#if Object.keys(customVars).length > 0}
				<div class="var-section">
					<span class="var-section-title">Custom Overrides</span>
					{#each Object.entries(customVars) as [key, value]}
						<div class="var-item">
							<span class="var-key">{key}</span>
							<input
								type="text"
								bind:value={customVars[key]}
								class="var-input"
								disabled={loading}
								placeholder="Value"
							/>
							<button
								class="var-remove"
								onclick={() => removeVariable(key)}
								disabled={loading}
								aria-label="Remove variable"
							>
								×
							</button>
						</div>
					{/each}
				</div>
			{/if}

			<!-- Add New Variable -->
			<div class="var-add">
				<input
					type="text"
					bind:value={newVarKey}
					class="var-input var-key-input"
					placeholder="Variable name"
					disabled={loading}
					onkeydown={(e) => e.key === 'Enter' && addVariable()}
				/>
				<input
					type="text"
					bind:value={newVarValue}
					class="var-input var-value-input"
					placeholder="Value"
					disabled={loading}
					onkeydown={(e) => e.key === 'Enter' && addVariable()}
				/>
				<button class="var-add-btn" onclick={addVariable} disabled={loading || !newVarKey.trim()}>
					<Settings size={16} />
				</button>
			</div>
		</div>
	{/if}
</div>

<style>
	.chat-input {
		@apply border-t border-[var(--color-border)] bg-[var(--color-bg-secondary)];
	}

	.input-wrapper {
		@apply flex items-center gap-2 p-3;
	}

	.prompt-select {
		@apply flex-1 px-3 py-2 rounded-lg bg-[var(--color-bg-primary)] border border-[var(--color-border)] text-[var(--color-text-primary)] text-sm focus:outline-none focus:border-[var(--color-accent-primary)] disabled:opacity-50 disabled:cursor-not-allowed;
	}

	.var-toggle,
	.submit-btn {
		@apply p-2 rounded-lg text-[var(--color-text-secondary)] hover:text-[var(--color-text-primary)] hover:bg-[var(--color-bg-tertiary)] transition-all disabled:opacity-50 disabled:cursor-not-allowed;
	}

	.submit-btn {
		@apply bg-[var(--color-accent-primary)] text-white hover:bg-[var(--color-accent-hover)];
	}

	.var-editor {
		@apply border-t border-[var(--color-border)] p-3 space-y-3;
	}

	.var-header {
		@apply flex items-center justify-between;
	}

	.var-title {
		@apply text-sm font-medium text-[var(--color-text-primary)];
	}

	.reset-btn {
		@apply text-xs text-[var(--color-accent-primary)] hover:text-[var(--color-accent-hover)] disabled:opacity-50 disabled:cursor-not-allowed;
	}

	.var-section {
		@apply space-y-2;
	}

	.var-section-title {
		@apply text-xs font-medium text-[var(--color-text-muted)] uppercase tracking-wide;
	}

	.var-item {
		@apply flex items-center gap-2;
	}

	.var-item-readonly {
		@apply opacity-75;
	}

	.var-key {
		@apply text-sm text-[var(--color-text-secondary)] min-w-24;
	}

	.var-value {
		@apply text-sm text-[var(--color-text-muted)] font-mono;
	}

	.var-input {
		@apply flex-1 px-2 py-1.5 rounded bg-[var(--color-bg-primary)] border border-[var(--color-border)] text-[var(--color-text-primary)] text-sm focus:outline-none focus:border-[var(--color-accent-primary)] disabled:opacity-50 disabled:cursor-not-allowed;
	}

	.var-key-input,
	.var-value-input {
		@apply flex-none min-w-0;
	}

	.var-key-input {
		@apply w-32;
	}

	.var-value-input {
		@apply flex-1;
	}

	.var-remove {
		@apply w-6 h-6 flex items-center justify-center rounded text-[var(--color-text-muted)] hover:text-red-400 hover:bg-red-950/50 transition-all disabled:opacity-50 disabled:cursor-not-allowed;
	}

	.var-add {
		@apply flex items-center gap-2;
	}

	.var-add-btn {
		@apply p-1.5 rounded text-[var(--color-text-secondary)] hover:text-[var(--color-text-primary)] hover:bg-[var(--color-bg-tertiary)] transition-all disabled:opacity-50 disabled:cursor-not-allowed;
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
</style>
