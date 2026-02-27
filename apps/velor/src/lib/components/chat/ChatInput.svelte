<script lang="ts">
	import { executionStore } from '$lib/stores';
	import type { ExecutionConfig, PromptTemplate } from '$lib/types';
	import { Send, Loader2, Settings } from 'lucide-svelte';

	interface Props {
		prompts: PromptTemplate[];
		loading?: boolean;
		onStart?: (config: ExecutionConfig) => void;
	}

	let { prompts = [], loading = false, onStart }: Props = $props();

	let selectedPrompt = $state<PromptTemplate | undefined>(undefined);
	let customVars = $state<Record<string, string>>({});
	let showVarEditor = $state(false);
	let newVarKey = $state('');
	let newVarValue = $state('');

	// Initialize selected prompt when prompts change
	$effect(() => {
		if (!selectedPrompt || !prompts.includes(selectedPrompt)) {
			selectedPrompt = prompts.find((p) => p.name === 'default') || prompts[0];
		}
	});

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
			vars: combinedVars()
		};

		if (onStart) {
			onStart(config);
		} else {
			await executionStore.start(config);
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

<div class="border-t border-border bg-card">
	<div class="flex items-center gap-2 p-3">
		<!-- Prompt Selector -->
		<select
			bind:value={selectedPrompt}
			class="flex-1 px-3 py-2 rounded-lg bg-background border border-border text-foreground text-sm focus:outline-none focus:border-primary disabled:opacity-50 disabled:cursor-not-allowed"
			disabled={loading}
			aria-label="Select prompt template"
		>
			{#each availablePrompts() as prompt (prompt.name)}
				<option value={prompt}>{prompt.name}</option>
			{/each}
		</select>

		<!-- Variable Editor Toggle -->
		<button
			class="p-2 rounded-lg text-muted-foreground hover:text-foreground hover:bg-muted transition-all disabled:opacity-50 disabled:cursor-not-allowed"
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
			class="p-2 rounded-lg bg-primary text-white hover:bg-[var(--color-accent-hover)] transition-all disabled:opacity-50 disabled:cursor-not-allowed"
			onclick={handleSubmit}
			disabled={loading || !selectedPrompt}
			title="Start execution (Enter)"
			aria-label="Start execution"
		>
			{#if loading}
				<Loader2 size={18} class="animate-spin" />
			{:else}
				<Send size={18} />
			{/if}
		</button>
	</div>

	<!-- Variable Editor Panel -->
	{#if showVarEditor}
		<div class="border-t border-border p-3 space-y-3">
			<div class="flex items-center justify-between">
				<span class="text-sm font-medium text-foreground">Variables</span>
				<button
					class="text-xs text-primary hover:text-[var(--color-accent-hover)] disabled:opacity-50 disabled:cursor-not-allowed"
					onclick={resetVars}
					disabled={loading || Object.keys(customVars).length === 0}
				>
					Reset
				</button>
			</div>

			<!-- Template Variables (read-only) -->
			{#if templateVars().length > 0}
				<div class="space-y-2">
					<span class="text-xs font-medium text-muted-foreground uppercase tracking-wide"
						>Template Defaults</span
					>
					{#each templateVars() as { key, default: defaultValue } (key)}
						<div class="flex items-center gap-2 opacity-75">
							<span class="text-sm text-muted-foreground min-w-24">{key}</span>
							<span class="text-sm text-muted-foreground font-mono">{String(defaultValue)}</span>
						</div>
					{/each}
				</div>
			{/if}

			<!-- Custom Variables -->
			{#if Object.keys(customVars).length > 0}
				<div class="space-y-2">
					<span class="text-xs font-medium text-muted-foreground uppercase tracking-wide"
						>Custom Overrides</span
					>
					{#each Object.entries(customVars) as [key] (key)}
						<div class="flex items-center gap-2">
							<span class="text-sm text-muted-foreground min-w-24">{key}</span>
							<input
								type="text"
								bind:value={customVars[key]}
								class="flex-1 px-2 py-1.5 rounded bg-background border border-border text-foreground text-sm focus:outline-none focus:border-primary disabled:opacity-50 disabled:cursor-not-allowed"
								disabled={loading}
								placeholder="Value"
							/>
							<button
								class="w-6 h-6 flex items-center justify-center rounded text-muted-foreground hover:text-red-400 hover:bg-red-950/50 transition-all disabled:opacity-50 disabled:cursor-not-allowed"
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
			<div class="flex items-center gap-2">
				<input
					type="text"
					bind:value={newVarKey}
					class="w-32 flex-none min-w-0 px-2 py-1.5 rounded bg-background border border-border text-foreground text-sm focus:outline-none focus:border-primary disabled:opacity-50 disabled:cursor-not-allowed"
					placeholder="Variable name"
					disabled={loading}
					onkeydown={(e) => e.key === 'Enter' && addVariable()}
				/>
				<input
					type="text"
					bind:value={newVarValue}
					class="flex-1 px-2 py-1.5 rounded bg-background border border-border text-foreground text-sm focus:outline-none focus:border-primary disabled:opacity-50 disabled:cursor-not-allowed"
					placeholder="Value"
					disabled={loading}
					onkeydown={(e) => e.key === 'Enter' && addVariable()}
				/>
				<button
					class="p-1.5 rounded text-muted-foreground hover:text-foreground hover:bg-muted transition-all disabled:opacity-50 disabled:cursor-not-allowed"
					onclick={addVariable}
					disabled={loading || !newVarKey.trim()}
				>
					<Settings size={16} />
				</button>
			</div>
		</div>
	{/if}
</div>
