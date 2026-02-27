<script lang="ts">
	import { onMount } from 'svelte';
	import { selectedSession, sessionsStore, sessions } from '$lib/stores';
	import { MessageSquare, Sparkles, ArrowRight } from 'lucide-svelte';
	import SessionDetail from '$lib/components/sessions/SessionDetail.svelte';
	import { goto } from '$app/navigation';

	/** Load sessions on mount */
	onMount(() => {
		sessionsStore.load(50);
	});

	/** Navigate to automations page */
	function goToAutomations() {
		goto('/automations');
	}

	/** Navigate to settings page */
	function goToSettings() {
		goto('/settings');
	}

	/** Handle close of session detail */
	function handleCloseSessionDetail() {
		sessionsStore.clearSelected();
	}

	/** Handle retry from session detail */
	function handleRetryFromSession(_promptName: string) {
		// Navigate to executions page with the prompt pre-selected
		goto('/executions');
	}
</script>

<div class="h-full flex flex-col">
	{#if $selectedSession}
		<!-- Show selected session details -->
		<SessionDetail
			session={$selectedSession}
			onClose={handleCloseSessionDetail}
			onRetry={handleRetryFromSession}
		/>
	{:else}
		<!-- Welcome/Empty state when no session is selected -->
		<div class="flex-1 flex items-center justify-center p-8">
			<div class="max-w-2xl w-full text-center space-y-8">
				<!-- Icon -->
				<div class="flex justify-center">
					<div class="relative">
						<div class="absolute inset-0 bg-primary/20 rounded-full blur-3xl"></div>
						<div class="relative p-6 rounded-2xl bg-primary/10 border border-primary/20">
							<MessageSquare size={48} class="text-primary" />
						</div>
					</div>
				</div>

				<!-- Heading -->
				<div class="space-y-3">
					<h1 class="text-3xl font-bold text-foreground">Welcome to Velor</h1>
					<p class="text-lg text-muted-foreground">
						Select a session from the sidebar to view details, or create a new automation to get started.
					</p>
				</div>

				<!-- Stats (if available) -->
				{#if $sessions && $sessions.length > 0}
					<div class="grid grid-cols-3 gap-4 py-6">
						<div class="p-4 rounded-lg bg-card border border-border">
							<div class="text-2xl font-bold text-foreground">{$sessions.length}</div>
							<div class="text-sm text-muted-foreground">Total Sessions</div>
						</div>
						<div class="p-4 rounded-lg bg-card border border-border">
							<div class="text-2xl font-bold text-[var(--color-success)]">
								{$sessions.filter(s => s.state === 'completed').length}
							</div>
							<div class="text-sm text-muted-foreground">Completed</div>
						</div>
						<div class="p-4 rounded-lg bg-card border border-border">
							<div class="text-2xl font-bold text-[var(--color-accent-primary)]">
								{$sessions.filter(s => s.state === 'running' || s.state === 'rendering').length}
							</div>
							<div class="text-sm text-muted-foreground">Active</div>
						</div>
					</div>
				{/if}

				<!-- Action Buttons -->
				<div class="flex items-center justify-center gap-4">
					<button
						onclick={goToAutomations}
						class="inline-flex items-center gap-2 px-6 py-3 rounded-lg bg-primary text-primary-foreground font-medium hover:bg-primary/90 transition-all"
					>
						<Sparkles size={18} />
						<span>Create Automation</span>
					</button>
					<button
						onclick={goToSettings}
						class="inline-flex items-center gap-2 px-6 py-3 rounded-lg bg-card border border-border text-foreground font-medium hover:bg-muted transition-all"
					>
						<span>Settings</span>
						<ArrowRight size={18} />
					</button>
				</div>

				<!-- Quick hint -->
				<div class="pt-6 text-sm text-muted-foreground">
					<p>Use the sidebar to browse sessions by project, pin important conversations, or manage your automation workflows.</p>
				</div>
			</div>
		</div>
	{/if}
</div>
