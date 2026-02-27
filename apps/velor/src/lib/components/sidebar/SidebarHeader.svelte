<script lang="ts">
	import * as Sidebar from "$lib/components/ui/sidebar/index.js";
	import { Button } from "$lib/components/ui/button/index.js";
	import { goto } from "$app/navigation";
	import { Plus, Calendar, FileText } from "lucide-svelte";
	import NewSessionDialog from "$lib/components/sessions/NewSessionDialog.svelte";

	let showNewSessionDialog = $state(false);

	/**
	 * Handle opening new session dialog
	 */
	function handleNewSession(): void {
		showNewSessionDialog = true;
	}

	/**
	 * Handle closing new session dialog
	 */
	function handleCloseDialog(): void {
		showNewSessionDialog = false;
	}

	/**
	 * Handle navigation to automations
	 */
	async function handleAutomations(): Promise<void> {
		await goto("/automations");
	}

	/**
	 * Handle navigation to plans
	 */
	async function handlePlans(): Promise<void> {
		await goto("/plans");
	}
</script>

<Sidebar.Header class="border-b border-border p-4">
	<div class="flex flex-col gap-2">
		<Button
			variant="default"
			class="w-full justify-start gap-2"
			onclick={handleNewSession}
		>
			<Plus size={16} />
			<span>New session</span>
		</Button>
		<Button
			variant="outline"
			class="w-full justify-start gap-2"
			onclick={handleAutomations}
		>
			<Calendar size={16} />
			<span>Automations</span>
		</Button>
		<Button
			variant="outline"
			class="w-full justify-start gap-2"
			onclick={handlePlans}
		>
			<FileText size={16} />
			<span>Plans</span>
		</Button>
	</div>
</Sidebar.Header>

{#if showNewSessionDialog}
	<NewSessionDialog onClose={handleCloseDialog} />
{/if}
