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

#### 2.4 Rewrite Sidebar Component ⏳ PENDING
- [ ] Sidebar toggle button (collapses to icons only)
- [ ] "New session" button (opens new session view)
- [ ] "Automations" button (navigates to automations)
- [ ] "Projects" section header
- [ ] Collapsible project groups
- [ ] Session list items under each project

#### 2.5 Create New Components ⏳ PENDING
- [ ] `SessionItem.svelte` - Session row with name, preview, actions
- [ ] `ProjectGroup.svelte` - Collapsible project container
- [ ] `SidebarHeader.svelte` - Action buttons section

#### 2.6 Update MainLayout ⏳ PENDING
- [ ] Add sidebar toggle state
- [ ] Pass toggle function to Header
- [ ] Adjust main content width when sidebar collapses

### Phase 3: UI Cleanup ⏳ PENDING

#### 3.1 Remove Unused Components/Pages ⏳ PENDING
- [ ] Remove placeholder pages that are no longer needed
- [ ] Clean up old navigation items

#### 3.2 Update Styling ⏳ PENDING
- [ ] Ensure dark theme consistency with existing app
- [ ] Match spacing and typography from target screenshot
- [ ] Add smooth collapse/expand animations

---

## Summary

**Completed:**
- Phase 1: Backend Changes (100%)
- Phase 2.1: Update Types (100%)
- Phase 2.2: Create Projects Store (100%)
- Phase 2.3: Update Sessions Store (100%)

**Remaining:**
- Phase 2.4: Rewrite Sidebar Component
- Phase 2.5: Create New Components
- Phase 2.6: Update MainLayout
- Phase 3: UI Cleanup

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
