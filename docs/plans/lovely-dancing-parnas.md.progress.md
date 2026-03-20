# Velor UI Enhancements Plan - Progress

## Completed Features

### Feature 1: Fix TOML "[object Object]" Bug ✅

**Status:** Complete

**Commit:** `fd4e29d fix(gui): serialize config to TOML on backend for proper nested structure display`

**Changes Made:**
1. Updated `ConfigResponse` in `/apps/velor/src-tauri/src/commands.rs` to include pre-serialized TOML strings (`merged_toml`, `home_toml`, `repo_toml`)
2. Updated `get_config` command to serialize configs using the `toml` crate
3. Updated `get_home_config` and `get_repo_config` commands to return TOML strings instead of `FileConfig` objects
4. Updated frontend types in `/apps/velor/src/lib/types/config.ts` to match new backend response structure
5. Updated config store in `/apps/velor/src/lib/stores/config.ts` to use pre-serialized TOML strings
6. Removed the buggy `tomlStringify` function from `ConfigEditor.svelte`
7. Added comprehensive test `test_toml_serialization_handles_nested_structures` to verify nested structures serialize correctly

**Verification:**
- All tests pass
- `just check` passes with no errors
- The "Effective" tab now properly displays nested TOML structures like `[prompts.*]` sections

---

### Feature 2: New Session Dialog ✅

**Status:** Complete

**Commit:** `0fe2756 feat(gui): add New Session dialog for prompt selection and variable configuration`

**Changes Made:**
1. Created `/apps/velor/src/lib/components/sessions/NewSessionDialog.svelte`:
   - Modal dialog with prompt dropdown selector
   - Variable editor panel showing config defaults and custom overrides
   - Advanced options section (max_iterations, max_retries)
   - Start and Cancel buttons
   - Error handling and loading states
   - Keyboard accessibility (Escape to close)

2. Updated `/apps/velor/src/lib/components/sidebar/SidebarHeader.svelte`:
   - Changed "New session" button to open dialog instead of navigating to /
   - Added dialog visibility state
   - Wired up close handler

3. Updated `/apps/velor/src/lib/components/sessions/index.ts`:
   - Exported NewSessionDialog component

**Verification:**
- All 359 tests pass
- `just check` passes with no errors
- Click "New session" → dialog appears
- Select prompts from dropdown
- Add/edit variable overrides
- Configure advanced options
- Start execution and navigate to /executions

---

### Feature 3: Consolidate SQLite Databases ✅

**Status:** Complete

**Commit:** `299f631 feat(gui): consolidate SQLite databases into unified velor.db`

**Changes Made:**
1. Created `/apps/velor/src-tauri/src/unified_store.rs` with:
   - Combined schema from both session_store.rs and automations crate store.rs
   - Single connection pool to `.velor/velor.db`
   - All session methods (insert, update, get, list, delete, stats, rename, pin, events)
   - All project methods (list, hide, show, rename, reorder)
   - All automation methods (insert_run, update_run, get_runs, locks)
   - Comprehensive unit and property tests

2. Migration logic:
   - `migrate_from_legacy()` detects existing `sessions.db` and `automations.db`
   - Copies all data to new unified `velor.db`
   - Renames legacy files to `.db.bak` after successful migration
   - Handles sessions, session_events, projects, automation_runs, and automation_locks tables

3. Updated `AppState` in `/apps/velor/src-tauri/src/state.rs`:
   - Replaced `session_store` + `automation_store` with single `store: UnifiedStore`
   - Added `init_store(velor_dir)` method replacing separate init methods
   - Updated `persist_session()` to use unified store
   - Updated tests to use new API

4. Updated `/apps/velor/src-tauri/src/lib.rs`:
   - Single `init_store(velor_dir)` call instead of two separate stores
   - Creates unified `velor.db` in `.velor/` directory

5. Updated `/apps/velor/src-tauri/src/commands.rs`:
   - All session commands use `state.store()` instead of `state.session_store()`
   - All automation run commands use `state.store()` instead of `state.automation_store()`
   - All project commands use unified store
   - Daemon creates `AutomationStore` from unified database for the runner

6. Updated `/apps/velor/src-tauri/src/daemon.rs`:
   - Continues to use `velor_automations::AutomationStore` for the `AutomationRunner`
   - Store points to the same unified `velor.db` file

**Verification:**
- All 359 tests pass
- `just check` passes with no errors
- Fresh install creates single `velor.db`
- Upgrade migrates existing data and renames old files to `.db.bak`

---

### Feature 4: Plan Mode in UI ✅

**Status:** Complete

**Commit:** `42ced00 feat(gui): add Plan Mode UI for AI-powered plan generation`

**Changes Made:**

1. Backend (Tauri commands):
   - Added `SpecFileInfo` struct for spec file metadata (name, path, content)
   - Added `GeneratePlanRequest` struct for plan generation options
   - Added `discover_specs` command to list .md files from specs/ directory
   - Added `build_plan_prompt` command for dry-run prompt preview
   - Added `generate_plan` command that calls OpenAI API with discovered specs
   - Added `reqwest` dependency for async HTTP client

2. Frontend components:
   - Created `/apps/velor/src/routes/plans/+page.svelte` - Plans page
   - Created `/apps/velor/src/lib/components/plan/PlanGenerator.svelte`:
     - Spec file list with multi-select checkboxes
     - Select all / Deselect all buttons
     - Model selection dropdown (gpt-4o, gpt-4o-mini, gpt-4-turbo, gpt-3.5-turbo)
     - Optional API key override input
     - Dry run mode checkbox to preview prompt without API call
     - Generated plan display with copy to clipboard
     - Loading and error states
   - Created `/apps/velor/src/lib/types/plan.ts` for TypeScript types
   - Added plan service functions to `/apps/velor/src/lib/services/tauri.ts`

3. Navigation:
   - Added "Plans" button to sidebar header with FileText icon

4. Tests:
   - `test_spec_file_info_serialization` - verifies JSON serialization
   - `test_spec_file_info_deserialization` - verifies JSON deserialization
   - `test_generate_plan_request_deserialization` - verifies request parsing
   - `test_generate_plan_request_defaults` - verifies optional fields
   - `test_build_plan_prompt_empty_specs` - verifies warning for empty specs
   - `test_build_plan_prompt_with_specs` - verifies prompt content
   - `test_build_plan_prompt_includes_instructions` - verifies prompt structure

**Verification:**
- All 88 tests pass
- `just check` passes with no errors
- Navigate to Plans page from sidebar
- See list of spec files from specs/ directory
- Select/deselect specs
- Configure model and optional API key
- Generate plan (or preview with dry run)
- Copy generated plan to clipboard

---

## Summary

All four features have been successfully implemented:

1. ✅ TOML Bug Fix - Config editor now properly displays nested TOML structures
2. ✅ New Session Dialog - Users can configure prompts and variables before starting
3. ✅ Database Consolidation - Single unified velor.db replaces separate databases
4. ✅ Plan Mode UI - AI-powered plan generation available in the GUI

The plan is complete.
