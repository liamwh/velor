<script lang="ts">
	import { PlanGenerator } from '$lib/components/plan';
	import { FileText } from 'lucide-svelte';
</script>

<div class="h-full flex flex-col p-6 max-w-7xl mx-auto">
	<header class="mb-6">
		<div class="flex items-center gap-4">
			<FileText size={28} class="text-primary" />
			<div>
				<h1 class="text-2xl font-bold text-foreground">Plans</h1>
				<p class="text-sm text-muted-foreground mt-0.5">
					Generate AI-powered implementation plans from spec files
				</p>
			</div>
		</div>
	</header>

	<div class="flex-1 min-h-0">
		<PlanGenerator />
	</div>
</div>
