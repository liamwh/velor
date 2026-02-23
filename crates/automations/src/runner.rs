//! Automation execution with worktree support.

use crate::config::Automation;
use crate::store::{AutomationRunStatus, AutomationStore};
use chrono::Utc;
use std::path::Path;
use std::process::Stdio;
use tokio::process::Command;
use tokio::sync::Semaphore;
use tokio::time::{Duration, timeout};
use tokio_util::sync::CancellationToken;

/// Result of running an automation.
#[derive(Debug)]
pub struct AutomationResult {
    /// The final status of the automation run.
    pub status: AutomationRunStatus,
    /// Number of iterations completed before termination.
    pub iterations_completed: u32,
    /// Exit code from the automation process (if available).
    pub exit_code: Option<i32>,
    /// Standard output from the automation run.
    pub output: String,
    /// Standard error from the automation run (if any).
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
    /// Create a new automation runner.
    #[must_use]
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

    /// Get a reference to the underlying store.
    #[must_use]
    pub const fn store(&self) -> &AutomationStore {
        &self.store
    }

    /// Run a single automation.
    ///
    /// # Errors
    ///
    /// Returns an error if the automation cannot be run.
    pub async fn run_automation(
        &self,
        automation: &Automation,
        scheduled_for: chrono::DateTime<Utc>,
        cancel_token: &CancellationToken,
    ) -> color_eyre::Result<AutomationResult> {
        let _permit = self.semaphore.acquire().await?;

        let started_at = Utc::now();

        // Create run record
        let run_id = self
            .store
            .insert_run(&automation.name, scheduled_for, started_at)
            .await?;

        // Try to acquire lock (prevent overlapping runs)
        let lock_acquired = self
            .store
            .try_acquire_lock(&automation.name, run_id)
            .await?;
        if lock_acquired.is_none() {
            tracing::info!(
                "Automation '{}' is already running, skipping",
                automation.name
            );
            self.store
                .update_run(
                    run_id,
                    AutomationRunStatus::Cancelled,
                    0,
                    None,
                    Some("Skipped due to overlapping run"),
                    None,
                )
                .await?;
            return Ok(AutomationResult {
                status: AutomationRunStatus::Cancelled,
                iterations_completed: 0,
                exit_code: None,
                output: String::new(),
                error: Some("Skipped due to overlapping run".to_string()),
            });
        }

        // Ensure lock is released when we're done
        struct LockGuard<'a> {
            store: &'a AutomationStore,
            automation_name: &'a str,
            released: std::cell::Cell<bool>,
        }

        impl<'a> Drop for LockGuard<'a> {
            fn drop(&mut self) {
                if !self.released.get() {
                    let rt = tokio::runtime::Handle::try_current();
                    if let Ok(rt) = rt {
                        let store = self.store.clone();
                        let name = self.automation_name.to_string();
                        rt.block_on(async move {
                            let _ = store.release_lock(&name).await;
                        });
                    }
                }
            }
        }

        let _lock_guard = LockGuard {
            store: &self.store,
            automation_name: &automation.name,
            released: std::cell::Cell::new(false),
        };

        // Update status to Running
        self.store
            .update_run(run_id, AutomationRunStatus::Running, 0, None, None, None)
            .await?;

        // Determine timeout
        let timeout_duration = Duration::from_secs(automation.timeout_seconds.unwrap_or(3600));

        // Execute velor with timeout
        let result = match timeout(
            timeout_duration,
            self.execute_velor(automation, cancel_token.clone()),
        )
        .await
        {
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

        // Update run record
        self.store
            .update_run(
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
            )
            .await?;

        // Release lock explicitly
        self.store.release_lock(&automation.name).await?;
        _lock_guard.released.set(true);

        Ok(result)
    }

    async fn execute_velor(
        &self,
        automation: &Automation,
        cancel_token: CancellationToken,
    ) -> color_eyre::Result<AutomationResult> {
        // Check for cancellation before starting
        if cancel_token.is_cancelled() {
            return Ok(AutomationResult {
                status: AutomationRunStatus::Cancelled,
                iterations_completed: 0,
                exit_code: None,
                output: String::new(),
                error: Some("Cancelled before start".to_string()),
            });
        }

        let child = Command::new(&self.velor_binary)
            .arg("once")
            .arg("--prompt")
            .arg(&automation.prompt)
            .current_dir(&self.git_root)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        // Wait for the child to complete
        // Note: We don't kill the child mid-execution on cancellation
        // The cancellation is primarily for the scheduler loop, not individual runs
        let output = child.wait_with_output().await?;

        // Check if we were cancelled during execution
        if cancel_token.is_cancelled() {
            return Ok(AutomationResult {
                status: AutomationRunStatus::Cancelled,
                iterations_completed: 0,
                exit_code: output.status.code(),
                output: String::new(),
                error: Some("Cancelled during execution".to_string()),
            });
        }

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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_automation_result_debug() {
        let result = AutomationResult {
            status: AutomationRunStatus::Completed,
            iterations_completed: 5,
            exit_code: Some(0),
            output: "test output".to_string(),
            error: None,
        };

        let debug_str = format!("{:?}", result);
        assert!(debug_str.contains("Completed"));
        assert!(debug_str.contains("5"));
    }

    #[tokio::test]
    async fn test_runner_new() {
        let temp_dir = TempDir::new().expect("tempdir should be created");
        let db_path = temp_dir.path().join("test.db");

        let store = AutomationStore::open(&db_path)
            .await
            .expect("store should be created");

        let runner = AutomationRunner::new(store, 3, temp_dir.path(), "velor".to_string(), 100_000);

        // Just verify the runner was created
        assert_eq!(runner.max_output_bytes, 100_000);
    }

    #[tokio::test]
    async fn test_runner_store_access() {
        let temp_dir = TempDir::new().expect("tempdir should be created");
        let db_path = temp_dir.path().join("test.db");

        let store = AutomationStore::open(&db_path)
            .await
            .expect("store should be created");

        let runner = AutomationRunner::new(store, 3, temp_dir.path(), "velor".to_string(), 100_000);

        // Verify we can access the store
        let store_ref = runner.store();

        // Try to query the store to verify it's functional
        let runs = store_ref
            .get_runs(None, 10)
            .await
            .expect("get_runs should succeed");
        assert_eq!(runs.len(), 0);
    }
}
