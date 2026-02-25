<script lang="ts">
	import AutomationList from '$lib/components/automations/AutomationList.svelte';
	import AutomationEditor from '$lib/components/automations/AutomationEditor.svelte';
	import AutomationRuns from '$lib/components/automations/AutomationRuns.svelte';
	import { automationsStore } from '$lib/stores';
	import type { Automation } from '$lib/types';

	let showEditor = $state(false);
	let editingAutomation = $state<Automation | undefined>(undefined);
	let showRuns = $state(false);
	let selectedAutomationName = $state('');

	function handleCreate() {
		editingAutomation = undefined;
		showEditor = true;
	}

	function handleEdit(automation: Automation) {
		editingAutomation = automation;
		showEditor = true;
	}

	function handleViewRuns(name: string) {
		selectedAutomationName = name;
		showRuns = true;
	}

	function handleEditorSave() {
		showEditor = false;
		editingAutomation = undefined;
		automationsStore.load();
	}

	function handleEditorCancel() {
		showEditor = false;
		editingAutomation = undefined;
	}

	function handleRunsClose() {
		showRuns = false;
		selectedAutomationName = '';
	}
</script>

<div class="automations-page">
	<AutomationList
		onCreate={handleCreate}
		onEdit={handleEdit}
		onViewRuns={handleViewRuns}
	/>

	{#if showEditor}
		<AutomationEditor
			automation={editingAutomation}
			onSave={handleEditorSave}
			onCancel={handleEditorCancel}
		/>
	{/if}

	{#if showRuns && selectedAutomationName}
		<AutomationRuns
			automationName={selectedAutomationName}
			onClose={handleRunsClose}
		/>
	{/if}
</div>

<style>
	.automations-page {
		@apply h-full;
	}
</style>
