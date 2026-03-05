# Progress Handoff - File-Based Automations

## Session Summary

Completed **Phase 2: Automation Cache (Discovery)** of the file-based automations plan. This phase provides the discovery and caching layer for loading automations from global and project directories.

## Changes Made

### New Files
- `crates/automations/src/cache.rs` - AutomationCache implementation with:
  - `AutomationCache` struct for managing home and repo automation directories
  - `get()` method - returns merged automations with project overriding global
  - `get_by_name()` method - fetches a single automation by name
  - `list_all()` method - lists all automations including duplicates (shows source)
  - `discover_automations()` private method - discovers TOML files in a directory
  - `parse_automation_file()` helper - parses and validates individual TOML files
  - `validate_and_convert()` helper - validates project paths asynchronously
  - Comprehensive test suite (21 tests covering all functionality)

### Modified Files
- `crates/automations/src/lib.rs` - Added `pub mod cache;` and exported `AutomationCache`

## Test Coverage
All 68 tests pass, including:
- 21 new cache tests (single/multiple automations, override behavior, error handling)
- 40 existing file_config tests (DST transitions, validation, parsing)
- 7 other automations tests (config, runner, scheduler, store)

Key test scenarios:
- Empty directory handling
- Single and multiple automation discovery
- Project overrides global by name
- Duplicate detection in list_all()
- Non-TOML file and subdirectory filtering
- Invalid TOML and cron expression error handling
- Project path validation (exists/not exists)
- 5-field and 6-field cron normalization
- Prompt file .md suffix stripping

## What's Next (Recommended)

**Phase 2b: Prompt Source Resolution** - Add `resolve()` method to `PromptSource` in `crates/automations/src/file_config.rs`:

```rust
impl PromptSource {
    pub async fn resolve(
        &self,
        prompt_cache: &PromptCache,
        home_dir: &Path,
        repo_dir: Option<&Path>,
    ) -> Result<String> {
        // Resolve Inline, PromptsDirFile, or Name to actual prompt content
    }
}
```

**Why this is next:** The cache layer can now load and parse automation definitions, but the prompt sources need to be resolved to actual content before they can be used by the runner. This is a focused addition to the existing `file_config.rs` module.

**Alternative:** If the helper function is desired, add `get_xdg_config_home()` to `apps/velor-cli/src/automations.rs` as specified in the plan.

## Remaining Phases (from plan)
- Phase 3: Variable Merging - `merge_automation_vars()` with built-ins
- Phase 4: Update AutomationRunner - git root resolution, worktree handling
- Phase 5: CLI Flags - list, validate, run, status, tick commands

## No Blockers

All dependencies resolved. Ready to proceed with Phase 2b.

## Commit Reference
(Will be created after this handoff is written)
