<script lang="ts">
	import { configStore, homeConfig, repoConfig, config } from '$lib/stores';
	import type { ConfigFileType } from '$lib/types';
	import { FileText, Globe, Folder, Save, Check, AlertCircle } from 'lucide-svelte';

	type ConfigTab = 'effective' | 'home' | 'project';

	let activeTab: ConfigTab = $state('home');
	let editorContent = $state('');
	let saveStatus = $state<{ type: 'success' | 'error' | 'none'; message: string }>({
		type: 'none',
		message: ''
	});
	let isSaving = $state(false);

	const tabs = [
		{ id: 'effective' as ConfigTab, label: 'Effective', icon: FileText, description: 'Merged configuration (read-only)' },
		{ id: 'home' as ConfigTab, label: 'Global', icon: Globe, description: 'Global configuration (~/.velor/velor.toml)' },
		{ id: 'project' as ConfigTab, label: 'Project', icon: Folder, description: 'Project configuration (.velor/velor.toml)' }
	];

	// Update editor content when tab changes
	$effect(() => {
		if (activeTab === 'effective' && $config) {
			editorContent = tomlStringify($config);
		} else if (activeTab === 'home' && $homeConfig) {
			editorContent = $homeConfig;
		} else if (activeTab === 'project' && $repoConfig) {
			editorContent = $repoConfig;
		} else if (activeTab !== 'effective') {
			editorContent = '';
		}
	});

	function setTab(tab: ConfigTab) {
		activeTab = tab;
		setSaveStatus('none', '');
	}

	function setSaveStatus(type: 'success' | 'error' | 'none', message: string) {
		saveStatus = { type, message };
		// Auto-clear success messages
		if (type === 'success') {
			setTimeout(() => setSaveStatus('none', ''), 3000);
		}
	}

	async function saveConfig() {
		if (activeTab === 'effective') {
			setSaveStatus('error', 'Cannot save effective config. Edit Global or Project config instead.');
			return;
		}

		isSaving = true;
		setSaveStatus('none', '');

		try {
			const configType: ConfigFileType = activeTab === 'home' ? 'home' : 'repo';
			await configStore.save(configType, editorContent);
			setSaveStatus('success', `Configuration saved to ${tabs.find((t) => t.id === activeTab)?.label}`);
		} catch (e) {
			setSaveStatus('error', e instanceof Error ? e.message : 'Failed to save configuration');
		} finally {
			isSaving = false;
		}
	}

	function handleInput(event: Event) {
		const target = event.target as HTMLTextAreaElement;
		editorContent = target.value;
		setSaveStatus('none', '');
	}

	// Simple TOML stringify for display (basic implementation)
	function tomlStringify(obj: unknown): string {
		const lines: string[] = [];
		const objVal = obj as Record<string, unknown>;

		for (const [key, value] of Object.entries(objVal)) {
			if (key.startsWith('_')) continue; // Skip internal keys

			if (value === null || value === undefined) {
				continue;
			}

			if (typeof value === 'string') {
				lines.push(`${key} = ${JSON.stringify(value)}`);
			} else if (typeof value === 'number') {
				lines.push(`${key} = ${value}`);
			} else if (typeof value === 'boolean') {
				lines.push(`${key} = ${value}`);
			} else if (Array.isArray(value)) {
				lines.push(`${key} = [`);
				for (const item of value) {
					if (typeof item === 'string') {
						lines.push(`  ${JSON.stringify(item)},`);
					} else {
						lines.push(`  ${JSON.stringify(item)},`);
					}
				}
				lines.push(`]`);
			} else if (typeof value === 'object') {
				lines.push(``);
				lines.push(`[${key}]`);
				for (const [k, v] of Object.entries(value as Record<string, unknown>)) {
					if (v === null || v === undefined) continue;
					if (typeof v === 'string') {
						lines.push(`${k} = ${JSON.stringify(v)}`);
					} else if (typeof v === 'number') {
						lines.push(`${k} = ${v}`);
					} else if (typeof v === 'boolean') {
						lines.push(`${k} = ${v}`);
					} else if (typeof v === 'object') {
						// Nested object
						lines.push(``);
						lines.push(`[${k}]`);
						for (const [nk, nv] of Object.entries(v as Record<string, unknown>)) {
							if (nv === null || nv === undefined) continue;
							if (typeof nv === 'string') {
								lines.push(`${nk} = ${JSON.stringify(nv)}`);
							} else if (typeof nv === 'number') {
								lines.push(`${nk} = ${nv}`);
							} else if (typeof nv === 'boolean') {
								lines.push(`${nk} = ${nv}`);
							}
						}
					}
				}
			}
		}

		return lines.join('\n');
	}

	const currentTab = $derived(tabs.find((t) => t.id === activeTab));
	const canSave = $derived((activeTab === 'home' || activeTab === 'project') && !isSaving);
	const hasContent = $derived(editorContent.length > 0);
</script>

<div class="config-editor">
	<!-- Sub-tabs for config type -->
	<nav class="config-tabs" aria-label="Configuration type">
		{#each tabs as tab (tab.id)}
			<button
				class="config-tab"
				class:active={activeTab === tab.id}
				onclick={() => setTab(tab.id)}
				aria-label={tab.label}
				aria-selected={activeTab === tab.id}
				role="tab"
			>
				<tab.icon size={16} />
				<span>{tab.label}</span>
			</button>
		{/each}
	</nav>

	{#if currentTab}
		<div class="tab-description">
			<p>{currentTab.description}</p>
		</div>
	{/if}

	<!-- Save status banner -->
	{#if saveStatus.type !== 'none'}
		<div class="status-banner" class:success={saveStatus.type === 'success'} class:error={saveStatus.type === 'error'}>
			{#if saveStatus.type === 'success'}
				<Check size={18} />
			{:else}
				<AlertCircle size={18} />
			{/if}
			<span>{saveStatus.message}</span>
		</div>
	{/if}

	<!-- Editor area -->
	<div class="editor-container">
		{#if activeTab === 'effective'}
			<textarea
				class="config-textarea readonly"
				readonly
				value={editorContent}
				aria-label="Effective configuration (read-only)"
			></textarea>
		{:else}
			<textarea
				class="config-textarea"
				bind:value={editorContent}
				oninput={handleInput}
				placeholder="# Enter your TOML configuration here..."
				aria-label={`${currentTab?.label} configuration editor`}
			></textarea>
		{/if}

		<!-- Action bar -->
		<div class="action-bar">
			<div class="editor-info">
				{#if hasContent}
					<span class="char-count">{editorContent.length} characters</span>
				{:else}
					<span class="empty-hint">No configuration found</span>
				{/if}
			</div>

			{#if canSave}
				<button
					class="save-btn"
					onclick={saveConfig}
					disabled={!hasContent || isSaving}
					aria-label="Save configuration"
				>
					{#if isSaving}
						<span class="spinner"></span>
						Saving...
					{:else}
						<Save size={16} />
						Save {currentTab?.label} Config
					{/if}
				</button>
			{/if}
		</div>
	</div>
</div>

<style>
	.config-editor {
		display: flex;
		flex-direction: column;
		gap: 1rem;
	}

	.config-tabs {
		display: flex;
		gap: 0.5rem;
		border-bottom: 1px solid var(--color-border);
		margin-left: -0.5rem;
		margin-right: -0.5rem;
	}

	.config-tab {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		padding-left: 1rem;
		padding-right: 1rem;
		padding-top: 0.625rem;
		padding-bottom: 0.625rem;
		font-size: 0.875rem;
		font-weight: 500;
		color: var(--color-text-secondary);
		border-bottom: 2px solid transparent;
		border-top-left-radius: 0.5rem;
		border-top-right-radius: 0.5rem;
		transition: all 0.2s ease-in-out;
	}

	.config-tab:hover {
		color: var(--color-text-primary);
		background-color: var(--color-bg-tertiary);
	}

	.config-tab.active {
		color: var(--color-accent-primary);
		border-bottom-color: var(--color-accent-primary);
		background-color: var(--color-bg-tertiary);
	}

	.tab-description {
		font-size: 0.875rem;
		color: var(--color-text-secondary);
		padding-top: 0.5rem;
		padding-bottom: 0.5rem;
	}

	.status-banner {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		padding-left: 1rem;
		padding-right: 1rem;
		padding-top: 0.75rem;
		padding-bottom: 0.75rem;
		border-radius: 0.5rem;
	}

	.status-banner.success {
		background-color: rgb(20 83 45 / 0.5);
		border: 1px solid rgb(21 128 61 / 0.5);
		color: rgb(134 239 172);
	}

	.status-banner.error {
		background-color: rgb(127 29 29 / 0.5);
		border: 1px solid rgb(185 28 28 / 0.5);
		color: rgb(253 186 116);
	}

	.editor-container {
		display: flex;
		flex-direction: column;
		background-color: var(--color-bg-secondary);
		border: 1px solid var(--color-border);
		border-radius: 0.5rem;
		overflow: hidden;
	}

	.config-textarea {
		width: 100%;
		height: 24rem;
		padding-left: 1rem;
		padding-right: 1rem;
		padding-top: 0.75rem;
		padding-bottom: 0.75rem;
		background-color: var(--color-bg-secondary);
		color: var(--color-text-primary);
		font-family: ui-monospace, monospace;
		font-size: 0.875rem;
		resize: vertical;
	}

	.config-textarea:focus {
		outline: none;
		box-shadow: 0 0 0 2px rgb(var(--color-accent-primary) / 0.5);
	}

	.config-textarea.readonly {
		color: var(--color-text-secondary);
		cursor: default;
	}

	.action-bar {
		display: flex;
		align-items: center;
		justify-content: space-between;
		padding-left: 1rem;
		padding-right: 1rem;
		padding-top: 0.75rem;
		padding-bottom: 0.75rem;
		border-top: 1px solid var(--color-border);
		background-color: var(--color-bg-tertiary);
	}

	.editor-info {
		display: flex;
		align-items: center;
		gap: 0.75rem;
		font-size: 0.875rem;
		color: var(--color-text-secondary);
	}

	.char-count {
		color: var(--color-text-muted);
	}

	.empty-hint {
		font-style: italic;
		color: var(--color-text-muted);
	}

	.save-btn {
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

	.save-btn:hover {
		background-color: var(--color-accent-hover);
	}

	.save-btn:disabled {
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
