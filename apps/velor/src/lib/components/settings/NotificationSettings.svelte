<script lang="ts">
	import { config, configStore } from '$lib/stores';
	import type { Notifications } from '$lib/types';
	import { Bell, Send, Check, AlertCircle, Volume2, Save } from 'lucide-svelte';

	let isSaving = $state(false);
	let saveStatus = $state({ type: 'none', message: '' });
	let testStatus = $state({ type: 'none', message: '' });

	// Notification settings form state
	let formData = $state({
		enabled: false,
		notifyOnSuccess: false,
		notifyOnMaxIterations: false,
		notifyOnFailure: false,
		outputPreviewChars: 500,
		// Telegram
		telegramEnabled: false,
		telegramBotTokenEnv: '',
		telegramChatId: '',
		telegramApiBaseUrl: 'https://api.telegram.org',
		telegramParseMode: 'MarkdownV2',
		// macOS
		macosEnabled: false,
		macosSound: 'default'
	});

	// Initialize form from config
	$effect(() => {
		if ($config?.notifications) {
			const notifs = $config.notifications;
			formData.enabled = notifs.enabled ?? false;
			formData.notifyOnSuccess = notifs.notify_on_success ?? false;
			formData.notifyOnMaxIterations = notifs.notify_on_max_iterations ?? false;
			formData.notifyOnFailure = notifs.notify_on_failure ?? true;
			formData.outputPreviewChars = notifs.output_preview_chars ?? 500;

			// Telegram settings
			if (notifs.telegram) {
				formData.telegramEnabled = notifs.telegram.enabled ?? false;
				formData.telegramBotTokenEnv = notifs.telegram.bot_token_env ?? '';
				formData.telegramChatId = notifs.telegram.chat_id ?? '';
				formData.telegramApiBaseUrl = notifs.telegram.api_base_url ?? 'https://api.telegram.org';
				formData.telegramParseMode = notifs.telegram.parse_mode ?? 'MarkdownV2';
			}

			// macOS settings
			if (notifs.macos) {
				formData.macosEnabled = notifs.macos.enabled ?? false;
				formData.macosSound = notifs.macos.sound ?? 'default';
			}
		}
	});

	function setSaveStatus(type: 'success' | 'error' | 'none', message: string) {
		saveStatus = { type, message };
		if (type === 'success') {
			setTimeout(() => setSaveStatus('none', ''), 3000);
		}
	}

	function setTestStatus(type: 'success' | 'error' | 'none', message: string) {
		testStatus = { type, message };
		if (type === 'success' || type === 'error') {
			setTimeout(() => setTestStatus('none', ''), 5000);
		}
	}

	async function saveSettings() {
		isSaving = true;
		setSaveStatus('none', '');
		setTestStatus('none', '');

		try {
			// Get current home config
			let configContent = await (async () => {
				try {
					const { getHomeConfig } = await import('$lib/services/tauri');
					return await getHomeConfig();
				} catch {
					return '';
				}
			})();

			// Build new notifications config
			const newNotifConfig: Record<string, unknown> = {
				enabled: formData.enabled,
				notify_on_success: formData.notifyOnSuccess,
				notify_on_max_iterations: formData.notifyOnMaxIterations,
				notify_on_failure: formData.notifyOnFailure,
				output_preview_chars: formData.outputPreviewChars
			};

			// Add Telegram config if enabled
			if (formData.telegramEnabled) {
				newNotifConfig.telegram = {
					enabled: true,
					bot_token_env: formData.telegramBotTokenEnv || 'TELEGRAM_BOT_TOKEN',
					chat_id: formData.telegramChatId,
					api_base_url: formData.telegramApiBaseUrl || undefined,
					parse_mode: formData.telegramParseMode
				};
			}

			// Add macOS config if enabled
			if (formData.macosEnabled) {
				newNotifConfig.macos = {
					enabled: true,
					sound: formData.macosSound === 'default' ? undefined : formData.macosSound
				};
			}

			// Simple TOML generation for notifications section
			let tomlSection = '\n\n[notifications]\n';
			tomlSection += `enabled = ${formData.enabled}\n`;
			tomlSection += `notify_on_success = ${formData.notifyOnSuccess}\n`;
			tomlSection += `notify_on_max_iterations = ${formData.notifyOnMaxIterations}\n`;
			tomlSection += `notify_on_failure = ${formData.notifyOnFailure}\n`;
			tomlSection += `output_preview_chars = ${formData.outputPreviewChars}\n`;

			if (formData.telegramEnabled) {
				tomlSection += '\n[notifications.telegram]\n';
				tomlSection += `enabled = true\n`;
				tomlSection += `bot_token_env = "${formData.telegramBotTokenEnv || 'TELEGRAM_BOT_TOKEN'}"\n`;
				tomlSection += `chat_id = "${formData.telegramChatId}"\n`;
				if (formData.telegramApiBaseUrl !== 'https://api.telegram.org') {
					tomlSection += `api_base_url = "${formData.telegramApiBaseUrl}"\n`;
				}
				tomlSection += `parse_mode = "${formData.telegramParseMode}"\n`;
			}

			if (formData.macosEnabled) {
				tomlSection += '\n[notifications.macos]\n';
				tomlSection += `enabled = true\n`;
				if (formData.macosSound !== 'default') {
					tomlSection += `sound = "${formData.macosSound}"\n`;
				}
			}

			// Remove existing notifications section and add new one
			let newConfigText = configContent;
			const notifIndex = newConfigText.indexOf('[notifications]');
			if (notifIndex >= 0) {
				// Find the end of the section (next section or end of file)
				const nextSectionIndex = newConfigText.indexOf('\n[', notifIndex + 15);
				if (nextSectionIndex > 0) {
					newConfigText = newConfigText.slice(0, notifIndex) + tomlSection + newConfigText.slice(nextSectionIndex);
				} else {
					newConfigText = newConfigText.slice(0, notifIndex) + tomlSection;
				}
			} else {
				newConfigText = newConfigText + tomlSection;
			}

			await configStore.save('home', newConfigText);
			setSaveStatus('success', 'Notification settings saved successfully');
		} catch (e) {
			setSaveStatus('error', e instanceof Error ? e.message : 'Failed to save notification settings');
		} finally {
			isSaving = false;
		}
	}

	async function testNotification() {
		setTestStatus('none', '');

		try {
			const { testNotification } = await import('$lib/services/tauri');
			await testNotification();
			setTestStatus('success', 'Test notification sent successfully!');
		} catch (e) {
			setTestStatus('error', e instanceof Error ? e.message : 'Failed to send test notification');
		}
	}

	const hasEnabledNotif = $derived(formData.enabled && (formData.telegramEnabled || formData.macosEnabled));
</script>

<div class="notification-settings">
	<div class="settings-header">
		<div>
			<h2>Notification Settings</h2>
			<p>Configure how and when you receive notifications from Velor</p>
		</div>
		<button class="test-btn" onclick={testNotification} disabled={!formData.enabled || !hasEnabledNotif}>
			<Send size={16} />
			Test Notification
		</button>
	</div>

	<!-- Test status banner -->
	{#if testStatus.type !== 'none'}
		<div class="status-banner" class:success={testStatus.type === 'success'} class:error={testStatus.type === 'error'}>
			{#if testStatus.type === 'success'}
				<Check size={18} />
			{:else}
				<AlertCircle size={18} />
			{/if}
			<span>{testStatus.message}</span>
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

	<div class="settings-grid">
		<!-- General Settings -->
		<section class="settings-section">
			<div class="section-header">
				<Bell size={20} />
				<h3>General Settings</h3>
			</div>

			<div class="form-group">
				<label class="toggle-label">
					<input type="checkbox" bind:checked={formData.enabled} />
					<span class="toggle-slider"></span>
					<span class="toggle-text">Enable Notifications</span>
				</label>
				<span class="hint">Master toggle for all notifications</span>
			</div>

			<div class="form-group">
				<label class="checkbox-label">
					<input type="checkbox" bind:checked={formData.notifyOnSuccess} disabled={!formData.enabled} />
					<span>Notify on Success</span>
				</label>
				<span class="hint">Send a notification when an agent completes successfully</span>
			</div>

			<div class="form-group">
				<label class="checkbox-label">
					<input type="checkbox" bind:checked={formData.notifyOnMaxIterations} disabled={!formData.enabled} />
					<span>Notify on Max Iterations</span>
				</label>
				<span class="hint">Send a notification when max iterations are reached</span>
			</div>

			<div class="form-group">
				<label class="checkbox-label">
					<input type="checkbox" bind:checked={formData.notifyOnFailure} disabled={!formData.enabled} />
					<span>Notify on Failure</span>
				</label>
				<span class="hint">Send a notification when an agent fails</span>
			</div>

			<div class="form-group">
				<label for="output-chars">Output Preview Characters</label>
				<input
					id="output-chars"
					type="number"
					bind:value={formData.outputPreviewChars}
					min="0"
					max="5000"
					step="100"
					disabled={!formData.enabled}
				/>
				<span class="hint">Number of output characters to include in notifications</span>
			</div>
		</section>

		<!-- Telegram Settings -->
		<section class="settings-section">
			<div class="section-header">
				<Send size={20} />
				<h3>Telegram</h3>
			</div>

			<div class="form-group">
				<label class="toggle-label">
					<input type="checkbox" bind:checked={formData.telegramEnabled} disabled={!formData.enabled} />
					<span class="toggle-slider"></span>
					<span class="toggle-text">Enable Telegram Notifications</span>
				</label>
				<span class="hint">Send notifications via Telegram bot</span>
			</div>

			<div class="form-group">
				<label for="telegram-token">Bot Token Environment Variable</label>
				<input
					id="telegram-token"
					type="text"
					bind:value={formData.telegramBotTokenEnv}
					placeholder="TELEGRAM_BOT_TOKEN"
					disabled={!formData.enabled || !formData.telegramEnabled}
				/>
				<span class="hint">Environment variable containing your bot token</span>
			</div>

			<div class="form-group">
				<label for="telegram-chat">Chat ID</label>
				<input
					id="telegram-chat"
					type="text"
					bind:value={formData.telegramChatId}
					placeholder="-1001234567890"
					disabled={!formData.enabled || !formData.telegramEnabled}
				/>
				<span class="hint">Your Telegram chat ID (message @userinfobot to get it)</span>
			</div>

			<div class="form-group">
				<label for="telegram-api">API Base URL</label>
				<input
					id="telegram-api"
					type="text"
					bind:value={formData.telegramApiBaseUrl}
					placeholder="https://api.telegram.org"
					disabled={!formData.enabled || !formData.telegramEnabled}
				/>
				<span class="hint">Optional: custom API URL (for proxies)</span>
			</div>

			<div class="form-group">
				<label for="telegram-parse">Parse Mode</label>
				<select
					id="telegram-parse"
					bind:value={formData.telegramParseMode}
					disabled={!formData.enabled || !formData.telegramEnabled}
				>
					<option value="MarkdownV2">MarkdownV2</option>
					<option value="Html">HTML</option>
				</select>
				<span class="hint">Message formatting mode</span>
			</div>
		</section>

		<!-- macOS Settings -->
		<section class="settings-section">
			<div class="section-header">
				<Volume2 size={20} />
				<h3>macOS Notifications</h3>
			</div>

			<div class="form-group">
				<label class="toggle-label">
					<input type="checkbox" bind:checked={formData.macosEnabled} disabled={!formData.enabled} />
					<span class="toggle-slider"></span>
					<span class="toggle-text">Enable macOS Notifications</span>
				</label>
				<span class="hint">Show native macOS notifications</span>
			</div>

			<div class="form-group">
				<label for="macos-sound">Notification Sound</label>
				<select
					id="macos-sound"
					bind:value={formData.macosSound}
					disabled={!formData.enabled || !formData.macosEnabled}
				>
					<option value="default">Default</option>
					<option value="Basso">Basso</option>
					<option value="Blow">Blow</option>
					<option value="Bottle">Bottle</option>
					<option value="Frog">Frog</option>
					<option value="Funk">Funk</option>
					<option value="Glass">Glass</option>
					<option value="Hero">Hero</option>
					<option value="Morse">Morse</option>
					<option value="Ping">Ping</option>
					<option value="Pop">Pop</option>
					<option value="Purr">Purr</option>
					<option value="Sosumi">Sosumi</option>
					<option value="Submarine">Submarine</option>
					<option value="Tink">Tink</option>
				</select>
				<span class="hint">Sound to play for notifications</span>
			</div>
		</section>
	</div>

	<!-- Save Button -->
	<div class="save-section">
		<button class="save-btn" onclick={saveSettings} disabled={isSaving}>
			{#if isSaving}
				<span class="spinner"></span>
				Saving...
			{:else}
				<Save size={16} />
				Save Settings
			{/if}
		</button>
	</div>
</div>

<style>
	.notification-settings {
		@apply flex flex-col gap-6;
	}

	.settings-header {
		@apply flex items-start justify-between;
	}

	.settings-header h2 {
		@apply text-xl font-semibold text-[var(--color-text-primary)];
	}

	.settings-header p {
		@apply text-sm text-[var(--color-text-secondary)] mt-1;
	}

	.test-btn {
		@apply flex items-center gap-2 px-4 py-2 rounded-lg bg-[var(--color-bg-secondary)] border border-[var(--color-border)] text-[var(--color-text-primary)] text-sm font-medium hover:bg-[var(--color-bg-tertiary)] hover:border-[var(--color-accent-primary)] disabled:opacity-50 disabled:cursor-not-allowed transition-all;
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

	.settings-grid {
		@apply grid grid-cols-1 lg:grid-cols-3 gap-6;
	}

	.settings-section {
		@apply flex flex-col p-5 bg-[var(--color-bg-secondary)] border border-[var(--color-border)] rounded-lg;
	}

	.section-header {
		@apply flex items-center gap-3 mb-5 pb-4 border-b border-[var(--color-border)];
	}

	.section-header :global(svg) {
		@apply text-[var(--color-accent-primary)];
	}

	.section-header h3 {
		@apply text-lg font-semibold text-[var(--color-text-primary)];
	}

	.form-group {
		@apply mb-5;
	}

	.form-group:last-child {
		@apply mb-0;
	}

	.form-group label {
		@apply block text-sm font-medium text-[var(--color-text-primary)] mb-2;
	}

	.form-group input[type="text"],
	.form-group input[type="number"],
	.form-group select {
		@apply w-full px-3 py-2 bg-[var(--color-bg-primary)] border border-[var(--color-border)] rounded-lg text-[var(--color-text-primary)] placeholder-[var(--color-text-muted)] focus:outline-none focus:ring-2 focus:ring-[var(--color-accent-primary)]/50 focus:border-[var(--color-accent-primary)] transition-all disabled:opacity-50 disabled:cursor-not-allowed;
	}

	.hint {
		@apply block text-xs text-[var(--color-text-muted)] mt-1.5;
	}

	/* Toggle Switch */
	.toggle-label {
		@apply flex items-center gap-3 cursor-pointer;
	}

	.toggle-label input[type="checkbox"] {
		@apply sr-only;
	}

	.toggle-slider {
		@apply relative w-11 h-6 bg-[var(--color-bg-tertiary)] rounded-full transition-colors duration-200 peer-checked:bg-[var(--color-accent-primary)];
	}

	.toggle-slider::before {
		content: '';
		@apply absolute top-0.5 left-0.5 w-5 h-5 bg-white rounded-full shadow transition-transform duration-200;
	}

	.toggle-label input:checked + .toggle-slider {
		@apply bg-[var(--color-accent-primary)];
	}

	.toggle-label input:checked + .toggle-slider::before {
		@apply translate-x-5;
	}

	.toggle-text {
		@apply text-sm font-medium text-[var(--color-text-primary)];
	}

	/* Checkbox */
	.checkbox-label {
		@apply flex items-center gap-3 cursor-pointer;
	}

	.checkbox-label input[type="checkbox"] {
		@apply w-4 h-4 rounded border-[var(--color-border)] bg-[var(--color-bg-primary)] text-[var(--color-accent-primary)] focus:ring-2 focus:ring-[var(--color-accent-primary)]/50 disabled:opacity-50 disabled:cursor-not-allowed;
	}

	.checkbox-label span {
		@apply text-sm text-[var(--color-text-primary)] disabled:opacity-50;
	}

	.save-section {
		@apply flex justify-end pt-4 border-t border-[var(--color-border)];
	}

	.save-btn {
		@apply flex items-center gap-2 px-6 py-2.5 rounded-lg bg-[var(--color-accent-primary)] text-white text-sm font-medium hover:bg-[var(--color-accent-hover)] disabled:opacity-50 disabled:cursor-not-allowed transition-all;
	}

	.spinner {
		@apply w-4 h-4 rounded-full border-2 border-white/30 border-t-white animate-spin;
	}
</style>
