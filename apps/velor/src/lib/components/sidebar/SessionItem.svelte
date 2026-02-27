<script lang="ts">
	import * as DropdownMenu from "$lib/components/ui/dropdown-menu/index.js";
	import * as Sidebar from "$lib/components/ui/sidebar/index.js";
	import { sessionsStore } from "$lib/stores";
	import type { ExecutionRecord } from "$lib/types";
	import { Pin, PinOff, Pencil, Trash2, MoreHorizontal } from "lucide-svelte";
	import { cn } from "$lib/utils";

	interface Props {
		/** The session record to display */
		session: ExecutionRecord;
		/** Whether this is the active/selected session */
		active?: boolean;
		/** Optional click handler for selecting the session */
		onselect?: (session: ExecutionRecord) => void;
	}

	let { session, active = false, onselect }: Props = $props();

	/**
	 * Get the display name for the session
	 */
	let displayName = $derived(() => {
		return session.name || session.prompt_name || "Untitled Session";
	});

	/**
	 * Get a preview snippet from the session events
	 */
	let previewText = $derived(() => {
		// Find the first output chunk event to use as preview
		const outputEvent = session.events.find(
			(e) => e.event_type === "output_chunk" && e.output
		);
		if (outputEvent?.output) {
			return outputEvent.output.slice(0, 60) + (outputEvent.output.length > 60 ? "..." : "");
		}
		// Fallback to prompt name if no output
		return session.prompt_name;
	});

	/**
	 * Get relative time string for the session
	 */
	let relativeTime = $derived(() => {
		const date = new Date(session.started_at);
		const now = new Date();
		const diffMs = now.getTime() - date.getTime();
		const diffMins = Math.floor(diffMs / 60000);
		const diffHours = Math.floor(diffMins / 60);
		const diffDays = Math.floor(diffHours / 24);

		if (diffMins < 1) return "Just now";
		if (diffMins < 60) return `${diffMins}m ago`;
		if (diffHours < 24) return `${diffHours}h ago`;
		return `${diffDays}d ago`;
	});

	/**
	 * Handle session selection
	 */
	function handleSelect(): void {
		onselect?.(session);
	}

	/**
	 * Toggle pin status
	 */
	async function handleTogglePin(): Promise<void> {
		try {
			await sessionsStore.togglePin(session.id);
		} catch (e) {
			console.error("Failed to toggle pin:", e);
		}
	}

	/**
	 * Delete the session
	 */
	async function handleDelete(): Promise<void> {
		const currentName = session.name || session.prompt_name || "Untitled Session";
		if (!confirm(`Delete session "${currentName}"?`)) {
			return;
		}
		try {
			await sessionsStore.delete(session.id);
		} catch (e) {
			console.error("Failed to delete session:", e);
		}
	}

	/**
	 * Handle session rename (prompt for new name)
	 */
	async function handleRename(): Promise<void> {
		const currentName = session.name || session.prompt_name || "Untitled Session";
		const newName = prompt("Enter new name:", currentName);
		if (newName === null || newName.trim() === "") {
			return;
		}
		try {
			await sessionsStore.rename(session.id, newName.trim());
		} catch (e) {
			console.error("Failed to rename session:", e);
		}
	}
</script>

<Sidebar.MenuItem>
	<Sidebar.MenuButton
		class={cn(
			"hover:bg-sidebar-accent hover:text-sidebar-accent-foreground",
			active && "bg-sidebar-accent text-sidebar-accent-foreground"
		)}
		onclick={handleSelect}
	>
			{#snippet child({ props })}
			<div {...props} class="flex items-center gap-2 w-full">
				{#if session.pinned}
					<Pin size={14} class="text-sidebar-primary shrink-0" />
				{:else}
					<div class="w-[14px]"></div>
				{/if}
				<div class="flex flex-col gap-0.5 flex-1 min-w-0">
					<span class="text-sm font-medium truncate">{displayName}</span>
					<span class="text-xs text-muted-foreground truncate">{previewText}</span>
				</div>
				<span class="text-xs text-muted-foreground shrink-0">{relativeTime}</span>
			</div>
		{/snippet}
	</Sidebar.MenuButton>
	<DropdownMenu.Root>
		<DropdownMenu.Trigger>
			{#snippet child({ props })}
				<Sidebar.MenuAction showOnHover {...props}>
					<MoreHorizontal size={14} />
					<span class="sr-only">More actions</span>
				</Sidebar.MenuAction>
			{/snippet}
		</DropdownMenu.Trigger>
		<DropdownMenu.Content class="w-48 rounded-lg" side="right" align="start">
			<DropdownMenu.Item onclick={handleRename}>
				<Pencil size={14} class="text-muted-foreground" />
				<span>Rename</span>
			</DropdownMenu.Item>
			<DropdownMenu.Item onclick={handleTogglePin}>
				{#if session.pinned}
					<PinOff size={14} class="text-muted-foreground" />
					<span>Unpin</span>
				{:else}
					<Pin size={14} class="text-muted-foreground" />
					<span>Pin</span>
				{/if}
			</DropdownMenu.Item>
			<DropdownMenu.Separator />
			<DropdownMenu.Item onclick={handleDelete} class="text-destructive">
				<Trash2 size={14} class="text-destructive" />
				<span>Delete</span>
			</DropdownMenu.Item>
		</DropdownMenu.Content>
	</DropdownMenu.Root>
</Sidebar.MenuItem>
