//! Async SQLite storage for automation execution history.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, SqlitePool, sqlite::SqliteConnectOptions};

/// Status of an automation run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[repr(i32)]
pub enum AutomationRunStatus {
    /// The run has been scheduled but not yet started.
    Pending = 0,
    /// The run is currently in progress.
    Running = 1,
    /// The run completed successfully.
    Completed = 2,
    /// The run failed with an error.
    Failed = 3,
    /// The run was cancelled before completion.
    Cancelled = 4,
}

impl AutomationRunStatus {
    /// Returns `true` if this status is terminal (no further transitions possible).
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }

    /// Returns the string representation of this status.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "Pending",
            Self::Running => "Running",
            Self::Completed => "Completed",
            Self::Failed => "Failed",
            Self::Cancelled => "Cancelled",
        }
    }

    /// Parses a status string into an `AutomationRunStatus`.
    #[must_use]
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
    /// Unique identifier for this run.
    pub id: i64,
    /// Name of the automation that was run.
    pub automation_name: String,
    /// When this run was scheduled to occur.
    pub scheduled_for: DateTime<Utc>,
    /// When this run actually started.
    pub started_at: DateTime<Utc>,
    /// When this run completed (if terminal).
    pub completed_at: Option<DateTime<Utc>>,
    /// The current status of this run.
    pub status: AutomationRunStatus,
    /// Number of iterations completed before termination.
    pub iterations_completed: u32,
    /// Exit code from the automation process (if available).
    pub exit_code: Option<i32>,
    /// Duration of the run in milliseconds (if terminal).
    pub duration_ms: Option<i64>,
    /// Standard output from the automation run (truncated if needed).
    pub output: Option<String>,
    /// Standard error from the automation run (if any).
    pub error: Option<String>,
}

/// Async storage for automation runs.
#[derive(Debug, Clone)]
pub struct AutomationStore {
    pool: SqlitePool,
}

impl AutomationStore {
    /// Create or open the database at the given path.
    ///
    /// # Errors
    ///
    /// Returns an error if the database cannot be created or opened.
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

    /// Initialize the database schema.
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
    ///
    /// # Errors
    ///
    /// Returns an error if the insert fails.
    pub async fn insert_run(
        &self,
        automation_name: &str,
        scheduled_for: DateTime<Utc>,
        started_at: DateTime<Utc>,
    ) -> color_eyre::Result<i64> {
        let result = sqlx::query(
            "INSERT INTO automation_runs (automation_name, scheduled_for, started_at, status)
             VALUES (?1, ?2, ?3, ?4)",
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
    ///
    /// # Errors
    ///
    /// Returns an error if the update fails.
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
            if let Ok(started_str) = sqlx::query_scalar::<_, String>(
                "SELECT started_at FROM automation_runs WHERE id = ?1",
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
             WHERE id = ?8",
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
    ///
    /// # Errors
    ///
    /// Returns an error if the lock operation fails.
    pub async fn try_acquire_lock(
        &self,
        automation_name: &str,
        run_id: i64,
    ) -> color_eyre::Result<Option<i64>> {
        loop {
            let result = sqlx::query(
                "INSERT INTO automation_locks (automation_name, locked_at, run_id)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(automation_name) DO NOTHING",
            )
            .bind(automation_name)
            .bind(Utc::now().to_rfc3339())
            .bind(run_id)
            .execute(&self.pool)
            .await?;

            if result.rows_affected() > 0 {
                return Ok(Some(run_id));
            }

            // Check if existing lock is stale (> 2 hours)
            let locked_at: Option<String> = sqlx::query_scalar(
                "SELECT locked_at FROM automation_locks WHERE automation_name = ?1",
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
                        continue;
                    }
                }
            }
            return Ok(None);
        }
    }

    /// Release the lock for an automation.
    ///
    /// # Errors
    ///
    /// Returns an error if the release fails.
    pub async fn release_lock(&self, automation_name: &str) -> color_eyre::Result<()> {
        sqlx::query("DELETE FROM automation_locks WHERE automation_name = ?1")
            .bind(automation_name)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Get recent runs, optionally filtered by automation name.
    ///
    /// # Errors
    ///
    /// Returns an error if the query fails.
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
                 LIMIT ?2",
            )
            .bind(name)
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query_as::<_, AutomationRunRow>(
                "SELECT * FROM automation_runs
                 ORDER BY started_at DESC
                 LIMIT ?1",
            )
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await?
        };

        rows.into_iter().map(|r| r.try_into()).collect()
    }
}

/// Raw row from database.
#[derive(FromRow)]
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

        let completed_at = row
            .completed_at
            .map(|s| {
                DateTime::parse_from_rfc3339(&s)
                    .map_err(|e| color_eyre::eyre::eyre!("Invalid completed_at RFC3339: {}", e))
            })
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

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::Row;
    use tempfile::TempDir;

    #[test]
    fn test_status_is_terminal() {
        assert!(AutomationRunStatus::Completed.is_terminal());
        assert!(AutomationRunStatus::Failed.is_terminal());
        assert!(AutomationRunStatus::Cancelled.is_terminal());
        assert!(!AutomationRunStatus::Pending.is_terminal());
        assert!(!AutomationRunStatus::Running.is_terminal());
    }

    #[test]
    fn test_status_as_str() {
        assert_eq!(AutomationRunStatus::Pending.as_str(), "Pending");
        assert_eq!(AutomationRunStatus::Running.as_str(), "Running");
        assert_eq!(AutomationRunStatus::Completed.as_str(), "Completed");
        assert_eq!(AutomationRunStatus::Failed.as_str(), "Failed");
        assert_eq!(AutomationRunStatus::Cancelled.as_str(), "Cancelled");
    }

    #[test]
    fn test_status_from_str() {
        assert_eq!(
            AutomationRunStatus::from_str("Pending"),
            Some(AutomationRunStatus::Pending)
        );
        assert_eq!(
            AutomationRunStatus::from_str("Running"),
            Some(AutomationRunStatus::Running)
        );
        assert_eq!(
            AutomationRunStatus::from_str("Completed"),
            Some(AutomationRunStatus::Completed)
        );
        assert_eq!(
            AutomationRunStatus::from_str("Failed"),
            Some(AutomationRunStatus::Failed)
        );
        assert_eq!(
            AutomationRunStatus::from_str("Cancelled"),
            Some(AutomationRunStatus::Cancelled)
        );
        assert_eq!(AutomationRunStatus::from_str("Invalid"), None);
    }

    #[tokio::test]
    async fn test_store_open_and_init() {
        let temp_dir = TempDir::new().expect("tempdir should be created");
        let db_path = temp_dir.path().join("test.db");

        let store = AutomationStore::open(&db_path)
            .await
            .expect("store should be created");

        // Verify tables exist by querying them
        let result = sqlx::query("SELECT name FROM sqlite_master WHERE type='table'")
            .fetch_all(&store.pool)
            .await
            .expect("query should succeed");

        let table_names: Vec<_> = result.iter().filter_map(|r| r.get(0)).collect();
        assert!(table_names.contains(&"automation_runs"));
        assert!(table_names.contains(&"automation_locks"));
    }

    #[tokio::test]
    async fn test_insert_and_get_run() {
        let temp_dir = TempDir::new().expect("tempdir should be created");
        let db_path = temp_dir.path().join("test.db");

        let store = AutomationStore::open(&db_path)
            .await
            .expect("store should be created");

        let scheduled_for = Utc::now();
        let started_at = Utc::now();

        let run_id = store
            .insert_run("test-automation", scheduled_for, started_at)
            .await
            .expect("insert should succeed");

        assert!(run_id > 0);

        let runs = store
            .get_runs(Some("test-automation"), 10)
            .await
            .expect("get_runs should succeed");

        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].automation_name, "test-automation");
        assert_eq!(runs[0].status, AutomationRunStatus::Pending);
    }

    #[tokio::test]
    async fn test_update_run_status() {
        let temp_dir = TempDir::new().expect("tempdir should be created");
        let db_path = temp_dir.path().join("test.db");

        let store = AutomationStore::open(&db_path)
            .await
            .expect("store should be created");

        let run_id = store
            .insert_run("test-automation", Utc::now(), Utc::now())
            .await
            .expect("insert should succeed");

        store
            .update_run(
                run_id,
                AutomationRunStatus::Completed,
                5,
                Some(0),
                Some("test output"),
                None,
            )
            .await
            .expect("update should succeed");

        let runs = store
            .get_runs(Some("test-automation"), 10)
            .await
            .expect("get_runs should succeed");

        assert_eq!(runs[0].status, AutomationRunStatus::Completed);
        assert_eq!(runs[0].iterations_completed, 5);
        assert_eq!(runs[0].exit_code, Some(0));
        assert_eq!(runs[0].output, Some("test output".to_string()));
        assert!(runs[0].completed_at.is_some());
        assert!(runs[0].duration_ms.is_some());
    }

    #[tokio::test]
    async fn test_lock_acquire_and_release() {
        let temp_dir = TempDir::new().expect("tempdir should be created");
        let db_path = temp_dir.path().join("test.db");

        let store = AutomationStore::open(&db_path)
            .await
            .expect("store should be created");

        // First acquisition should succeed
        let lock1 = store
            .try_acquire_lock("test-automation", 1)
            .await
            .expect("acquire should succeed");

        assert_eq!(lock1, Some(1));

        // Second acquisition should fail (same automation)
        let lock2 = store
            .try_acquire_lock("test-automation", 2)
            .await
            .expect("acquire should succeed");

        assert_eq!(lock2, None);

        // After release, acquisition should succeed again
        store
            .release_lock("test-automation")
            .await
            .expect("release should succeed");

        let lock3 = store
            .try_acquire_lock("test-automation", 3)
            .await
            .expect("acquire should succeed");

        assert_eq!(lock3, Some(3));
    }

    #[tokio::test]
    async fn test_get_runs_limits() {
        let temp_dir = TempDir::new().expect("tempdir should be created");
        let db_path = temp_dir.path().join("test.db");

        let store = AutomationStore::open(&db_path)
            .await
            .expect("store should be created");

        // Insert 5 runs
        for _ in 0..5 {
            store
                .insert_run("test-automation", Utc::now(), Utc::now())
                .await
                .expect("insert should succeed");
        }

        // Request only 3
        let runs = store
            .get_runs(Some("test-automation"), 3)
            .await
            .expect("get_runs should succeed");

        assert_eq!(runs.len(), 3);
    }
}
