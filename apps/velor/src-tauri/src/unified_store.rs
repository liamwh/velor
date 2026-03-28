//! Unified SQLite storage for sessions, automations, and projects.
//!
//! This module provides a single database for all persistent data, combining:
//! - Session history (from session_store.rs)
//! - Automation runs and locks (from automations crate store.rs)
//!
//! The unified store supports migration from legacy databases (sessions.db, automations.db).

use chrono::{DateTime, Utc};
use color_eyre::Result;
use color_eyre::eyre::WrapErr;
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, SqlitePool, sqlite::SqliteConnectOptions};
use std::path::Path;
use std::str::FromStr;
use tracing::{debug, info, instrument, warn};

use velor_core::{
    ExecutionConfig, ExecutionEvent, ExecutionId, ExecutionMetrics, ExecutionRecord, ExecutionState,
};

// ============================================================================
// Session Types
// ============================================================================

/// Parse an execution state from its string label.
///
/// Returns `None` if the string doesn't match a valid state.
fn parse_execution_state(s: &str) -> Option<ExecutionState> {
    match s {
        "Pending" => Some(ExecutionState::Pending),
        "Rendering" => Some(ExecutionState::Rendering),
        "Running" => Some(ExecutionState::Running),
        "Retrying" => Some(ExecutionState::Retrying),
        "Completed" => Some(ExecutionState::Completed),
        "Failed" => Some(ExecutionState::Failed),
        "Cancelled" => Some(ExecutionState::Cancelled),
        _ => None,
    }
}

/// Discover the git root directory from a given path.
///
/// Returns `None` if the path is not within a git repository.
async fn discover_git_root_from_path(path: &str) -> Option<String> {
    let path = std::path::Path::new(path);
    velor_core::git::discover_git_root(path)
        .ok()
        .map(|p| p.to_string_lossy().to_string())
}

/// Statistics about sessions.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionStats {
    /// Total number of sessions.
    pub total: u64,
    /// Number of completed sessions.
    pub completed: u64,
    /// Number of failed sessions.
    pub failed: u64,
    /// Number of cancelled sessions.
    pub cancelled: u64,
    /// Number of active (non-terminal) sessions.
    pub active: u64,
}

/// Project metadata for organizing sessions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    /// Unique path to the project (git root).
    pub path: String,
    /// User-editable display name.
    pub display_name: String,
    /// Whether this project is hidden from the sidebar.
    pub hidden: bool,
    /// Sort order for display (lower numbers appear first).
    pub sort_order: i64,
    /// Number of sessions associated with this project.
    pub session_count: u64,
}

// ============================================================================
// Automation Types
// ============================================================================

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
}

impl FromStr for AutomationRunStatus {
    type Err = ParseAutomationRunStatusError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "Pending" => Ok(Self::Pending),
            "Running" => Ok(Self::Running),
            "Completed" => Ok(Self::Completed),
            "Failed" => Ok(Self::Failed),
            "Cancelled" => Ok(Self::Cancelled),
            _ => Err(ParseAutomationRunStatusError),
        }
    }
}

/// Error returned when parsing an `AutomationRunStatus` from a string fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseAutomationRunStatusError;

impl std::fmt::Display for ParseAutomationRunStatusError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid automation run status")
    }
}

impl std::error::Error for ParseAutomationRunStatusError {}

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

// ============================================================================
// Database Row Types
// ============================================================================

/// Raw session row from database.
#[derive(FromRow)]
#[allow(dead_code)]
struct SessionRow {
    id: String,
    prompt_name: String,
    state: String,
    started_at: String,
    ended_at: Option<String>,
    config_json: String,
    metrics_json: String,
    output: Option<String>,
    error: Option<String>,
    name: Option<String>,
    pinned: i64,
    project_path: Option<String>,
}

/// Raw event row from database.
#[derive(FromRow)]
struct EventRow {
    event_type: String,
    event_data: String,
    timestamp: String,
}

/// Raw event row from database with session_id (for migration).
#[derive(FromRow)]
struct EventRowWithSession {
    session_id: String,
    event_type: String,
    event_data: String,
    timestamp: String,
}

/// Raw project row from database.
#[derive(FromRow)]
struct ProjectRow {
    path: String,
    display_name: Option<String>,
    hidden: i64,
    sort_order: i64,
    session_count: i64,
}

/// Raw automation run row from database.
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

// ============================================================================
// Unified Store
// ============================================================================

/// Unified async SQLite storage for all application data.
#[derive(Debug, Clone)]
pub struct UnifiedStore {
    pool: SqlitePool,
}

impl UnifiedStore {
    /// Create or open the unified database at the given path.
    ///
    /// This method also handles migration from legacy databases if they exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the database cannot be created or opened.
    #[instrument(level = "debug")]
    pub async fn open(velor_dir: &Path) -> Result<Self> {
        debug!(?velor_dir, "Opening unified store");

        // Ensure parent directory exists
        tokio::fs::create_dir_all(velor_dir)
            .await
            .wrap_err_with(|| format!("Failed to create velor directory: {:?}", velor_dir))?;

        let db_path = velor_dir.join("velor.db");

        // Check for legacy databases
        let sessions_db = velor_dir.join("sessions.db");
        let automations_db = velor_dir.join("automations.db");
        let needs_migration = sessions_db.exists() || automations_db.exists();

        let options = SqliteConnectOptions::new()
            .filename(&db_path)
            .create_if_missing(true);

        let pool = SqlitePool::connect_with(options)
            .await
            .wrap_err("Failed to connect to unified store database")?;

        // Enable WAL mode for better concurrency
        sqlx::query("PRAGMA journal_mode=WAL;")
            .execute(&pool)
            .await
            .wrap_err("Failed to enable WAL mode")?;

        let store = Self { pool };
        store.init_schema().await?;

        // Migrate from legacy databases if they exist
        if needs_migration {
            store.migrate_from_legacy(velor_dir).await?;
        }

        debug!("Unified store opened successfully");
        Ok(store)
    }

    /// Initialize the database schema with all tables.
    async fn init_schema(&self) -> Result<()> {
        // Sessions table
        let sessions_table = r#"
            CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                prompt_name TEXT NOT NULL,
                state TEXT NOT NULL,
                started_at TEXT NOT NULL,
                ended_at TEXT,
                config_json TEXT NOT NULL,
                metrics_json TEXT,
                output TEXT,
                error TEXT,
                automation_name TEXT,
                automation_run_id INTEGER,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                name TEXT,
                pinned INTEGER NOT NULL DEFAULT 0,
                project_path TEXT
            );
        "#;

        sqlx::query(sessions_table)
            .execute(&self.pool)
            .await
            .wrap_err("Failed to create sessions table")?;

        // Session events table
        let events_table = r#"
            CREATE TABLE IF NOT EXISTS session_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                event_type TEXT NOT NULL,
                event_data TEXT NOT NULL,
                timestamp TEXT NOT NULL,
                FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
            );
        "#;

        sqlx::query(events_table)
            .execute(&self.pool)
            .await
            .wrap_err("Failed to create session_events table")?;

        // Projects table
        let projects_table = r#"
            CREATE TABLE IF NOT EXISTS projects (
                path TEXT PRIMARY KEY,
                display_name TEXT NOT NULL DEFAULT '',
                hidden INTEGER NOT NULL DEFAULT 0,
                sort_order INTEGER NOT NULL DEFAULT 0
            );
        "#;

        sqlx::query(projects_table)
            .execute(&self.pool)
            .await
            .wrap_err("Failed to create projects table")?;

        // Automation runs table
        let automation_runs_table = r#"
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
        "#;

        sqlx::query(automation_runs_table)
            .execute(&self.pool)
            .await
            .wrap_err("Failed to create automation_runs table")?;

        // Automation locks table
        let automation_locks_table = r#"
            CREATE TABLE IF NOT EXISTS automation_locks (
                automation_name TEXT PRIMARY KEY,
                locked_at TEXT NOT NULL,
                run_id INTEGER
            );
        "#;

        sqlx::query(automation_locks_table)
            .execute(&self.pool)
            .await
            .wrap_err("Failed to create automation_locks table")?;

        // Run migrations for existing databases
        self.migrate_sessions_table().await?;
        self.migrate_projects_table().await?;

        // Create indexes
        let indexes = r#"
            CREATE INDEX IF NOT EXISTS idx_sessions_started_at ON sessions(started_at DESC);
            CREATE INDEX IF NOT EXISTS idx_sessions_state ON sessions(state);
            CREATE INDEX IF NOT EXISTS idx_sessions_automation_name ON sessions(automation_name);
            CREATE INDEX IF NOT EXISTS idx_session_events_session_id ON session_events(session_id);
            CREATE INDEX IF NOT EXISTS idx_sessions_project_path ON sessions(project_path);
            CREATE INDEX IF NOT EXISTS idx_sessions_pinned ON sessions(pinned);
            CREATE INDEX IF NOT EXISTS idx_projects_sort_order ON projects(sort_order);
            CREATE INDEX IF NOT EXISTS idx_automation_name ON automation_runs(automation_name);
            CREATE INDEX IF NOT EXISTS idx_started_at_runs ON automation_runs(started_at);
        "#;

        sqlx::query(indexes)
            .execute(&self.pool)
            .await
            .wrap_err("Failed to create indexes")?;

        Ok(())
    }

    /// Migrate sessions table to add new columns.
    async fn migrate_sessions_table(&self) -> Result<()> {
        let migrations = vec![
            "ALTER TABLE sessions ADD COLUMN name TEXT;",
            "ALTER TABLE sessions ADD COLUMN pinned INTEGER NOT NULL DEFAULT 0;",
            "ALTER TABLE sessions ADD COLUMN project_path TEXT;",
        ];

        for migration in migrations {
            let result = sqlx::query(migration).execute(&self.pool).await;
            if let Err(e) = result {
                let error_msg = e.to_string();
                if error_msg.contains("duplicate column") {
                    debug!("Column already exists, skipping migration");
                } else {
                    debug!("Migration skipped: {}", error_msg);
                }
            } else {
                debug!("Applied migration: {}", migration.trim());
            }
        }

        Ok(())
    }

    /// Migrate projects table.
    async fn migrate_projects_table(&self) -> Result<()> {
        Ok(())
    }

    /// Migrate data from legacy databases.
    ///
    /// This copies data from sessions.db and automations.db into the unified
    /// velor.db, then renames the old files to .bak.
    #[instrument(skip(self), level = "debug")]
    async fn migrate_from_legacy(&self, velor_dir: &Path) -> Result<()> {
        info!("Starting migration from legacy databases");

        let sessions_db = velor_dir.join("sessions.db");
        let automations_db = velor_dir.join("automations.db");

        // Migrate sessions.db if it exists
        if sessions_db.exists() {
            self.migrate_sessions_db(&sessions_db).await?;
            self.rename_to_backup(&sessions_db).await?;
        }

        // Migrate automations.db if it exists
        if automations_db.exists() {
            self.migrate_automations_db(&automations_db).await?;
            self.rename_to_backup(&automations_db).await?;
        }

        info!("Migration from legacy databases completed");
        Ok(())
    }

    /// Migrate data from legacy sessions.db.
    async fn migrate_sessions_db(&self, sessions_db: &Path) -> Result<()> {
        info!(path = ?sessions_db, "Migrating sessions.db");

        // Connect to legacy database
        let options = SqliteConnectOptions::new()
            .filename(sessions_db)
            .create_if_missing(false);

        let legacy_pool = SqlitePool::connect_with(options)
            .await
            .wrap_err("Failed to connect to legacy sessions.db")?;

        // Check if there's data to migrate
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions")
            .fetch_one(&legacy_pool)
            .await
            .unwrap_or(0);

        if count == 0 {
            debug!("No sessions to migrate");
            return Ok(());
        }

        // Migrate sessions
        let sessions: Vec<SessionRow> = sqlx::query_as("SELECT * FROM sessions")
            .fetch_all(&legacy_pool)
            .await
            .wrap_err("Failed to read sessions from legacy database")?;

        let sessions_count = sessions.len();

        for session in sessions {
            // Check if session already exists
            let exists: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM sessions WHERE id = ?1")
                .bind(&session.id)
                .fetch_optional(&self.pool)
                .await?;

            if exists.is_none() {
                sqlx::query(
                    "INSERT INTO sessions (id, prompt_name, state, started_at, ended_at, config_json, metrics_json, output, error, name, pinned, project_path)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                )
                .bind(&session.id)
                .bind(&session.prompt_name)
                .bind(&session.state)
                .bind(&session.started_at)
                .bind(&session.ended_at)
                .bind(&session.config_json)
                .bind(&session.metrics_json)
                .bind(&session.output)
                .bind(&session.error)
                .bind(&session.name)
                .bind(session.pinned)
                .bind(&session.project_path)
                .execute(&self.pool)
                .await
                .wrap_err_with(|| format!("Failed to migrate session {}", session.id))?;
            }
        }

        // Migrate session events
        let events: Vec<EventRowWithSession> = sqlx::query_as(
            "SELECT session_id, event_type, event_data, timestamp FROM session_events",
        )
        .fetch_all(&legacy_pool)
        .await
        .wrap_err("Failed to read session events from legacy database")?;

        for event in events {
            sqlx::query(
                "INSERT INTO session_events (session_id, event_type, event_data, timestamp)
                 VALUES (?1, ?2, ?3, ?4)",
            )
            .bind(&event.session_id)
            .bind(&event.event_type)
            .bind(&event.event_data)
            .bind(&event.timestamp)
            .execute(&self.pool)
            .await
            .ok(); // Ignore errors for duplicate events
        }

        // Migrate projects
        let projects: Vec<ProjectRow> = sqlx::query_as(
            "SELECT path, display_name, hidden, sort_order, 0 as session_count FROM projects",
        )
        .fetch_all(&legacy_pool)
        .await
        .wrap_err("Failed to read projects from legacy database")?;

        for project in projects {
            sqlx::query(
                "INSERT INTO projects (path, display_name, hidden, sort_order)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(path) DO NOTHING",
            )
            .bind(&project.path)
            .bind(&project.display_name)
            .bind(project.hidden)
            .bind(project.sort_order)
            .execute(&self.pool)
            .await
            .ok();
        }

        info!(
            count = sessions_count,
            "Migrated sessions from legacy database"
        );
        Ok(())
    }

    /// Migrate data from legacy automations.db.
    async fn migrate_automations_db(&self, automations_db: &Path) -> Result<()> {
        info!(path = ?automations_db, "Migrating automations.db");

        let options = SqliteConnectOptions::new()
            .filename(automations_db)
            .create_if_missing(false);

        let legacy_pool = SqlitePool::connect_with(options)
            .await
            .wrap_err("Failed to connect to legacy automations.db")?;

        // Check if there's data to migrate
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM automation_runs")
            .fetch_one(&legacy_pool)
            .await
            .unwrap_or(0);

        if count == 0 {
            debug!("No automation runs to migrate");
            return Ok(());
        }

        // Migrate automation runs
        let runs: Vec<AutomationRunRow> = sqlx::query_as("SELECT * FROM automation_runs")
            .fetch_all(&legacy_pool)
            .await
            .wrap_err("Failed to read automation runs from legacy database")?;

        let runs_count = runs.len();

        for run in runs {
            // Check if run already exists (by matching all fields since id is autoincrement)
            let exists: Option<(i64,)> = sqlx::query_as(
                "SELECT 1 FROM automation_runs WHERE automation_name = ?1 AND started_at = ?2",
            )
            .bind(&run.automation_name)
            .bind(&run.started_at)
            .fetch_optional(&self.pool)
            .await?;

            if exists.is_none() {
                sqlx::query(
                    "INSERT INTO automation_runs (automation_name, scheduled_for, started_at, completed_at, status, iterations_completed, exit_code, duration_ms, output, error)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                )
                .bind(&run.automation_name)
                .bind(&run.scheduled_for)
                .bind(&run.started_at)
                .bind(&run.completed_at)
                .bind(run.status)
                .bind(run.iterations_completed)
                .bind(run.exit_code)
                .bind(run.duration_ms)
                .bind(&run.output)
                .bind(&run.error)
                .execute(&self.pool)
                .await
                .wrap_err_with(|| format!("Failed to migrate automation run for {}", run.automation_name))?;
            }
        }

        // Migrate locks
        let locks: Vec<(String, String, Option<i64>)> =
            sqlx::query_as("SELECT automation_name, locked_at, run_id FROM automation_locks")
                .fetch_all(&legacy_pool)
                .await
                .wrap_err("Failed to read automation locks from legacy database")?;

        for (automation_name, locked_at, run_id) in locks {
            sqlx::query(
                "INSERT INTO automation_locks (automation_name, locked_at, run_id)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(automation_name) DO NOTHING",
            )
            .bind(&automation_name)
            .bind(&locked_at)
            .bind(run_id)
            .execute(&self.pool)
            .await
            .ok();
        }

        info!(
            count = runs_count,
            "Migrated automation runs from legacy database"
        );
        Ok(())
    }

    /// Rename a file to .bak suffix.
    async fn rename_to_backup(&self, path: &Path) -> Result<()> {
        let backup_path = path.with_extension("db.bak");
        tokio::fs::rename(path, &backup_path)
            .await
            .wrap_err_with(|| format!("Failed to rename {:?} to {:?}", path, backup_path))?;
        info!(from = ?path, to = ?backup_path, "Renamed legacy database to backup");
        Ok(())
    }

    // ========================================================================
    // Session Methods
    // ========================================================================

    /// Insert a new session record.
    ///
    /// # Errors
    ///
    /// Returns an error if the insert fails.
    #[instrument(skip(self, record), level = "debug")]
    pub async fn insert_session(&self, record: &ExecutionRecord) -> Result<()> {
        let config_json = serde_json::to_string(&record.config)
            .wrap_err("Failed to serialize ExecutionConfig")?;
        let metrics_json = serde_json::to_string(&record.metrics)
            .wrap_err("Failed to serialize ExecutionMetrics")?;

        let project_path = if record.project_path.is_some() {
            record.project_path.clone()
        } else {
            discover_git_root_from_path(&record.config.cwd).await
        };

        sqlx::query(
            "INSERT INTO sessions (id, prompt_name, state, started_at, ended_at, config_json, metrics_json, output, error, name, pinned, project_path)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        )
        .bind(record.id.as_str())
        .bind(&record.config.prompt_name)
        .bind(record.state.label())
        .bind(record.started_at.to_rfc3339())
        .bind(record.ended_at.map(|t| t.to_rfc3339()))
        .bind(&config_json)
        .bind(&metrics_json)
        .bind(&record.output)
        .bind(&record.error)
        .bind(&record.name)
        .bind(record.pinned as i64)
        .bind(&project_path)
        .execute(&self.pool)
        .await
        .wrap_err("Failed to insert session")?;

        debug!(id = %record.id, "Session inserted");
        Ok(())
    }

    /// Update an existing session record.
    ///
    /// # Errors
    ///
    /// Returns an error if the update fails.
    #[instrument(skip(self, record), level = "debug")]
    pub async fn update_session(&self, record: &ExecutionRecord) -> Result<()> {
        let metrics_json = serde_json::to_string(&record.metrics)
            .wrap_err("Failed to serialize ExecutionMetrics")?;

        let ended_at = record.ended_at.map(|t| t.to_rfc3339());

        sqlx::query(
            "UPDATE sessions
             SET state = ?1, ended_at = ?2, metrics_json = ?3, output = ?4, error = ?5, name = ?6, pinned = ?7, project_path = ?8, updated_at = datetime('now')
             WHERE id = ?9",
        )
        .bind(record.state.label())
        .bind(ended_at)
        .bind(&metrics_json)
        .bind(&record.output)
        .bind(&record.error)
        .bind(&record.name)
        .bind(record.pinned as i64)
        .bind(&record.project_path)
        .bind(record.id.as_str())
        .execute(&self.pool)
        .await
        .wrap_err("Failed to update session")?;

        debug!(id = %record.id, "Session updated");
        Ok(())
    }

    /// Get a session by ID.
    ///
    /// # Errors
    ///
    /// Returns an error if the query fails or data cannot be deserialized.
    #[instrument(skip(self), level = "debug")]
    pub async fn get_session(&self, id: &str) -> Result<Option<ExecutionRecord>> {
        let row: Option<SessionRow> = sqlx::query_as("SELECT * FROM sessions WHERE id = ?1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .wrap_err("Failed to get session")?;

        match row {
            Some(row) => {
                let record = self.row_to_record(row).await?;
                Ok(Some(record))
            }
            None => Ok(None),
        }
    }

    /// List sessions with pagination.
    ///
    /// # Errors
    ///
    /// Returns an error if the query fails or data cannot be deserialized.
    #[instrument(skip(self), level = "debug")]
    pub async fn list_sessions(&self, limit: u32, offset: u32) -> Result<Vec<ExecutionRecord>> {
        let rows: Vec<SessionRow> =
            sqlx::query_as("SELECT * FROM sessions ORDER BY started_at DESC LIMIT ?1 OFFSET ?2")
                .bind(limit as i64)
                .bind(offset as i64)
                .fetch_all(&self.pool)
                .await
                .wrap_err("Failed to list sessions")?;

        let mut records = Vec::with_capacity(rows.len());
        for row in rows {
            let record = self.row_to_record(row).await?;
            records.push(record);
        }

        Ok(records)
    }

    /// Delete a session by ID.
    ///
    /// # Errors
    ///
    /// Returns an error if the delete fails.
    #[instrument(skip(self), level = "debug")]
    pub async fn delete_session(&self, id: &str) -> Result<()> {
        sqlx::query("DELETE FROM sessions WHERE id = ?1")
            .bind(id)
            .execute(&self.pool)
            .await
            .wrap_err("Failed to delete session")?;

        debug!(id, "Session deleted");
        Ok(())
    }

    /// Get the total number of sessions.
    ///
    /// # Errors
    ///
    /// Returns an error if the query fails.
    #[instrument(skip(self), level = "debug")]
    pub async fn get_session_count(&self) -> Result<u64> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions")
            .fetch_one(&self.pool)
            .await
            .wrap_err("Failed to count sessions")?;

        Ok(count as u64)
    }

    /// Get session statistics.
    ///
    /// # Errors
    ///
    /// Returns an error if the query fails.
    #[instrument(skip(self), level = "debug")]
    pub async fn get_session_stats(&self) -> Result<SessionStats> {
        let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions")
            .fetch_one(&self.pool)
            .await
            .wrap_err("Failed to count total sessions")?;

        let completed: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM sessions WHERE state = 'Completed'")
                .fetch_one(&self.pool)
                .await
                .unwrap_or(0);

        let failed: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM sessions WHERE state = 'Failed'")
                .fetch_one(&self.pool)
                .await
                .unwrap_or(0);

        let cancelled: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM sessions WHERE state = 'Cancelled'")
                .fetch_one(&self.pool)
                .await
                .unwrap_or(0);

        let terminal = completed + failed + cancelled;
        let active = total - terminal;

        Ok(SessionStats {
            total: total as u64,
            completed: completed as u64,
            failed: failed as u64,
            cancelled: cancelled as u64,
            active: active as u64,
        })
    }

    /// Rename a session.
    ///
    /// # Errors
    ///
    /// Returns an error if the update fails.
    #[instrument(skip(self), level = "debug")]
    pub async fn rename_session(&self, id: &str, name: Option<String>) -> Result<()> {
        sqlx::query("UPDATE sessions SET name = ?1, updated_at = datetime('now') WHERE id = ?2")
            .bind(&name)
            .bind(id)
            .execute(&self.pool)
            .await
            .wrap_err("Failed to rename session")?;

        debug!(id, name = ?name, "Session renamed");
        Ok(())
    }

    /// Toggle the pinned status of a session.
    ///
    /// # Errors
    ///
    /// Returns an error if the update fails.
    #[instrument(skip(self), level = "debug")]
    pub async fn toggle_session_pin(&self, id: &str) -> Result<bool> {
        let current: Option<(i64,)> = sqlx::query_as("SELECT pinned FROM sessions WHERE id = ?1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .wrap_err("Failed to get session pin state")?;

        let Some((pinned,)) = current else {
            return Ok(false);
        };

        let new_state = if pinned != 0 { 0 } else { 1 };

        sqlx::query("UPDATE sessions SET pinned = ?1, updated_at = datetime('now') WHERE id = ?2")
            .bind(new_state)
            .bind(id)
            .execute(&self.pool)
            .await
            .wrap_err("Failed to toggle session pin")?;

        debug!(id, new_pinned = new_state, "Session pin toggled");
        Ok(new_state != 0)
    }

    /// Append an event to the session events table.
    ///
    /// # Errors
    ///
    /// Returns an error if the insert fails.
    #[instrument(skip(self, event), level = "debug")]
    pub async fn append_event(&self, session_id: &str, event: &ExecutionEvent) -> Result<()> {
        let (event_type, event_data, timestamp) = match event {
            ExecutionEvent::StateChanged { state, timestamp } => {
                ("StateChanged", serde_json::to_string(&state)?, *timestamp)
            }
            ExecutionEvent::OutputChunk { text, timestamp } => {
                ("OutputChunk", serde_json::to_string(&text)?, *timestamp)
            }
            ExecutionEvent::Error {
                message,
                retryable,
                timestamp,
            } => (
                "Error",
                serde_json::to_string(&(message, retryable))?,
                *timestamp,
            ),
            ExecutionEvent::IterationCompleted {
                iteration,
                completed,
                timestamp,
            } => (
                "IterationCompleted",
                serde_json::to_string(&(iteration, completed))?,
                *timestamp,
            ),
            ExecutionEvent::MetricsUpdated { metrics, timestamp } => (
                "MetricsUpdated",
                serde_json::to_string(&metrics)?,
                *timestamp,
            ),
            ExecutionEvent::Activity {
                activity,
                timestamp,
            } => ("Activity", serde_json::to_string(&activity)?, *timestamp),
        };

        sqlx::query(
            "INSERT INTO session_events (session_id, event_type, event_data, timestamp)
             VALUES (?1, ?2, ?3, ?4)",
        )
        .bind(session_id)
        .bind(event_type)
        .bind(&event_data)
        .bind(timestamp.to_rfc3339())
        .execute(&self.pool)
        .await
        .wrap_err("Failed to append event")?;

        Ok(())
    }

    /// Get all events for a session.
    ///
    /// # Errors
    ///
    /// Returns an error if the query fails or data cannot be deserialized.
    #[instrument(skip(self), level = "debug")]
    pub async fn get_session_events(&self, session_id: &str) -> Result<Vec<ExecutionEvent>> {
        let rows: Vec<EventRow> = sqlx::query_as(
            "SELECT event_type, event_data, timestamp FROM session_events WHERE session_id = ?1 ORDER BY id",
        )
        .bind(session_id)
        .fetch_all(&self.pool)
        .await
        .wrap_err("Failed to get session events")?;

        let mut events = Vec::with_capacity(rows.len());
        for row in rows {
            let event = self.row_to_event(row)?;
            events.push(event);
        }

        Ok(events)
    }

    /// Convert a database row to an ExecutionRecord.
    async fn row_to_record(&self, row: SessionRow) -> Result<ExecutionRecord> {
        let config: ExecutionConfig = serde_json::from_str(&row.config_json)
            .wrap_err("Failed to deserialize ExecutionConfig")?;

        let metrics: ExecutionMetrics = serde_json::from_str(&row.metrics_json)
            .wrap_err("Failed to deserialize ExecutionMetrics")?;

        let state = parse_execution_state(&row.state)
            .ok_or_else(|| color_eyre::eyre::eyre!("Invalid execution state: {}", row.state))?;

        let started_at = DateTime::parse_from_rfc3339(&row.started_at)
            .wrap_err("Invalid started_at RFC3339 format")?
            .with_timezone(&Utc);

        let ended_at = row
            .ended_at
            .map(|s| DateTime::parse_from_rfc3339(&s).map(|dt| dt.with_timezone(&Utc)))
            .transpose()
            .wrap_err("Invalid ended_at RFC3339 format")?;

        let events = self.get_session_events(&row.id).await?;

        Ok(ExecutionRecord {
            id: ExecutionId::from_string(row.id),
            config,
            state,
            metrics,
            output: row.output.unwrap_or_default(),
            error: row.error,
            started_at,
            ended_at,
            events,
            name: row.name,
            pinned: row.pinned != 0,
            project_path: row.project_path,
        })
    }

    /// Convert a database row to an ExecutionEvent.
    fn row_to_event(&self, row: EventRow) -> Result<ExecutionEvent> {
        let timestamp = DateTime::parse_from_rfc3339(&row.timestamp)
            .wrap_err("Invalid timestamp RFC3339 format")?
            .with_timezone(&Utc);

        let event = match row.event_type.as_str() {
            "StateChanged" => {
                let state: ExecutionState = serde_json::from_str(&row.event_data)
                    .wrap_err("Failed to deserialize ExecutionState")?;
                ExecutionEvent::StateChanged { state, timestamp }
            }
            "OutputChunk" => {
                let text: String = serde_json::from_str(&row.event_data)
                    .wrap_err("Failed to deserialize output text")?;
                ExecutionEvent::OutputChunk { text, timestamp }
            }
            "Error" => {
                let (message, retryable): (String, bool) = serde_json::from_str(&row.event_data)
                    .wrap_err("Failed to deserialize error data")?;
                ExecutionEvent::Error {
                    message,
                    retryable,
                    timestamp,
                }
            }
            "IterationCompleted" => {
                let (iteration, completed): (u32, bool) = serde_json::from_str(&row.event_data)
                    .wrap_err("Failed to deserialize iteration data")?;
                ExecutionEvent::IterationCompleted {
                    iteration,
                    completed,
                    timestamp,
                }
            }
            "MetricsUpdated" => {
                let metrics: ExecutionMetrics = serde_json::from_str(&row.event_data)
                    .wrap_err("Failed to deserialize ExecutionMetrics")?;
                ExecutionEvent::MetricsUpdated { metrics, timestamp }
            }
            "Activity" => {
                let activity: velor_core::ExecutionActivity = serde_json::from_str(&row.event_data)
                    .wrap_err("Failed to deserialize ExecutionActivity")?;
                ExecutionEvent::Activity {
                    activity,
                    timestamp,
                }
            }
            _ => {
                return Err(color_eyre::eyre::eyre!(
                    "Unknown event type: {}",
                    row.event_type
                ));
            }
        };

        Ok(event)
    }

    // ========================================================================
    // Project Methods
    // ========================================================================

    /// List all projects with their metadata.
    ///
    /// # Errors
    ///
    /// Returns an error if the query fails.
    #[instrument(skip(self), level = "debug")]
    pub async fn list_projects(&self) -> Result<Vec<Project>> {
        let rows: Vec<ProjectRow> = sqlx::query_as(
            r#"
            SELECT
                COALESCE(p.path, s.project_path) as path,
                COALESCE(p.display_name, s.project_path) as display_name,
                COALESCE(p.hidden, 0) as hidden,
                COALESCE(p.sort_order, 0) as sort_order,
                COUNT(s.id) as session_count
            FROM projects p
            FULL OUTER JOIN sessions s ON s.project_path = p.path
            WHERE s.project_path IS NOT NULL OR p.path IS NOT NULL
            GROUP BY COALESCE(p.path, s.project_path), p.display_name, p.hidden, p.sort_order
            ORDER BY COALESCE(p.sort_order, 0) ASC
            "#,
        )
        .fetch_all(&self.pool)
        .await
        .wrap_err("Failed to list projects")?;

        let projects = rows
            .into_iter()
            .map(|row| {
                let display_name = row.display_name.unwrap_or_else(|| {
                    row.path
                        .rsplit('/')
                        .next()
                        .or(row.path.rsplit('\\').next())
                        .unwrap_or(&row.path)
                        .to_string()
                });
                Project {
                    path: row.path,
                    display_name,
                    hidden: row.hidden != 0,
                    sort_order: row.sort_order,
                    session_count: row.session_count as u64,
                }
            })
            .collect();

        Ok(projects)
    }

    /// Hide a project from the sidebar.
    ///
    /// # Errors
    ///
    /// Returns an error if the update fails.
    #[instrument(skip(self), level = "debug")]
    pub async fn hide_project(&self, path: &str) -> Result<()> {
        sqlx::query(
            "INSERT INTO projects (path, display_name, hidden, sort_order) VALUES (?1, ?2, 1, 0)
             ON CONFLICT(path) DO UPDATE SET hidden = 1",
        )
        .bind(path)
        .bind(path)
        .execute(&self.pool)
        .await
        .wrap_err("Failed to hide project")?;

        debug!(path, "Project hidden");
        Ok(())
    }

    /// Show a hidden project.
    ///
    /// # Errors
    ///
    /// Returns an error if the update fails.
    #[instrument(skip(self), level = "debug")]
    pub async fn show_project(&self, path: &str) -> Result<()> {
        sqlx::query(
            "INSERT INTO projects (path, display_name, hidden, sort_order) VALUES (?1, ?2, 0, 0)
             ON CONFLICT(path) DO UPDATE SET hidden = 0",
        )
        .bind(path)
        .bind(path)
        .execute(&self.pool)
        .await
        .wrap_err("Failed to show project")?;

        debug!(path, "Project shown");
        Ok(())
    }

    /// Update the display name of a project.
    ///
    /// # Errors
    ///
    /// Returns an error if the update fails.
    #[instrument(skip(self), level = "debug")]
    pub async fn rename_project(&self, path: &str, display_name: String) -> Result<()> {
        sqlx::query(
            "INSERT INTO projects (path, display_name, hidden, sort_order) VALUES (?1, ?2, 0, 0)
             ON CONFLICT(path) DO UPDATE SET display_name = ?2",
        )
        .bind(path)
        .bind(&display_name)
        .execute(&self.pool)
        .await
        .wrap_err("Failed to rename project")?;

        debug!(path, display_name, "Project renamed");
        Ok(())
    }

    /// Reorder projects by updating their sort_order values.
    ///
    /// # Errors
    ///
    /// Returns an error if the update fails.
    #[instrument(skip(self), level = "debug")]
    pub async fn reorder_projects(&self, paths: Vec<String>) -> Result<()> {
        let mut tx = self
            .pool
            .begin()
            .await
            .wrap_err("Failed to begin transaction")?;

        for (index, path) in paths.iter().enumerate() {
            sqlx::query(
                "INSERT INTO projects (path, display_name, hidden, sort_order) VALUES (?1, ?2, 0, ?3)
                 ON CONFLICT(path) DO UPDATE SET sort_order = ?3",
            )
            .bind(path)
            .bind(path)
            .bind(index as i64)
            .execute(&mut *tx)
            .await
            .wrap_err_with(|| format!("Failed to reorder project at index {}", index))?;
        }

        tx.commit().await.wrap_err("Failed to commit transaction")?;
        debug!("Projects reordered");
        Ok(())
    }

    // ========================================================================
    // Automation Methods
    // ========================================================================

    /// Insert a new automation run record. Returns the run ID.
    ///
    /// # Errors
    ///
    /// Returns an error if the insert fails.
    #[instrument(skip(self), level = "debug")]
    pub async fn insert_automation_run(
        &self,
        automation_name: &str,
        scheduled_for: DateTime<Utc>,
        started_at: DateTime<Utc>,
    ) -> Result<i64> {
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

    /// Update automation run status and completion info.
    ///
    /// # Errors
    ///
    /// Returns an error if the update fails.
    #[instrument(skip(self), level = "debug")]
    pub async fn update_automation_run(
        &self,
        id: i64,
        status: AutomationRunStatus,
        iterations_completed: u32,
        exit_code: Option<i32>,
        output: Option<&str>,
        error: Option<&str>,
    ) -> Result<()> {
        let now = Utc::now();
        let duration_ms = if status.is_terminal() {
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
    #[instrument(skip(self), level = "debug")]
    pub async fn try_acquire_automation_lock(
        &self,
        automation_name: &str,
        run_id: i64,
    ) -> Result<Option<i64>> {
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

            if let Some(locked_str) = locked_at
                && let Ok(locked) = DateTime::parse_from_rfc3339(&locked_str)
            {
                let locked = locked.with_timezone(&Utc);
                let stale_threshold = Utc::now() - chrono::Duration::hours(2);
                if locked < stale_threshold {
                    self.release_automation_lock(automation_name).await?;
                    continue;
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
    #[instrument(skip(self), level = "debug")]
    pub async fn release_automation_lock(&self, automation_name: &str) -> Result<()> {
        sqlx::query("DELETE FROM automation_locks WHERE automation_name = ?1")
            .bind(automation_name)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Get recent automation runs, optionally filtered by automation name.
    ///
    /// # Errors
    ///
    /// Returns an error if the query fails.
    #[instrument(skip(self), level = "debug")]
    pub async fn get_automation_runs(
        &self,
        automation_name: Option<&str>,
        limit: u32,
    ) -> Result<Vec<AutomationRun>> {
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

        rows.into_iter()
            .map(|r| self.automation_row_to_run(r))
            .collect()
    }

    /// Convert a database row to an AutomationRun.
    fn automation_row_to_run(&self, row: AutomationRunRow) -> Result<AutomationRun> {
        let status = match row.status {
            0 => AutomationRunStatus::Pending,
            1 => AutomationRunStatus::Running,
            2 => AutomationRunStatus::Completed,
            3 => AutomationRunStatus::Failed,
            4 => AutomationRunStatus::Cancelled,
            n => {
                warn!("Unknown status code {} in database, treating as Failed", n);
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

        Ok(AutomationRun {
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

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_config() -> ExecutionConfig {
        ExecutionConfig::new("test-prompt".to_string())
    }

    fn create_test_record() -> ExecutionRecord {
        ExecutionRecord::new(create_test_config())
    }

    // Session tests
    #[tokio::test]
    async fn test_store_open_and_init() {
        let temp_dir = TempDir::new().expect("tempdir should be created");
        let velor_dir = temp_dir.path().join(".velor");

        let store = UnifiedStore::open(&velor_dir)
            .await
            .expect("store should be created");

        let count = store.get_session_count().await.expect("count should work");
        assert_eq!(count, 0, "New store should have 0 sessions");
    }

    #[tokio::test]
    async fn test_insert_and_get_session() {
        let temp_dir = TempDir::new().expect("tempdir should be created");
        let velor_dir = temp_dir.path().join(".velor");

        let store = UnifiedStore::open(&velor_dir)
            .await
            .expect("store should be created");

        let mut record = create_test_record();
        record.append_output("Test output");

        store
            .insert_session(&record)
            .await
            .expect("insert should succeed");

        let retrieved = store
            .get_session(record.id.as_str())
            .await
            .expect("get should succeed")
            .expect("session should exist");

        assert_eq!(retrieved.id.as_str(), record.id.as_str());
        assert_eq!(retrieved.config.prompt_name, "test-prompt");
        assert_eq!(retrieved.output, "Test output");
        assert_eq!(retrieved.state, ExecutionState::Pending);
    }

    #[tokio::test]
    async fn test_update_session() {
        let temp_dir = TempDir::new().expect("tempdir should be created");
        let velor_dir = temp_dir.path().join(".velor");

        let store = UnifiedStore::open(&velor_dir)
            .await
            .expect("store should be created");

        let mut record = create_test_record();
        store
            .insert_session(&record)
            .await
            .expect("insert should succeed");

        record.set_state(ExecutionState::Running);
        record.append_output("More output");
        record.metrics.iteration = 5;

        store
            .update_session(&record)
            .await
            .expect("update should succeed");

        let retrieved = store
            .get_session(record.id.as_str())
            .await
            .expect("get should succeed")
            .expect("session should exist");

        assert_eq!(retrieved.state, ExecutionState::Running);
        assert!(retrieved.output.contains("More output"));
        assert_eq!(retrieved.metrics.iteration, 5);
    }

    #[tokio::test]
    async fn test_delete_session() {
        let temp_dir = TempDir::new().expect("tempdir should be created");
        let velor_dir = temp_dir.path().join(".velor");

        let store = UnifiedStore::open(&velor_dir)
            .await
            .expect("store should be created");

        let record = create_test_record();
        store
            .insert_session(&record)
            .await
            .expect("insert should succeed");

        let count_before = store.get_session_count().await.expect("count should work");
        assert_eq!(count_before, 1);

        store
            .delete_session(record.id.as_str())
            .await
            .expect("delete should succeed");

        let count_after = store.get_session_count().await.expect("count should work");
        assert_eq!(count_after, 0);

        let retrieved = store
            .get_session(record.id.as_str())
            .await
            .expect("get should succeed");
        assert!(retrieved.is_none());
    }

    #[tokio::test]
    async fn test_list_sessions() {
        let temp_dir = TempDir::new().expect("tempdir should be created");
        let velor_dir = temp_dir.path().join(".velor");

        let store = UnifiedStore::open(&velor_dir)
            .await
            .expect("store should be created");

        for i in 0..5 {
            let mut config = create_test_config();
            config.prompt_name = format!("prompt-{}", i);
            let record = ExecutionRecord::new(config);
            store
                .insert_session(&record)
                .await
                .expect("insert should succeed");
            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        }

        let sessions = store
            .list_sessions(3, 0)
            .await
            .expect("list should succeed");
        assert_eq!(sessions.len(), 3);

        let sessions = store
            .list_sessions(3, 2)
            .await
            .expect("list should succeed");
        assert_eq!(sessions.len(), 3);

        let sessions = store
            .list_sessions(10, 0)
            .await
            .expect("list should succeed");
        assert_eq!(sessions.len(), 5);
    }

    #[tokio::test]
    async fn test_session_stats() {
        let temp_dir = TempDir::new().expect("tempdir should be created");
        let velor_dir = temp_dir.path().join(".velor");

        let store = UnifiedStore::open(&velor_dir)
            .await
            .expect("store should be created");

        // Create sessions in different states
        let mut record1 = create_test_record();
        record1.set_state(ExecutionState::Completed);
        store.insert_session(&record1).await.unwrap();

        let mut record2 = create_test_record();
        record2.set_state(ExecutionState::Failed);
        store.insert_session(&record2).await.unwrap();

        let mut record3 = create_test_record();
        record3.set_state(ExecutionState::Running);
        store.insert_session(&record3).await.unwrap();

        let stats = store.get_session_stats().await.expect("stats should work");
        assert_eq!(stats.total, 3);
        assert_eq!(stats.completed, 1);
        assert_eq!(stats.failed, 1);
        assert_eq!(stats.active, 1);
    }

    #[tokio::test]
    async fn test_rename_and_pin_session() {
        let temp_dir = TempDir::new().expect("tempdir should be created");
        let velor_dir = temp_dir.path().join(".velor");

        let store = UnifiedStore::open(&velor_dir)
            .await
            .expect("store should be created");

        let record = create_test_record();
        store.insert_session(&record).await.unwrap();

        // Rename
        store
            .rename_session(record.id.as_str(), Some("My Session".to_string()))
            .await
            .unwrap();

        let retrieved = store
            .get_session(record.id.as_str())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(retrieved.name, Some("My Session".to_string()));
        assert!(!retrieved.pinned);

        // Pin
        let pinned = store.toggle_session_pin(record.id.as_str()).await.unwrap();
        assert!(pinned);

        let retrieved = store
            .get_session(record.id.as_str())
            .await
            .unwrap()
            .unwrap();
        assert!(retrieved.pinned);

        // Unpin
        let pinned = store.toggle_session_pin(record.id.as_str()).await.unwrap();
        assert!(!pinned);
    }

    // Automation tests
    #[test]
    fn test_automation_status_is_terminal() {
        assert!(AutomationRunStatus::Completed.is_terminal());
        assert!(AutomationRunStatus::Failed.is_terminal());
        assert!(AutomationRunStatus::Cancelled.is_terminal());
        assert!(!AutomationRunStatus::Pending.is_terminal());
        assert!(!AutomationRunStatus::Running.is_terminal());
    }

    #[test]
    fn test_automation_status_as_str() {
        assert_eq!(AutomationRunStatus::Pending.as_str(), "Pending");
        assert_eq!(AutomationRunStatus::Running.as_str(), "Running");
        assert_eq!(AutomationRunStatus::Completed.as_str(), "Completed");
        assert_eq!(AutomationRunStatus::Failed.as_str(), "Failed");
        assert_eq!(AutomationRunStatus::Cancelled.as_str(), "Cancelled");
    }

    #[test]
    fn test_automation_status_from_str() {
        assert_eq!(
            AutomationRunStatus::from_str("Pending"),
            Ok(AutomationRunStatus::Pending)
        );
        assert_eq!(
            AutomationRunStatus::from_str("Running"),
            Ok(AutomationRunStatus::Running)
        );
        assert_eq!(
            AutomationRunStatus::from_str("Completed"),
            Ok(AutomationRunStatus::Completed)
        );
        assert_eq!(
            AutomationRunStatus::from_str("Failed"),
            Ok(AutomationRunStatus::Failed)
        );
        assert_eq!(
            AutomationRunStatus::from_str("Cancelled"),
            Ok(AutomationRunStatus::Cancelled)
        );
        assert!(AutomationRunStatus::from_str("Invalid").is_err());
    }

    #[tokio::test]
    async fn test_insert_and_get_automation_run() {
        let temp_dir = TempDir::new().expect("tempdir should be created");
        let velor_dir = temp_dir.path().join(".velor");

        let store = UnifiedStore::open(&velor_dir)
            .await
            .expect("store should be created");

        let scheduled_for = Utc::now();
        let started_at = Utc::now();

        let run_id = store
            .insert_automation_run("test-automation", scheduled_for, started_at)
            .await
            .expect("insert should succeed");

        assert!(run_id > 0);

        let runs = store
            .get_automation_runs(Some("test-automation"), 10)
            .await
            .expect("get_runs should succeed");

        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].automation_name, "test-automation");
        assert_eq!(runs[0].status, AutomationRunStatus::Pending);
    }

    #[tokio::test]
    async fn test_update_automation_run_status() {
        let temp_dir = TempDir::new().expect("tempdir should be created");
        let velor_dir = temp_dir.path().join(".velor");

        let store = UnifiedStore::open(&velor_dir)
            .await
            .expect("store should be created");

        let run_id = store
            .insert_automation_run("test-automation", Utc::now(), Utc::now())
            .await
            .expect("insert should succeed");

        store
            .update_automation_run(
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
            .get_automation_runs(Some("test-automation"), 10)
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
    async fn test_automation_lock_acquire_and_release() {
        let temp_dir = TempDir::new().expect("tempdir should be created");
        let velor_dir = temp_dir.path().join(".velor");

        let store = UnifiedStore::open(&velor_dir)
            .await
            .expect("store should be created");

        // First acquisition should succeed
        let lock1 = store
            .try_acquire_automation_lock("test-automation", 1)
            .await
            .expect("acquire should succeed");
        assert_eq!(lock1, Some(1));

        // Second acquisition should fail (same automation)
        let lock2 = store
            .try_acquire_automation_lock("test-automation", 2)
            .await
            .expect("acquire should succeed");
        assert_eq!(lock2, None);

        // After release, acquisition should succeed again
        store
            .release_automation_lock("test-automation")
            .await
            .expect("release should succeed");

        let lock3 = store
            .try_acquire_automation_lock("test-automation", 3)
            .await
            .expect("acquire should succeed");
        assert_eq!(lock3, Some(3));
    }

    #[tokio::test]
    async fn test_automation_runs_limit() {
        let temp_dir = TempDir::new().expect("tempdir should be created");
        let velor_dir = temp_dir.path().join(".velor");

        let store = UnifiedStore::open(&velor_dir)
            .await
            .expect("store should be created");

        // Insert 5 runs
        for _ in 0..5 {
            store
                .insert_automation_run("test-automation", Utc::now(), Utc::now())
                .await
                .expect("insert should succeed");
        }

        // Request only 3
        let runs = store
            .get_automation_runs(Some("test-automation"), 3)
            .await
            .expect("get_runs should succeed");
        assert_eq!(runs.len(), 3);
    }

    // Project tests
    #[tokio::test]
    async fn test_list_projects_empty() {
        let temp_dir = TempDir::new().expect("tempdir should be created");
        let velor_dir = temp_dir.path().join(".velor");

        let store = UnifiedStore::open(&velor_dir)
            .await
            .expect("store should be created");

        let projects = store.list_projects().await.expect("list should succeed");
        assert!(projects.is_empty());
    }

    #[tokio::test]
    async fn test_hide_show_rename_project() {
        let temp_dir = TempDir::new().expect("tempdir should be created");
        let velor_dir = temp_dir.path().join(".velor");

        let store = UnifiedStore::open(&velor_dir)
            .await
            .expect("store should be created");

        // Add a session with a project path
        let mut record = create_test_record();
        record.project_path = Some("/test/project".to_string());
        store.insert_session(&record).await.unwrap();

        // Hide project
        store.hide_project("/test/project").await.unwrap();

        let projects = store.list_projects().await.unwrap();
        assert_eq!(projects.len(), 1);
        assert!(projects[0].hidden);

        // Show project
        store.show_project("/test/project").await.unwrap();

        let projects = store.list_projects().await.unwrap();
        assert!(!projects[0].hidden);

        // Rename project
        store
            .rename_project("/test/project", "My Project".to_string())
            .await
            .unwrap();

        let projects = store.list_projects().await.unwrap();
        assert_eq!(projects[0].display_name, "My Project");
    }

    // Migration tests
    #[tokio::test]
    async fn test_creates_single_db_file() {
        let temp_dir = TempDir::new().expect("tempdir should be created");
        let velor_dir = temp_dir.path().join(".velor");

        let _store = UnifiedStore::open(&velor_dir)
            .await
            .expect("store should be created");

        // Should create velor.db, not sessions.db or automations.db
        assert!(velor_dir.join("velor.db").exists());
        assert!(!velor_dir.join("sessions.db").exists());
        assert!(!velor_dir.join("automations.db").exists());
    }
}

#[cfg(test)]
mod proptest_tests {
    use super::*;
    use proptest::prelude::*;
    use tempfile::TempDir;

    proptest! {
        #[test]
        fn test_automation_status_roundtrip(
            status_code in 0i32..=4i32
        ) {
            let status = match status_code {
                0 => AutomationRunStatus::Pending,
                1 => AutomationRunStatus::Running,
                2 => AutomationRunStatus::Completed,
                3 => AutomationRunStatus::Failed,
                4 => AutomationRunStatus::Cancelled,
                _ => unreachable!(),
            };

            let as_str = status.as_str();
            let parsed = AutomationRunStatus::from_str(as_str);
            prop_assert!(parsed.is_ok());
            prop_assert_eq!(parsed.unwrap(), status);
        }

        #[test]
        fn test_session_name_preservation(
            name in "[a-zA-Z0-9 ]{0,50}",
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            let result = rt.block_on(async {
                let temp_dir = TempDir::new().expect("tempdir should be created");
                let velor_dir = temp_dir.path().join(".velor");

                let store = UnifiedStore::open(&velor_dir)
                    .await
                    .expect("store should be created");

                let record = ExecutionRecord::new(ExecutionConfig::new("test".to_string()));
                store.insert_session(&record).await.unwrap();

                store.rename_session(record.id.as_str(), if name.is_empty() { None } else { Some(name.clone()) }).await.unwrap();

                let retrieved = store.get_session(record.id.as_str()).await.unwrap().unwrap();
                retrieved.name
            });

            let expected = if name.is_empty() { None } else { Some(name) };
            prop_assert_eq!(result, expected);
        }
    }
}
