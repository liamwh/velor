# Progress Handoff - File-Based Automations (Phase 5)

## Session Summary

Completed **Phase 5: CLI Flags** of the file-based automations plan. This phase updates all CLI commands to use `AutomationCache` for dual-location discovery (global + project automations) and adds missing flags.

## Changes Made

### Modified Files

1. **`apps/velor-cli/src/automations.rs`** (completely rewritten)
   - Added `get_xdg_config_home()` helper function for XDG config directory resolution
   - Updated `AutomationsCommand` enum with new flags:
     - `List { all: bool }` - Added `--all` flag to show disabled automations
     - `Run { name, force: bool }` - Added `--force` flag to run disabled automations
   - Replaced `load_automations()` with `AutomationCache` for dual-location discovery:
     - Global: `XDG_CONFIG_HOME/velor/automations/`
     - Project: `{repo}/.velor/automations/`
     - Project automations override global automations with the same name
   - Updated `run_list()`:
     - Uses `AutomationCache::list_all()` to get all automations with source info
     - Shows source icon (🌍 global, 📁 project, ⚠️ legacy)
     - Respects `--all` flag for filtering enabled/disabled automations
   - Updated `run_validate()`:
     - Validates prompt resolution using `PromptCache`
     - Checks `catch_up` policy consistency
     - Warns about legacy format usage
     - Returns detailed error/warning messages
   - Updated `run_run()`:
     - Uses `AutomationCache::get_by_name()` for lookup
     - Respects `--force` flag to run disabled automations
     - Converts `AutomationFile` to legacy `Automation` for runner compatibility
     - Properly merges variables (home -> repo -> automation -> built-ins)
   - Updated `run_status()`:
     - Shows recent execution history from state database
   - Updated `run_daemon()`:
     - Uses `AutomationCache::get()` for merged automations
     - Respects project override precedence
     - Resolves prompts dynamically during execution

2. **`apps/velor-cli/src/main.rs`** (minor fix)
   - Updated `run_automations()` dispatch to pass new flag arguments:
     - `List { all }` -> `run_list(all, ...)`
     - `Run { name, force }` -> `run_run(name, force, ...)`
   - Fixed TUI menu choice to pass `all: false` for `run_list()`

3. **`crates/automations/src/state.rs`** (minor fix)
   - Re-added `use sqlx::Row;` import (needed for test code)

### Key Implementation Details

**Dual-Location Discovery:**
- `AutomationCache::new(home_dir, repo_dir)` takes both global and project paths
- `get()` returns merged automations with project overriding global
- `list_all()` returns all automations including duplicates with source info
- `get_by_name()` respects override precedence automatically

**AutomationFile to Automation Conversion:**
- `AutomationFile.timezone` is `chrono_tz::Tz`, needs `.to_string()` for legacy `Automation`
- `AutomationFile.schedule_raw` is used for display
- Prompt is resolved from `PromptSource` before creating legacy `Automation`

**Variable Merging:**
- Variables are merged in precedence order: automation -> repo -> home -> built-ins
- Uses `merge_automation_vars()` function with proper path resolution

**Timezone Handling:**
- `AutomationFile` stores timezone as parsed `Tz` for scheduler
- Legacy `Automation` stores timezone as `String`
- Conversion uses `.to_string()` on `Tz` to get IANA timezone name

## Remaining Work (from plan)

- **Phase 6: Exports** - Already complete in lib.rs (all modules exported)
- **Optional**: Add `tick` command for single-tick execution (plan mentions preferring daemon approach, but tick could be useful for launchd/cron integration)

## What's Next (Recommended)

The file-based automations feature is essentially complete. Remaining optional work:
1. Consider adding a single-tick command for use with external schedulers (launchd/cron)
2. Consider adding migration tooling from legacy `.velor/automations.d/` to new format

## No Blockers

All dependencies resolved. All checks pass with only pre-existing warnings (unused import in state.rs used by tests, unwrap() warnings in prompts.rs, Svelte warnings).

## Commit Reference

(To be committed after this handoff)
