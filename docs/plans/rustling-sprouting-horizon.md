# Database Consolidation: Migrate to velor.db

## Context

Currently, the Velor automations system uses a split database approach:
- **CLI** uses `.velor/automations.db` (default via `state_db_path` config)
- **GUI** uses `.velor/velor.db` (hardcoded)

This causes confusion because:
1. The `vel automations status` command shows "No recent runs" because it's reading from the wrong database
2. There are two active databases with different data
3. The GUI already has migration logic to unify data into `velor.db`

The goal is to consolidate everything to use `velor.db` as the single unified database.

## Recommended Approach

### Phase 1: Change Default Database Path

**File:** `crates/velor-core/src/config.rs`

Change the default `state_db_path` from `.velor/automations.db` to `.velor/velor.db`:

```rust
fn default() -> Self {
    Self {
        automations_dir: ".velor/automations.d".to_string(),
        state_db_path: ".velor/velor.db".to_string(),  // Changed from .velor/automations.db
        // ... rest of defaults
    }
}
```

### Phase 2: Add Migration Logic to AutomationStore

**File:** `crates/automations/src/store.rs`

Add a migration function similar to the GUI's `migrate_automations_db()` method. The function should:

1. Check if legacy `.velor/automations.db` exists
2. If the new `.velor/velor.db` is empty or doesn't exist, migrate all data
3. Copy `automation_runs` and `automation_locks` tables
4. Rename legacy database to `.automations.db.bak` after successful migration

Add these methods to `AutomationStore`:

```rust
impl AutomationStore {
    /// Open the database with automatic migration from legacy automations.db
    pub async fn open_with_migration(path: impl AsRef<std::path::Path>) -> color_eyre::Result<Self> {
        let path = path.as_ref();
        let velor_dir = path.parent().unwrap_or(path);

        // Check for legacy automations.db in the same directory
        let legacy_db = velor_dir.join("automations.db");

        // If velor.db doesn't exist or is empty, and legacy exists, migrate first
        let needs_migration = legacy_db.exists() &&
            (!path.exists() || Self::is_database_empty(path).await?);

        if needs_migration {
            Self::migrate_from_legacy(&legacy_db, path).await?;
        }

        Self::open(path).await
    }

    /// Check if a database is empty (no automation_runs)
    async fn is_database_empty(path: impl AsRef<std::path::Path>) -> color_eyre::Result<bool> {
        // Implementation using SQLite query to check count
    }

    /// Migrate data from legacy automations.db to velor.db
    async fn migrate_from_legacy(
        legacy_path: &Path,
        new_path: &Path,
    ) -> color_eyre::Result<()> {
        // Similar to GUI's migrate_automations_db logic:
        // 1. Connect to legacy database
        // 2. Copy automation_runs (check for duplicates)
        // 3. Copy automation_locks
        // 4. Rename legacy to .bak
    }
}
```

### Phase 3: Update CLI Commands to Use Migration

**File:** `apps/velor-cli/src/automations.rs`

Update all places where `AutomationStore::open()` is called to use `AutomationStore::open_with_migration()` instead:

1. `run_run()` (line ~326)
2. `run_status()` (line ~418)
3. `process_project_tick()` (line ~650)
4. `run_daemon()` (line ~753)

### Phase 4: Cleanup and Testing

1. Test migration with existing data
2. Verify that `vel automations status` shows recent runs after migration
3. Remove `.automations.db.bak` files after successful migration
4. Update documentation to reflect `velor.db` as the standard

## Critical Files to Modify

1. **`crates/velor-core/src/config.rs`** - Change default `state_db_path`
2. **`crates/automations/src/store.rs`** - Add migration logic
3. **`apps/velor-cli/src/automations.rs`** - Use `open_with_migration()`

## Existing Code to Reference

- **GUI Migration Logic:** `apps/velor/src-tauri/src/unified_store.rs` (lines 454-658)
  - `migrate_from_legacy()` - Overall migration coordinator
  - `migrate_automations_db()` - Automations-specific migration
  - `rename_to_backup()` - Renames legacy files to .bak

## Verification Steps

1. Build the CLI: `cargo build --release -p velor-cli`
2. Install: `cp target/release/vel ~/bin/vel`
3. Run status: `vel automations status` - Should show "No automation state database" or migrate automatically
4. If migration occurs, verify: `ls -la .velor/*.db*`
5. Check runs are visible: `vel automations status`
6. Test tick: `vel automations tick` - Should write to velor.db
7. Verify GUI still works with unified database

## Notes

- The GUI already uses `velor.db` with hardcoded path, so this change aligns CLI with GUI
- Existing `automations.db` files will be automatically migrated on first access
- After successful migration, legacy files are renamed to `.bak` for safety
- The migration is idempotent - running it multiple times is safe
