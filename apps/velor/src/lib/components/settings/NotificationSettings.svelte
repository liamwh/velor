<script lang="ts">
	import { config, configStore } from '$lib/stores';
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
		display: flex;
		flex-direction: column;
		gap: 1.5rem;
	}

	.settings-header {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
	}

	.settings-header h2 {
		font-size: 1.25rem;
		font-weight: 600;
		color: var(--color-text-primary);
	}

	.settings-header p {
		font-size: 0.875rem;
		color: var(--color-text-secondary);
		margin-top: 0.25rem;
	}

	.test-btn {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		padding-left: 1rem;
		padding-right: 1rem;
		padding-top: 0.5rem;
		padding-bottom: 0.5rem;
		border-radius: 0.5rem;
		background-color: var(--color-bg-secondary);
		border: 1px solid var(--color-border);
		color: var(--color-text-primary);
		font-size: 0.875rem;
		font-weight: 500;
		transition: all 0.15s ease-in-out;
	}

	.test-btn:hover {
		background-color: var(--color-bg-tertiary);
		border-color: var(--color-accent-primary);
	}

	.test-btn:disabled {
		opacity: 0.5;
		cursor: not-allowed;
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

	.settings-grid {
		display: grid;
		grid-template-columns: repeat(1, minmax(0, 1fr));
		gap: 1.5rem;
	}

	@media (min-width: 1024px) {
		.settings-grid {
			grid-template-columns: repeat(3, minmax(0, 1fr));
		}
	}

	.settings-section {
		display: flex;
		flex-direction: column;
		padding: 1.25rem;
		background-color: var(--color-bg-secondary);
		border: 1px solid var(--color-border);
		border-radius: 0.5rem;
	}

	.section-header {
		display: flex;
		align-items: center;
		gap: 0.75rem;
		margin-bottom: 1.25rem;
		padding-bottom: 1rem;
		border-bottom: 1px solid var(--color-border);
	}

	.section-header :global(svg) {
		color: var(--color-accent-primary);
	}

	.section-header h3 {
		font-size: 1.125rem;
		font-weight: 600;
		color: var(--color-text-primary);
	}

	.form-group {
		margin-bottom: 1.25rem;
	}

	.form-group:last-child {
		margin-bottom: 0;
	}

	.form-group label {
		display: block;
		font-size: 0.875rem;
		font-weight: 500;
		color: var(--color-text-primary);
		margin-bottom: 0.5rem;
	}

	.form-group input[type="text"],
	.form-group input[type="number"],
	.form-group select {
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

	.form-group input[type="text"]::placeholder,
	.form-group input[type="number"]::placeholder {
		color: var(--color-text-muted);
	}

	.form-group input[type="text"]:focus,
	.form-group input[type="number"]:focus,
	.form-group select:focus {
		outline: none;
		box-shadow: 0 0 0 2px rgb(var(--color-accent-primary) / 0.5);
		border-color: var(--color-accent-primary);
	}

	.form-group input:disabled,
	.form-group select:disabled {
		opacity: 0.5;
		cursor: not-allowed;
	}

	.hint {
		display: block;
		font-size: 0.75rem;
		color: var(--color-text-muted);
		margin-top: 0.375rem;
	}

	/* Toggle Switch */
	.toggle-label {
		display: flex;
		align-items: center;
		gap: 0.75rem;
		cursor: pointer;
	}

	.toggle-label input[type="checkbox"] {
		position: absolute;
		width: 1px;
		height: 1px;
		padding: 0;
		margin: -1px;
		overflow: hidden;
		clip: rect(0, 0, 0, 0);
		white-space: nowrap;
		border-width: 0;
	}

	.toggle-slider {
		position: relative;
		width: 2.75rem;
		height: 1.5rem;
		background-color: var(--color-bg-tertiary);
		border-radius: 9999px;
		transition: background-color 0.2s ease-in-out;
	}

	.toggle-slider::before {
		content: '';
		position: absolute;
		top: 0.125rem;
		left: 0.125rem;
		width: 1.25rem;
		height: 1.25rem;
		background-color: white;
		border-radius: 9999px;
		box-shadow: 0 1px 2px 0 rgb(0 0 0 / 0.05);
		transition: transform 0.2s ease-in-out;
	}

	.toggle-label input:checked + .toggle-slider {
		background-color: var(--color-accent-primary);
	}

	.toggle-label input:checked + .toggle-slider::before {
		transform: translateX(1.25rem);
	}

	.toggle-text {
		font-size: 0.875rem;
		font-weight: 500;
		color: var(--color-text-primary);
	}

	/* Checkbox */
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
		background-color: var(--color-bg-primary);
	}

	.checkbox-label input[type="checkbox"]:focus {
		box-shadow: 0 0 0 2px rgb(var(--color-accent-primary) / 0.5);
	}

	.checkbox-label input:disabled {
		opacity: 0.5;
		cursor: not-allowed;
	}

	.checkbox-label span {
		font-size: 0.875rem;
		color: var(--color-text-primary);
	}

	.checkbox-label input:disabled ~ span {
		opacity: 0.5;
	}

	.save-section {
		display: flex;
		justify-content: flex-end;
		padding-top: 1rem;
		border-top: 1px solid var(--color-border);
	}

	.save-btn {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		padding-left: 1.5rem;
		padding-right: 1.5rem;
		padding-top: 0.625rem;
		padding-bottom: 0.625rem;
		border-radius: 0.5rem;
		background-color: var(--color-accent-primary);
		color: white;
		font-size: 0.875rem;
		font-weight: 500;
		transition: all 0.15s ease-in-out;
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
