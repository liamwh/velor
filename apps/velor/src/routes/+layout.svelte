<script lang="ts">
    import "./app.css";
    import { onMount } from "svelte";
    import { configStore } from "$lib/stores";
    import MainLayout from "$lib/components/layout/MainLayout.svelte";
    import { ModeWatcher } from "mode-watcher";
    let { children } = $props();

    // Load configuration on app mount
    onMount(() => {
        configStore.load();
    });
</script>

<svelte:head>
    <meta charset="utf-8" />
    <link rel="icon" href="%sveltekit.assets%/favicon.png" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>Velor - AI Agent CLI</title>
</svelte:head>

<ModeWatcher />
<MainLayout>
    {@render children?.()}
</MainLayout>
