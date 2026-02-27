<script lang="ts">
	import { onMount } from "svelte";
	import { SvelteMap } from "svelte/reactivity";
	import { goto } from "$app/navigation";
	import * as Sidebar from "$lib/components/ui/sidebar/index.js";
	import { SidebarRail } from "$lib/components/ui/sidebar/index.js";
	import SidebarHeader from "$lib/components/sidebar/SidebarHeader.svelte";
	import ProjectGroup from "$lib/components/sidebar/ProjectGroup.svelte";
	import { sessionsStore, projects, projectsStore, automationsStore, daemonRunning } from "$lib/stores";
	import { EVENT_SERVICE } from "$lib/services/events";
	import type { ExecutionRecord } from "$lib/types";
	import { Settings, Power, PowerOff } from "lucide-svelte";

	/** Track grouped sessions by project path */
	let groupedSessions = new SvelteMap<string, ExecutionRecord[]>();

	/** Track the currently selected session */
	let selectedSessionId = $state<string | null>(null);

	/** Track loading state */
	let loading = $state(true);

	/**
	 * Group sessions by their project path
	 */
	function groupSessionsByProject(): SvelteMap<string, ExecutionRecord[]> {
		const grouped = new SvelteMap<string, ExecutionRecord[]>();
		const sessionsByProject = sessionsStore.groupByProject();

		for (const [projectPath, sessionList] of sessionsByProject) {
			grouped.set(projectPath, sessionList);
		}

		return grouped;
	}

	/**
	 * Load sessions and projects on mount
	 */
	onMount(() => {
		const loadData = async () => {
			try {
				loading = true;
				await Promise.all([sessionsStore.load(), projectsStore.load()]);
				groupedSessions = groupSessionsByProject();
			} catch (e) {
				console.error("Failed to load sidebar data:", e);
			} finally {
				loading = false;
			}
		};

		const setupEventListeners = async () => {
			// Listen for daemon events from backend
			await EVENT_SERVICE.onDaemonStarted(({ running }) => {
				automationsStore.setDaemonRunning(running);
			});
			await EVENT_SERVICE.onDaemonStopped(({ running }) => {
				automationsStore.setDaemonRunning(running);
			});
		};

		loadData();
		setupEventListeners();

		// Subscribe to sessions changes
		const unsubscribe = sessionsStore.subscribe((state) => {
			groupedSessions = groupSessionsByProject();
			if (state.selectedSession) {
				selectedSessionId = state.selectedSession.id;
			}
		});

		return unsubscribe;
	});

	/**
	 * Handle session selection
	 */
	function handleSessionSelect(session: ExecutionRecord): void {
		selectedSessionId = session.id;
		sessionsStore.select(session);
	}

	/**
	 * Navigate to settings
	 */
	function goToSettings(): void {
		goto("/settings");
	}

	/**
	 * Toggle daemon status
	 */
	async function toggleDaemon(): Promise<void> {
		if ($daemonRunning) {
			await automationsStore.stopDaemon();
		} else {
			await automationsStore.startDaemon();
		}
	}

	/** Get sorted projects with their sessions */
	let projectsWithSessions = $derived(
		$projects
			.filter((p) => !p.hidden && groupedSessions.has(p.path))
			.sort((a, b) => a.sort_order - b.sort_order)
	);
</script>

<Sidebar.Root collapsible="icon">
	<Sidebar.Content class="bg-sidebar">
		<SidebarHeader />

		<Sidebar.Content class="flex-1 overflow-y-auto">
			{#if loading}
				<div class="p-4 text-sm text-muted-foreground">Loading...</div>
			{:else if projectsWithSessions.length === 0}
				<div class="p-4 text-sm text-muted-foreground">
					No sessions yet. Create a new session to get started.
				</div>
			{:else}
				{#each projectsWithSessions as project (project.path)}
					<ProjectGroup
						{project}
						sessions={groupedSessions.get(project.path) || []}
						{selectedSessionId}
						onselect={handleSessionSelect}
					/>
				{/each}
			{/if}
		</Sidebar.Content>

		<Sidebar.Footer>
			<Sidebar.Menu>
				<Sidebar.MenuItem>
					<Sidebar.MenuButton
						onclick={goToSettings}
						class="data-[active=true]:bg-sidebar-accent data-[active=true]:text-sidebar-accent-foreground"
					>
						{#snippet child({ props })}
							<button {...props} class="flex items-center gap-2 w-full">
								<Settings size={16} />
								<span>Settings</span>
							</button>
						{/snippet}
					</Sidebar.MenuButton>
				</Sidebar.MenuItem>
				<Sidebar.MenuItem>
					<Sidebar.MenuButton
						onclick={toggleDaemon}
						class="data-[active=true]:bg-sidebar-accent data-[active=true]:text-sidebar-accent-foreground"
					>
						{#snippet child({ props })}
							<button {...props} class="flex items-center gap-2 w-full">
								{#if $daemonRunning}
									<PowerOff size={16} class="text-green-500" />
									<span>Stop Daemon</span>
								{:else}
									<Power size={16} />
									<span>Start Daemon</span>
								{/if}
							</button>
						{/snippet}
					</Sidebar.MenuButton>
				</Sidebar.MenuItem>
			</Sidebar.Menu>
		</Sidebar.Footer>
	</Sidebar.Content>
	<SidebarRail />
</Sidebar.Root>
