# Fix Velor GUI Theming Issues

## Context

The Velor GUI application has a critical CSS variable mismatch causing visual inconsistencies and broken theming. The issue was identified from a screenshot showing mixed light/dark elements and poor color contrast.

### Root Cause

**CSS Variable Naming Mismatch:**
- `app.css` defines shadcn-svelte standard variables (`--background`, `--foreground`, `--primary`, `--card`, etc.)
- Components are using semantic variables that **don't exist** (`--color-bg-primary`, `--color-text-primary`, `--color-accent-primary`, etc.)
- This causes browsers to use fallback/empty values, resulting in broken styling

### Affected Files (19 components)
- Chat: `ChatInput.svelte`, `ChatStream.svelte`, `ChatMessage.svelte`
- Layout: `Sidebar.svelte`, `Header.svelte`, `MainLayout.svelte`
- Settings: `NotificationSettings.svelte`, `PromptEditor.svelte`, `ConfigEditor.svelte`
- Automations: `AutomationRuns.svelte`, `AutomationEditor.svelte`, `AutomationList.svelte`, `AutomationCard.svelte`
- Execution: `ExecutionControls.svelte`, `ExecutionStatus.svelte`
- Pages: `+page.svelte`, `executions/+page.svelte`, `settings/+page.svelte`

## Implementation Plan

### Step 1: Add Semantic CSS Variables to `app.css`

Add a new section in `app.css` that maps semantic variable names to the existing shadcn-svelte theme variables. This maintains the existing component code while providing proper color values.

**Location:** `/Users/liam/git/velor/apps/velor/src/routes/app.css`

**Add after line 39 (before `.dark` class):**

```css
  /* Semantic color variables for components */
  --color-bg-primary: var(--background);
  --color-bg-secondary: var(--card);
  --color-bg-tertiary: var(--muted);

  --color-text-primary: var(--foreground);
  --color-text-secondary: var(--muted-foreground);
  --color-text-tertiary: var(--muted-foreground);
  --color-text-muted: var(--muted-foreground);

  --color-accent-primary: var(--primary);
  --color-accent-hover: var(--primary-foreground);
  --color-accent-active: var(--accent-foreground);
  --color-accent-light: oklch(from var(--primary) l c calc(h + 15)); /* Lighter variant */

  --color-border-hover: var(--input);

  --color-success: oklch(0.6 0.2 142); /* Green for success states */
```

**Also add to `.dark` class (after line 73):**

```css
  /* Semantic color variables for components */
  --color-bg-primary: var(--background);
  --color-bg-secondary: var(--card);
  --color-bg-tertiary: var(--muted);

  --color-text-primary: var(--foreground);
  --color-text-secondary: var(--muted-foreground);
  --color-text-tertiary: var(--muted-foreground);
  --color-text-muted: var(--muted-foreground);

  --color-accent-primary: var(--primary);
  --color-accent-hover: var(--primary-foreground);
  --color-accent-active: var(--accent-foreground);
  --color-accent-light: oklch(from var(--primary) calc(l + 0.1) c calc(h + 15)); /* Lighter variant for dark mode */

  --color-border-hover: var(--input);

  --color-success: oklch(0.65 0.2 142); /* Adjusted for dark mode */
```

### Step 2: Update Tailwind Theme Mapping (Optional Enhancement)

Add mappings to the `@theme inline` section so Tailwind recognizes these semantic variables:

**Location:** `/Users/liam/git/velor/apps/velor/src/routes/app.css` (in `@theme inline` block, after line 111)

```css
  --color-bg-primary: var(--color-bg-primary);
  --color-bg-secondary: var(--color-bg-secondary);
  --color-bg-tertiary: var(--color-bg-tertiary);
  --color-text-primary: var(--color-text-primary);
  --color-text-secondary: var(--color-text-secondary);
  --color-text-tertiary: var(--color-text-tertiary);
  --color-text-muted: var(--color-text-muted);
  --color-accent-primary: var(--color-accent-primary);
  --color-accent-hover: var(--color-accent-hover);
  --color-accent-active: var(--color-accent-active);
  --color-accent-light: var(--color-accent-light);
  --color-border-hover: var(--color-border-hover);
  --color-success: var(--color-success);
```

### Step 3: Fix Hardcoded Dark-Mode State Colors

Some components use hardcoded Tailwind colors for state indicators that only work in dark mode (e.g., `bg-green-900/30`). These should be replaced with theme-aware variables.

**Files to update:**
- `ExecutionStatus.svelte` - lines 207-228 (state indicator colors)
- `ExecutionControls.svelte` - lines 194, 206 (button colors)

**Replace hardcoded colors with:**
- Use existing semantic variables where appropriate
- Consider using shadcn-svelte's `destructive` variable for error states
- Define new state-specific variables if needed (e.g., `--color-state-pending`, `--color-state-running`)

### Step 4: Verification

1. Build the application: `cd apps/velor && npm run build`
2. Run the Tauri app: `npm run tauri dev`
3. Verify visually:
   - All elements have consistent theming
   - Light mode and dark mode both work correctly
   - Text contrast is readable
   - State indicators (pending, running, completed, failed) are visually distinct
4. Test all component types: chat, settings, automations, executions

## Key Files

- `/Users/liam/git/velor/apps/velor/src/routes/app.css` - Main CSS file (primary changes)
- `/Users/liam/git/velor/apps/velor/src/lib/components/execution/ExecutionStatus.svelte` - For state color fixes
- `/Users/liam/git/velor/apps/velor/src/lib/components/execution/ExecutionControls.svelte` - For button color fixes

## Notes

- The semantic variable naming (`--color-bg-primary`, etc.) is actually more intuitive than shadcn-svelte's naming
- This fix maintains backward compatibility - no component code changes required for basic functionality
- The OKLCH color space is used throughout for better perceptual uniformity
- Consider adding a proper dark mode toggle in a future enhancement (currently `.dark` class styles exist but no toggle mechanism)
