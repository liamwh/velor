<script lang="ts">
	import * as Sidebar from "$lib/components/ui/sidebar/index.js";
	import { Inset as SidebarInset } from "$lib/components/ui/sidebar/index.js";
	import SidebarComponent from "./Sidebar.svelte";
	import Header from "./Header.svelte";
	import type { Snippet } from "svelte";

	interface Props {
		class?: string;
		children: Snippet;
	}

	let { class: className = "", children }: Props = $props();
</script>

<Sidebar.Provider>
	<SidebarComponent />
	<SidebarInset class="flex-1 flex flex-col overflow-hidden {className}">
		<Header />
		<main class="content flex-1 overflow-auto p-6">
			{@render children()}
		</main>
	</SidebarInset>
</Sidebar.Provider>
