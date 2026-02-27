<script lang="ts">
	import * as Collapsible from "$lib/components/ui/collapsible/index.js";
	import * as Sidebar from "$lib/components/ui/sidebar/index.js";
	import type { ExecutionRecord, Project } from "$lib/types";
	import SessionItem from "./SessionItem.svelte";
	import { Folder, FolderOpen, ChevronRight, ChevronDown } from "lucide-svelte";

	interface Props {
		/** The project metadata */
		project: Project;
		/** Sessions belonging to this project */
		sessions: ExecutionRecord[];
		/** Currently selected session ID */
		selectedSessionId?: string | null;
		/** Callback when a session is selected */
		onselect?: (session: ExecutionRecord) => void;
		/** Initially collapsed state */
		collapsed?: boolean;
	}

	let {
		project,
		sessions,
		selectedSessionId = null,
		onselect,
		collapsed = false
	}: Props = $props();

	/** Track whether this group is collapsed - starts with the inverse of the collapsed prop */
	const initialOpen = !collapsed;
	let isOpen = $state(initialOpen);

	/**
	 * Get the project display name from path
	 */
	let projectName = $derived(() => {
		return project.display_name || project.path.split("/").pop() || project.path;
	});

	/**
	 * Toggle collapse state
	 */
	function toggleCollapse(): void {
		isOpen = !isOpen;
	}
</script>

<Sidebar.Group>
	<Sidebar.GroupLabel class="group/label flex items-center justify-between px-2 text-xs font-medium text-sidebar-foreground/70">
		<button
			onclick={toggleCollapse}
			class="flex items-center gap-1.5 hover:text-sidebar-foreground transition-colors"
		>
			{#if isOpen}
				<ChevronDown size={14} />
			{:else}
				<ChevronRight size={14} />
			{/if}
			{#if isOpen}
				<FolderOpen size={14} class="text-sidebar-accent-foreground" />
			{:else}
				<Folder size={14} class="text-sidebar-accent-foreground" />
			{/if}
			<span>{projectName}</span>
			<span class="ml-auto text-muted-foreground">({sessions.length})</span>
		</button>
	</Sidebar.GroupLabel>
	<Collapsible.Root open={isOpen}>
		<Sidebar.Menu class="gap-1">
			{#each sessions as session (session.id)}
				<SessionItem
					{session}
					active={session.id === selectedSessionId}
					onselect={onselect}
				/>
			{/each}
		</Sidebar.Menu>
	</Collapsible.Root>
</Sidebar.Group>
