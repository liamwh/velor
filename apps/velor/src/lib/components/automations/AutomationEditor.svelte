<script lang="ts">
	import { onMount } from 'svelte';
	import { X, Plus, Trash2, Clock, Save } from 'lucide-svelte';
	import { configStore, config } from '$lib/stores';
	import * as tauri from '$lib/services/tauri';
	import type { Automation, CreateAutomationRequest, UpdateAutomationRequest } from '$lib/types';

	interface Props {
		automation?: Automation;
		onSave?: () => void;
		onCancel?: () => void;
	}

	let { automation, onSave, onCancel }: Props = $props();

	let formData = $state({
		name: '',
		description: '',
		prompt: '',
		schedule: '',
		timezone: 'UTC',
		catch_up: 'Skip' as 'Skip' | 'RunOnce' | 'RunAll',
		enabled: true,
		vars: [] as { key: string; value: string }[]
	});

	let errors = $state<Record<string, string>>({});
	let isSaving = $state(false);
	let isEdit = $state(false);

	// Derived available prompts from config
	const availablePrompts = $derived(() => {
		return Object.keys($config?.prompts || {});
	});

	// Common cron presets (6-field: seconds minutes hours day month weekday)
	const cronPresets = [
		{ label: 'Every minute', value: '* * * * * *' },
		{ label: 'Every 5 minutes', value: '*/5 * * * * *' },
		{ label: 'Every hour', value: '0 * * * * *' },
		{ label: 'Every day at midnight', value: '0 0 * * * *' },
		{ label: 'Every day at 9 AM', value: '0 0 9 * * *' },
		{ label: 'Every Monday at 9 AM', value: '0 0 9 * * 1' },
		{ label: 'Every weekday at 9 AM', value: '0 0 9 * * 1-5' },
		{ label: 'Custom...', value: 'custom' }
	];

	const commonTimezones = [
		'UTC',
		'America/New_York',
		'America/Chicago',
		'America/Denver',
		'America/Los_Angeles',
		'Europe/London',
		'Europe/Paris',
		'Europe/Berlin',
		'Asia/Tokyo',
		'Asia/Shanghai',
		'Australia/Sydney'
	];

	onMount(async () => {
		// Load available prompts
		await configStore.load();

		if (automation) {
			// Edit mode - populate form
			isEdit = true;
			formData.name = automation.name;
			formData.description = automation.description || '';
			formData.prompt = automation.prompt;
			formData.schedule = automation.schedule;
			formData.timezone = automation.timezone;
			formData.catch_up = automation.catch_up;
			formData.enabled = automation.enabled;
			formData.vars = Object.entries(automation.vars).map(([key, value]) => ({
				key,
				value
			}));
		}
	});

	function validateForm(): boolean {
		const newErrors: Record<string, string> = {};

		if (!formData.name.trim()) {
			newErrors.name = 'Name is required';
		}
		if (!formData.prompt) {
			newErrors.prompt = 'Prompt is required';
		}
		if (!formData.schedule.trim()) {
			newErrors.schedule = 'Cron expression is required';
		} else if (!isValidCron(formData.schedule)) {
			newErrors.schedule = 'Invalid cron expression (must be 6 fields)';
		}

		errors = newErrors;
		return Object.keys(newErrors).length === 0;
	}

	function isValidCron(cron: string): boolean {
		// Basic cron validation: 6 parts separated by spaces (seconds minutes hours day month weekday)
		const parts = cron.trim().split(/\s+/);
		return parts.length === 6;
	}

	async function handleSave() {
		if (!validateForm()) return;

		isSaving = true;
		try {
			// Build vars object
			const vars: Record<string, string> = {};
			for (const v of formData.vars) {
				if (v.key.trim()) {
					vars[v.key] = v.value;
				}
			}

			if (isEdit && automation) {
				// Update existing automation
				const request: UpdateAutomationRequest = {
					current_name: automation.name,
					name: formData.name.trim() || undefined,
					description: formData.description.trim() || undefined,
					schedule: formData.schedule.trim() || undefined,
					timezone: formData.timezone,
					prompt: formData.prompt,
					enabled: formData.enabled,
					vars: Object.keys(vars).length > 0 ? vars : undefined,
					catch_up: formData.catch_up,
					notify_on_success: true,
					notify_on_failure: true
				};
				await tauri.updateAutomation(request);
			} else {
				// Create new automation
				const request: CreateAutomationRequest = {
					name: formData.name.trim(),
					description: formData.description.trim() || undefined,
					schedule: formData.schedule.trim(),
					timezone: formData.timezone,
					prompt: formData.prompt,
					enabled: formData.enabled,
					vars: Object.keys(vars).length > 0 ? vars : undefined,
					catch_up: formData.catch_up,
					notify_on_success: true,
					notify_on_failure: true
				};
				await tauri.createAutomation(request);
			}

			if (onSave) onSave();
		} catch (e) {
			console.error('Failed to save automation:', e);
			errors.general = e instanceof Error ? e.message : 'Failed to save automation';
		} finally {
			isSaving = false;
		}
	}

	function addVar() {
		formData.vars = [...formData.vars, { key: '', value: '' }];
	}

	function removeVar(index: number) {
		formData.vars = formData.vars.filter((_, i) => i !== index);
	}

	function setPreset(preset: string) {
		if (preset !== 'custom') {
			formData.schedule = preset;
		}
	}
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="editor-overlay" onclick={onCancel} onkeydown={(e) => e.key === 'Escape' && onCancel?.()}>
	<!-- svelte-ignore a11y_no_static_element_interactions -->
	<div class="editor-dialog" onclick={(e) => e.stopPropagation()} onkeydown={(e) => e.stopPropagation()}>
		<!-- Header -->
		<div class="editor-header">
			<h2>{isEdit ? 'Edit Automation' : 'Create Automation'}</h2>
			<button class="close-btn" onclick={onCancel} aria-label="Close">
				<X size={20} />
			</button>
		</div>

		<!-- Form -->
		<div class="editor-body">
			{#if errors.general}
				<div class="error-banner">
					<span class="error-icon">⚠</span>
					<span class="error-text">{errors.general}</span>
				</div>
			{/if}

			<!-- Name -->
			<div class="form-group">
				<label for="name">Name *</label>
				<input
					type="text"
					id="name"
					bind:value={formData.name}
					class="form-input"
					class:invalid={errors.name}
					placeholder="my-automation"
					aria-invalid={!!errors.name}
				/>
				{#if errors.name}
					<span class="error-text">{errors.name}</span>
				{/if}
			</div>

			<!-- Description -->
			<div class="form-group">
				<label for="description">Description</label>
				<textarea
					id="description"
					bind:value={formData.description}
					class="form-textarea"
					placeholder="What does this automation do?"
					rows="2"
				></textarea>
			</div>

			<!-- Prompt -->
			<div class="form-group">
				<label for="prompt">Prompt Template *</label>
				<select
					id="prompt"
					bind:value={formData.prompt}
					class="form-select"
					class:invalid={errors.prompt}
					aria-invalid={!!errors.prompt}
				>
					<option value="">Select a prompt...</option>
					{#each availablePrompts() as prompt (prompt)}
						<option value={prompt}>{prompt}</option>
					{/each}
				</select>
				{#if errors.prompt}
					<span class="error-text">{errors.prompt}</span>
				{/if}
			</div>

			<!-- Schedule -->
			<div class="form-group">
				<label for="schedule">Schedule (Cron) *</label>
				<div class="schedule-inputs">
					<input
						type="text"
						id="schedule"
						bind:value={formData.schedule}
						class="form-input"
						class:invalid={errors.schedule}
						placeholder="0 0 9 * * *"
						aria-invalid={!!errors.schedule}
					/>
					<select class="form-select preset-select" onchange={(e) => setPreset((e.target as HTMLSelectElement).value)}>
						{#each cronPresets as preset (preset.value)}
							<option value={preset.value}>{preset.label}</option>
						{/each}
					</select>
				</div>
				{#if errors.schedule}
					<span class="error-text">{errors.schedule}</span>
				{/if}
				<span class="help-text">
					6-field cron: seconds minutes hours day month weekday. Example: <code>0 0 9 * * *</code> = 9:00:00 AM daily
				</span>
			</div>

			<!-- Timezone -->
			<div class="form-group">
				<label for="timezone">Timezone</label>
				<select id="timezone" bind:value={formData.timezone} class="form-select">
					{#each commonTimezones as tz (tz)}
						<option value={tz}>{tz}</option>
					{/each}
				</select>
			</div>

			<!-- Catch-up Policy -->
			<div class="form-group">
				<label for="catch_up">Catch-up Policy</label>
				<select id="catch_up" bind:value={formData.catch_up} class="form-select">
					<option value="Skip">Skip missed runs</option>
					<option value="RunOnce">Run once on catch-up</option>
					<option value="RunAll">Run all missed runs</option>
				</select>
				<span class="help-text">What to do when the daemon was stopped during scheduled times</span>
			</div>

			<!-- Variables -->
			<div class="form-group">
				<div class="vars-header">
					<span class="vars-label">Template Variables</span>
					<button class="btn-small" onclick={addVar}>
						<Plus size={14} />
						<span>Add</span>
					</button>
				</div>
				<div class="vars-list">
					{#each formData.vars as v, index (index)}
						<div class="var-row">
							<input
								type="text"
								bind:value={v.key}
								class="form-input var-key"
								placeholder="key"
							/>
							<span class="var-separator">=</span>
							<input
								type="text"
								bind:value={v.value}
								class="form-input var-value"
								placeholder="value"
							/>
							<button class="btn-icon" onclick={() => removeVar(index)} aria-label="Remove variable">
								<Trash2 size={14} />
							</button>
						</div>
					{:else}
						<div class="empty-vars">
							<p>No variables defined.</p>
							<button class="btn-small" onclick={addVar}>Add a variable</button>
						</div>
					{/each}
				</div>
			</div>

			<!-- Enabled Toggle (edit mode only) -->
			{#if isEdit}
				<div class="form-group">
					<label class="toggle-label">
						<input type="checkbox" bind:checked={formData.enabled} class="toggle-checkbox" />
						<span>Enabled</span>
					</label>
				</div>
			{/if}
		</div>

		<!-- Footer -->
		<div class="editor-footer">
			<button class="btn-secondary" onclick={onCancel} disabled={isSaving}>Cancel</button>
			<button class="btn-primary" onclick={handleSave} disabled={isSaving}>
				{#if isSaving}
					<span class="spinning"><Clock size={16} /></span>
				{:else}
					<Save size={16} />
				{/if}
				<span>{isSaving ? 'Saving...' : isEdit ? 'Save Changes' : 'Create Automation'}</span>
			</button>
		</div>
	</div>
</div>

<style>
	.editor-overlay {
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

	.editor-dialog {
		width: 100%;
		max-width: 42rem;
		max-height: 90vh;
		display: flex;
		flex-direction: column;
		background-color: var(--color-bg-secondary);
		border: 1px solid var(--color-border);
		border-radius: 0.5rem;
		box-shadow: 0 20px 25px -5px rgb(0 0 0 / 0.1), 0 8px 10px -6px rgb(0 0 0 / 0.1);
	}

	.editor-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		padding-left: 1.5rem;
		padding-right: 1.5rem;
		padding-top: 1rem;
		padding-bottom: 1rem;
		border-bottom: 1px solid var(--color-border);
	}

	.editor-header h2 {
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

	.editor-body {
		flex: 1;
		overflow-y: auto;
		padding-left: 1.5rem;
		padding-right: 1.5rem;
		padding-top: 1rem;
		padding-bottom: 1rem;
		display: flex;
		flex-direction: column;
		gap: 1rem;
	}

	.error-banner {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		padding: 0.75rem;
		background-color: rgb(127 29 29 / 0.5);
		border: 1px solid rgb(185 28 28 / 0.5);
		border-radius: 0.5rem;
		color: rgb(253 186 116);
		font-size: 0.875rem;
	}

	.error-icon {
		font-size: 1.125rem;
	}

	.error-text {
		flex: 1;
	}

	.form-group {
		display: flex;
		flex-direction: column;
		gap: 0.375rem;
	}

	.form-group label {
		display: block;
		font-size: 0.875rem;
		font-weight: 500;
		color: var(--color-text-secondary);
	}

	.form-input,
	.form-select,
	.form-textarea {
		width: 100%;
		padding-left: 0.75rem;
		padding-right: 0.75rem;
		padding-top: 0.5rem;
		padding-bottom: 0.5rem;
		background-color: var(--color-bg-primary);
		border: 1px solid var(--color-border);
		border-radius: 0.5rem;
		color: var(--color-text-primary);
		transition: all 0.15s ease-in-out;
	}

	.form-input::placeholder,
	.form-textarea::placeholder {
		color: var(--color-text-muted);
	}

	.form-input:focus,
	.form-select:focus,
	.form-textarea:focus {
		outline: none;
		border-color: var(--color-accent-primary);
		box-shadow: 0 0 0 1px var(--color-accent-primary);
	}

	.form-input.invalid,
	.form-select.invalid {
		border-color: rgb(239 68 68);
	}

	.form-textarea {
		resize: none;
	}

	.error-text {
		font-size: 0.75rem;
		color: rgb(248 113 113);
	}

	.help-text {
		font-size: 0.75rem;
		color: var(--color-text-muted);
	}

	.help-text code {
		padding-left: 0.25rem;
		padding-right: 0.25rem;
		padding-top: 0.125rem;
		padding-bottom: 0.125rem;
		border-radius: 0.25rem;
		background-color: var(--color-bg-tertiary);
		color: var(--color-accent-primary);
		font-family: ui-monospace, monospace;
	}

	.schedule-inputs {
		display: flex;
		gap: 0.5rem;
	}

	.schedule-inputs .form-input {
		flex: 1;
	}

	.preset-select {
		width: 12rem;
	}

	.vars-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
	}

	.vars-label {
		display: block;
		font-size: 0.875rem;
		font-weight: 500;
		color: var(--color-text-secondary);
	}

	.vars-list {
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
	}

	.var-row {
		display: flex;
		align-items: center;
		gap: 0.5rem;
	}

	.var-key {
		flex: 1;
		font-family: ui-monospace, monospace;
		font-size: 0.875rem;
	}

	.var-separator {
		color: var(--color-text-muted);
	}

	.var-value {
		flex: 2;
		font-family: ui-monospace, monospace;
		font-size: 0.875rem;
	}

	.btn-small {
		display: flex;
		align-items: center;
		gap: 0.25rem;
		padding-left: 0.5rem;
		padding-right: 0.5rem;
		padding-top: 0.25rem;
		padding-bottom: 0.25rem;
		border-radius: 0.25rem;
		font-size: 0.75rem;
		font-weight: 500;
		background-color: var(--color-bg-tertiary);
		color: var(--color-text-secondary);
		transition: all 0.15s ease-in-out;
	}

	.btn-small:hover {
		color: var(--color-text-primary);
		background-color: var(--color-border);
	}

	.btn-icon {
		padding: 0.25rem;
		border-radius: 0.25rem;
		color: var(--color-text-secondary);
		transition: all 0.15s ease-in-out;
	}

	.btn-icon:hover {
		color: rgb(248 113 113);
		background-color: var(--color-bg-tertiary);
	}

	.empty-vars {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 0.5rem;
		padding-top: 1rem;
		padding-bottom: 1rem;
		text-align: center;
		color: var(--color-text-muted);
	}

	.empty-vars p {
		font-size: 0.875rem;
	}

	.toggle-label {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		cursor: pointer;
	}

	.toggle-checkbox {
		width: 1rem;
		height: 1rem;
		border-radius: 0.25rem;
		border: 1px solid var(--color-border);
		background-color: var(--color-bg-primary);
		color: var(--color-accent-primary);
	}

	.toggle-checkbox:focus {
		box-shadow: 0 0 0 2px var(--color-accent-primary), 0 0 0 4px var(--color-bg-secondary);
	}

	.editor-footer {
		display: flex;
		align-items: center;
		justify-content: flex-end;
		gap: 0.5rem;
		padding-left: 1.5rem;
		padding-right: 1.5rem;
		padding-top: 1rem;
		padding-bottom: 1rem;
		border-top: 1px solid var(--color-border);
	}

	.btn-primary,
	.btn-secondary {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		padding-left: 1rem;
		padding-right: 1rem;
		padding-top: 0.5rem;
		padding-bottom: 0.5rem;
		border-radius: 0.5rem;
		font-size: 0.875rem;
		font-weight: 500;
		transition: all 0.15s ease-in-out;
	}

	.btn-primary:disabled,
	.btn-secondary:disabled {
		opacity: 0.5;
		cursor: not-allowed;
	}

	.btn-primary {
		background-color: var(--color-accent-primary);
		color: white;
	}

	.btn-primary:hover {
		background-color: var(--color-accent-hover);
	}

	.btn-secondary {
		background-color: var(--color-bg-tertiary);
		border: 1px solid var(--color-border);
		color: var(--color-text-primary);
	}

	.btn-secondary:hover {
		background-color: var(--color-border);
	}

	/* svelte-ignore css_unused_selector */
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
