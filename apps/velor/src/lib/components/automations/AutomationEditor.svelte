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

<div class="editor-overlay" onclick={onCancel}>
	<div class="editor-dialog" onclick={(e) => e.stopPropagation()}>
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
				/>
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
					<label>Template Variables</label>
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
					<Clock size={16} class="spinning" />
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
		@apply fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm p-4;
	}

	.editor-dialog {
		@apply w-full max-w-2xl max-h-[90vh] flex flex-col bg-[var(--color-bg-secondary)] border border-[var(--color-border)] rounded-lg shadow-xl;
	}

	.editor-header {
		@apply flex items-center justify-between px-6 py-4 border-b border-[var(--color-border)];
	}

	.editor-header h2 {
		@apply text-lg font-semibold text-[var(--color-text-primary)];
	}

	.close-btn {
		@apply p-1 rounded text-[var(--color-text-secondary)] hover:text-[var(--color-text-primary)] hover:bg-[var(--color-bg-tertiary)] transition-all;
	}

	.editor-body {
		@apply flex-1 overflow-y-auto px-6 py-4 space-y-4;
	}

	.error-banner {
		@apply flex items-center gap-2 p-3 bg-red-950/50 border border-red-900/50 rounded-lg text-red-300 text-sm;
	}

	.error-icon {
		@apply text-lg;
	}

	.error-text {
		@apply flex-1;
	}

	.form-group {
		@apply space-y-1.5;
	}

	.form-group label {
		@apply block text-sm font-medium text-[var(--color-text-secondary)];
	}

	.form-input,
	.form-select,
	.form-textarea {
		@apply w-full px-3 py-2 bg-[var(--color-bg-primary)] border border-[var(--color-border)] rounded-lg text-[var(--color-text-primary)] placeholder-[var(--color-text-muted)] transition-all;
	}

	.form-input:focus,
	.form-select:focus,
	.form-textarea:focus {
		@apply outline-none border-[var(--color-accent-primary)] ring-1 ring-[var(--color-accent-primary)];
	}

	.form-input.invalid,
	.form-select.invalid {
		@apply border-red-500;
	}

	.form-textarea {
		@apply resize-none;
	}

	.form-text {
		@apply text-xs text-[var(--color-text-muted)];
	}

	.error-text {
		@apply text-xs text-red-400;
	}

	.help-text {
		@apply text-xs text-[var(--color-text-muted)];
	}

	.help-text code {
		@apply px-1 py-0.5 rounded bg-[var(--color-bg-tertiary)] text-[var(--color-accent-primary)] font-mono;
	}

	.schedule-inputs {
		@apply flex gap-2;
	}

	.schedule-inputs .form-input {
		@apply flex-1;
	}

	.preset-select {
		@apply w-48;
	}

	.vars-header {
		@apply flex items-center justify-between;
	}

	.vars-list {
		@apply space-y-2;
	}

	.var-row {
		@apply flex items-center gap-2;
	}

	.var-key {
		@apply flex-1 font-mono text-sm;
	}

	.var-separator {
		@apply text-[var(--color-text-muted)];
	}

	.var-value {
		@apply flex-[2] font-mono text-sm;
	}

	.btn-small {
		@apply flex items-center gap-1 px-2 py-1 rounded text-xs font-medium bg-[var(--color-bg-tertiary)] text-[var(--color-text-secondary)] hover:text-[var(--color-text-primary)] hover:bg-[var(--color-border)] transition-all;
	}

	.btn-icon {
		@apply p-1 rounded text-[var(--color-text-secondary)] hover:text-red-400 hover:bg-[var(--color-bg-tertiary)] transition-all;
	}

	.empty-vars {
		@apply flex flex-col items-center gap-2 py-4 text-center text-[var(--color-text-muted)];
	}

	.empty-vars p {
		@apply text-sm;
	}

	.toggle-label {
		@apply flex items-center gap-2 cursor-pointer;
	}

	.toggle-checkbox {
		@apply w-4 h-4 rounded border-[var(--color-border)] bg-[var(--color-bg-primary)] text-[var(--color-accent-primary)] focus:ring-2 focus:ring-[var(--color-accent-primary)] focus:ring-offset-2 focus:ring-offset-[var(--color-bg-secondary)];
	}

	.editor-footer {
		@apply flex items-center justify-end gap-2 px-6 py-4 border-t border-[var(--color-border)];
	}

	.btn-primary,
	.btn-secondary {
		@apply flex items-center gap-2 px-4 py-2 rounded-lg text-sm font-medium transition-all disabled:opacity-50 disabled:cursor-not-allowed;
	}

	.btn-primary {
		@apply bg-[var(--color-accent-primary)] text-white hover:bg-[var(--color-accent-hover)];
	}

	.btn-secondary {
		@apply bg-[var(--color-bg-tertiary)] border border-[var(--color-border)] text-[var(--color-text-primary)] hover:bg-[var(--color-border)];
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
