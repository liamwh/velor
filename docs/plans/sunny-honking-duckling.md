# Plan: Implement Shadcn-Svelte Sidebar for Velor GUI

## Context

The Velor GUI currently has a custom sidebar implementation in `layout/Sidebar.svelte`, but the `MainLayout.svelte` already imports shadcn-svelte sidebar components. The actual sidebar UI components from shadcn-svelte are not installed yet. This plan migrates to the official shadcn-svelte sidebar system for better composability, collapsibility (icon mode), and consistency with the component library.

## Current State

- **CSS Variables**: Sidebar CSS variables are already in `app.css` (lines 32-39, 112-119)
- **Sample components exist**: `app-sidebar.svelte`, `nav-main.svelte`, etc. reference sidebar components
- **Missing**: Actual `ui/sidebar/`, `ui/breadcrumb/`, and `ui/collapsible/` components not installed
- **Old sidebar**: `layout/Sidebar.svelte` has the Velor-specific navigation logic

## Implementation Steps

### 1. Install Shadcn-Svelte Components

Run the CLI to install the required components:

```bash
cd apps/velor
npx shadcn-svelte@latest add sidebar
npx shadcn-svelte@latest add breadcrumb
npx shadcn-svelte@latest add collapsible
```

This will create:
- `src/lib/components/ui/sidebar/` - All sidebar subcomponents
- `src/lib/components/ui/breadcrumb/` - Breadcrumb components
- `src/lib/components/ui/collapsible/` - Collapsible components

### 2. Update `app-sidebar.svelte`

Replace the sample data with Velor-specific navigation:

**Navigation items** (from old Sidebar.svelte):
- Home (`/`) - Home icon
- Executions (`/executions`) - History icon
- Automations (`/automations`) - Calendar icon
- Settings (`/settings`) - Settings icon

**Quick Actions section**:
- New Prompt - Plus icon
- Run Now - Play icon

**Footer section**:
- Daemon toggle (Start/Stop) with status indicator
- Uses `automationsStore` and `EVENT_SERVICE` for daemon state

**Header section**:
- Velor logo/branding

### 3. Update `MainLayout.svelte`

The current file has placeholder content. Update to:
- Render children properly via `{@render children?.()}`
- Keep breadcrumb header with `Sidebar.Trigger`
- Remove placeholder grid content

```svelte
<Sidebar.Provider>
  <AppSidebar />
  <Sidebar.Inset>
    <header class="...">
      <Sidebar.Trigger />
      <Separator />
      <!-- Optional: Breadcrumb based on current route -->
    </header>
    <main class="flex flex-1 flex-col gap-4 p-4">
      {@render children?.()}
    </main>
  </Sidebar.Inset>
</Sidebar.Provider>
```

### 4. Clean Up

- Delete `src/lib/components/layout/Sidebar.svelte` (replaced by app-sidebar.svelte)
- Delete unused sample nav components if not needed:
  - `nav-main.svelte` (can be replaced with simpler direct menu)
  - `nav-projects.svelte` (not needed)
  - `nav-user.svelte` (not needed)
  - `team-switcher.svelte` (not needed, delete if exists)

## Critical Files

| File | Action |
|------|--------|
| `apps/velor/src/lib/components/ui/sidebar/` | Install via CLI |
| `apps/velor/src/lib/components/ui/breadcrumb/` | Install via CLI |
| `apps/velor/src/lib/components/ui/collapsible/` | Install via CLI |
| `apps/velor/src/lib/components/app-sidebar.svelte` | Rewrite with Velor nav |
| `apps/velor/src/lib/components/layout/MainLayout.svelte` | Update to render children |
| `apps/velor/src/lib/components/layout/Sidebar.svelte` | Delete |

## Verification

1. Run `just check` to verify no type errors
2. Run `pnpm tauri dev` and verify:
   - Sidebar renders with Velor navigation
   - Navigation items navigate to correct routes
   - Active state highlights current page
   - Sidebar collapses to icons (Cmd/Ctrl+B)
   - Daemon toggle works and shows status
   - Dark mode works correctly
   - Mobile responsive behavior works
