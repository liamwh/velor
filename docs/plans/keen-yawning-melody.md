# Plan: Add Automations (Cron Job Capability) to Velor

## Context

This plan adds an "automations" feature to Velor, similar to [OpenAI Codex automations](https://developers.openai.com/codex/app/automations). Automations allow scheduled, recurring execution of prompts - essentially cron jobs for AI agents.

**Architecture Change**: This is the first step in splitting Velor into a modular workspace structure. The automations feature will be implemented as a separate crate (`velor-automations`) that the CLI depends on.

## Workspace Structure

```
velor/
├── Cargo.toml                    # Workspace root
├── crates/
│   ├── automations/              # NEW: velor-automations library crate
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── config.rs
│   │       ├── scheduler.rs
│   │       ├── runner.rs
│   │       └── store.rs
│   └── cli/                      # NEW: main CLI binary (move src/ here)
│       ├── Cargo.toml
│       └── src/
│           ├── main.rs
│           └── ...
```

## Critical Files to Modify

| File | Purpose |
|------|---------|
| `Cargo.toml` | Convert to workspace root |
| `crates/automations/Cargo.toml` | New automations library crate |
| `crates/automations/src/store.rs` | SQLite state storage (sqlx-based) |
| `crates/cli/Cargo.toml` | Main binary (moved from root) |
| `crates/cli/src/main.rs` | Add `Automations` subcommand |

## Implementation Steps

### Step 1: Convert to Workspace Structure

**Root `Cargo.toml`** (replace existing):
```toml
[workspace]
resolver = "2"
members = ["crates/*"]

[workspace.lints.rust]
missing_docs = { level = "warn", priority = 1 }

[workspace.lints.clippy]
unwrap_used = { deny }

[workspace.dependencies]
# Shared dependencies
chrono = "0.4"
chrono-tz = "0.10"
color-eyre = "0.6"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tokio = { version = "1", features = ["rt-multi-thread", "process", "io-util", "net", "fs", "signal", "macros", "sync", "time"] }
tokio-util = { version = "0.7", features = ["compat"] }
toml = "0.8"
tracing = "0.1"

# Automation-specific dependencies
sqlx = { version = "0.8", features = ["runtime-tokio", "sqlite", "chrono"] }
cron = "0.12"
```

### Step 2: Create `velor-automations` Crate

**`crates/automations/Cargo.toml`**:
```toml
[package]
name = "velor-automations"
version = "0.1.0"
edition = "2024"
license = "UNLICENSED"

[dependencies]
chrono = { workspace = true }
chrono-tz = { workspace = true }
color-eyre = { workspace = true }
cron = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
sqlx = { workspace = true }
tokio = { workspace = true }
tokio-util = { workspace = true }
toml = { workspace = true }
tracing = { workspace = true }

[dev-dependencies]
tempfile = "3"
```

### Step 3: Implement Automation Store (`store.rs`) with sqlx

**Key changes from feedback:**
- Use `sqlx::SqlitePool` for async + Send/Sync
- Add `scheduled_for` field (when the run was intended)
- `completed_at` only set for terminal states
- Add `duration_ms`, `exit_code` fields
- Add CHECK constraint for status
- Use WAL mode for better concurrency

```rust
//! Async SQLite storage for automation execution history.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{SqlitePool, sqlite::SqliteConnectOptions};

/// Status of an automation run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[repr(i32)]
pub enum AutomationRunStatus {
    Pending = 0,
    Running = 1,
    Completed = 2,
    Failed = 3,
    Cancelled = 4,
}

impl AutomationRunStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "Pending",
            Self::Running => "Running",
            Self::Completed => "Completed",
            Self::Failed => "Failed",
            Self::Cancelled => "Cancelled",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "Pending" => Some(Self::Pending),
            "Running" => Some(Self::Running),
            "Completed" => Some(Self::Completed),
            "Failed" => Some(Self::Failed),
            "Cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

/// A record of an automation run.
#[derive(Debug, Clone, Serialize)]
pub struct AutomationRun {
    pub id: i64,
    pub automation_name: String,
    pub scheduled_for: DateTime<Utc>,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub status: AutomationRunStatus,
    pub iterations_completed: u32,
    pub exit_code: Option<i32>,
    pub duration_ms: Option<i64>,
    pub output: Option<String>,
    pub error: Option<String>,
}

/// Async storage for automation runs.
pub struct AutomationStore {
    pool: SqlitePool,
}

impl AutomationStore {
    /// Create or open the database at the given path.
    pub async fn open(path: impl AsRef<std::path::Path>) -> color_eyre::Result<Self> {
        let path = path.as_ref();

        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true);

        let pool = SqlitePool::connect_with(options).await?;

        // Enable WAL mode for better concurrency
        sqlx::query("PRAGMA journal_mode=WAL;")
            .execute(&pool)
            .await?;

        let store = Self { pool };
        store.init_schema().await?;
        Ok(store)
    }

    async fn init_schema(&self) -> color_eyre::Result<()> {
        let query = r#"
            CREATE TABLE IF NOT EXISTS automation_runs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                automation_name TEXT NOT NULL,
                scheduled_for TEXT NOT NULL,
                started_at TEXT NOT NULL,
                completed_at TEXT,
                status INTEGER NOT NULL,
                iterations_completed INTEGER DEFAULT 0,
                exit_code INTEGER,
                duration_ms INTEGER,
                output TEXT,
                error TEXT,
                CHECK(status IN (0, 1, 2, 3, 4))
            );

            CREATE INDEX IF NOT EXISTS idx_automation_name
                ON automation_runs(automation_name);

            CREATE INDEX IF NOT EXISTS idx_started_at
                ON automation_runs(started_at);

            CREATE TABLE IF NOT EXISTS automation_locks (
                automation_name TEXT PRIMARY KEY,
                locked_at TEXT NOT NULL,
                run_id INTEGER
            );
        "#;

        sqlx::query(query).execute(&self.pool).await?;
        Ok(())
    }

    /// Insert a new run record. Returns the run ID.
    pub async fn insert_run(
        &self,
        automation_name: &str,
        scheduled_for: DateTime<Utc>,
        started_at: DateTime<Utc>,
    ) -> color_eyre::Result<i64> {
        let result = sqlx::query(
            "INSERT INTO automation_runs (automation_name, scheduled_for, started_at, status)
             VALUES (?1, ?2, ?3, ?4)"
        )
        .bind(automation_name)
        .bind(scheduled_for.to_rfc3339())
        .bind(started_at.to_rfc3339())
        .bind(AutomationRunStatus::Pending)
        .execute(&self.pool)
        .await?;

        Ok(result.last_insert_rowid())
    }

    /// Update run status and completion info.
    pub async fn update_run(
        &self,
        id: i64,
        status: AutomationRunStatus,
        iterations_completed: u32,
        exit_code: Option<i32>,
        output: Option<&str>,
        error: Option<&str>,
    ) -> color_eyre::Result<()> {
        let now = Utc::now();
        let duration_ms = if status.is_terminal() {
            // Calculate duration from started_at
            if let Ok(Some(started_str)) = sqlx::query_scalar::<_, String>(
                "SELECT started_at FROM automation_runs WHERE id = ?1"
            )
            .bind(id)
            .fetch_one(&self.pool)
            .await
            {
                if let Ok(started) = DateTime::parse_from_rfc3339(&started_str) {
                    let started = started.with_timezone(&Utc);
                    Some(now.signed_duration_since(started).num_milliseconds())
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };

        let completed_at = if status.is_terminal() {
            Some(now.to_rfc3339())
        } else {
            None
        };

        sqlx::query(
            "UPDATE automation_runs
             SET completed_at = ?1, status = ?2, iterations_completed = ?3,
                 exit_code = ?4, duration_ms = ?5, output = ?6, error = ?7
             WHERE id = ?8"
        )
        .bind(completed_at)
        .bind(status)
        .bind(iterations_completed as i64)
        .bind(exit_code)
        .bind(duration_ms)
        .bind(output)
        .bind(error)
        .bind(id)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Attempt to acquire a lock for running an automation.
    /// Returns Ok(Some(run_id)) if lock acquired, Ok(None) if already locked.
    pub async fn try_acquire_lock(
        &self,
        automation_name: &str,
        run_id: i64,
    ) -> color_eyre::Result<Option<i64>> {
        let result = sqlx::query(
            "INSERT INTO automation_locks (automation_name, locked_at, run_id)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(automation_name) DO NOTHING"
        )
        .bind(automation_name)
        .bind(Utc::now().to_rfc3339())
        .bind(run_id)
        .execute(&self.pool)
        .await?;

        if result.rows_affected() > 0 {
            Ok(Some(run_id))
        } else {
            // Check if existing lock is stale (> 2 hours)
            let locked_at: Option<String> = sqlx::query_scalar(
                "SELECT locked_at FROM automation_locks WHERE automation_name = ?1"
            )
            .bind(automation_name)
            .fetch_optional(&self.pool)
            .await?;

            if let Some(locked_str) = locked_at {
                if let Ok(locked) = DateTime::parse_from_rfc3339(&locked_str) {
                    let locked = locked.with_timezone(&Utc);
                    let stale_threshold = Utc::now() - chrono::Duration::hours(2);
                    if locked < stale_threshold {
                        // Lock is stale, remove it and retry
                        self.release_lock(automation_name).await?;
                        return self.try_acquire_lock(automation_name, run_id).await;
                    }
                }
            }
            Ok(None)
        }
    }

    /// Release the lock for an automation.
    pub async fn release_lock(&self, automation_name: &str) -> color_eyre::Result<()> {
        sqlx::query("DELETE FROM automation_locks WHERE automation_name = ?1")
            .bind(automation_name)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Get recent runs, optionally filtered by automation name.
    pub async fn get_runs(
        &self,
        automation_name: Option<&str>,
        limit: u32,
    ) -> color_eyre::Result<Vec<AutomationRun>> {
        let rows = if let Some(name) = automation_name {
            sqlx::query_as::<_, AutomationRunRow>(
                "SELECT * FROM automation_runs
                 WHERE automation_name = ?1
                 ORDER BY started_at DESC
                 LIMIT ?2"
            )
            .bind(name)
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query_as::<_, AutomationRunRow>(
                "SELECT * FROM automation_runs
                 ORDER BY started_at DESC
                 LIMIT ?1"
            )
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await?
        };

        rows.into_iter()
            .map(|r| r.try_into())
            .collect()
    }
}

/// Raw row from database.
struct AutomationRunRow {
    id: i64,
    automation_name: String,
    scheduled_for: String,
    started_at: String,
    completed_at: Option<String>,
    status: i32,
    iterations_completed: i64,
    exit_code: Option<i64>,
    duration_ms: Option<i64>,
    output: Option<String>,
    error: Option<String>,
}

impl TryFrom<AutomationRunRow> for AutomationRun {
    type Error = color_eyre::Report;

    fn try_from(row: AutomationRunRow) -> Result<Self, Self::Error> {
        let status = match row.status {
            0 => AutomationRunStatus::Pending,
            1 => AutomationRunStatus::Running,
            2 => AutomationRunStatus::Completed,
            3 => AutomationRunStatus::Failed,
            4 => AutomationRunStatus::Cancelled,
            n => {
                tracing::warn!("Unknown status code {} in database, treating as Failed", n);
                AutomationRunStatus::Failed
            }
        };

        let scheduled_for = DateTime::parse_from_rfc3339(&row.scheduled_for)
            .map_err(|e| color_eyre::eyre::eyre!("Invalid scheduled_for RFC3339: {}", e))?
            .with_timezone(&Utc);

        let started_at = DateTime::parse_from_rfc3339(&row.started_at)
            .map_err(|e| color_eyre::eyre::eyre!("Invalid started_at RFC3339: {}", e))?
            .with_timezone(&Utc);

        let completed_at = row.completed_at
            .map(|s| DateTime::parse_from_rfc3339(&s).map_err(|e| color_eyre::eyre::eyre!("Invalid completed_at RFC3339: {}", e)))
            .transpose()?
            .map(|dt| dt.with_timezone(&Utc));

        Ok(Self {
            id: row.id,
            automation_name: row.automation_name,
            scheduled_for,
            started_at,
            completed_at,
            status,
            iterations_completed: row.iterations_completed as u32,
            exit_code: row.exit_code.map(|c| c as i32),
            duration_ms: row.duration_ms,
            output: row.output,
            error: row.error,
        })
    }
}
```

### Step 4: Implement Config (`config.rs`)

**Key changes:**
- Add 6-field cron format (with seconds)
- Add timezone handling
- Add catch-up policy
- Add timeout settings

```rust
//! Automation configuration.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Configuration for the automations feature.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct AutomationsConfig {
    /// Directory for automation definitions (relative to git root).
    pub automations_dir: String,

    /// Path to the state database for tracking runs.
    pub state_db_path: String,

    /// Default maximum concurrent automations.
    pub max_concurrent: u32,

    /// Default timezone for schedule parsing (IANA tz database name).
    pub default_timezone: String,

    /// Default timeout for automation runs (seconds).
    pub default_timeout_seconds: u64,

    /// Maximum output size to store (bytes).
    pub max_output_bytes: usize,
}

impl Default for AutomationsConfig {
    fn default() -> Self {
        Self {
            automations_dir: ".velor/automations.d".to_string(),
            state_db_path: ".velor/automations.db".to_string(),
            max_concurrent: 3,
            default_timezone: "UTC".to_string(),
            default_timeout_seconds: 3600, // 1 hour
            max_output_bytes: 100_000,     // 100 KB
        }
    }
}

/// Catch-up policy for missed runs.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CatchUpPolicy {
    /// Skip all missed runs, run only once on next tick.
    Skip,
    /// Run once regardless of how many were missed.
    RunOnce,
    /// Run all missed schedules (may be dangerous!).
    RunAll,
}

impl Default for CatchUpPolicy {
    fn default() -> Self {
        Self::Skip
    }
}

/// An automation definition loaded from TOML.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Automation {
    /// Unique name of the automation.
    pub name: String,

    /// Human-readable description.
    pub description: String,

    /// Cron schedule expression (6-field: seconds minutes hours day month weekday).
    pub schedule: String,

    /// Timezone for the schedule (IANA tz database name, e.g. "America/New_York").
    /// Defaults to config default_timezone.
    #[serde(default)]
    pub timezone: String,

    /// Prompt template name or inline content.
    pub prompt: String,

    /// Whether this automation is enabled.
    #[serde(default = "default_enabled")]
    pub enabled: bool,

    /// Variables to pass to the prompt.
    #[serde(default)]
    pub vars: BTreeMap<String, String>,

    /// Policy for handling missed runs.
    #[serde(default)]
    pub catch_up: CatchUpPolicy,

    /// Maximum number of catch-up runs to execute.
    #[serde(default)]
    pub max_catch_up: u32,

    /// Timeout for this automation (seconds).
    /// Defaults to config default_timeout_seconds.
    #[serde(default)]
    pub timeout_seconds: Option<u64>,

    /// Send notification on success.
    #[serde(default = "default_true")]
    pub notify_on_success: bool,

    /// Send notification on failure.
    #[serde(default = "default_true")]
    pub notify_on_failure: bool,
}

fn default_enabled() -> bool { true }
fn default_true() -> bool { true }

/// Load all automations from a directory.
pub async fn load_automations(
    dir: impl AsRef<std::path::Path>,
) -> color_eyre::Result<Vec<Automation>> {
    let dir = dir.as_ref();
    let mut automations = Vec::new();

    if !dir.exists() {
        tokio::fs::create_dir_all(dir).await?;
        return Ok(automations);
    }

    let mut entries = tokio::fs::read_dir(dir).await?;
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("toml") {
            let content = tokio::fs::read_to_string(&path).await?;

            // Validate cron expression format (6 fields expected)
            let automation: Automation = toml::from_str(&content)?;

            // Validate cron has 6 fields
            let parts: Vec<&str> = automation.schedule.split_whitespace().collect();
            if parts.len() != 6 {
                return Err(color_eyre::eyre::eyre!(
                    "Invalid cron expression '{}': expected 6 fields (seconds minutes hours day month weekday), got {}",
                    automation.schedule,
                    parts.len()
                ));
            }

            // Validate timezone
            if automation.timezone.is_empty() {
                // Will be set to default later
            } else {
                chrono_tz::Tz::from_str_insensitive(&automation.timezone)
                    .map_err(|_| color_eyre::eyre::eyre!("Invalid timezone: {}", automation.timezone))?;
            }

            automations.push(automation);
        }
    }

    Ok(automations)
}
```

### Step 5: Implement Scheduler (`scheduler.rs`)

**Key changes:**
- 6-field cron format
- Timezone-aware scheduling
- Next due time calculation

```rust
//! Cron-based scheduling for automations with timezone support.

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use cron::Schedule;

/// Automation scheduler using cron expressions.
pub struct Scheduler {
    schedule: Schedule,
    timezone: Tz,
}

impl Scheduler {
    /// Create a new scheduler from a cron expression and timezone.
    pub fn new(
        cron_expression: &str,
        timezone: Tz,
    ) -> color_eyre::Result<Self> {
        // Validate 6-field format
        let parts: Vec<&str> = cron_expression.split_whitespace().collect();
        if parts.len() != 6 {
            return Err(color_eyre::eyre::eyre!(
                "Invalid cron expression: expected 6 fields (seconds minutes hours day month weekday), got {}",
                parts.len()
            ));
        }

        let schedule = Schedule::from_str(cron_expression)?;
        Ok(Self { schedule, timezone })
    }

    /// Get the next scheduled time after the given timestamp.
    pub fn next_after(&self, after: DateTime<Utc>) -> DateTime<Utc> {
        // Convert to timezone for calculation
        let after_tz = after.with_timezone(&self.timezone);
        self.schedule.after(after_tz)
            .next()
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(|| {
                // If no next time (shouldn't happen with valid cron), return far future
                Utc::now() + chrono::Duration::days(365 * 100)
            })
    }

    /// Calculate all missed schedules between last run and now.
    pub fn missed_runs_since(
        &self,
        last_run: DateTime<Utc>,
        now: DateTime<Utc>,
        max_count: u32,
    ) -> Vec<DateTime<Utc>> {
        let mut missed = Vec::new();
        let mut current = self.next_after(last_run);

        while current <= now && missed.len() < max_count as usize {
            missed.push(current);
            current = self.next_after(current);
        }

        missed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scheduler_requires_6_fields() {
        let tz = chrono_tz::UTC;
        assert!(Scheduler::new("0 * * * *", tz).is_err());  // 5 fields
        assert!(Scheduler::new("0 0 * * * *", tz).is_ok());  // 6 fields
    }
}
```

### Step 6: Implement Runner (`runner.rs`)

**Key changes:**
- Proper git worktree creation/cleanup
- Timeout handling
- Output size limits
- Per-automation locking
- Explicit error handling (no unwrap)

```rust
//! Automation execution with worktree support.

use crate::config::Automation;
use crate::store::{AutomationStore, AutomationRunStatus, CatchUpPolicy};
use chrono::Utc;
use std::path::Path;
use std::process::Stdio;
use tokio::process::Command;
use tokio::sync::Semaphore;
use tokio::time::{timeout, Duration};
use tokio_util::sync::CancellationToken;

/// Result of running an automation.
#[derive(Debug)]
pub struct AutomationResult {
    pub status: AutomationRunStatus,
    pub iterations_completed: u32,
    pub exit_code: Option<i32>,
    pub output: String,
    pub error: Option<String>,
}

/// Runs automations with concurrency control.
pub struct AutomationRunner {
    store: AutomationStore,
    semaphore: Semaphore,
    git_root: std::path::PathBuf,
    velor_binary: String,
    max_output_bytes: usize,
}

impl AutomationRunner {
    pub fn new(
        store: AutomationStore,
        max_concurrent: u32,
        git_root: impl AsRef<Path>,
        velor_binary: String,
        max_output_bytes: usize,
    ) -> Self {
        Self {
            store,
            semaphore: Semaphore::new(max_concurrent as usize),
            git_root: git_root.as_ref().to_path_buf(),
            velor_binary,
            max_output_bytes,
        }
    }

    /// Run a single automation.
    pub async fn run_automation(
        &self,
        automation: &Automation,
        scheduled_for: chrono::DateTime<Utc>,
        cancel_token: &CancellationToken,
    ) -> color_eyre::Result<AutomationResult> {
        let _permit = self.semaphore.acquire().await?;

        let started_at = Utc::now();

        // Create run record
        let run_id = self.store.insert_run(
            &automation.name,
            scheduled_for,
            started_at,
        ).await?;

        // Try to acquire lock (prevent overlapping runs)
        let lock_acquired = self.store.try_acquire_lock(&automation.name, run_id).await?;
        if lock_acquired.is_none() {
            tracing::info!("Automation '{}' is already running, skipping", automation.name);
            self.store.update_run(
                run_id,
                AutomationRunStatus::Cancelled,
                0,
                None,
                Some("Skipped due to overlapping run"),
                None,
            ).await?;
            return Ok(AutomationResult {
                status: AutomationRunStatus::Cancelled,
                iterations_completed: 0,
                exit_code: None,
                output: String::new(),
                error: Some("Skipped due to overlapping run".to_string()),
            });
        }

        // Ensure lock is released
        defer::defer({
            let store = self.store.clone();
            let name = automation.name.clone();
            async move {
                let _ = store.release_lock(&name).await;
            }
        });

        // Update status to Running
        self.store.update_run(
            run_id,
            AutomationRunStatus::Running,
            0,
            None,
            None,
            None,
        ).await?;

        // Create worktree for git repos
        let worktree_cleanup = self.setup_worktree(automation).await?;
        let work_dir = worktree_cleanup.as_ref().map(|wc| &wc.path).unwrap_or(&self.git_root);

        // Determine timeout
        let timeout_duration = Duration::from_secs(
            automation.timeout_seconds.unwrap_or(3600)
        );

        // Execute velor with timeout
        let result = match timeout(
            timeout_duration,
            self.execute_velor(automation, work_dir, cancel_token.clone()),
        ).await {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => AutomationResult {
                status: AutomationRunStatus::Failed,
                iterations_completed: 0,
                exit_code: None,
                output: String::new(),
                error: Some(e.to_string()),
            },
            Err(_) => AutomationResult {
                status: AutomationRunStatus::Failed,
                iterations_completed: 0,
                exit_code: Some(124), // convention: 124 = timeout
                output: String::new(),
                error: Some("Timed out".to_string()),
            },
        };

        // Cleanup worktree
        if let Some(wc) = worktree_cleanup {
            wc.cleanup().await?;
        }

        // Update run record
        self.store.update_run(
            run_id,
            result.status,
            result.iterations_completed,
            result.exit_code,
            if result.output.len() <= self.max_output_bytes {
                Some(&result.output)
            } else {
                Some(&result.output[..self.max_output_bytes])
            },
            result.error.as_deref(),
        ).await?;

        Ok(result)
    }

    async fn setup_worktree(
        &self,
        automation: &Automation,
    ) -> color_eyre::Result<Option<WorktreeCleanup>> {
        let git_dir = self.git_root.join(".git");
        if !git_dir.exists() {
            return Ok(None);
        }

        let wt_name = format!(
            "automation-{}-{}",
            automation.name,
            Utc::now().format("%Y%m%d-%H%M%S")
        );
        let wt_path = self.git_root.join("..").join(&wt_name);

        // Create worktree using git
        let status = Command::new("git")
            .args(["worktree", "add", &wt_path.to_string_lossy()])
            .current_dir(&self.git_root)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .status()
            .await?;

        if !status.success() {
            return Err(color_eyre::eyre::eyre!(
                "Failed to create worktree: git exited with {:?}",
                status.code()
            ));
        }

        Ok(Some(WorktreeCleanup {
            path: wt_path,
            git_root: self.git_root.clone(),
        }))
    }

    async fn execute_velor(
        &self,
        automation: &Automation,
        work_dir: &Path,
        cancel_token: CancellationToken,
    ) -> color_eyre::Result<AutomationResult> {
        let mut child = Command::new(&self.velor_binary)
            .arg("once")
            .arg("--prompt")
            .arg(&automation.prompt)
            .current_dir(work_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        // Wait for cancellation or completion
        let output = tokio::select! {
            _ = cancel_token.cancelled() => {
                child.kill().await.ok();
                return Ok(AutomationResult {
                    status: AutomationRunStatus::Cancelled,
                    iterations_completed: 0,
                    exit_code: None,
                    output: String::new(),
                    error: Some("Cancelled".to_string()),
                });
            }
            result = child.wait_with_output() => {
                result?
            }
        };

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let exit_code = output.status.code();

        let status = if output.status.success() {
            AutomationRunStatus::Completed
        } else {
            AutomationRunStatus::Failed
        };

        // Check for completion token in output
        let (status, iterations) = if stdout.contains("<promise>COMPLETE</promise>") {
            (AutomationRunStatus::Completed, 1)
        } else {
            (status, 0)
        };

        let error = if !stderr.is_empty() {
            Some(stderr)
        } else {
            None
        };

        Ok(AutomationResult {
            status,
            iterations_completed: iterations,
            exit_code,
            output: stdout,
            error,
        })
    }
}

struct WorktreeCleanup {
    path: std::path::PathBuf,
    git_root: std::path::PathBuf,
}

impl WorktreeCleanup {
    async fn cleanup(self) -> color_eyre::Result<()> {
        // Remove worktree
        Command::new("git")
            .args(["worktree", "remove", &self.path.to_string_lossy()])
            .current_dir(&self.git_root)
            .status()
            .await?;

        Ok(())
    }
}

// Simple defer mechanism
mod defer {
    use std::future::Future;

    pub struct Defer<F>(F);

    impl<F> Defer<F>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = ()> + Send + 'static,
    {
        pub async fn done(self) {
            self.0().await;
        }
    }

    pub fn defer<F, Fut>(f: F) -> Defer<F>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = ()> + Send + 'static,
    {
        Defer(f)
    }
}
```

### Step 7: Update CLI

Add to `crates/cli/src/main.rs`:
```rust
use velor_automations::{Automation, AutomationsConfig, load_automations, AutomationRunner};

// Add to Commands enum:
/// Manage and run scheduled automations
Automations(AutomationsArgs),

#[derive(Debug, Args)]
struct AutomationsArgs {
    #[command(subcommand)]
    command: AutomationsCommand,
}

#[derive(Debug, Subcommand)]
enum AutomationsCommand {
    /// List all automations
    List,
    /// Validate automation definitions
    Validate,
    /// Run an automation immediately
    Run { name: String },
    /// Show automation status and recent runs
    Status { name: Option<String> },
    /// Start the daemon (runs continuously)
    Daemon,
}
```

## Verification Steps

1. **Build workspace:**
   ```bash
   cargo build --workspace
   cargo clippy --workspace --deny=warnings
   ```

2. **Create test automation:**
   ```bash
   mkdir -p .velor/automations.d
   cat > .velor/automations.d/test.toml << 'EOF'
   name = "test"
   description = "Test automation"
   schedule = "0 0 * * * *"  # Top of every hour
   timezone = "UTC"
   prompt = "once"
   enabled = true

   [vars]
   test = "value"
   EOF
   ```

3. **Validate automations:**
   ```bash
   cargo run --bin velor -- automations validate
   ```

4. **List automations:**
   ```bash
   cargo run --bin velor -- automations list
   ```

5. **Check status:**
   ```bash
   cargo run --bin velor -- automations status
   ```

## Summary of Key Design Decisions

| Decision | Rationale |
|----------|-----------|
| **sqlx + SqlitePool** | Async + Send/Sync, better for daemon |
| **6-field cron** | Explicit seconds field, less ambiguity |
| **chrono-tz** | Proper timezone handling for schedules |
| **Per-automation locks** | Prevent overlapping runs of same automation |
| **WAL mode** | Better SQLite concurrency for daemon |
| **Terminal-only completed_at** | Correct observability for in-progress runs |
| **Catch-up policies** | Configurable behavior for missed runs |
| **Output size caps** | Prevent database bloat |
| **Explicit error handling** | Satisfies clippy deny(unwrap_used) |
| **Worktree cleanup on failure** | Don't leave git state corrupted |

## Status ✅ COMPLETE

**Final Commit:** `788835a`

This plan has been fully implemented. All 7 steps are complete:

1. ✅ **Step 1:** Convert to Workspace Structure (Commit: `e0a14c5`)
2. ✅ **Step 2:** Create velor-automations Crate (Commit: `e0a14c5`)
3. ✅ **Step 3:** Implement Automation Store (Commit: `e0a14c5`)
4. ✅ **Step 4:** Implement Config (Commit: `e0a14c5`)
5. ✅ **Step 5:** Implement Scheduler (Commit: `e0a14c5`)
6. ✅ **Step 6:** Implement Runner (Commits: `e0a14c5`, `788835a`)
7. ✅ **Step 7:** Update CLI (Commit: `e3c8130`)

**Additional Enhancements:**
- Bug fixes and comprehensive testing (Commit: `839fe7f`)
- Catch-up policy implementation (Commit: `b7e37d2`)
- Worktree support for isolated execution (Commit: `788835a`)

**Final Test Results:**
```
Summary [2.369s] 261 tests run: 261 passed, 0 skipped
```

All clippy checks pass with no warnings.
