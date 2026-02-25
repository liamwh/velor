<script lang="ts">
	import { config, configStore } from '$lib/stores';
	import type { Prompt, Prompts } from '$lib/types';
	import { Plus, Trash2, Save, FileText, Check, AlertCircle } from 'lucide-svelte';

	type ViewMode = 'list' | 'create' | 'edit';

	let viewMode: ViewMode = $state('list');
	let selectedPromptName = $state('');

	// Form state for creating/editing prompts
	let formData = $state({
		name: '',
		template: '',
		completeToken: '',
		isAdvanced: false
	});

	let isSaving = $state(false);
	let validationError = $state('');

	// Get prompts from config
	const prompts = $derived($config?.prompts || {});
	const promptNames = $derived(Object.keys(prompts).sort());

	function setViewMode(mode: ViewMode, promptName = '') {
		viewMode = mode;
		selectedPromptName = promptName;
		validationError = '';

		if (mode === 'create') {
			formData = { name: '', template: '', completeToken: '', isAdvanced: false };
		} else if (mode === 'edit' && promptName) {
			const prompt = prompts[promptName];
			if (typeof prompt === 'string') {
				formData = {
					name: promptName,
					template: prompt,
					completeToken: '',
					isAdvanced: false
				};
			} else if (prompt && typeof prompt === 'object' && 'template' in prompt) {
				const promptObj = prompt as { template: string; complete_token?: string };
				formData = {
					name: promptName,
					template: promptObj.template || '',
					completeToken: promptObj.complete_token || '',
					isAdvanced: true
				};
			}
		}
	}

	function validateForm(): boolean {
		if (!formData.name.trim()) {
			validationError = 'Prompt name is required';
			return false;
		}
		if (!formData.template.trim()) {
			validationError = 'Template content is required';
			return false;
		}
		// Check for duplicate names when creating
		if (viewMode === 'create' && formData.name in prompts) {
			validationError = 'A prompt with this name already exists';
			return false;
		}
		return true;
	}

	function getPromptForSave(): string | { template: string; complete_token?: string } {
		if (formData.isAdvanced && formData.completeToken) {
			return {
				template: formData.template,
				complete_token: formData.completeToken
			};
		}
		return formData.template;
	}

	async function savePrompt() {
		if (!validateForm()) return;

		isSaving = true;

		try {
			// Load the config store first
			await configStore.load();

			// Get current home config directly
			let homeConfig = '';
			try {
				const { getHomeConfig } = await import('$lib/services/tauri');
				homeConfig = await getHomeConfig();
			} catch {
				// Use empty config if none exists
			}

			// Parse and update the TOML config
			let configContent = homeConfig || '';

			// Simple TOML manipulation - in a real app, use a proper TOML parser
			// For now, we'll append the new prompt to the config
			let newConfigText = configContent;

			// Check if prompts section exists
			if (newConfigText.includes('[prompts]')) {
				// Append to existing prompts section
				const promptEntry = formData.isAdvanced
					? `\n\n[prompts.${formData.name}]\n  template = """\n${formData.template}\n  """\n  complete_token = "${formData.completeToken}"`
					: `\n\n[prompts.${formData.name}]\n  """\n${formData.template}\n  """`;

				// Find the end of prompts section (heuristic)
				const promptsIndex = newConfigText.indexOf('[prompts]');
				const nextSectionIndex = newConfigText.indexOf('\n[', promptsIndex + 10);
				if (nextSectionIndex > 0) {
					newConfigText = newConfigText.slice(0, nextSectionIndex) + promptEntry + newConfigText.slice(nextSectionIndex);
				} else {
					newConfigText = newConfigText + promptEntry;
				}
			} else {
				// Add new prompts section
				const promptEntry = formData.isAdvanced
					? `\n\n[prompts.${formData.name}]\n  template = """\n${formData.template}\n  """\n  complete_token = "${formData.completeToken}"`
					: `\n\n[prompts.${formData.name}]\n  """\n${formData.template}\n  """`;
				newConfigText = newConfigText + promptEntry;
			}

			await configStore.save('home', newConfigText);
			setViewMode('list');
		} catch (e) {
			validationError = e instanceof Error ? e.message : 'Failed to save prompt';
		} finally {
			isSaving = false;
		}
	}

	function deletePrompt(name: string) {
		if (!confirm(`Are you sure you want to delete the "${name}" prompt?`)) {
			return;
		}
		// Note: In a full implementation, this would update the config
		// For now, we'll just show a message
		alert('Delete functionality requires TOML parsing library. Please edit the config directly.');
	}

	function cancel() {
		setViewMode('list');
	}

	const selectedPrompt = $derived(
		selectedPromptName ? prompts[selectedPromptName] : null
	);

	const promptTemplate = $derived(() => {
		if (!selectedPrompt) return '';
		return typeof selectedPrompt === 'string' ? selectedPrompt : selectedPrompt.template || '';
	});

	const promptCompleteToken = $derived(() => {
		if (!selectedPrompt || typeof selectedPrompt === 'string') return '';
		return (selectedPrompt as { complete_token?: string }).complete_token || '';
	});

	// Helper function to get template for a specific prompt name
	function getPromptTemplate(name: string): string {
		const prompt = prompts[name];
		if (!prompt) return 'No template content';
		if (typeof prompt === 'string') return prompt;
		// Type guard for object type
		if ('template' in prompt) return (prompt as { template: string }).template || 'No template content';
		return 'No template content';
	}
</script>

<div class="prompt-editor">
	{#if viewMode === 'list'}
		<!-- List View -->
		<div class="prompt-list">
			<div class="list-header">
				<div>
					<h2>Manage Prompts</h2>
					<p>Create and edit prompt templates for your AI agents</p>
				</div>
				<button class="create-btn" onclick={() => setViewMode('create')}>
					<Plus size={18} />
					New Prompt
				</button>
			</div>

			{#if promptNames.length === 0}
				<div class="empty-state">
					<FileText size={48} />
					<h3>No Prompts Found</h3>
					<p>Create your first prompt template to get started.</p>
				</div>
			{:else}
				<div class="prompts-grid">
					{#each promptNames as name}
						<div class="prompt-card">
							<div class="card-header">
								<div class="prompt-icon">
									<FileText size={20} />
								</div>
								<h3>{name}</h3>
							</div>
							<p class="card-preview">
								{getPromptTemplate(name)}
							</p>
							<div class="card-actions">
								<button class="action-btn edit" onclick={() => setViewMode('edit', name)} aria-label="Edit prompt">
									Edit
								</button>
								<button class="action-btn delete" onclick={() => deletePrompt(name)} aria-label="Delete prompt">
									<Trash2 size={16} />
								</button>
							</div>
						</div>
					{/each}
				</div>
			{/if}
		</div>

	{:else}
		<!-- Create/Edit View -->
		<div class="prompt-form">
			<div class="form-header">
				<button class="back-btn" onclick={cancel}>← Back to Prompts</button>
				<h2>{viewMode === 'create' ? 'Create New Prompt' : `Edit "${selectedPromptName}"`}</h2>
			</div>

			<!-- Validation Error -->
			{#if validationError}
				<div class="error-banner">
					<AlertCircle size={18} />
					<span>{validationError}</span>
				</div>
			{/if}

			<form onsubmit={(e) => { e.preventDefault(); savePrompt(); }}>
				<div class="form-group">
					<label for="prompt-name">
						Prompt Name
						<span class="required">*</span>
					</label>
					<input
						id="prompt-name"
						type="text"
						bind:value={formData.name}
						placeholder="e.g., code-review, bug-fix, feature-request"
						disabled={viewMode === 'edit'}
						required
					/>
					<span class="hint">A unique identifier for this prompt template</span>
				</div>

				<div class="form-group">
					<label for="prompt-template">
						Template Content
						<span class="required">*</span>
					</label>
					<textarea
						id="prompt-template"
						bind:value={formData.template}
						placeholder={'Enter your prompt template here... Use {{variable}} for dynamic content.'}
						rows="12"
						required
					></textarea>
					<span class="hint">Use {'{{'}variable_name{'}}'} syntax for template variables</span>
				</div>

				<div class="form-group">
					<label class="checkbox-label">
						<input
							type="checkbox"
							bind:checked={formData.isAdvanced}
						/>
						<span>Custom completion token</span>
					</label>
					<span class="hint">Specify a custom token to detect when the agent is complete</span>
				</div>

				{#if formData.isAdvanced}
					<div class="form-group">
						<label for="complete-token">Completion Token</label>
						<input
							id="complete-token"
							type="text"
							bind:value={formData.completeToken}
							placeholder="<promise>COMPLETE</promise>"
						/>
						<span class="hint">The token that signals the agent has finished its task</span>
					</div>
				{/if}

				<div class="form-actions">
					<button type="button" class="cancel-btn" onclick={cancel}>
						Cancel
					</button>
					<button type="submit" class="submit-btn" disabled={isSaving}>
						{#if isSaving}
							<span class="spinner"></span>
							Saving...
						{:else}
							<Save size={16} />
							Save Prompt
						{/if}
					</button>
				</div>
			</form>
		</div>
	{/if}
</div>

<style>
	.prompt-editor {
		@apply flex flex-col gap-6;
	}

	/* List View */
	.list-header {
		@apply flex items-start justify-between;
	}

	.list-header h2 {
		@apply text-xl font-semibold text-[var(--color-text-primary)];
	}

	.list-header p {
		@apply text-sm text-[var(--color-text-secondary)] mt-1;
	}

	.create-btn {
		@apply flex items-center gap-2 px-4 py-2 rounded-lg bg-[var(--color-accent-primary)] text-white text-sm font-medium hover:bg-[var(--color-accent-hover)] transition-all duration-200;
	}

	.empty-state {
		@apply flex flex-col items-center justify-center py-16 text-[var(--color-text-secondary)] gap-4;
	}

	.empty-state h3 {
		@apply text-lg font-semibold text-[var(--color-text-primary)];
	}

	.prompts-grid {
		@apply grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-4;
	}

	.prompt-card {
		@apply flex flex-col p-4 bg-[var(--color-bg-secondary)] border border-[var(--color-border)] rounded-lg hover:border-[var(--color-accent-primary)] transition-all duration-200;
	}

	.card-header {
		@apply flex items-center gap-3 mb-3;
	}

	.prompt-icon {
		@apply w-10 h-10 rounded-lg bg-[var(--color-accent-light)] flex items-center justify-center text-[var(--color-accent-primary)];
	}

	.card-header h3 {
		@apply font-semibold text-[var(--color-text-primary)];
	}

	.card-preview {
		@apply flex-1 text-sm text-[var(--color-text-secondary)] line-clamp-3 mb-4;
	}

	.card-actions {
		@apply flex items-center gap-2;
	}

	.action-btn {
		@apply px-3 py-1.5 rounded text-sm font-medium transition-all duration-200;
	}

	.action-btn.edit {
		@apply text-[var(--color-accent-primary)] hover:bg-[var(--color-accent-light)] flex-1;
	}

	.action-btn.delete {
		@apply text-red-400 hover:bg-red-950/30 px-2;
	}

	/* Form View */
	.form-header {
		@apply mb-6;
	}

	.back-btn {
		@apply text-sm text-[var(--color-text-secondary)] hover:text-[var(--color-accent-primary)] mb-2 transition-colors;
	}

	.form-header h2 {
		@apply text-xl font-semibold text-[var(--color-text-primary)];
	}

	.error-banner {
		@apply flex items-center gap-2 px-4 py-3 rounded-lg bg-red-950/50 border border-red-900/50 text-red-300 mb-4;
	}

	.prompt-form {
		@apply max-w-2xl;
	}

	.form-group {
		@apply mb-5;
	}

	.form-group label {
		@apply block text-sm font-medium text-[var(--color-text-primary)] mb-2;
	}

	.required {
		@apply text-red-400 ml-1;
	}

	.form-group input[type="text"],
	.form-group textarea {
		@apply w-full px-4 py-2.5 bg-[var(--color-bg-secondary)] border border-[var(--color-border)] rounded-lg text-[var(--color-text-primary)] placeholder-[var(--color-text-muted)] focus:outline-none focus:ring-2 focus:ring-[var(--color-accent-primary)]/50 focus:border-[var(--color-accent-primary)] transition-all;
	}

	.form-group input:disabled,
	.form-group textarea:disabled {
		@apply opacity-60 cursor-not-allowed;
	}

	.form-group textarea {
		@apply font-mono text-sm resize-y min-h-[200px];
	}

	.hint {
		@apply block text-xs text-[var(--color-text-muted)] mt-1.5;
	}

	.checkbox-label {
		@apply flex items-center gap-3 cursor-pointer;
	}

	.checkbox-label input[type="checkbox"] {
		@apply w-4 h-4 rounded border-[var(--color-border)] bg-[var(--color-bg-secondary)] text-[var(--color-accent-primary)] focus:ring-2 focus:ring-[var(--color-accent-primary)]/50;
	}

	.checkbox-label span {
		@apply text-sm font-medium text-[var(--color-text-primary)];
	}

	.form-actions {
		@apply flex items-center justify-end gap-3 pt-4 border-t border-[var(--color-border)];
	}

	.cancel-btn {
		@apply px-4 py-2 rounded-lg text-sm font-medium text-[var(--color-text-secondary)] hover:text-[var(--color-text-primary)] hover:bg-[var(--color-bg-tertiary)] transition-all;
	}

	.submit-btn {
		@apply flex items-center gap-2 px-4 py-2 rounded-lg bg-[var(--color-accent-primary)] text-white text-sm font-medium hover:bg-[var(--color-accent-hover)] disabled:opacity-50 disabled:cursor-not-allowed transition-all;
	}

	.spinner {
		@apply w-4 h-4 rounded-full border-2 border-white/30 border-t-white animate-spin;
	}
</style>
