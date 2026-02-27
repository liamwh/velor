# Velor GUI Sidebar Redesign - Progress

## Plan: `spicy-nibbling-mountain.md`

### Phase 1: Backend Changes ✅ COMPLETED

#### 1.1 Database Schema Migration ✅
- [x] Add `name` TEXT column to sessions table
- [x] Add `pinned` INTEGER column to sessions table
- [x] Add `project_path` TEXT column to sessions table
- [x] Create `projects` table with path, display_name, hidden, sort_order columns
- [x] Implement migration logic for existing databases

**Files modified:**
- `apps/velor/src-tauri/src/session_store.rs`
  - Added `Project` and `ProjectRow` structs
  - Added `discover_git_root_from_path` helper
  - Updated `init_schema` to include projects table and new indexes
  - Added `migrate_sessions_table` method
  - Updated `insert_session` and `update_session` to include new fields
  - Updated `row_to_record` to handle new fields

#### 1.2 Add Session Commands ✅
- [x] `rename_session(id, name)` - Update session name
- [x] `toggle_session_pin(id)` - Pin/unpin session

#### 1.3 Add Project Commands ✅
- [x] `list_projects()` - Get unique project paths from sessions with metadata
- [x] `hide_project(path)` - Mark project as hidden
- [x] `show_project(path)` - Unhide a project
- [x] `rename_project(path, display_name)` - Update project display name
- [x] `reorder_projects(paths)` - Update sort order

**Files modified:**
- `apps/velor/src-tauri/src/commands.rs`
- `apps/velor/src-tauri/src/lib.rs` (registered new commands)

#### 1.4 Update ExecutionConfig ✅
- [x] Added `name`, `pinned`, `project_path` fields to `ExecutionRecord` struct

**Files modified:**
- `crates/velor-core/src/execution.rs`

### Phase 2: Frontend Changes ✅ COMPLETED

#### 2.1 Update Types ✅
- [x] Added `name`, `pinned`, `project_path` fields to `ExecutionRecord` interface
- [x] Added `Project` interface

**Files modified:**
- `apps/velor/src/lib/types/execution.ts`

#### 2.2 Create Projects Store ✅
- [x] `load()` - Load all projects from backend
- [x] `hide(path)` - Hide a project
- [x] `show(path)` - Unhide a project
- [x] `rename(path, displayName)` - Rename project
- [x] `reorder(paths)` - Change sort order

**Files created:**
- `apps/velor/src/lib/stores/projects.ts`

#### 2.3 Update Sessions Store ✅
- [x] `rename(id, name)` - Rename session
- [x] `togglePin(id)` - Pin/unpin session
- [x] `groupByProject()` - Group sessions by project path

**Files modified:**
- `apps/velor/src/lib/stores/sessions.ts`

#### 2.4 Rewrite Sidebar Component ✅
- [x] Sidebar toggle button (collapses to icons only)
- [x] "New session" button (opens new session view)
- [x] "Automations" button (navigates to automations)
- [x] "Projects" section header
- [x] Collapsible project groups
- [x] Session list items under each project
- [x] Daemon status toggle in footer

**Files modified:**
- `apps/velor/src/lib/components/layout/Sidebar.svelte` - Complete rewrite using shadcn-svelte sidebar components

#### 2.5 Create New Components ✅
- [x] `SessionItem.svelte` - Session row with name, preview, actions (rename, pin/unpin, delete)
- [x] `ProjectGroup.svelte` - Collapsible project container with session list
- [x] `SidebarHeader.svelte` - Action buttons section (New session, Automations)

**Files created:**
- `apps/velor/src/lib/components/sidebar/SessionItem.svelte`
- `apps/velor/src/lib/components/sidebar/ProjectGroup.svelte`
- `apps/velor/src/lib/components/sidebar/SidebarHeader.svelte`

#### 2.6 Update MainLayout ✅
- [x] Add SidebarProvider wrapper for sidebar state management
- [x] Use SidebarInset for main content area
- [x] Add SidebarTrigger button to Header for sidebar toggle

**Files modified:**
- `apps/velor/src/lib/components/layout/MainLayout.svelte`
- `apps/velor/src/lib/components/layout/Header.svelte`

### Phase 3: UI Cleanup ✅ COMPLETED

#### 3.1 Remove Unused Components/Pages ✅
- [x] Remove placeholder pages that are no longer needed (sidebar-07/)
- [x] Clean up old navigation items (app-sidebar.svelte, nav-*.svelte, team-switcher.svelte)

#### 3.2 Update Styling ✅
- [x] Ensure dark theme consistency with existing app (verified in app.css)
- [x] Fix main page to show selected session or welcome state

**Files modified:**
- `apps/velor/src/routes/+page.svelte` - Complete rewrite to show selected session or welcome state
- `apps/velor/src/lib/components/layout/Sidebar.svelte` - Fixed reactivity issue (update SvelteMap in place)
- `apps/velor/src/lib/components/sidebar/ProjectGroup.svelte` - Fixed collapse state with svelte-ignore

**Files removed:**
- `apps/velor/src/lib/components/app-sidebar.svelte` - Old shadcn demo sidebar
- `apps/velor/src/lib/components/nav-main.svelte` - Demo navigation component
- `apps/velor/src/lib/components/nav-projects.svelte` - Demo projects component
- `apps/velor/src/lib/components/nav-user.svelte` - Demo user component
- `apps/velor/src/lib/components/team-switcher.svelte` - Demo team switcher component
- `apps/velor/src/routes/sidebar-07/` - Demo sidebar page

---

## Summary

**Completed:**
- Phase 1: Backend Changes (100%)
- Phase 2: Frontend Changes (100%)
  - 2.1: Update Types (100%)
  - 2.2: Create Projects Store (100%)
  - 2.3: Update Sessions Store (100%)
  - 2.4: Rewrite Sidebar Component (100%)
  - 2.5: Create New Components (100%)
  - 2.6: Update MainLayout (100%)
- Phase 3: UI Cleanup (100%)
  - 3.1: Remove Unused Components/Pages (100%)
  - 3.2: Update Styling (100%)

**All phases complete!** ✅

## Files Modified/Created

### Backend (Rust)
- `crates/velor-core/src/execution.rs` - Added new fields to ExecutionRecord
- `apps/velor/src-tauri/src/session_store.rs` - Schema migration, new fields, project methods
- `apps/velor/src-tauri/src/commands.rs` - New session and project commands
- `apps/velor/src-tauri/src/lib.rs` - Registered new commands

### Frontend (Svelte/TypeScript)
- `apps/velor/src/lib/types/execution.ts` - Updated types
- `apps/velor/src/lib/services/tauri.ts` - New command wrappers
- `apps/velor/src/lib/stores/projects.ts` - New projects store
- `apps/velor/src/lib/stores/sessions.ts` - New methods
- `apps/velor/src/lib/stores/index.ts` - Export projects store
- `apps/velor/src/lib/components/layout/Sidebar.svelte` - Complete rewrite
- `apps/velor/src/lib/components/layout/MainLayout.svelte` - Updated with SidebarProvider
- `apps/velor/src/lib/components/layout/Header.svelte` - Added SidebarTrigger
- `apps/velor/src/lib/components/sidebar/SidebarHeader.svelte` - New component
- `apps/velor/src/lib/components/sidebar/SessionItem.svelte` - New component
- `apps/velor/src/lib/components/sidebar/ProjectGroup.svelte` - New component
- `apps/velor/src/routes/+page.svelte` - Updated to show selected session

### Commits
- `1eec74b` - feat(gui): implement project-based sidebar with collapsible groups
- `6bd0eb2` - feat(gui): complete Phase 3 UI cleanup for sidebar redesign
