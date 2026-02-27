# Velor GUI Sidebar Redesign Plan

## Context

The Velor GUI currently has a placeholder sidebar that doesn't reflect the application's purpose. This plan redesigns the sidebar to show projects (git repos) with their sessions, matching the target UI from the screenshot.

**Current State:**
- Simple sidebar with navigation items (Home, Executions, Automations, Settings)
- Sessions exist globally without project association
- Session metrics include `total_tokens` but not separate input/output tokens
- No "project" concept in the data model

**Target State:**
- Collapsible sidebar with toggle button
- Top action buttons: "New session" and "Automations"
- Projects section with collapsible project groups (git repos)
- Sessions under each project with conversation preview, timestamps, and actions
- Sessions are pinnable, renameable, and deletable

**Design Decisions (Confirmed):**
1. **Projects**: Hybrid approach - auto-discover from session's `cwd`/git root, but allow removing and reorganizing
2. **Session Display**: Show conversation context (what was prompted and model's response)
3. **Session Names**: Add optional `name` field for user-editable session titles

---

## Implementation Plan

### Phase 1: Backend Changes

#### 1.1 Database Schema Migration
Add new columns to the sessions table:
- `name` TEXT - User-editable session name (nullable, defaults to prompt_name)
- `pinned` INTEGER - Boolean for pinned status (default 0)
- `project_path` TEXT - Git root path at time of session creation

Add new table for project preferences:
- `projects` table with path, display_name, hidden, sort_order columns

**Files to modify:**
- `apps/velor/src-tauri/src/session_store.rs` - Schema migration, new fields
- `apps/velor/src-tauri/src/commands.rs` - New commands

#### 1.2 Add Session Commands
- `rename_session(id, name)` - Update session name
- `toggle_session_pin(id)` - Pin/unpin session
- `update_session_name(id, name)` - Rename session

#### 1.3 Add Project Commands
- `list_projects()` - Get unique project paths from sessions with metadata
- `hide_project(path)` - Mark project as hidden
- `show_project(path)` - Unhide a project
- `reorder_projects(paths)` - Update sort order

#### 1.4 Update ExecutionConfig
- Ensure `cwd` is captured when session starts (already exists)
- Extract git root from cwd and store as `project_path`

### Phase 2: Frontend Changes

#### 2.1 Update Types
Add new fields to TypeScript types:
```typescript
// In execution.ts
export interface ExecutionRecord {
  // ... existing fields
  name?: string;      // User-editable name
  pinned: boolean;    // Pin status
  project_path: string; // Git root path
}

export interface Project {
  path: string;
  display_name: string;
  hidden: boolean;
  sort_order: number;
  session_count: number;
}
```

**Files to modify:**
- `apps/velor/src/lib/types/execution.ts`

#### 2.2 Create Projects Store
New store for project management:
- `load()` - Load all projects from backend
- `hide(path)` - Hide a project
- `show(path)` - Unhide a project
- `reorder(paths)` - Change sort order

**New files:**
- `apps/velor/src/lib/stores/projects.ts`

#### 2.3 Update Sessions Store
Add new methods:
- `rename(id, name)` - Rename session
- `togglePin(id)` - Pin/unpin session
- `groupByProject()` - Group sessions by project path

**Files to modify:**
- `apps/velor/src/lib/stores/sessions.ts`

#### 2.4 Rewrite Sidebar Component
Complete rewrite using shadcn-svelte sidebar components:
- Sidebar toggle button (collapses to icons only)
- "New session" button (opens new session view)
- "Automations" button (navigates to automations)
- "Projects" section header
- Collapsible project groups (using Collapsible component)
- Session list items under each project:
  - Session name (editable inline)
  - Pin icon (if pinned)
  - Conversation preview snippet
  - Timestamp (relative)
  - Dropdown menu with: Rename, Pin/Unpin, Delete

**Files to modify:**
- `apps/velor/src/lib/components/layout/Sidebar.svelte` - Complete rewrite

#### 2.5 Create New Components
- `SessionItem.svelte` - Session row with name, preview, actions
- `ProjectGroup.svelte` - Collapsible project container
- `SidebarHeader.svelte` - Action buttons section

**New files:**
- `apps/velor/src/lib/components/sidebar/SessionItem.svelte`
- `apps/velor/src/lib/components/sidebar/ProjectGroup.svelte`
- `apps/velor/src/lib/components/sidebar/SidebarHeader.svelte`

#### 2.6 Update MainLayout
Add sidebar toggle state and collapsed mode support:
- Track sidebar collapsed state
- Pass toggle function to Header
- Adjust main content width when sidebar collapses

**Files to modify:**
- `apps/velor/src/lib/components/layout/MainLayout.svelte`
- `apps/velor/src/lib/components/layout/Header.svelte`

### Phase 3: UI Cleanup

#### 3.1 Remove Unused Components/Pages
- Remove placeholder pages that are no longer needed
- Clean up old navigation items
- Remove unused nav-projects.svelte (replace with new ProjectGroup)

**Files to remove/modify:**
- `apps/velor/src/lib/components/nav-projects.svelte` - Remove or repurpose
- `apps/velor/src/routes/+page.svelte` - Update if needed

#### 3.2 Update Styling
- Ensure dark theme consistency with existing app
- Match spacing and typography from target screenshot
- Add smooth collapse/expand animations

**Files to modify:**
- `apps/velor/src/routes/app.css` - Any global style updates

---

## Verification

### Functional Testing
1. **Session Management:**
   - Create new session → appears under correct project
   - Pin session → shows pin icon, stays at top of project list
   - Rename session → inline edit works, persists on refresh
   - Delete session → removes from list with confirmation

2. **Project Management:**
   - Sessions auto-group by project (git root)
   - Hide project → project no longer shows in sidebar
   - Show hidden project → project reappears
   - Collapse/expand project groups → state persists

3. **Sidebar Behavior:**
   - Toggle sidebar → collapses to icon-only mode
   - Responsive behavior on resize
   - Navigation works correctly

### Visual Verification
- Compare against target screenshot
- Verify dark theme consistency
- Check hover states and transitions
- Verify icon usage (lucide-svelte)

---

## File Summary

### Backend (Rust) - Modified
- `apps/velor/src-tauri/src/session_store.rs` - Schema migration, new fields
- `apps/velor/src-tauri/src/commands.rs` - New commands for rename, pin, projects

### Frontend (Svelte) - Modified
- `apps/velor/src/lib/components/layout/Sidebar.svelte` - Complete rewrite
- `apps/velor/src/lib/components/layout/MainLayout.svelte` - Toggle support
- `apps/velor/src/lib/components/layout/Header.svelte` - Toggle button
- `apps/velor/src/lib/stores/sessions.ts` - New methods
- `apps/velor/src/lib/types/execution.ts` - Updated types

### Frontend (Svelte) - New Files
- `apps/velor/src/lib/stores/projects.ts` - Projects store
- `apps/velor/src/lib/components/sidebar/SessionItem.svelte`
- `apps/velor/src/lib/components/sidebar/ProjectGroup.svelte`
- `apps/velor/src/lib/components/sidebar/SidebarHeader.svelte`

### Frontend (Svelte) - Removed
- `apps/velor/src/lib/components/nav-projects.svelte` (placeholder, not needed)

---

## Implementation Order

1. **Backend first** - Add database fields and commands
2. **Types and stores** - Update TypeScript types and create stores
3. **Sidebar components** - Build new sidebar piece by piece
4. **Integration** - Connect sidebar to stores and backend
5. **Cleanup** - Remove old components and polish styling
