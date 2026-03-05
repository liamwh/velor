# Progress: Multi-Repo Velor Automations with Binary-Managed Launchd

## Session Date: 2025-03-05

## What Changed (Facts)

### Phase 1: Project Registry - COMPLETED

**New File: `/Users/liam/git/velor/crates/automations/src/registry.rs`**

Implemented complete project registry module:

1. **Structs defined:**
   - `ProjectRegistry` - Registry configuration with list of projects
   - `ProjectEntry` - Single project entry with id, path, and enabled flag

2. **Methods implemented:**
   - `registry_path()` - Returns path to `~/.config/velor/projects.toml`
   - `load()` - Loads registry from disk, returns empty if missing
   - `save()` - Persists registry to disk with directory creation
   - `add()` - Adds project with git repo validation and duplicate detection
   - `remove()` - Removes project by ID with error if not found
   - `enable()` - Enables a project by ID
   - `disable()` - Disables a project by ID
   - `list()` - Returns reference to all projects
   - `enabled_projects()` - Returns only enabled projects

3. **Features:**
   - Git repository validation (checks for `.git` directory)
   - Relative path resolution (resolves to absolute paths)
   - Duplicate ID detection
   - Optional ID (defaults to directory name)
   - Proper error handling with color-eyre
   - Comprehensive test coverage (21 tests, all passing)

**Modified File: `/Users/liam/git/velor/crates/automations/src/lib.rs`**

- Added `pub mod registry;`
- Re-exported `ProjectEntry` and `ProjectRegistry`

### Phase 2: Project Management Commands - COMPLETED

**New File: `/Users/liam/git/velor/apps/velor-cli/src/projects.rs`**

Implemented complete project management CLI:

1. **Command structures:**
   - `ProjectArgs` - Top-level command arguments
   - `ProjectCommand` enum with variants: Add, Remove, List, Enable, Disable

2. **Command handlers:**
   - `run_add()` - Registers a project (path defaults to current directory)
   - `run_remove()` - Removes a project by ID
   - `run_list()` - Lists all registered projects with status
   - `run_enable()` - Enables a disabled project
   - `run_disable()` - Disables a project temporarily
   - `run_project()` - Main dispatch function

3. **Features:**
   - Emoji status indicators (✅ for enabled, ❌ for disabled)
   - User-friendly output messages
   - Proper error handling with context
   - Tracing instrumentation for debugging

**Modified File: `/Users/liam/git/velor/apps/velor-cli/src/main.rs`**

- Added `mod projects;` import
- Added `Project(ProjectArgs)` variant to `Commands` enum
- Added `run_project()` function as wrapper
- Added dispatch for Project command in main match statement

## Status
- **Phase 1 (Project Registry):** COMPLETE
- **Phase 2 (Project Management Commands):** COMPLETE
- **Phase 3 (Multi-Repo Tick):** TODO
- **Phase 4 (Launchd Management Commands):** TODO
- **Phase 5 (Dependency Addition):** COMPLETE (dirs already in dependencies)
- **Phase 6 (Clean Up Old Script):** TODO

## What's Next

**Phase 3: Multi-Repo Tick with Lock**

This is the next most important task because:
1. It enables the core multi-repo automation functionality
2. The tick command processes all registered projects for due automations
3. Phase 4 (Launchd Management) depends on the tick command working

Implementation:
1. Modify `run_tick()` in `apps/velor-cli/src/automations.rs` to:
   - Add file-based locking (`fs2` crate) for single-instance guarantee
   - Load `ProjectRegistry` to get all enabled projects
   - Fall back to legacy mode if registry is empty
   - Process each project with path-explicit execution (no `set_current_dir`)
2. Update precedence documentation: global config → repo config → automation file fields

## Blockers / Open Questions

None.

## Verification

- All 292 tests pass (21 new registry tests including enable/disable)
- `cargo check -q` passes (no compiler errors or warnings)
- `just check` passes (all tests pass, Svelte warnings unrelated)
- Can register, list, enable, disable, and remove projects via CLI

## Commit References

Previous sessions:
- Commit: 7a160ce fix(cli): make Ctrl+C handler registration more graceful
- Commit: d8bd0c2 feat(automations): add Phase 1 - Project Registry for multi-repo automation discovery

This session (Phase 2):
- Modified `crates/automations/src/registry.rs` - Added `enable()` and `disable()` methods with tests
- Created `apps/velor-cli/src/projects.rs` - Complete project management CLI module
- Modified `apps/velor-cli/src/main.rs` - Added Project command and dispatch
- Progress file update
