<script lang="ts">
	import { X, Play, ChevronDown, ChevronUp } from 'lucide-svelte';
	import { config, executionStore } from '$lib/stores';
	import { goto } from '$app/navigation';
	import type { ExecutionConfig, PromptTemplate } from '$lib/types';

	interface Props {
		onClose?: () => void;
	}

	let { onClose }: Props = $props();

	let selectedPromptName = $state<string>('');
	let customVars = $state<Record<string, string>>({});
	let showAdvanced = $state(false);
	let maxIterations = $state<number | undefined>(undefined);
	let maxRetries = $state<number | undefined>(undefined);
	let isStarting = $state(false);
	let error = $state<string | null>(null);

	// Extract available prompts from config
	const availablePrompts = $derived((): PromptTemplate[] => {
		if (!$config?.prompts) return [];

		// Convert Vars to the correct type for PromptTemplate
		const convertVars = (): Record<string, string | number | boolean> => {
			const result: Record<string, string | number | boolean> = {};
			for (const [key, value] of Object.entries($config?.vars ?? {})) {
				if (value !== undefined) {
					if (typeof value === 'string' || typeof value === 'number' || typeof value === 'boolean') {
						result[key] = value;
					} else {
						result[key] = String(value);
					}
				}
			}
			return result;
		};

		return Object.entries($config.prompts)
			.filter(([name]) => !name.startsWith('_')) // Filter out template prompts
			.map(([name, prompt]) => {
				// Handle both string and object prompt formats
				let template: string;
				let completeToken: string | undefined;

				if (typeof prompt === 'string') {
					template = prompt;
				} else if (prompt && typeof prompt === 'object' && 'template' in prompt) {
					const promptObj = prompt as { template: string; complete_token?: string };
					template = promptObj.template;
					completeToken = promptObj.complete_token;
				} else {
					template = '';
				}

				return {
					name,
					vars: convertVars(),
					template,
					complete_token: completeToken,
					is_template: false
				};
			});
	});

	// Get the selected prompt object (for potential future use)
	const _selectedPrompt = $derived(
		availablePrompts().find((p) => p.name === selectedPromptName)
	);

	// Get template vars from selected prompt's default vars
	const templateVars = $derived(() => {
		if (!$config?.vars) return [];
		return Object.entries($config.vars).map(([key, value]) => ({
			key,
			default: value
		}));
	});

	// Combined vars (template defaults + custom overrides)
	const combinedVars = $derived(() => {
		const vars: Record<string, string | number | boolean> = {};
		// Start with template defaults from config
		for (const [key, value] of Object.entries($config?.vars ?? {})) {
			vars[key] = String(value);
		}
		// Apply custom overrides
		for (const [key, value] of Object.entries(customVars)) {
			vars[key] = value;
		}
		return vars;
	});

	// Initialize selected prompt when prompts change
	$effect(() => {
		const prompts = availablePrompts();
		if (prompts.length > 0 && !selectedPromptName) {
			// Try to select 'default' or first prompt
			const defaultPrompt = prompts.find((p) => p.name === 'default');
			selectedPromptName = defaultPrompt?.name ?? prompts[0].name;
		}
	});

	// Add a new variable
	function addVariable(key: string, value: string) {
		if (key.trim()) {
			customVars = { ...customVars, [key.trim()]: value };
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

	// Start execution
	async function handleStart() {
		if (!selectedPromptName || isStarting) return;

		isStarting = true;
		error = null;

		try {
			const execConfig: ExecutionConfig = {
				prompt_name: selectedPromptName,
				vars: combinedVars()
			};

			// Add optional advanced settings
			if (maxIterations !== undefined && maxIterations > 0) {
				execConfig.max_iterations = maxIterations;
			}
			if (maxRetries !== undefined && maxRetries > 0) {
				execConfig.max_retries = maxRetries;
			}

			await executionStore.start(execConfig);

			// Close dialog and navigate to executions page
			if (onClose) onClose();
			await goto('/executions');
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			isStarting = false;
		}
	}

	// Handle backdrop click
	function handleBackdropClick() {
		if (onClose) onClose();
	}

	// Handle dialog click (prevent propagation)
	function handleDialogClick(e: Event) {
		e.stopPropagation();
	}

	// Handle key press
	function handleKeydown(e: KeyboardEvent) {
		if (e.key === 'Escape' && onClose) {
			onClose();
		}
	}
</script>

<svelte:window onkeydown={handleKeydown} />

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="dialog-overlay" onclick={handleBackdropClick} onkeydown={(e) => e.key === 'Escape' && handleBackdropClick()}>
	<!-- svelte-ignore a11y_no_static_element_interactions -->
	<div class="dialog-container" onclick={handleDialogClick} onkeydown={(e) => e.stopPropagation()}>
		<!-- Header -->
		<div class="dialog-header">
			<h2>New Session</h2>
			<button class="close-btn" onclick={() => onClose?.()} aria-label="Close">
				<X size={20} />
			</button>
		</div>

		<!-- Content -->
		<div class="dialog-body">
			<!-- Error display -->
			{#if error}
				<div class="error-banner">
					<span>{error}</span>
					<button onclick={() => (error = null)} aria-label="Dismiss error">&times;</button>
				</div>
			{/if}

			<!-- Prompt Selection -->
			<div class="form-group">
				<label for="prompt-select">Prompt Template</label>
				<select
					id="prompt-select"
					bind:value={selectedPromptName}
					class="select-input"
					disabled={isStarting}
				>
					{#each availablePrompts() as prompt (prompt.name)}
						<option value={prompt.name}>{prompt.name}</option>
					{/each}
				</select>
				{#if availablePrompts().length === 0}
					<span class="hint">No prompts configured. Add prompts to your velor.toml.</span>
				{/if}
			</div>

			<!-- Variables Section -->
			{#if templateVars().length > 0 || Object.keys(customVars).length > 0}
				<div class="variables-section">
					<div class="section-header">
						<span class="section-title">Variables</span>
						{#if Object.keys(customVars).length > 0}
							<button class="reset-btn" onclick={resetVars} disabled={isStarting}>
								Reset
							</button>
						{/if}
					</div>

					<!-- Template Variables (config defaults) -->
					{#if templateVars().length > 0}
						<div class="vars-group">
							<span class="vars-label">Config Defaults</span>
							<div class="vars-list">
								{#each templateVars() as { key, default: defaultValue } (key)}
									<div class="var-item">
										<span class="var-key">{key}</span>
										<span class="var-default">{String(defaultValue)}</span>
									</div>
								{/each}
							</div>
						</div>
					{/if}

					<!-- Custom Overrides -->
					{#if Object.keys(customVars).length > 0}
						<div class="vars-group">
							<span class="vars-label">Custom Overrides</span>
							<div class="vars-list">
								{#each Object.entries(customVars) as [key, _value] (key)}
									<div class="var-item-editable">
										<span class="var-key">{key}</span>
										<input
											type="text"
											bind:value={customVars[key]}
											class="var-input"
											disabled={isStarting}
											placeholder="Value"
										/>
										<button
											class="var-remove-btn"
											onclick={() => removeVariable(key)}
											disabled={isStarting}
											aria-label="Remove variable"
										>
											&times;
										</button>
									</div>
								{/each}
							</div>
						</div>
					{/if}

					<!-- Add Variable -->
					{#if templateVars().length > 0}
						<div class="add-var-form">
							<input
								type="text"
								class="var-input"
								placeholder="Variable name"
								disabled={isStarting}
								onkeydown={(e) => {
									if (e.key === 'Enter') {
										const input = e.target as HTMLInputElement;
										addVariable(input.value, '');
										input.value = '';
									}
								}}
							/>
							<span class="add-var-hint">Press Enter to add a variable override</span>
						</div>
					{/if}
				</div>
			{/if}

			<!-- Advanced Options -->
			<div class="advanced-section">
				<button
					class="advanced-toggle"
					onclick={() => (showAdvanced = !showAdvanced)}
					type="button"
				>
					<span>Advanced Options</span>
					{#if showAdvanced}
						<ChevronUp size={16} />
					{:else}
						<ChevronDown size={16} />
					{/if}
				</button>

				{#if showAdvanced}
					<div class="advanced-content">
						<div class="form-row">
							<div class="form-group">
								<label for="max-iterations">Max Iterations</label>
								<input
									id="max-iterations"
									type="number"
									bind:value={maxIterations}
									class="input"
									disabled={isStarting}
									placeholder="Default from config"
									min="1"
								/>
							</div>
							<div class="form-group">
								<label for="max-retries">Max Retries</label>
								<input
									id="max-retries"
									type="number"
									bind:value={maxRetries}
									class="input"
									disabled={isStarting}
									placeholder="Default from config"
									min="0"
								/>
							</div>
						</div>
					</div>
				{/if}
			</div>
		</div>

		<!-- Footer -->
		<div class="dialog-footer">
			<button
				class="btn btn-secondary"
				onclick={() => onClose?.()}
				disabled={isStarting}
			>
				Cancel
			</button>
			<button
				class="btn btn-primary"
				onclick={handleStart}
				disabled={isStarting || !selectedPromptName || availablePrompts().length === 0}
			>
				{#if isStarting}
					<span class="spinner"></span>
					<span>Starting...</span>
				{:else}
					<Play size={16} />
					<span>Start Session</span>
				{/if}
			</button>
		</div>
	</div>
</div>

<style>
	.dialog-overlay {
		position: fixed;
		inset: 0;
		z-index: 50;
		display: flex;
		align-items: center;
		justify-content: center;
		background-color: rgb(0 0 0 / 0.6);
		backdrop-filter: blur(4px);
		padding: 1rem;
	}

	.dialog-container {
		width: 100%;
		max-width: 32rem;
		display: flex;
		flex-direction: column;
		background-color: var(--color-bg-secondary);
		border: 1px solid var(--color-border);
		border-radius: 0.5rem;
		box-shadow: 0 20px 25px -5px rgb(0 0 0 / 0.1), 0 8px 10px -6px rgb(0 0 0 / 0.1);
	}

	.dialog-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		padding: 1rem 1.5rem;
		border-bottom: 1px solid var(--color-border);
	}

	.dialog-header h2 {
		font-size: 1.125rem;
		font-weight: 600;
		color: var(--color-text-primary);
	}

	.close-btn {
		padding: 0.25rem;
		border-radius: 0.25rem;
		color: var(--color-text-secondary);
		transition: all 0.15s ease-in-out;
	}

	.close-btn:hover {
		color: var(--color-text-primary);
		background-color: var(--color-bg-tertiary);
	}

	.dialog-body {
		flex: 1;
		overflow-y: auto;
		padding: 1.5rem;
		display: flex;
		flex-direction: column;
		gap: 1.25rem;
	}

	.error-banner {
		display: flex;
		align-items: center;
		justify-content: space-between;
		padding: 0.75rem 1rem;
		background-color: rgb(127 29 29 / 0.5);
		border: 1px solid rgb(185 28 28 / 0.5);
		border-radius: 0.375rem;
		color: rgb(248 113 113);
		font-size: 0.875rem;
	}

	.error-banner button {
		padding: 0 0.5rem;
		font-size: 1.25rem;
		color: rgb(248 113 113);
		opacity: 0.7;
	}

	.error-banner button:hover {
		opacity: 1;
	}

	.form-group {
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
	}

	.form-group label {
		font-size: 0.875rem;
		font-weight: 500;
		color: var(--color-text-secondary);
	}

	.form-row {
		display: grid;
		grid-template-columns: 1fr 1fr;
		gap: 1rem;
	}

	.select-input,
	.input {
		width: 100%;
		padding: 0.625rem 0.875rem;
		border-radius: 0.375rem;
		background-color: var(--color-bg-tertiary);
		border: 1px solid var(--color-border);
		color: var(--color-text-primary);
		font-size: 0.875rem;
		transition: border-color 0.15s ease-in-out;
	}

	.select-input:focus,
	.input:focus {
		outline: none;
		border-color: var(--color-accent-primary);
	}

	.select-input:disabled,
	.input:disabled {
		opacity: 0.5;
		cursor: not-allowed;
	}

	.hint {
		font-size: 0.75rem;
		color: var(--color-text-muted);
	}

	.variables-section {
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
	}

	.section-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
	}

	.section-title {
		font-size: 0.875rem;
		font-weight: 500;
		color: var(--color-text-secondary);
	}

	.reset-btn {
		font-size: 0.75rem;
		color: var(--color-accent-primary);
		padding: 0.25rem 0.5rem;
		border-radius: 0.25rem;
		transition: all 0.15s ease-in-out;
	}

	.reset-btn:hover {
		background-color: var(--color-bg-tertiary);
	}

	.reset-btn:disabled {
		opacity: 0.5;
		cursor: not-allowed;
	}

	.vars-group {
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
	}

	.vars-label {
		font-size: 0.75rem;
		font-weight: 500;
		color: var(--color-text-muted);
		text-transform: uppercase;
		letter-spacing: 0.05em;
	}

	.vars-list {
		display: flex;
		flex-direction: column;
		gap: 0.375rem;
	}

	.var-item {
		display: flex;
		align-items: center;
		gap: 0.75rem;
		padding: 0.5rem 0.75rem;
		background-color: var(--color-bg-tertiary);
		border: 1px solid var(--color-border);
		border-radius: 0.375rem;
		opacity: 0.75;
	}

	.var-key {
		min-width: 6rem;
		font-size: 0.875rem;
		color: var(--color-text-muted);
	}

	.var-default {
		flex: 1;
		font-size: 0.875rem;
		font-family: ui-monospace, monospace;
		color: var(--color-text-primary);
	}

	.var-item-editable {
		display: flex;
		align-items: center;
		gap: 0.5rem;
	}

	.var-item-editable .var-key {
		min-width: 6rem;
		padding: 0.5rem 0.75rem;
		background-color: var(--color-bg-tertiary);
		border: 1px solid var(--color-border);
		border-radius: 0.375rem 0 0 0.375rem;
		font-size: 0.875rem;
		color: var(--color-text-muted);
	}

	.var-input {
		flex: 1;
		padding: 0.5rem 0.75rem;
		background-color: var(--color-bg-tertiary);
		border: 1px solid var(--color-border);
		border-radius: 0.375rem;
		font-size: 0.875rem;
		color: var(--color-text-primary);
	}

	.var-input:focus {
		outline: none;
		border-color: var(--color-accent-primary);
	}

	.var-item-editable .var-input {
		border-radius: 0;
		border-left: none;
	}

	.var-item-editable .var-input:focus {
		border-left: 1px solid var(--color-accent-primary);
	}

	.var-remove-btn {
		padding: 0.5rem 0.75rem;
		background-color: var(--color-bg-tertiary);
		border: 1px solid var(--color-border);
		border-left: none;
		border-radius: 0 0.375rem 0.375rem 0;
		color: var(--color-text-muted);
		font-size: 1rem;
		transition: all 0.15s ease-in-out;
	}

	.var-remove-btn:hover {
		color: rgb(248 113 113);
		background-color: rgb(239 68 68 / 0.1);
	}

	.add-var-form {
		display: flex;
		flex-direction: column;
		gap: 0.375rem;
	}

	.add-var-form .var-input {
		width: 100%;
	}

	.add-var-hint {
		font-size: 0.75rem;
		color: var(--color-text-muted);
	}

	.advanced-section {
		display: flex;
		flex-direction: column;
		border: 1px solid var(--color-border);
		border-radius: 0.375rem;
		overflow: hidden;
	}

	.advanced-toggle {
		display: flex;
		align-items: center;
		justify-content: space-between;
		width: 100%;
		padding: 0.75rem 1rem;
		background-color: var(--color-bg-tertiary);
		color: var(--color-text-secondary);
		font-size: 0.875rem;
		font-weight: 500;
		transition: all 0.15s ease-in-out;
	}

	.advanced-toggle:hover {
		background-color: var(--color-bg-secondary);
	}

	.advanced-content {
		padding: 1rem;
		border-top: 1px solid var(--color-border);
		background-color: var(--color-bg-secondary);
	}

	.dialog-footer {
		display: flex;
		align-items: center;
		justify-content: flex-end;
		gap: 0.75rem;
		padding: 1rem 1.5rem;
		border-top: 1px solid var(--color-border);
		background-color: var(--color-bg-tertiary);
	}

	.btn {
		display: inline-flex;
		align-items: center;
		gap: 0.5rem;
		padding: 0.625rem 1.25rem;
		border-radius: 0.375rem;
		font-size: 0.875rem;
		font-weight: 500;
		transition: all 0.15s ease-in-out;
	}

	.btn:disabled {
		opacity: 0.5;
		cursor: not-allowed;
	}

	.btn-secondary {
		background-color: var(--color-bg-tertiary);
		border: 1px solid var(--color-border);
		color: var(--color-text-secondary);
	}

	.btn-secondary:hover:not(:disabled) {
		background-color: var(--color-bg-secondary);
		color: var(--color-text-primary);
	}

	.btn-primary {
		background-color: var(--color-accent-primary);
		border: 1px solid var(--color-accent-primary);
		color: white;
	}

	.btn-primary:hover:not(:disabled) {
		opacity: 0.9;
	}

	.spinner {
		width: 1rem;
		height: 1rem;
		border: 2px solid transparent;
		border-top-color: currentColor;
		border-radius: 50%;
		animation: spin 0.8s linear infinite;
	}

	@keyframes spin {
		to {
			transform: rotate(360deg);
		}
	}
</style>
