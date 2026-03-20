# Velor UI Enhancements Plan

## Context

This plan addresses four UI/infrastructure improvements for the Velor application:
1. Add plan mode capabilities to the GUI (currently CLI-only)
2. Fix the TOML "[object Object]" display bug in ConfigEditor
3. Enhance "New Session" flow with prompt/variable selection dialog
4. Consolidate two SQLite databases into a single unified database

---

## Feature 1: Fix TOML "[object Object]" Bug

**Problem:** The "Effective" tab in ConfigEditor shows "[object Object]" for nested structures like `prompts` because the custom `tomlStringify` function can't handle deeply nested objects.

**Location:** `/apps/velor/src/lib/components/settings/ConfigEditor.svelte:75-133`

**Solution:** Have the backend serialize configs to TOML strings instead of relying on frontend serialization.

### Changes

1. **Update `ConfigResponse` in `/apps/velor/src-tauri/src/commands.rs`:**
   ```rust
   pub struct ConfigResponse {
       pub merged: FileConfig,
       pub merged_toml: String,      // Add: pre-serialized TOML
       pub home: Option<FileConfig>,
       pub home_toml: Option<String>, // Add
       pub repo: Option<FileConfig>,
       pub repo_toml: Option<String>, // Add
   }
   ```

2. **Update `get_config` command** to serialize using the `toml` crate

3. **Update `/apps/velor/src/lib/stores/config.ts`:** Store TOML strings from backend

4. **Simplify ConfigEditor.svelte:** Use pre-serialized strings, remove `tomlStringify` function

### Verification
- Open Settings → Config, verify "Effective" tab shows proper TOML
- Check `[prompts.*]` sections render correctly with nested tables

---

## Feature 2: New Session Dialog

**Problem:** "New session" button just navigates to "/" without asking for prompt/variable configuration.

**Location:** `/apps/velor/src/lib/components/sidebar/SidebarHeader.svelte:10-12`

**Solution:** Create a modal dialog for prompt selection and variable overrides before starting a session.

### Changes

1. **Create `/apps/velor/src/lib/components/sessions/NewSessionDialog.svelte`:**
   - Modal with prompt dropdown (reuse logic from ChatInput)
   - Variable editor panel showing template defaults + editable overrides
   - Advanced options: max_iterations, max_retries
   - Start/Cancel buttons

2. **Update `SidebarHeader.svelte`:**
   - Add dialog visibility state
   - Change `handleNewSession` to open dialog instead of navigating

3. **Wire up execution start:**
   - Call `executionStore.start(config)` on dialog submit
   - Navigate to `/executions` to show running session

### Verification
- Click "New session" → dialog appears
- Select different prompts → variables update accordingly
- Edit variables → start execution with correct config

---

## Feature 3: Consolidate SQLite Databases

**Problem:** Two separate databases complicate data management:
- `.velor/sessions.db` (sessions, session_events, projects tables)
- `.velor/automations.db` (automation_runs, automation_locks tables)

**Locations:**
- `/apps/velor/src-tauri/src/session_store.rs`
- `/crates/automations/src/store.rs`

**Solution:** Merge into single `.velor/velor.db` with migration support.

### Changes

1. **Create `/apps/velor/src-tauri/src/unified_store.rs`:**
   - Combined schema from both stores
   - Single connection pool
   - All table definitions in one place

2. **Add migration logic:**
   ```rust
   async fn migrate_from_legacy(db_path: &Path) -> Result<()> {
       // Check for sessions.db and automations.db
       // Copy data to new velor.db
       // Rename old files to .bak
   }
   ```

3. **Update `AppState` in `/apps/velor/src-tauri/src/state.rs`:**
   - Replace `session_store` + `automation_store` with single `store: UnifiedStore`

4. **Update all command handlers** to use unified store

### Verification
- Fresh install: Single `velor.db` created
- Upgrade: Data migrated, old files renamed to `.bak`
- All session and automation operations work correctly

---

## Feature 4: Plan Mode in UI

**Problem:** Plan generation (reading specs, calling OpenAI) is CLI-only.

**Location:** `/apps/velor-cli/src/plan.rs`

**Solution:** Expose plan generation via Tauri commands and create a UI.

### Changes

1. **Move plan logic to shared crate** (or keep in velor-core):
   - Extract `run_plan_generation`, `discover_specs`, `build_plan_prompt` to be reusable
   - Make async-compatible for Tauri

2. **Add Tauri commands in `/apps/velor/src-tauri/src/commands.rs`:**
   ```rust
   #[tauri::command]
   async fn discover_specs(specs_dir: String) -> CommandResult<Vec<SpecFileInfo>>;

   #[tauri::command]
   async fn generate_plan(specs_dir: String, dry_run: bool) -> CommandResult<String>;
   ```

3. **Create UI components:**
   - `/apps/velor/src/routes/plans/+page.svelte` - Plans page
   - `/apps/velor/src/lib/components/plan/PlanGenerator.svelte`:
     - Display available spec files
     - Select which specs to include
     - Show generation progress
     - Display generated plan with copy/save options

4. **Add to sidebar navigation**

### Verification
- Navigate to Plans page
- See list of spec files from `specs/` directory
- Generate plan, verify output displayed correctly
- Test dry-run mode

---

## Implementation Order

| Order | Feature | Dependencies | Est. Effort |
|-------|---------|--------------|-------------|
| 1 | TOML Bug Fix | None | Low |
| 2 | Database Consolidation | None | Medium |
| 3 | New Session Dialog | Database (for persistence) | Medium |
| 4 | Plan Mode UI | None (can parallel with #3) | Medium-High |

---

## Critical Files

- `/apps/velor/src-tauri/src/commands.rs` - Add commands, update ConfigResponse
- `/apps/velor/src/lib/components/settings/ConfigEditor.svelte` - Fix TOML display
- `/apps/velor/src/lib/components/sidebar/SidebarHeader.svelte` - New session dialog trigger
- `/apps/velor/src-tauri/src/session_store.rs` - Schema reference for DB consolidation
- `/crates/automations/src/store.rs` - Schema reference for DB consolidation
- `/apps/velor-cli/src/plan.rs` - Plan logic to expose via Tauri

---

## End-to-End Verification

1. **Config Editor:** Open Settings → Config, verify all tabs show proper TOML
2. **New Session:** Click "New session", select prompt, edit variables, start execution
3. **Database:** Check `.velor/` contains only `velor.db`, verify session history and automations work
4. **Plan Mode:** Navigate to Plans, select specs, generate plan, verify output
