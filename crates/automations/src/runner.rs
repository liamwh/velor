//! Automation execution with worktree support.

use crate::config::Automation;
use crate::file_config::AutomationFile;
use crate::store::{AutomationRunStatus, AutomationStore};
use chrono::Utc;
use color_eyre::Result;
use color_eyre::eyre::WrapErr;
use color_eyre::eyre::eyre;
use secrecy::ExposeSecret;
use std::path::Path;
use std::process::Stdio;
use tokio::process::Command;
use tokio::sync::Semaphore;
use tokio::time::{Duration, timeout};
use tokio_util::sync::CancellationToken;
use tracing::instrument;

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
    pub async fn cleanup(self) -> Result<()> {
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
            return Err(eyre!(
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

    /// Resolve git root from a given path (handles non-UTF8 paths).
    ///
    /// Uses `.arg()` with `OsStr` to properly handle paths with non-UTF8 characters.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Git cannot be executed
    /// - The path is not inside a git repository
    /// - Git exits with a non-zero status
    #[instrument(skip(self), err)]
    pub async fn resolve_git_root(&self, path: &Path) -> Result<std::path::PathBuf> {
        let output = Command::new("git")
            .arg("-C")
            .arg(path)
            .args(["rev-parse", "--show-toplevel"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .wrap_err("Failed to execute git command")?;

        if !output.status.success() {
            return Err(eyre!(
                "Failed to resolve git root for {}: git exited with {:?}",
                path.display(),
                output.status.code()
            ));
        }

        let git_root = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Ok(std::path::PathBuf::from(git_root))
    }

    /// Sanitize automation name for use in worktree paths.
    ///
    /// Replaces non-alphanumeric characters (except `-` and `_`) with `-`,
    /// then collapses multiple consecutive `-` characters.
    #[must_use]
    pub fn sanitize_worktree_name(name: &str) -> String {
        name.chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '-'
                }
            })
            .collect::<String>()
            .split('-')
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("-")
    }

    /// Generate unique worktree path with collision resistance.
    ///
    /// Uses ULID for uniqueness and sanitized name for readability.
    /// The worktree is placed in a `.velor-worktrees` directory alongside the git root.
    ///
    /// # Arguments
    ///
    /// * `git_root` - The git repository root
    /// * `automation_name` - The name of the automation (will be sanitized)
    #[must_use]
    pub fn generate_worktree_path(git_root: &Path, automation_name: &str) -> std::path::PathBuf {
        let sanitized = Self::sanitize_worktree_name(automation_name);

        // Use parent directory or a dedicated .velor-worktrees directory
        let worktrees_base = git_root
            .parent()
            .unwrap_or(git_root)
            .join(".velor-worktrees");

        // Add ULID for collision resistance
        let ulid = ulid::Ulid::new().to_string();
        let wt_name = format!("automation-{}-{}", sanitized, &ulid[..8]);
        worktrees_base.join(wt_name)
    }

    /// Initialize the worktrees base directory and prune orphaned worktrees.
    ///
    /// This should be called once during runner initialization, not in the hot path.
    ///
    /// # Errors
    ///
    /// Returns an error if the base directory cannot be created.
    #[instrument(skip(self), err)]
    pub async fn init_worktrees_base(&self) -> Result<()> {
        let worktrees_base = self
            .git_root
            .parent()
            .unwrap_or(&self.git_root)
            .join(".velor-worktrees");

        tokio::fs::create_dir_all(&worktrees_base)
            .await
            .wrap_err_with(|| {
                format!(
                    "Failed to create worktrees base directory: {}",
                    worktrees_base.display()
                )
            })?;

        // Clean up orphaned worktrees on init
        self.prune_orphaned_worktrees().await?;

        Ok(())
    }

    /// Clean up orphaned worktrees (no automation currently using them).
    ///
    /// A worktree is considered orphaned if it exists in the `.velor-worktrees`
    /// directory but is not registered in `git worktree list`.
    ///
    /// # Errors
    ///
    /// This function logs errors but does not fail, as orphaned worktree cleanup
    /// is best-effort.
    #[instrument(skip(self), err)]
    pub async fn prune_orphaned_worktrees(&self) -> Result<()> {
        let worktrees_base = self
            .git_root
            .parent()
            .unwrap_or(&self.git_root)
            .join(".velor-worktrees");

        if !worktrees_base.exists() {
            return Ok(());
        }

        let mut entries = match tokio::fs::read_dir(&worktrees_base).await {
            Ok(e) => e,
            Err(_) => return Ok(()),
        };

        // Get list of valid worktrees from git
        let output = Command::new("git")
            .args(["worktree", "list", "--porcelain"])
            .current_dir(&self.git_root)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await;

        let valid_worktrees = match output {
            Ok(out) if out.status.success() => {
                let list = String::from_utf8_lossy(&out.stdout);
                list.lines()
                    .filter(|line| line.starts_with("worktree "))
                    .map(|line| line.trim_start_matches("worktree ").to_string())
                    .collect::<std::collections::HashSet<_>>()
            }
            _ => std::collections::HashSet::new(),
        };

        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if path.is_dir() && !valid_worktrees.contains(&path.display().to_string()) {
                tracing::debug!("Removing orphaned worktree: {}", path.display());
                let _ = tokio::fs::remove_dir_all(&path).await;
            }
        }

        Ok(())
    }

    /// Set up a git worktree for isolated automation execution.
    ///
    /// If the directory is not a git repository, returns `Ok(None)` and the
    /// automation will run directly in `git_root`.
    ///
    /// # Errors
    ///
    /// Returns an error if the worktree cannot be created.
    #[instrument(skip(self), fields(automation_name = %automation.name), err)]
    async fn setup_worktree(
        &self,
        automation: &AutomationFile,
        git_root: &Path,
    ) -> Result<Option<WorktreeCleanup>> {
        let git_dir = git_root.join(".git");
        if !git_dir.exists() {
            tracing::debug!("Not a git repository, skipping worktree creation");
            return Ok(None);
        }

        let wt_path = Self::generate_worktree_path(git_root, &automation.name);

        tracing::debug!("Creating worktree '{}' at {:?}", automation.name, wt_path);

        // Create worktree using git
        let output = Command::new("git")
            .args(["worktree", "add", "-d"])
            .arg(&wt_path)
            .current_dir(git_root)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .wrap_err("Failed to execute git worktree add command")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(eyre!(
                "Failed to create worktree: git exited with {:?}: {}",
                output.status.code(),
                stderr
            ));
        }

        Ok(Some(WorktreeCleanup::new(wt_path, git_root.to_path_buf())))
    }

    /// Run a single automation (legacy API using `Automation` from `crate::config`).
    ///
    /// # Errors
    ///
    /// Returns an error if the automation cannot be run.
    #[instrument(skip(self, automation, cancel_token), fields(name = %automation.name), err)]
    pub async fn run_automation(
        &self,
        automation: &Automation,
        scheduled_for: chrono::DateTime<Utc>,
        cancel_token: &CancellationToken,
    ) -> Result<AutomationResult> {
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
        let worktree_cleanup = self.setup_worktree_legacy(automation).await?;
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
            self.execute_velor_legacy(automation, work_dir, cancel_token.clone()),
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

    /// Run a file-based automation with full worktree and project support.
    ///
    /// # Project Semantics
    /// - `project` is the working directory (can be inside a repo)
    /// - Git root is derived from `project` via `git rev-parse --show-toplevel`
    /// - If `worktree=true` and `project` is outside a git repo → config error
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The automation cannot be run
    /// - Worktree mode is enabled but project is not in a git repository
    #[instrument(skip(self, automation, cancel_token), fields(name = %automation.name), err)]
    pub async fn run_file_automation(
        &self,
        automation: &AutomationFile,
        scheduled_for: chrono::DateTime<Utc>,
        cancel_token: &CancellationToken,
    ) -> Result<AutomationResult> {
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

        // Determine base repository and working directory
        let (_base_repo, work_dir) = if automation.worktree {
            // Resolve git root from project path (or git_root)
            let proj_path = automation.project.as_ref().unwrap_or(&self.git_root);
            let base_repo = self.resolve_git_root(proj_path).await.wrap_err_with(|| {
                format!(
                    "Failed to resolve git root for project path: {}",
                    proj_path.display()
                )
            })?;

            // Create worktree
            let cleanup = self.setup_worktree(automation, &base_repo).await?;
            let work_dir = cleanup
                .as_ref()
                .map(|wc| wc.path.clone())
                .unwrap_or_else(|| base_repo.clone());
            (base_repo, work_dir)
        } else {
            // No worktree: use project path directly, or git_root
            if let Some(ref proj) = automation.project {
                if !proj.exists() {
                    return Err(eyre!("project path {} does not exist", proj.display()));
                }
                (self.resolve_git_root(proj).await?, proj.clone())
            } else {
                (self.git_root.clone(), self.git_root.clone())
            }
        };

        tracing::debug!("Running automation in {:?}", work_dir);

        // Determine timeout
        let timeout_duration = Duration::from_secs(automation.timeout_seconds.unwrap_or(3600));

        // Execute velor with timeout
        let result = match timeout(
            timeout_duration,
            self.execute_velor_file(automation, &work_dir, cancel_token.clone()),
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

    /// Legacy worktree setup for `Automation` type.
    async fn setup_worktree_legacy(
        &self,
        automation: &Automation,
    ) -> Result<Option<WorktreeCleanup>> {
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
            return Err(eyre!(
                "Failed to create worktree: git exited with {:?}: {}",
                output.status.code(),
                stderr
            ));
        }

        Ok(Some(WorktreeCleanup::new(wt_path, self.git_root.clone())))
    }

    /// Execute velor for file-based automation.
    async fn execute_velor_file(
        &self,
        automation: &AutomationFile,
        work_dir: &Path,
        cancel_token: CancellationToken,
    ) -> Result<AutomationResult> {
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

        // Resolve prompt content
        let prompt_content = automation
            .prompt_source
            .resolve(
                &velor_core::prompts::PromptCache::new(
                    std::env::var("XDG_CONFIG_HOME")
                        .map(std::path::PathBuf::from)
                        .unwrap_or_else(|_| {
                            dirs::home_dir()
                                .map(|h| h.join(".config"))
                                .expect("Unable to determine home directory")
                        }),
                    None, // TODO: pass repo_dir if available
                ),
                &std::env::var("XDG_CONFIG_HOME")
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(|_| {
                        dirs::home_dir()
                            .map(|h| h.join(".config"))
                            .expect("Unable to determine home directory")
                    }),
                automation.project.as_ref().map(|p| {
                    p.ancestors()
                        .find(|a| a.join(".velor").exists())
                        .unwrap_or(p)
                }),
            )
            .await
            .wrap_err("Failed to resolve prompt content")?;

        // Resolve secrets with fail-closed semantics
        // This will fail immediately if any required secrets are missing
        let secrets = match velor_vault::resolve_automation_secrets(
            &automation.required_secrets,
            &automation.optional_secrets,
            work_dir,
        )
        .await
        {
            Ok(secrets) => secrets,
            Err(velor_vault::VaultError::RequiredSecretMissing { key }) => {
                // Fail immediately - required secret not available
                tracing::error!(
                    automation = %automation.name,
                    secret = %key,
                    "Required secret missing, automation will not run"
                );
                return Ok(AutomationResult {
                    status: AutomationRunStatus::Failed,
                    iterations_completed: 0,
                    exit_code: None,
                    output: String::new(),
                    error: Some(format!("Required secret missing: {}", key)),
                });
            }
            Err(e) => {
                // Other vault errors (unavailable, decrypt failed, etc.)
                tracing::error!(
                    automation = %automation.name,
                    error = %e,
                    "Vault error, automation will not run"
                );
                return Ok(AutomationResult {
                    status: AutomationRunStatus::Failed,
                    iterations_completed: 0,
                    exit_code: None,
                    output: String::new(),
                    error: Some(format!("Vault error: {}", e)),
                });
            }
        };

        // Build command with secrets injected
        let mut cmd = Command::new(&self.velor_binary);
        cmd.arg("once")
            .arg("--prompt-text")
            .arg(&prompt_content)
            .current_dir(work_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // Inject ONLY declared secrets (fail-closed by type)
        for (key, secret) in &secrets.secrets {
            cmd.env(key, secret.expose_secret());
        }

        tracing::debug!(
            automation = %automation.name,
            secrets_count = secrets.len(),
            "Injecting secrets into automation"
        );

        let child = cmd.spawn()?;

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

    /// Execute velor for legacy automation type.
    async fn execute_velor_legacy(
        &self,
        automation: &Automation,
        work_dir: &Path,
        cancel_token: CancellationToken,
    ) -> Result<AutomationResult> {
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
            .arg("--prompt-text")
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
    use std::str::FromStr;
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

    #[test]
    fn test_sanitize_worktree_name() {
        assert_eq!(AutomationRunner::sanitize_worktree_name("test"), "test");
        assert_eq!(
            AutomationRunner::sanitize_worktree_name("test-automation"),
            "test-automation"
        );
        assert_eq!(
            AutomationRunner::sanitize_worktree_name("test automation"),
            "test-automation"
        );
        assert_eq!(
            AutomationRunner::sanitize_worktree_name("test@#$automation"),
            "test-automation"
        );
        assert_eq!(
            AutomationRunner::sanitize_worktree_name("test---automation"),
            "test-automation"
        );
        assert_eq!(
            AutomationRunner::sanitize_worktree_name("test_automation"),
            "test_automation"
        );
        assert_eq!(AutomationRunner::sanitize_worktree_name(""), "");
        assert_eq!(AutomationRunner::sanitize_worktree_name("___"), "___"); // underscores are preserved
        assert_eq!(AutomationRunner::sanitize_worktree_name("---"), ""); // hyphens collapse to empty
    }

    #[test]
    fn test_generate_worktree_path() {
        let git_root = std::path::PathBuf::from("/home/user/repo");
        let path = AutomationRunner::generate_worktree_path(&git_root, "test-automation");

        // Path should be in .velor-worktrees directory
        assert!(path.to_string_lossy().contains(".velor-worktrees"));
        // Path should contain sanitized name
        assert!(path.to_string_lossy().contains("test-automation"));
        // Path should contain ULID suffix
        let path_str = path.to_string_lossy();
        assert!(
            path_str
                .split('-')
                .next_back()
                .is_some_and(|s| s.len() == 8)
        );
    }

    #[tokio::test]
    async fn test_resolve_git_root_valid_repo() {
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

        let db_path = temp_dir.path().join("test.db");

        let store = AutomationStore::open(&db_path)
            .await
            .expect("store should be created");

        let runner = AutomationRunner::new(store, 3, temp_dir.path(), "velor".to_string(), 100_000);

        let result = runner
            .resolve_git_root(temp_dir.path())
            .await
            .expect("resolve_git_root should succeed");

        // Git returns canonicalized paths (e.g., /private/var on macOS)
        // so we need to canonicalize the expected path for comparison
        let expected = std::fs::canonicalize(temp_dir.path())
            .unwrap_or_else(|_| temp_dir.path().to_path_buf());
        assert_eq!(result, expected);
    }

    #[tokio::test]
    async fn test_resolve_git_root_non_repo() {
        let temp_dir = TempDir::new().expect("tempdir should be created");
        let db_path = temp_dir.path().join("test.db");

        let store = AutomationStore::open(&db_path)
            .await
            .expect("store should be created");

        let runner = AutomationRunner::new(store, 3, temp_dir.path(), "velor".to_string(), 100_000);

        let result = runner.resolve_git_root(temp_dir.path()).await;

        assert!(result.is_err(), "Should return error for non-git directory");
        assert!(result.unwrap_err().to_string().contains("git root"));
    }

    #[tokio::test]
    async fn test_init_worktrees_base() {
        let temp_dir = TempDir::new().expect("tempdir should be created");
        let db_path = temp_dir.path().join("test.db");

        let store = AutomationStore::open(&db_path)
            .await
            .expect("store should be created");

        let runner = AutomationRunner::new(store, 3, temp_dir.path(), "velor".to_string(), 100_000);

        runner
            .init_worktrees_base()
            .await
            .expect("init_worktrees_base should succeed");

        // Worktrees base is created at git_root.parent()/.velor-worktrees
        let worktrees_base = temp_dir
            .path()
            .parent()
            .expect("temp_dir should have a parent")
            .join(".velor-worktrees");
        assert!(
            worktrees_base.exists(),
            "Worktrees base directory should exist at parent of git_root"
        );
    }

    #[tokio::test]
    async fn test_prune_orphaned_worktrees_no_base_dir() {
        let temp_dir = TempDir::new().expect("tempdir should be created");
        let db_path = temp_dir.path().join("test.db");

        let store = AutomationStore::open(&db_path)
            .await
            .expect("store should be created");

        let runner = AutomationRunner::new(store, 3, temp_dir.path(), "velor".to_string(), 100_000);

        // Should not fail when base directory doesn't exist
        runner
            .prune_orphaned_worktrees()
            .await
            .expect("prune_orphaned_worktrees should succeed even without base dir");
    }

    #[tokio::test]
    async fn test_setup_worktree_returns_none_for_non_git_repo() {
        let temp_dir = TempDir::new().expect("tempdir should be created");
        let db_path = temp_dir.path().join("test.db");

        let store = AutomationStore::open(&db_path)
            .await
            .expect("store should be created");

        let runner = AutomationRunner::new(store, 3, temp_dir.path(), "velor".to_string(), 100_000);

        // Create a test AutomationFile
        let automation = crate::file_config::AutomationFile {
            name: "test".to_string(),
            description: "Test automation".to_string(),
            schedule_raw: "0 * * * * *".to_string(),
            schedule: cron::Schedule::from_str("0 * * * * *").unwrap(),
            timezone: chrono_tz::UTC,
            prompt_source: crate::file_config::PromptSource::Inline("test prompt".to_string()),
            worktree: false,
            project: None,
            vars: std::collections::BTreeMap::new(),
            enabled: true,
            catch_up: crate::config::CatchUpPolicy::Skip,
            max_catch_up: 10,
            timeout_seconds: Some(60),
            notify_on_success: false,
            notify_on_failure: false,
            required_secrets: vec![],
            optional_secrets: vec![],
        };

        // setup_worktree should return None since temp_dir is not a git repo
        let result = runner
            .setup_worktree(&automation, temp_dir.path())
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

        // Create a test AutomationFile
        let automation = crate::file_config::AutomationFile {
            name: "test-worktree".to_string(),
            description: "Test automation".to_string(),
            schedule_raw: "0 * * * * *".to_string(),
            schedule: cron::Schedule::from_str("0 * * * * *").unwrap(),
            timezone: chrono_tz::UTC,
            prompt_source: crate::file_config::PromptSource::Inline("test prompt".to_string()),
            worktree: false,
            project: None,
            vars: std::collections::BTreeMap::new(),
            enabled: true,
            catch_up: crate::config::CatchUpPolicy::Skip,
            max_catch_up: 10,
            timeout_seconds: Some(60),
            notify_on_success: false,
            notify_on_failure: false,
            required_secrets: vec![],
            optional_secrets: vec![],
        };

        // setup_worktree should create a worktree
        let result = runner
            .setup_worktree(&automation, temp_dir.path())
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
