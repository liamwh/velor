//! State database for tracking automation runs with idempotency.
//!
//! This module provides a simple, async state tracking mechanism for scheduled
//! automation runs. The UNIQUE constraint on (automation_name, scheduled_for) ensures
//! that the same scheduled run can never be executed twice, even if tick is invoked
//! concurrently or the process crashes mid-execution.

use color_eyre::Result;
use color_eyre::eyre::WrapErr;
use color_eyre::eyre::eyre;
use jiff::{Span, Timestamp};
use sqlx::{SqlitePool, sqlite::SqliteConnectOptions};
use std::path::Path;
use std::str::FromStr;

/// Run status with string constants for consistency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    /// The run has been scheduled but not yet started.
    Pending,
    /// The run is currently in progress.
    Running,
    /// The run completed successfully.
    Completed,
    /// The run failed with an error.
    Failed,
}

impl RunStatus {
    /// Returns the string representation of this status.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }
}

impl FromStr for RunStatus {
    type Err = ParseRunStatusError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(Self::Pending),
            "running" => Ok(Self::Running),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            _ => Err(ParseRunStatusError),
        }
    }
}

/// Error returned when parsing a `RunStatus` from a string fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseRunStatusError;

impl std::fmt::Display for ParseRunStatusError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid run status")
    }
}

impl std::error::Error for ParseRunStatusError {}

/// State database for tracking automation runs with idempotency.
///
/// This uses a UNIQUE constraint on (automation_name, scheduled_for) to ensure
/// that the same scheduled run can never be executed twice. All scheduled_for
/// values are stored in UTC (RFC3339) to ensure the UNIQUE constraint is stable
/// across DST transitions.
#[derive(Debug, Clone)]
pub struct AutomationState {
    pool: SqlitePool,
}

impl AutomationState {
    /// Open or create state database at given path.
    ///
    /// # Errors
    ///
    /// Returns an error if the database cannot be created or opened.
    pub async fn open(path: &Path) -> Result<Self> {
        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.wrap_err_with(|| {
                format!(
                    "Failed to create state database directory: {}",
                    parent.display()
                )
            })?;
        }

        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true);

        let pool = SqlitePool::connect_with(options)
            .await
            .wrap_err_with(|| format!("Failed to open state database at {}", path.display()))?;

        // Enable WAL mode for better concurrency
        sqlx::query("PRAGMA journal_mode=WAL;")
            .execute(&pool)
            .await
            .wrap_err("Failed to enable WAL mode")?;

        // Create runs table with UNIQUE constraint for idempotency
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS runs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                automation_name TEXT NOT NULL,
                scheduled_for TEXT NOT NULL,
                status TEXT NOT NULL,
                started_at TEXT,
                finished_at TEXT,
                error_message TEXT,
                UNIQUE(automation_name, scheduled_for)
            )
            "#,
        )
        .execute(&pool)
        .await
        .wrap_err("Failed to create runs table")?;

        // Create index for faster queries
        sqlx::query(
            r#"
            CREATE INDEX IF NOT EXISTS idx_runs_automation
             ON runs(automation_name, scheduled_for DESC)
            "#,
        )
        .execute(&pool)
        .await
        .wrap_err("Failed to create index on runs table")?;

        Ok(Self { pool })
    }

    /// Attempt to start a run for an automation at a scheduled time.
    ///
    /// Returns `Ok(id)` if the run was started, `Err` if already running/completed.
    ///
    /// Stale runs (exceeded stale_timeout) are allowed to retry.
    ///
    /// # Errors
    ///
    /// Returns an error if the database operation fails.
    pub async fn try_start_run(
        &self,
        name: &str,
        scheduled_for: Timestamp,
        stale_timeout: Span,
    ) -> Result<i64> {
        let scheduled_str = scheduled_for.to_string();
        let started_at = Timestamp::now().to_string();
        let now = Timestamp::now();
        let stale_cutoff = now - stale_timeout;

        // Try to insert - UNIQUE constraint prevents duplicates
        match sqlx::query(
            r#"
            INSERT INTO runs (automation_name, scheduled_for, status, started_at)
            VALUES (?1, ?2, ?3, ?4)
            "#,
        )
        .bind(name)
        .bind(&scheduled_str)
        .bind(RunStatus::Running.as_str())
        .bind(&started_at)
        .execute(&self.pool)
        .await
        {
            Ok(result) => Ok(result.last_insert_rowid()),
            Err(sqlx::Error::Database(db_err)) if db_err.is_unique_violation() => {
                // Check if there's a stale running run
                if let Some((id, started_at_str)) = self.get_run_info(name, scheduled_for).await?
                    && let Ok(started) = started_at_str.parse::<Timestamp>()
                {
                    if started < stale_cutoff {
                        // Stale run - allow retry by updating it
                        sqlx::query(
                            r#"
                            UPDATE runs SET status = ?1, started_at = ?2, error_message = NULL
                             WHERE id = ?3
                            "#,
                        )
                        .bind(RunStatus::Running.as_str())
                        .bind(&started_at)
                        .bind(id)
                        .execute(&self.pool)
                        .await
                        .wrap_err("Failed to update stale run")?;
                        return Ok(id);
                    }
                }
                Err(eyre!(
                    "Run already exists for {} at {}",
                    name,
                    scheduled_for
                ))
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Mark a run as completed.
    ///
    /// # Errors
    ///
    /// Returns an error if the database operation fails.
    pub async fn complete_run(&self, id: i64) -> Result<()> {
        let finished_at = Timestamp::now().to_string();
        sqlx::query("UPDATE runs SET status = ?1, finished_at = ?2 WHERE id = ?3")
            .bind(RunStatus::Completed.as_str())
            .bind(&finished_at)
            .bind(id)
            .execute(&self.pool)
            .await
            .wrap_err_with(|| format!("Failed to mark run {} as completed", id))?;
        Ok(())
    }

    /// Mark a run as failed with an error message.
    ///
    /// # Errors
    ///
    /// Returns an error if the database operation fails.
    pub async fn fail_run(&self, id: i64, error: &str) -> Result<()> {
        let finished_at = Timestamp::now().to_string();
        sqlx::query(
            "UPDATE runs SET status = ?1, finished_at = ?2, error_message = ?3 WHERE id = ?4",
        )
        .bind(RunStatus::Failed.as_str())
        .bind(&finished_at)
        .bind(error)
        .bind(id)
        .execute(&self.pool)
        .await
        .wrap_err_with(|| format!("Failed to mark run {} as failed", id))?;
        Ok(())
    }

    /// Get the most recent completed run for an automation.
    ///
    /// Returns `Ok(None)` if no completed runs exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    pub async fn get_last_completed_run(&self, name: &str) -> Result<Option<Timestamp>> {
        let row = sqlx::query_as::<_, (String,)>(
            "SELECT scheduled_for FROM runs
             WHERE automation_name = ?1 AND status = ?2
             ORDER BY scheduled_for DESC
             LIMIT 1",
        )
        .bind(name)
        .bind(RunStatus::Completed.as_str())
        .fetch_optional(&self.pool)
        .await
        .wrap_err("Failed to query last completed run")?;

        match row {
            Some((s,)) => {
                let ts = s
                    .parse::<Timestamp>()
                    .wrap_err("Failed to parse scheduled_for timestamp")?;
                Ok(Some(ts))
            }
            None => Ok(None),
        }
    }

    /// Get run info for idempotency check.
    ///
    /// Returns `Ok(None)` if no run exists for the given automation and scheduled time.
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    async fn get_run_info(
        &self,
        name: &str,
        scheduled_for: Timestamp,
    ) -> Result<Option<(i64, String)>> {
        let scheduled_str = scheduled_for.to_string();

        let row = sqlx::query_as::<_, (i64, String)>(
            "SELECT id, started_at FROM runs
             WHERE automation_name = ?1 AND scheduled_for = ?2",
        )
        .bind(name)
        .bind(&scheduled_str)
        .fetch_optional(&self.pool)
        .await
        .wrap_err("Failed to query run info")?;

        Ok(row)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::ToSpan;
    use sqlx::Row;

    #[test]
    fn test_run_status_as_str() {
        assert_eq!(RunStatus::Pending.as_str(), "pending");
        assert_eq!(RunStatus::Running.as_str(), "running");
        assert_eq!(RunStatus::Completed.as_str(), "completed");
        assert_eq!(RunStatus::Failed.as_str(), "failed");
    }

    #[test]
    fn test_run_status_from_str() {
        assert_eq!(RunStatus::from_str("pending"), Ok(RunStatus::Pending));
        assert_eq!(RunStatus::from_str("running"), Ok(RunStatus::Running));
        assert_eq!(RunStatus::from_str("completed"), Ok(RunStatus::Completed));
        assert_eq!(RunStatus::from_str("failed"), Ok(RunStatus::Failed));
        assert_eq!(RunStatus::from_str("unknown"), Err(ParseRunStatusError));
    }

    #[tokio::test]
    async fn test_state_open_creates_tables() {
        let temp_dir = tempfile::TempDir::new().expect("tempdir should be created");
        let db_path = temp_dir.path().join("test.db");

        let state = AutomationState::open(&db_path)
            .await
            .expect("state should be created");

        // Verify tables exist by querying them
        let result =
            sqlx::query("SELECT name FROM sqlite_master WHERE type='table' AND name='runs'")
                .fetch_one(&state.pool)
                .await
                .expect("query should succeed");

        let name: String = result.try_get("name").expect("name column should exist");
        assert_eq!(name, "runs");
    }

    #[tokio::test]
    async fn test_try_start_run_new() {
        let temp_dir = tempfile::TempDir::new().expect("tempdir should be created");
        let db_path = temp_dir.path().join("test.db");

        let state = AutomationState::open(&db_path)
            .await
            .expect("state should be created");
        let scheduled_for = Timestamp::now();

        let result = state
            .try_start_run("test-automation", scheduled_for, 2_i64.hours())
            .await
            .expect("start_run should succeed");

        assert!(result > 0);
    }

    #[tokio::test]
    async fn test_try_start_run_idempotent() {
        let temp_dir = tempfile::TempDir::new().expect("tempdir should be created");
        let db_path = temp_dir.path().join("test.db");

        let state = AutomationState::open(&db_path)
            .await
            .expect("state should be created");
        let scheduled_for = Timestamp::now();

        let result1 = state
            .try_start_run("test-automation", scheduled_for, 2_i64.hours())
            .await
            .expect("first start_run should succeed");
        let result2 = state
            .try_start_run("test-automation", scheduled_for, 2_i64.hours())
            .await;

        assert!(result1 > 0);
        assert!(result2.is_err());
    }

    #[tokio::test]
    async fn test_try_start_run_stale_retry() {
        let temp_dir = tempfile::TempDir::new().expect("tempdir should be created");
        let db_path = temp_dir.path().join("test.db");

        let state = AutomationState::open(&db_path)
            .await
            .expect("state should be created");
        let scheduled_for = Timestamp::now();

        // Start a run
        let id1 = state
            .try_start_run("test-automation", scheduled_for, 2_i64.hours())
            .await
            .expect("first start_run should succeed");

        // Manually set started_at to be in the past (stale)
        let stale_time = Timestamp::now() - 3_i64.hours();
        sqlx::query("UPDATE runs SET started_at = ?1 WHERE id = ?2")
            .bind(stale_time.to_string())
            .bind(id1)
            .execute(&state.pool)
            .await
            .expect("should update started_at");

        // Try to start again - should succeed due to stale run
        let id2 = state
            .try_start_run("test-automation", scheduled_for, 2_i64.hours())
            .await
            .expect("second start_run should succeed with stale retry");

        // Should return the same id (updated)
        assert_eq!(id1, id2);
    }

    #[tokio::test]
    async fn test_complete_run() {
        let temp_dir = tempfile::TempDir::new().expect("tempdir should be created");
        let db_path = temp_dir.path().join("test.db");

        let state = AutomationState::open(&db_path)
            .await
            .expect("state should be created");
        let scheduled_for = Timestamp::now();

        let id = state
            .try_start_run("test-automation", scheduled_for, 2_i64.hours())
            .await
            .expect("start_run should succeed");

        state
            .complete_run(id)
            .await
            .expect("complete_run should succeed");

        // Verify status is completed
        let result = sqlx::query("SELECT status FROM runs WHERE id = ?1")
            .bind(id)
            .fetch_one(&state.pool)
            .await
            .expect("query should succeed");

        let status: String = result
            .try_get("status")
            .expect("status column should exist");
        assert_eq!(status, "completed");

        // Verify finished_at is set
        let result = sqlx::query("SELECT finished_at FROM runs WHERE id = ?1")
            .bind(id)
            .fetch_one(&state.pool)
            .await
            .expect("query should succeed");

        let finished_at: Option<String> = result
            .try_get("finished_at")
            .expect("finished_at should exist");
        assert!(finished_at.is_some());
    }

    #[tokio::test]
    async fn test_fail_run() {
        let temp_dir = tempfile::TempDir::new().expect("tempdir should be created");
        let db_path = temp_dir.path().join("test.db");

        let state = AutomationState::open(&db_path)
            .await
            .expect("state should be created");
        let scheduled_for = Timestamp::now();

        let id = state
            .try_start_run("test-automation", scheduled_for, 2_i64.hours())
            .await
            .expect("start_run should succeed");

        state
            .fail_run(id, "Test error message")
            .await
            .expect("fail_run should succeed");

        // Verify status is failed
        let result = sqlx::query("SELECT status FROM runs WHERE id = ?1")
            .bind(id)
            .fetch_one(&state.pool)
            .await
            .expect("query should succeed");

        let status: String = result
            .try_get("status")
            .expect("status column should exist");
        assert_eq!(status, "failed");

        // Verify error message
        let result = sqlx::query("SELECT error_message FROM runs WHERE id = ?1")
            .bind(id)
            .fetch_one(&state.pool)
            .await
            .expect("query should succeed");

        let error_msg: String = result
            .try_get("error_message")
            .expect("error_message column should exist");
        assert_eq!(error_msg, "Test error message");
    }

    #[tokio::test]
    async fn test_get_last_completed_run_none() {
        let temp_dir = tempfile::TempDir::new().expect("tempdir should be created");
        let db_path = temp_dir.path().join("test.db");

        let state = AutomationState::open(&db_path)
            .await
            .expect("state should be created");

        let result = state
            .get_last_completed_run("nonexistent")
            .await
            .expect("query should succeed");

        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_get_last_completed_run_some() {
        let temp_dir = tempfile::TempDir::new().expect("tempdir should be created");
        let db_path = temp_dir.path().join("test.db");

        let state = AutomationState::open(&db_path)
            .await
            .expect("state should be created");

        // Create multiple runs at different times
        let scheduled1 = Timestamp::now() - 3_i64.hours();
        let scheduled2 = Timestamp::now() - 2_i64.hours();
        let scheduled3 = Timestamp::now() - 1_i64.hours();

        let id1 = state
            .try_start_run("test-automation", scheduled1, 2_i64.hours())
            .await
            .expect("start_run should succeed");
        let _id2 = state
            .try_start_run("test-automation", scheduled2, 2_i64.hours())
            .await
            .expect("start_run should succeed");
        let _id3 = state
            .try_start_run("test-automation", scheduled3, 2_i64.hours())
            .await
            .expect("start_run should succeed");

        // Complete only run 1
        state
            .complete_run(id1)
            .await
            .expect("complete_run should succeed");
        // Leave runs 2 and 3 running

        let last_run = state
            .get_last_completed_run("test-automation")
            .await
            .expect("query should succeed");

        assert_eq!(last_run, Some(scheduled1));
    }

    #[tokio::test]
    async fn test_unique_constraint_prevents_duplicates() {
        let temp_dir = tempfile::TempDir::new().expect("tempdir should be created");
        let db_path = temp_dir.path().join("test.db");

        let state = AutomationState::open(&db_path)
            .await
            .expect("state should be created");
        let scheduled_for = Timestamp::now();

        // First run should succeed
        state
            .try_start_run("test-automation", scheduled_for, 2_i64.hours())
            .await
            .expect("first start_run should succeed");

        // Complete the run
        let result = sqlx::query("SELECT id FROM runs WHERE automation_name = ?1")
            .bind("test-automation")
            .fetch_one(&state.pool)
            .await
            .expect("query should succeed");

        let id: i64 = result.try_get("id").expect("id column should exist");

        state
            .complete_run(id)
            .await
            .expect("complete_run should succeed");

        // Try to start the same run again - should fail due to UNIQUE constraint
        let result = state
            .try_start_run("test-automation", scheduled_for, 2_i64.hours())
            .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_different_automations_same_time() {
        let temp_dir = tempfile::TempDir::new().expect("tempdir should be created");
        let db_path = temp_dir.path().join("test.db");

        let state = AutomationState::open(&db_path)
            .await
            .expect("state should be created");
        let scheduled_for = Timestamp::now();

        // Two different automations at the same time should both succeed
        let id1 = state
            .try_start_run("automation-1", scheduled_for, 2_i64.hours())
            .await
            .expect("first start_run should succeed");
        let id2 = state
            .try_start_run("automation-2", scheduled_for, 2_i64.hours())
            .await
            .expect("second start_run should succeed");

        assert_ne!(id1, id2);
    }

    #[tokio::test]
    async fn test_same_automation_different_times() {
        let temp_dir = tempfile::TempDir::new().expect("tempdir should be created");
        let db_path = temp_dir.path().join("test.db");

        let state = AutomationState::open(&db_path)
            .await
            .expect("state should be created");

        let scheduled1 = Timestamp::now();
        let scheduled2 = Timestamp::now() + 1_i64.hours();

        // Same automation at different times should both succeed
        let id1 = state
            .try_start_run("test-automation", scheduled1, 2_i64.hours())
            .await
            .expect("first start_run should succeed");
        let id2 = state
            .try_start_run("test-automation", scheduled2, 2_i64.hours())
            .await
            .expect("second start_run should succeed");

        assert_ne!(id1, id2);
    }
}
