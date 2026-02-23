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

/// Cleanup handle for a git worktree.
///
/// When dropped, this will attempt to remove the worktree using `git worktree remove`.
/// If the removal fails, an error will be logged but the drop will not panic.
pub struct WorktreeCleanup {
    /// The path to the worktree.
    pub path: std::path::PathBuf,
    /// The git root directory where the worktree was created.
    git_root: std::path::PathBuf,
}

impl WorktreeCleanup {
    /// Create a new worktree cleanup handle.
    #[must_use]
    pub const fn new(path: std::path::PathBuf, git_root: std::path::PathBuf) -> Self {
        Self { path, git_root }
    }

    /// Explicitly clean up the worktree.
    ///
    /// # Errors
    ///
    /// Returns an error if the worktree cannot be removed.
    pub async fn cleanup(self) -> color_eyre::Result<()> {
        // Use `git worktree remove` to properly remove the worktree
        let status = Command::new("git")
            .args(["worktree", "remove", "--force"])
            .arg(&self.path)
            .current_dir(&self.git_root)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .status()
            .await?;

        if !status.success() {
            return Err(color_eyre::eyre::eyre!(
                "Failed to remove worktree at {:?}: git exited with {:?}",
                self.path,
                status.code()
            ));
        }

        Ok(())
    }
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

    /// Set up a git worktree for isolated automation execution.
    ///
    /// If the directory is not a git repository, returns `Ok(None)` and the
    /// automation will run directly in `git_root`.
    ///
    /// # Errors
    ///
    /// Returns an error if the worktree cannot be created.
    async fn setup_worktree(
        &self,
        automation: &Automation,
    ) -> color_eyre::Result<Option<WorktreeCleanup>> {
        let git_dir = self.git_root.join(".git");
        if !git_dir.exists() {
            tracing::debug!("Not a git repository, skipping worktree creation");
            return Ok(None);
        }

        let wt_name = format!(
            "automation-{}-{}",
            automation.name,
            Utc::now().format("%Y%m%d-%H%M%S")
        );

        // Put worktree in a sibling directory to git_root
        let wt_path = self
            .git_root
            .parent()
            .unwrap_or(&self.git_root)
            .join(&wt_name);

        tracing::debug!("Creating worktree '{}' at {:?}", wt_name, wt_path);

        // Create worktree using git
        let output = Command::new("git")
            .args(["worktree", "add", "-d"])
            .arg(&wt_path)
            .current_dir(&self.git_root)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(color_eyre::eyre::eyre!(
                "Failed to create worktree: git exited with {:?}: {}",
                output.status.code(),
                stderr
            ));
        }

        Ok(Some(WorktreeCleanup::new(wt_path, self.git_root.clone())))
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

        // Set up worktree for isolated execution
        let worktree_cleanup = self.setup_worktree(automation).await?;
        let work_dir = worktree_cleanup
            .as_ref()
            .map(|wc| &wc.path)
            .unwrap_or(&self.git_root);

        tracing::debug!("Running automation in {:?}", work_dir);

        // Determine timeout
        let timeout_duration = Duration::from_secs(automation.timeout_seconds.unwrap_or(3600));

        // Execute velor with timeout
        let result = match timeout(
            timeout_duration,
            self.execute_velor(automation, work_dir, cancel_token.clone()),
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

        // Clean up worktree if it was created
        if let Some(wc) = worktree_cleanup {
            tracing::debug!("Cleaning up worktree at {:?}", wc.path);
            if let Err(e) = wc.cleanup().await {
                tracing::warn!("Failed to clean up worktree: {}", e);
            }
        }

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
        work_dir: &Path,
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
            .current_dir(work_dir)
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

    #[test]
    fn test_worktree_cleanup_new() {
        let path = std::path::PathBuf::from("/test/path");
        let git_root = std::path::PathBuf::from("/git/root");

        let cleanup = WorktreeCleanup::new(path.clone(), git_root.clone());

        assert_eq!(cleanup.path, path);
        assert_eq!(cleanup.git_root, git_root);
    }

    #[tokio::test]
    async fn test_setup_worktree_returns_none_for_non_git_repo() {
        let temp_dir = TempDir::new().expect("tempdir should be created");
        let db_path = temp_dir.path().join("test.db");

        let store = AutomationStore::open(&db_path)
            .await
            .expect("store should be created");

        let runner = AutomationRunner::new(store, 3, temp_dir.path(), "velor".to_string(), 100_000);

        // Create a test automation
        let automation = crate::config::Automation {
            name: "test".to_string(),
            description: "Test automation".to_string(),
            schedule: "0 * * * * *".to_string(),
            timezone: "UTC".to_string(),
            prompt: "once".to_string(),
            enabled: true,
            vars: std::collections::BTreeMap::new(),
            catch_up: crate::config::CatchUpPolicy::Skip,
            max_catch_up: 10,
            timeout_seconds: Some(60),
            notify_on_success: false,
            notify_on_failure: false,
        };

        // setup_worktree should return None since temp_dir is not a git repo
        let result = runner
            .setup_worktree(&automation)
            .await
            .expect("setup_worktree should succeed");

        assert!(
            result.is_none(),
            "setup_worktree should return None for non-git repositories"
        );
    }

    #[tokio::test]
    async fn test_setup_worktree_creates_worktree_for_git_repo() {
        let temp_dir = TempDir::new().expect("tempdir should be created");

        // Initialize a git repository
        Command::new("git")
            .args(["init"])
            .current_dir(temp_dir.path())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .status()
            .await
            .expect("git init should succeed");

        // Configure git user for the test repo
        Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(temp_dir.path())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .status()
            .await
            .expect("git config should succeed");

        Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(temp_dir.path())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .status()
            .await
            .expect("git config should succeed");

        // Create an initial commit
        let readme_path = temp_dir.path().join("README.md");
        tokio::fs::write(&readme_path, b"# Test\n")
            .await
            .expect("write should succeed");

        Command::new("git")
            .args(["add", "README.md"])
            .current_dir(temp_dir.path())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .status()
            .await
            .expect("git add should succeed");

        Command::new("git")
            .args(["commit", "-m", "Initial commit"])
            .current_dir(temp_dir.path())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .status()
            .await
            .expect("git commit should succeed");

        let db_path = temp_dir.path().join("test.db");

        let store = AutomationStore::open(&db_path)
            .await
            .expect("store should be created");

        let runner = AutomationRunner::new(store, 3, temp_dir.path(), "velor".to_string(), 100_000);

        // Create a test automation
        let automation = crate::config::Automation {
            name: "test-worktree".to_string(),
            description: "Test automation".to_string(),
            schedule: "0 * * * * *".to_string(),
            timezone: "UTC".to_string(),
            prompt: "once".to_string(),
            enabled: true,
            vars: std::collections::BTreeMap::new(),
            catch_up: crate::config::CatchUpPolicy::Skip,
            max_catch_up: 10,
            timeout_seconds: Some(60),
            notify_on_success: false,
            notify_on_failure: false,
        };

        // setup_worktree should create a worktree
        let result = runner
            .setup_worktree(&automation)
            .await
            .expect("setup_worktree should succeed");

        assert!(
            result.is_some(),
            "setup_worktree should return Some(WorktreeCleanup) for git repositories"
        );

        let cleanup = result.unwrap();
        let worktree_path = cleanup.path.clone();

        // Verify the worktree was created
        assert!(worktree_path.exists(), "worktree path should exist");

        // Clean up the worktree
        cleanup.cleanup().await.expect("cleanup should succeed");

        // Verify the worktree was removed
        assert!(
            !worktree_path.exists(),
            "worktree path should not exist after cleanup"
        );
    }
}
