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
   - `list()` - Returns reference to all projects
   - `enabled_projects()` - Returns only enabled projects

3. **Features:**
   - Git repository validation (checks for `.git` directory)
   - Relative path resolution (resolves to absolute paths)
   - Duplicate ID detection
   - Optional ID (defaults to directory name)
   - Proper error handling with color-eyre
   - Comprehensive test coverage (13 tests, all passing)

**Modified File: `/Users/liam/git/velor/crates/automations/src/lib.rs`**

- Added `pub mod registry;`
- Re-exported `ProjectEntry` and `ProjectRegistry`

## Status
- **Phase 1 (Project Registry):** COMPLETE
- **Phase 2 (Project Management Commands):** TODO
- **Phase 3 (Multi-Repo Tick):** TODO
- **Phase 4 (Launchd Management Commands):** TODO
- **Phase 5 (Dependency Addition):** TODO (dirs already in dependencies)
- **Phase 6 (Clean Up Old Script):** TODO

## What's Next

**Phase 2: Project Management Commands**

This is the next most important task because:
1. It provides the CLI interface for managing projects
2. Testing the registry requires commands to use it
3. Phase 3 (Multi-Repo Tick) depends on projects being registered via commands

Implementation:
1. Create `apps/velor-cli/src/projects.rs` with `run_project()` handler
2. Add `Project` top-level command to main.rs with Add/Remove/List subcommands
3. Implement handlers for Add (register), Remove (unregister), List (show all)

## Blockers / Open Questions

None.

## Verification

- All 13 registry module tests pass
- `cargo check -q` passes (no compiler errors or warnings)
- `just check` passes (all tests pass, Svelte warnings unrelated)

## Commit References

Previous session (preparation):
- Commit: 7a160ce fix(cli): make Ctrl+C handler registration more graceful

This session (Phase 1):
- Created `crates/automations/src/registry.rs` - Complete project registry module with 13 tests
- Updated `crates/automations/src/lib.rs` - Added registry module and re-exports
- Progress file update
