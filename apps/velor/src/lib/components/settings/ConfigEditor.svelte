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
		{#each tabs as tab}
			<button
				class="config-tab"
				class:active={activeTab === tab.id}
				onclick={() => setTab(tab.id)}
				aria-label={tab.label}
				aria-selected={activeTab === tab.id}
				role="tab"
			>
				<svelte:component this={tab.icon} size={16} />
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
		@apply flex flex-col gap-4;
	}

	.config-tabs {
		@apply flex gap-2 border-b border-[var(--color-border)] -mx-2;
	}

	.config-tab {
		@apply flex items-center gap-2 px-4 py-2.5 text-sm font-medium text-[var(--color-text-secondary)] border-b-2 border-transparent hover:text-[var(--color-text-primary)] hover:bg-[var(--color-bg-tertiary)] rounded-t-lg transition-all duration-200;
	}

	.config-tab.active {
		@apply text-[var(--color-accent-primary)] border-b-[var(--color-accent-primary)] bg-[var(--color-bg-tertiary)];
	}

	.tab-description {
		@apply text-sm text-[var(--color-text-secondary)] py-2;
	}

	.status-banner {
		@apply flex items-center gap-2 px-4 py-3 rounded-lg;
	}

	.status-banner.success {
		@apply bg-green-950/50 border border-green-900/50 text-green-300;
	}

	.status-banner.error {
		@apply bg-red-950/50 border border-red-900/50 text-red-300;
	}

	.editor-container {
		@apply flex flex-col bg-[var(--color-bg-secondary)] border border-[var(--color-border)] rounded-lg overflow-hidden;
	}

	.config-textarea {
		@apply w-full h-96 px-4 py-3 bg-[var(--color-bg-secondary)] text-[var(--color-text-primary)] font-mono text-sm resize-y focus:outline-none focus:ring-2 focus:ring-[var(--color-accent-primary)]/50;
	}

	.config-textarea.readonly {
		@apply text-[var(--color-text-secondary)] cursor-default;
	}

	.action-bar {
		@apply flex items-center justify-between px-4 py-3 border-t border-[var(--color-border)] bg-[var(--color-bg-tertiary)];
	}

	.editor-info {
		@apply flex items-center gap-3 text-sm text-[var(--color-text-secondary)];
	}

	.char-count {
		@apply text-[var(--color-text-muted)];
	}

	.empty-hint {
		@apply italic text-[var(--color-text-muted)];
	}

	.save-btn {
		@apply flex items-center gap-2 px-4 py-2 rounded-lg bg-[var(--color-accent-primary)] text-white text-sm font-medium hover:bg-[var(--color-accent-hover)] disabled:opacity-50 disabled:cursor-not-allowed transition-all duration-200;
	}

	.save-btn:disabled {
		@apply opacity-50 cursor-not-allowed;
	}

	.spinner {
		@apply w-4 h-4 rounded-full border-2 border-white/30 border-t-white animate-spin;
	}
</style>
