<script lang="ts">
    import Sidebar from "./Sidebar.svelte";
    import Header from "./Header.svelte";
    import type { Snippet } from "svelte";

    interface Props {
        class?: string;
        children: Snippet;
    }

    let { class: className = "", children }: Props = $props();
</script>

<div class="layout {className}">
    <Sidebar />
    <div class="main-content">
        <Header />
        <main class="content">
            {@render children()}
        </main>
    </div>
</div>

<style>
    .layout {
        @apply flex h-screen w-full overflow-hidden bg-bg-primary;
    }

    .main-content {
        @apply flex flex-col flex-1 overflow-hidden;
    }

    .content {
        @apply flex-1 overflow-auto p-6;
    }
</style>
