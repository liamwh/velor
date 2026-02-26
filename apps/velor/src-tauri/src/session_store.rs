//! SQLite storage for execution session history.
//!
//! This module provides persistent storage for execution sessions, allowing
//! the GUI to display historical executions with full input/output capture.

use chrono::{DateTime, Utc};
use color_eyre::Result;
use color_eyre::eyre::WrapErr;
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, SqlitePool, sqlite::SqliteConnectOptions};
use std::path::Path;
use tracing::{debug, instrument};

use velor_core::{
    ExecutionConfig, ExecutionEvent, ExecutionId, ExecutionMetrics, ExecutionRecord, ExecutionState,
};

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

/// Async SQLite storage for execution sessions.
#[derive(Debug, Clone)]
pub struct SessionStore {
    pool: SqlitePool,
}

impl SessionStore {
    /// Create or open the database at the given path.
    ///
    /// # Errors
    ///
    /// Returns an error if the database cannot be created or opened.
    #[instrument(level = "debug")]
    pub async fn open(path: &Path) -> Result<Self> {
        debug!(?path, "Opening session store");

        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.wrap_err_with(|| {
                format!("Failed to create session store directory: {:?}", parent)
            })?;
        }

        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true);

        let pool = SqlitePool::connect_with(options)
            .await
            .wrap_err("Failed to connect to session store database")?;

        // Enable WAL mode for better concurrency
        sqlx::query("PRAGMA journal_mode=WAL;")
            .execute(&pool)
            .await
            .wrap_err("Failed to enable WAL mode")?;

        let store = Self { pool };
        store.init_schema().await?;

        debug!("Session store opened successfully");
        Ok(store)
    }

    /// Initialize the database schema.
    async fn init_schema(&self) -> Result<()> {
        let query = r#"
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
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE IF NOT EXISTS session_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                event_type TEXT NOT NULL,
                event_data TEXT NOT NULL,
                timestamp TEXT NOT NULL,
                FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
            );

            CREATE INDEX IF NOT EXISTS idx_sessions_started_at ON sessions(started_at DESC);
            CREATE INDEX IF NOT EXISTS idx_sessions_state ON sessions(state);
            CREATE INDEX IF NOT EXISTS idx_sessions_automation_name ON sessions(automation_name);
            CREATE INDEX IF NOT EXISTS idx_session_events_session_id ON session_events(session_id);
        "#;

        sqlx::query(query)
            .execute(&self.pool)
            .await
            .wrap_err("Failed to initialize session store schema")?;

        Ok(())
    }

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

        sqlx::query(
            "INSERT INTO sessions (id, prompt_name, state, started_at, ended_at, config_json, metrics_json, output, error)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
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
             SET state = ?1, ended_at = ?2, metrics_json = ?3, output = ?4, error = ?5, updated_at = datetime('now')
             WHERE id = ?6",
        )
        .bind(record.state.label())
        .bind(ended_at)
        .bind(&metrics_json)
        .bind(&record.output)
        .bind(&record.error)
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

        // Load events from the events table
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
            _ => {
                return Err(color_eyre::eyre::eyre!(
                    "Unknown event type: {}",
                    row.event_type
                ));
            }
        };

        Ok(event)
    }
}

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
}

/// Raw event row from database.
#[derive(FromRow)]
struct EventRow {
    event_type: String,
    event_data: String,
    timestamp: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::time::Duration;
    use tempfile::TempDir;

    fn create_test_config() -> ExecutionConfig {
        ExecutionConfig::new("test-prompt".to_string())
    }

    fn create_test_record() -> ExecutionRecord {
        ExecutionRecord::new(create_test_config())
    }

    #[tokio::test]
    async fn test_store_open_and_init() {
        let temp_dir = TempDir::new().expect("tempdir should be created");
        let db_path = temp_dir.path().join("sessions.db");

        let store = SessionStore::open(&db_path)
            .await
            .expect("store should be created");

        // Verify tables exist by querying them
        let count = store.get_session_count().await.expect("count should work");
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn test_insert_and_get_session() {
        let temp_dir = TempDir::new().expect("tempdir should be created");
        let db_path = temp_dir.path().join("sessions.db");

        let store = SessionStore::open(&db_path)
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
        let db_path = temp_dir.path().join("sessions.db");

        let store = SessionStore::open(&db_path)
            .await
            .expect("store should be created");

        let mut record = create_test_record();
        store
            .insert_session(&record)
            .await
            .expect("insert should succeed");

        // Update the record
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
        let db_path = temp_dir.path().join("sessions.db");

        let store = SessionStore::open(&db_path)
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
        let db_path = temp_dir.path().join("sessions.db");

        let store = SessionStore::open(&db_path)
            .await
            .expect("store should be created");

        // Insert 5 sessions with slight delays to ensure different timestamps
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

        // List with limit
        let sessions = store
            .list_sessions(3, 0)
            .await
            .expect("list should succeed");
        assert_eq!(sessions.len(), 3);

        // List with offset
        let sessions = store
            .list_sessions(3, 2)
            .await
            .expect("list should succeed");
        assert_eq!(sessions.len(), 3);

        // List all
        let sessions = store
            .list_sessions(10, 0)
            .await
            .expect("list should succeed");
        assert_eq!(sessions.len(), 5);

        // Verify ordering (most recent first)
        assert!(sessions[0].started_at >= sessions[1].started_at);
    }

    #[tokio::test]
    async fn test_session_stats() {
        let temp_dir = TempDir::new().expect("tempdir should be created");
        let db_path = temp_dir.path().join("sessions.db");

        let store = SessionStore::open(&db_path)
            .await
            .expect("store should be created");

        // Create sessions with different states
        let mut completed_record = create_test_record();
        completed_record.set_state(ExecutionState::Completed);
        store
            .insert_session(&completed_record)
            .await
            .expect("insert should succeed");

        let mut failed_record = create_test_record();
        failed_record.set_state(ExecutionState::Failed);
        store
            .insert_session(&failed_record)
            .await
            .expect("insert should succeed");

        let mut cancelled_record = create_test_record();
        cancelled_record.set_state(ExecutionState::Cancelled);
        store
            .insert_session(&cancelled_record)
            .await
            .expect("insert should succeed");

        // Keep one in pending state
        let pending_record = create_test_record();
        store
            .insert_session(&pending_record)
            .await
            .expect("insert should succeed");

        let stats = store
            .get_session_stats()
            .await
            .expect("stats should succeed");

        assert_eq!(stats.total, 4);
        assert_eq!(stats.completed, 1);
        assert_eq!(stats.failed, 1);
        assert_eq!(stats.cancelled, 1);
        assert_eq!(stats.active, 1);
    }

    #[tokio::test]
    async fn test_append_and_get_events() {
        let temp_dir = TempDir::new().expect("tempdir should be created");
        let db_path = temp_dir.path().join("sessions.db");

        let store = SessionStore::open(&db_path)
            .await
            .expect("store should be created");

        let record = create_test_record();
        store
            .insert_session(&record)
            .await
            .expect("insert should succeed");

        // Append events
        let state_event = ExecutionEvent::StateChanged {
            state: ExecutionState::Running,
            timestamp: Utc::now(),
        };
        store
            .append_event(record.id.as_str(), &state_event)
            .await
            .expect("append should succeed");

        let output_event = ExecutionEvent::OutputChunk {
            text: "Hello, world!".to_string(),
            timestamp: Utc::now(),
        };
        store
            .append_event(record.id.as_str(), &output_event)
            .await
            .expect("append should succeed");

        // Get events
        let events = store
            .get_session_events(record.id.as_str())
            .await
            .expect("get events should succeed");

        assert_eq!(events.len(), 2);

        // Verify event types
        assert!(matches!(
            &events[0],
            ExecutionEvent::StateChanged {
                state: ExecutionState::Running,
                ..
            }
        ));
        assert!(
            matches!(&events[1], ExecutionEvent::OutputChunk { text, .. } if text == "Hello, world!")
        );
    }

    #[tokio::test]
    async fn test_session_with_full_config() {
        let temp_dir = TempDir::new().expect("tempdir should be created");
        let db_path = temp_dir.path().join("sessions.db");

        let store = SessionStore::open(&db_path)
            .await
            .expect("store should be created");

        let mut vars = BTreeMap::new();
        vars.insert("key1".to_string(), "value1".to_string());
        vars.insert("key2".to_string(), "value2".to_string());

        let config = ExecutionConfig::new("complex-prompt".to_string())
            .with_template_vars(vars.clone())
            .with_max_iterations(100)
            .with_complete_token("DONE".to_string())
            .with_binary("custom-binary".to_string())
            .with_permission_mode("acceptAll".to_string())
            .with_cwd("/custom/path".to_string())
            .with_rules(true, ".custom/rules".to_string());

        let record = ExecutionRecord::new(config);
        store
            .insert_session(&record)
            .await
            .expect("insert should succeed");

        let retrieved = store
            .get_session(record.id.as_str())
            .await
            .expect("get should succeed")
            .expect("session should exist");

        assert_eq!(retrieved.config.prompt_name, "complex-prompt");
        assert_eq!(retrieved.config.template_vars, vars);
        assert_eq!(retrieved.config.max_iterations, 100);
        assert_eq!(retrieved.config.complete_token, "DONE");
        assert_eq!(retrieved.config.binary, "custom-binary");
        assert_eq!(retrieved.config.permission_mode, "acceptAll");
        assert_eq!(retrieved.config.cwd, "/custom/path");
        assert!(retrieved.config.rules_enabled);
        assert_eq!(retrieved.config.rules_dir, ".custom/rules");
    }

    #[tokio::test]
    async fn test_session_with_metrics() {
        let temp_dir = TempDir::new().expect("tempdir should be created");
        let db_path = temp_dir.path().join("sessions.db");

        let store = SessionStore::open(&db_path)
            .await
            .expect("store should be created");

        let mut record = create_test_record();
        record.metrics = ExecutionMetrics {
            iteration: 10,
            max_iterations: 50,
            retries: 3,
            max_retries: 5,
            total_duration: Duration::from_secs(300),
            current_iteration_duration: Duration::from_secs(30),
            total_tokens: Some(5000),
            total_cost: Some(0.25),
        };

        store
            .insert_session(&record)
            .await
            .expect("insert should succeed");

        let retrieved = store
            .get_session(record.id.as_str())
            .await
            .expect("get should succeed")
            .expect("session should exist");

        assert_eq!(retrieved.metrics.iteration, 10);
        assert_eq!(retrieved.metrics.max_iterations, 50);
        assert_eq!(retrieved.metrics.retries, 3);
        assert_eq!(retrieved.metrics.total_tokens, Some(5000));
        assert_eq!(retrieved.metrics.total_cost, Some(0.25));
    }

    #[tokio::test]
    async fn test_delete_session_cascades_events() {
        let temp_dir = TempDir::new().expect("tempdir should be created");
        let db_path = temp_dir.path().join("sessions.db");

        let store = SessionStore::open(&db_path)
            .await
            .expect("store should be created");

        let record = create_test_record();
        store
            .insert_session(&record)
            .await
            .expect("insert should succeed");

        // Append events
        let event = ExecutionEvent::StateChanged {
            state: ExecutionState::Running,
            timestamp: Utc::now(),
        };
        store
            .append_event(record.id.as_str(), &event)
            .await
            .expect("append should succeed");

        // Delete session
        store
            .delete_session(record.id.as_str())
            .await
            .expect("delete should succeed");

        // Verify events are also deleted (cascade)
        let events = store
            .get_session_events(record.id.as_str())
            .await
            .expect("get events should succeed");
        assert!(events.is_empty());
    }
}

#[cfg(test)]
mod proptest_tests {
    use super::*;
    use proptest::prelude::*;
    use tempfile::TempDir;

    proptest! {
        #[test]
        fn test_output_preservation(output in ".*") {
            let rt = tokio::runtime::Runtime::new().unwrap();
            let result = rt.block_on(async {
                let temp_dir = TempDir::new().expect("tempdir should be created");
                let db_path = temp_dir.path().join("sessions.db");

                let store = SessionStore::open(&db_path)
                    .await
                    .expect("store should be created");

                let mut record = ExecutionRecord::new(ExecutionConfig::new("test".to_string()));
                record.append_output(&output);
                store.insert_session(&record).await.expect("insert should succeed");

                let retrieved = store.get_session(record.id.as_str())
                    .await
                    .expect("get should succeed")
                    .expect("session should exist");

                retrieved.output == output
            });

            prop_assert!(result, "output should be preserved");
        }

        #[test]
        fn test_template_vars_preservation(
            vars in prop::collection::btree_map("[a-z]{1,10}", ".*", 0..10)
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            let result = rt.block_on(async {
                let temp_dir = TempDir::new().expect("tempdir should be created");
                let db_path = temp_dir.path().join("sessions.db");

                let store = SessionStore::open(&db_path)
                    .await
                    .expect("store should be created");

                let config = ExecutionConfig::new("test".to_string())
                    .with_template_vars(vars.clone());

                let record = ExecutionRecord::new(config);
                store.insert_session(&record).await.expect("insert should succeed");

                let retrieved = store.get_session(record.id.as_str())
                    .await
                    .expect("get should succeed")
                    .expect("session should exist");

                retrieved.config.template_vars == vars
            });

            prop_assert!(result, "template vars should be preserved");
        }

        #[test]
        fn test_multiple_sessions_unique_ids(
            prompt_names in prop::collection::vec("[a-z]{1,10}", 1..10)
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            let result = rt.block_on(async {
                let temp_dir = TempDir::new().expect("tempdir should be created");
                let db_path = temp_dir.path().join("sessions.db");

                let store = SessionStore::open(&db_path)
                    .await
                    .expect("store should be created");

                let mut ids = std::collections::HashSet::new();
                for prompt_name in &prompt_names {
                    let config = ExecutionConfig::new(prompt_name.to_string());
                    let record = ExecutionRecord::new(config);
                    ids.insert(record.id.as_str().to_string());
                    store.insert_session(&record).await.expect("insert should succeed");
                }

                let count = store.get_session_count().await.expect("count should work");
                let unique_count = ids.len();

                (count == prompt_names.len() as u64) && (unique_count == prompt_names.len())
            });

            prop_assert!(result, "all sessions should have unique IDs");
        }
    }
}
