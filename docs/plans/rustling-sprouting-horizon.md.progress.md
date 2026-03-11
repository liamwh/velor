# Progress Handoff: Database Consolidation - Phase 2 Migration Logic

## What Changed (Facts Only)

### Implementation Completed: Phase 2 - Add Migration Logic to AutomationStore

**File Modified:** `crates/automations/src/store.rs`

**Changes:**
1. Added `open_with_migration()` method - Opens database with automatic migration from legacy `automations.db`
2. Added `is_database_empty()` helper - Checks if a database has no automation_runs
3. Added `migrate_from_legacy()` method - Migrates automation_runs and automation_locks from legacy database
4. Added `LegacyAutomationRunRow` struct - For reading legacy database rows during migration
5. Added tracing instrumentation with `info` and `instrument` macros
6. Added imports: `color_eyre::eyre::WrapErr`, `std::path::Path`, `tracing::{info, instrument}`

**Tests Added:**
1. `test_open_with_migration_no_legacy_db` - Verifies opening without legacy database works
2. `test_open_with_migration_empty_velor_db_with_legacy` - Verifies migration from legacy to empty velor.db
3. `test_open_with_migration_existing_velor_db_skips_migration` - Verifies migration is skipped when velor.db has data
4. `test_is_database_empty` - Verifies database empty detection logic
5. `test_migration_with_locks` - Verifies legacy database is renamed to .bak after migration

**Migration Behavior:**
- Checks for `.velor/automations.db` (legacy) in the same directory as target path
- Migrates only if target doesn't exist OR is empty
- Copies `automation_runs` and `automation_locks` tables
- Renames legacy database to `automations.db.bak` after successful migration
- Cleans up associated WAL files

**All tests pass:** `cargo test -p velor-automations --lib store` - 14 passed, 0 failed

## What's Next (The Next Best Task)

**Phase 1: Change Default Database Path in config.rs**

The default `state_db_path` in `crates/velor-core/src/config.rs` (line 249) is still `.velor/automations.db`. This needs to be changed to `.velor/velor.db` to align with the migration logic just implemented.

**Also need to update:** `crates/automations/src/config.rs` (line 33) which has the same default.

**Why this is next:** The migration logic is now complete, but the default path still points to the legacy database name. Changing the default will cause the migration to trigger automatically on first use for existing users.

## Blockers / Open Questions

**None identified.**

## References

- **Plan file:** `docs/plans/rustling-sprouting-horizon.md`
- **Current code:** `crates/automations/src/store.rs`
- **Tests:** All migration tests passing in store.rs test module
