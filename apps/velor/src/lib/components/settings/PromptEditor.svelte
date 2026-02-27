<script lang="ts">
	import { config, configStore } from '$lib/stores';
	import { Plus, Trash2, Save, FileText, AlertCircle } from 'lucide-svelte';

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
					{#each promptNames as name (name)}
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
					<span class="hint">Use {`{{variable_name}}`} syntax for template variables</span>
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
		display: flex;
		flex-direction: column;
		gap: 1.5rem;
	}

	/* List View */
	.list-header {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
	}

	.list-header h2 {
		font-size: 1.25rem;
		font-weight: 600;
		color: var(--color-text-primary);
	}

	.list-header p {
		font-size: 0.875rem;
		color: var(--color-text-secondary);
		margin-top: 0.25rem;
	}

	.create-btn {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		padding-left: 1rem;
		padding-right: 1rem;
		padding-top: 0.5rem;
		padding-bottom: 0.5rem;
		border-radius: 0.5rem;
		background-color: var(--color-accent-primary);
		color: white;
		font-size: 0.875rem;
		font-weight: 500;
		transition: all 0.2s ease-in-out;
	}

	.create-btn:hover {
		background-color: var(--color-accent-hover);
	}

	.empty-state {
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		padding-top: 4rem;
		padding-bottom: 4rem;
		color: var(--color-text-secondary);
		gap: 1rem;
	}

	.empty-state h3 {
		font-size: 1.125rem;
		font-weight: 600;
		color: var(--color-text-primary);
	}

	.prompts-grid {
		display: grid;
		grid-template-columns: repeat(1, minmax(0, 1fr));
		gap: 1rem;
	}

	@media (min-width: 768px) {
		.prompts-grid {
			grid-template-columns: repeat(2, minmax(0, 1fr));
		}
	}

	@media (min-width: 1024px) {
		.prompts-grid {
			grid-template-columns: repeat(3, minmax(0, 1fr));
		}
	}

	.prompt-card {
		display: flex;
		flex-direction: column;
		padding: 1rem;
		background-color: var(--color-bg-secondary);
		border: 1px solid var(--color-border);
		border-radius: 0.5rem;
		transition: all 0.2s ease-in-out;
	}

	.prompt-card:hover {
		border-color: var(--color-accent-primary);
	}

	.card-header {
		display: flex;
		align-items: center;
		gap: 0.75rem;
		margin-bottom: 0.75rem;
	}

	.prompt-icon {
		width: 2.5rem;
		height: 2.5rem;
		border-radius: 0.5rem;
		background-color: var(--color-accent-light);
		display: flex;
		align-items: center;
		justify-content: center;
		color: var(--color-accent-primary);
	}

	.card-header h3 {
		font-weight: 600;
		color: var(--color-text-primary);
	}

	.card-preview {
		flex: 1;
		font-size: 0.875rem;
		color: var(--color-text-secondary);
		display: -webkit-box;
		-webkit-line-clamp: 3;
		-webkit-box-orient: vertical;
		overflow: hidden;
		margin-bottom: 1rem;
	}

	.card-actions {
		display: flex;
		align-items: center;
		gap: 0.5rem;
	}

	.action-btn {
		padding-left: 0.75rem;
		padding-right: 0.75rem;
		padding-top: 0.375rem;
		padding-bottom: 0.375rem;
		border-radius: 0.25rem;
		font-size: 0.875rem;
		font-weight: 500;
		transition: all 0.2s ease-in-out;
	}

	.action-btn.edit {
		color: var(--color-accent-primary);
		flex: 1;
	}

	.action-btn.edit:hover {
		background-color: var(--color-accent-light);
	}

	.action-btn.delete {
		color: rgb(248 113 113);
		padding-left: 0.5rem;
		padding-right: 0.5rem;
	}

	.action-btn.delete:hover {
		background-color: rgb(127 29 29 / 0.3);
	}

	/* Form View */
	.form-header {
		margin-bottom: 1.5rem;
	}

	.back-btn {
		font-size: 0.875rem;
		color: var(--color-text-secondary);
		margin-bottom: 0.5rem;
		transition: color 0.15s ease-in-out;
	}

	.back-btn:hover {
		color: var(--color-accent-primary);
	}

	.form-header h2 {
		font-size: 1.25rem;
		font-weight: 600;
		color: var(--color-text-primary);
	}

	.error-banner {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		padding-left: 1rem;
		padding-right: 1rem;
		padding-top: 0.75rem;
		padding-bottom: 0.75rem;
		border-radius: 0.5rem;
		background-color: rgb(127 29 29 / 0.5);
		border: 1px solid rgb(185 28 28 / 0.5);
		color: rgb(253 186 116);
		margin-bottom: 1rem;
	}

	.prompt-form {
		max-width: 42rem;
	}

	.form-group {
		margin-bottom: 1.25rem;
	}

	.form-group label {
		display: block;
		font-size: 0.875rem;
		font-weight: 500;
		color: var(--color-text-primary);
		margin-bottom: 0.5rem;
	}

	.required {
		color: rgb(248 113 113);
		margin-left: 0.25rem;
	}

	.form-group input[type="text"],
	.form-group textarea {
		width: 100%;
		padding-left: 1rem;
		padding-right: 1rem;
		padding-top: 0.625rem;
		padding-bottom: 0.625rem;
		background-color: var(--color-bg-secondary);
		border: 1px solid var(--color-border);
		border-radius: 0.5rem;
		color: var(--color-text-primary);
		transition: all 0.15s ease-in-out;
	}

	.form-group input[type="text"]::placeholder,
	.form-group textarea::placeholder {
		color: var(--color-text-muted);
	}

	.form-group input[type="text"]:focus,
	.form-group textarea:focus {
		outline: none;
		box-shadow: 0 0 0 2px rgb(var(--color-accent-primary) / 0.5);
		border-color: var(--color-accent-primary);
	}

	.form-group input:disabled,
	.form-group textarea:disabled {
		opacity: 0.6;
		cursor: not-allowed;
	}

	.form-group textarea {
		font-family: ui-monospace, monospace;
		font-size: 0.875rem;
		resize: vertical;
		min-height: 200px;
	}

	.hint {
		display: block;
		font-size: 0.75rem;
		color: var(--color-text-muted);
		margin-top: 0.375rem;
	}

	.checkbox-label {
		display: flex;
		align-items: center;
		gap: 0.75rem;
		cursor: pointer;
	}

	.checkbox-label input[type="checkbox"] {
		width: 1rem;
		height: 1rem;
		border-radius: 0.25rem;
		border: 1px solid var(--color-border);
		background-color: var(--color-bg-secondary);
	}

	.checkbox-label input[type="checkbox"]:focus {
		box-shadow: 0 0 0 2px rgb(var(--color-accent-primary) / 0.5);
	}

	.checkbox-label span {
		font-size: 0.875rem;
		font-weight: 500;
		color: var(--color-text-primary);
	}

	.form-actions {
		display: flex;
		align-items: center;
		justify-content: flex-end;
		gap: 0.75rem;
		padding-top: 1rem;
		border-top: 1px solid var(--color-border);
	}

	.cancel-btn {
		padding-left: 1rem;
		padding-right: 1rem;
		padding-top: 0.5rem;
		padding-bottom: 0.5rem;
		border-radius: 0.5rem;
		font-size: 0.875rem;
		font-weight: 500;
		color: var(--color-text-secondary);
		transition: all 0.15s ease-in-out;
	}

	.cancel-btn:hover {
		color: var(--color-text-primary);
		background-color: var(--color-bg-tertiary);
	}

	.submit-btn {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		padding-left: 1rem;
		padding-right: 1rem;
		padding-top: 0.5rem;
		padding-bottom: 0.5rem;
		border-radius: 0.5rem;
		background-color: var(--color-accent-primary);
		color: white;
		font-size: 0.875rem;
		font-weight: 500;
		transition: all 0.15s ease-in-out;
	}

	.submit-btn:hover {
		background-color: var(--color-accent-hover);
	}

	.submit-btn:disabled {
		opacity: 0.5;
		cursor: not-allowed;
	}

	.spinner {
		width: 1rem;
		height: 1rem;
		border-radius: 9999px;
		border: 2px solid rgb(255 255 255 / 0.3);
		border-top-color: white;
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
