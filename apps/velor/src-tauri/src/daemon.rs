//! Background daemon for scheduled automation execution.
//!
//! The daemon runs in the background, periodically checking for automations
//! that are due to run based on their cron schedules. It executes these
//! automations and emits events to the frontend for real-time updates.

use chrono::{DateTime, Utc};
use color_eyre::Result;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio::time::interval;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, instrument, warn};

use velor_automations::scheduler::Scheduler;
use velor_automations::{
    Automation, AutomationResult, AutomationRunner, CatchUpPolicy, load_automations,
};

/// Default tick interval for the daemon (60 seconds).
const DEFAULT_TICK_INTERVAL: Duration = Duration::from_secs(60);

/// Default maximum concurrent automations.
const DEFAULT_MAX_CONCURRENT: u32 = 3;

/// Default maximum output bytes to capture.
const DEFAULT_MAX_OUTPUT_BYTES: usize = 100_000;

/// Background daemon for scheduled automation execution.
///
/// The daemon runs a tick loop that periodically checks for automations
/// that are due to run based on their cron schedules. It maintains
/// tracking of last run times for each automation and handles catch-up
/// policies for missed runs.
#[derive(Debug, Clone)]
pub struct BackgroundDaemon {
    /// Git root directory for automation definitions.
    git_root: Arc<RwLock<Option<PathBuf>>>,
    /// Automation store for the runner (points to unified velor.db).
    automation_store: Arc<RwLock<Option<velor_automations::AutomationStore>>>,
    /// Merged configuration (for automation directory, etc.).
    config: Arc<RwLock<Option<velor_core::FileConfig>>>,
    /// Last run time for each automation (name -> timestamp).
    last_run_times: Arc<RwLock<HashMap<String, DateTime<Utc>>>>,
    /// Tick interval for checking automations.
    tick_interval: Duration,
    /// Velor binary path for executing automations.
    velor_binary: String,
    /// Maximum concurrent automations.
    max_concurrent: u32,
    /// Maximum output bytes to capture.
    max_output_bytes: usize,
}

impl BackgroundDaemon {
    /// Creates a new background daemon.
    #[must_use]
    pub fn new() -> Self {
        Self {
            git_root: Arc::new(RwLock::new(None)),
            automation_store: Arc::new(RwLock::new(None)),
            config: Arc::new(RwLock::new(None)),
            last_run_times: Arc::new(RwLock::new(HashMap::new())),
            tick_interval: DEFAULT_TICK_INTERVAL,
            velor_binary: "velor".to_string(),
            max_concurrent: DEFAULT_MAX_CONCURRENT,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
        }
    }

    /// Sets the git root directory.
    #[instrument(skip(self), level = "debug")]
    pub async fn set_git_root(&self, path: PathBuf) {
        debug!(?path, "Setting daemon git root");
        *self.git_root.write().await = Some(path);
    }

    /// Sets the automation store.
    #[instrument(skip(self), level = "debug")]
    pub async fn set_automation_store(&self, store: velor_automations::AutomationStore) {
        debug!("Setting daemon automation store");
        *self.automation_store.write().await = Some(store);
    }

    /// Sets the configuration.
    #[instrument(skip(self), level = "debug")]
    pub async fn set_config(&self, config: velor_core::FileConfig) {
        debug!("Setting daemon config");
        *self.config.write().await = Some(config);
    }

    /// Sets the velor binary path.
    #[must_use]
    pub fn with_velor_binary(mut self, binary: String) -> Self {
        self.velor_binary = binary;
        self
    }

    /// Sets the tick interval.
    #[must_use]
    pub fn with_tick_interval(mut self, interval: Duration) -> Self {
        self.tick_interval = interval;
        self
    }

    /// Sets the maximum concurrent automations.
    #[must_use]
    pub fn with_max_concurrent(mut self, max: u32) -> Self {
        self.max_concurrent = max;
        self
    }

    /// Starts the daemon loop.
    ///
    /// This runs indefinitely until the cancel token is triggered.
    /// On each tick, it checks for automations that are due and executes them.
    ///
    /// # Errors
    ///
    /// Returns an error if the daemon cannot be started.
    #[instrument(skip(self, cancel_token), level = "debug", err)]
    pub async fn run(&self, cancel_token: CancellationToken) -> Result<()> {
        info!("Starting background daemon");

        let mut ticker = interval(self.tick_interval);
        ticker.tick().await; // First tick completes immediately

        loop {
            tokio::select! {
                _ = cancel_token.cancelled() => {
                    info!("Daemon cancel token triggered, shutting down");
                    break;
                }
                _ = ticker.tick() => {
                    if let Err(e) = self.tick().await {
                        error!("Error during daemon tick: {}", e);
                        // Continue running despite errors
                    }
                }
            }
        }

        info!("Background daemon stopped");
        Ok(())
    }

    /// Performs a single tick: checks for due automations and executes them.
    ///
    /// # Errors
    ///
    /// Returns an error if the tick cannot be completed.
    #[instrument(skip(self), level = "debug", err)]
    async fn tick(&self) -> Result<()> {
        debug!("Starting daemon tick");

        // Get required components
        let git_root = self.git_root.read().await;
        let git_root = git_root
            .as_ref()
            .ok_or_else(|| color_eyre::eyre::eyre!("Git root not set for daemon"))?;
        let store_opt = self.automation_store.read().await;
        let store = store_opt.as_ref().ok_or_else(|| {
            color_eyre::eyre::eyre!("Automation store not initialized for daemon")
        })?;
        let config_opt = self.config.read().await;
        let config = config_opt
            .as_ref()
            .ok_or_else(|| color_eyre::eyre::eyre!("Configuration not set for daemon"))?;

        let automations_dir = git_root.join(&config.automations.automations_dir);

        // Load all automations
        let automations = load_automations(&automations_dir).await?;
        debug!(count = automations.len(), "Loaded automations for tick");

        // Filter enabled automations
        let enabled_automations: Vec<_> = automations.into_iter().filter(|a| a.enabled).collect();

        if enabled_automations.is_empty() {
            debug!("No enabled automations to process");
            return Ok(());
        }

        // Check each automation for due execution
        let mut due_automations = Vec::new();
        let last_runs = self.last_run_times.read().await;

        for automation in enabled_automations {
            match self.check_if_due(&automation, &last_runs, store).await {
                Ok(Some(scheduled_for)) => {
                    due_automations.push((automation, scheduled_for));
                }
                Ok(None) => {
                    debug!(name = %automation.name, "Automation not due");
                }
                Err(e) => {
                    warn!(
                        name = %automation.name,
                        error = %e,
                        "Error checking if automation is due"
                    );
                }
            }
        }

        drop(last_runs);

        if due_automations.is_empty() {
            debug!("No automations due in this tick");
            return Ok(());
        }

        info!(count = due_automations.len(), "Found due automations");

        // Create runner and execute due automations
        let runner = AutomationRunner::new(
            store.clone(),
            self.max_concurrent,
            git_root,
            self.velor_binary.clone(),
            self.max_output_bytes,
        );

        for (automation, scheduled_for) in due_automations {
            let cancel_token = CancellationToken::new();
            let automation_name = automation.name.clone();

            // Update last run time before execution to prevent double-runs
            {
                let mut last_runs = self.last_run_times.write().await;
                last_runs.insert(automation_name.clone(), Utc::now());
            }

            match runner
                .run_automation(&automation, scheduled_for, &cancel_token)
                .await
            {
                Ok(result) => {
                    info!(
                        name = %automation_name,
                        status = ?result.status,
                        iterations = result.iterations_completed,
                        "Automation execution completed"
                    );

                    // Emit event to frontend
                    self.emit_automation_completed(&automation_name, &result)
                        .await;
                }
                Err(e) => {
                    error!(
                        name = %automation_name,
                        error = %e,
                        "Automation execution failed"
                    );

                    // Emit error event to frontend
                    self.emit_automation_failed(&automation_name, &e.to_string())
                        .await;
                }
            }
        }

        debug!("Daemon tick completed");
        Ok(())
    }

    /// Checks if an automation is due to run based on its schedule and last run time.
    ///
    /// # Returns
    ///
    /// - `Ok(Some(scheduled_for))` if the automation is due, with the scheduled time
    /// - `Ok(None)` if the automation is not due
    /// - `Err` if checking fails
    #[instrument(skip(self, automation, last_runs, store), level = "trace", ret, err)]
    async fn check_if_due(
        &self,
        automation: &Automation,
        last_runs: &HashMap<String, DateTime<Utc>>,
        store: &velor_automations::AutomationStore,
    ) -> Result<Option<DateTime<Utc>>> {
        let now = Utc::now();

        // Get the last run time from our tracking
        let last_run = *last_runs.get(&automation.name).unwrap_or(&now);

        // Also check the store for the most recent completed run
        let store_runs = store.get_runs(Some(&automation.name), 1).await?;
        let last_completed_run = store_runs.first().and_then(|r| {
            if r.status.is_terminal() {
                Some(r.completed_at.unwrap_or(r.started_at))
            } else {
                None
            }
        });

        // Use the most recent of either source
        let effective_last = last_completed_run
            .map(|t| t.max(last_run))
            .unwrap_or(last_run);

        // Parse timezone
        let tz: chrono_tz::Tz = automation.timezone.parse().unwrap_or(chrono_tz::UTC);

        // Create scheduler
        let scheduler = Scheduler::new(&automation.schedule, tz)?;

        // Check for next scheduled time
        let next_scheduled = scheduler.next_after(effective_last);

        if next_scheduled <= now {
            // Automation is due - check for catch-up policy
            let missed = scheduler.missed_runs_since(effective_last, now, automation.max_catch_up);

            match automation.catch_up {
                CatchUpPolicy::Skip => {
                    // Skip all missed, run once
                    debug!(
                        name = %automation.name,
                        missed_count = missed.len(),
                        "Skipping missed runs, executing once"
                    );
                    Ok(Some(now))
                }
                CatchUpPolicy::RunOnce => {
                    // Run once regardless of how many were missed
                    debug!(
                        name = %automation.name,
                        missed_count = missed.len(),
                        "Running once for missed runs"
                    );
                    Ok(Some(now))
                }
                CatchUpPolicy::RunAll => {
                    // Run all missed schedules (potentially dangerous)
                    if missed.is_empty() {
                        Ok(Some(now))
                    } else {
                        warn!(
                            name = %automation.name,
                            missed_count = missed.len(),
                            "Running all missed schedules (may be dangerous)"
                        );
                        Ok(Some(missed[0]))
                    }
                }
            }
        } else {
            // Not due yet
            Ok(None)
        }
    }

    /// Emits an automation completed event to the frontend.
    #[instrument(skip(self, name, result), level = "trace")]
    async fn emit_automation_completed(&self, name: &str, result: &AutomationResult) {
        debug!(
            name = %name,
            status = ?result.status,
            "Emitting automation completed event"
        );
        // TODO: Emit Tauri event when frontend is integrated
        // For now, this is a placeholder for event emission
    }

    /// Emits an automation failed event to the frontend.
    #[instrument(skip(self, name, error), level = "trace")]
    async fn emit_automation_failed(&self, name: &str, error: &str) {
        debug!(
            name = %name,
            error = %error,
            "Emitting automation failed event"
        );
        // TODO: Emit Tauri event when frontend is integrated
        // For now, this is a placeholder for event emission
    }

    /// Returns the last run time for an automation.
    #[must_use]
    pub async fn last_run_time(&self, name: &str) -> Option<DateTime<Utc>> {
        self.last_run_times.read().await.get(name).copied()
    }

    /// Returns all tracked last run times.
    #[must_use]
    pub async fn all_last_run_times(&self) -> HashMap<String, DateTime<Utc>> {
        self.last_run_times.read().await.clone()
    }

    /// Clears the last run time tracking for an automation.
    #[instrument(skip(self), level = "debug")]
    pub async fn clear_last_run_time(&self, name: &str) {
        self.last_run_times.write().await.remove(name);
    }

    /// Clears all last run time tracking.
    #[instrument(skip(self), level = "debug")]
    pub async fn clear_all_last_run_times(&self) {
        self.last_run_times.write().await.clear();
    }
}

impl Default for BackgroundDaemon {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use tokio::time::{sleep, timeout};

    #[test]
    fn test_background_daemon_new() {
        let daemon = BackgroundDaemon::new();

        assert_eq!(daemon.velor_binary, "velor");
        assert_eq!(daemon.tick_interval, DEFAULT_TICK_INTERVAL);
        assert_eq!(daemon.max_concurrent, DEFAULT_MAX_CONCURRENT);
        assert_eq!(daemon.max_output_bytes, DEFAULT_MAX_OUTPUT_BYTES);
    }

    #[test]
    fn test_background_daemon_default() {
        let daemon = BackgroundDaemon::default();

        assert_eq!(daemon.velor_binary, "velor");
    }

    #[test]
    fn test_background_daemon_builder() {
        let daemon = BackgroundDaemon::new()
            .with_velor_binary("custom-velor".to_string())
            .with_tick_interval(Duration::from_secs(30))
            .with_max_concurrent(5);

        assert_eq!(daemon.velor_binary, "custom-velor");
        assert_eq!(daemon.tick_interval, Duration::from_secs(30));
        assert_eq!(daemon.max_concurrent, 5);
    }

    #[tokio::test]
    async fn test_set_git_root() {
        let daemon = BackgroundDaemon::new();
        let path = PathBuf::from("/test/path");

        daemon.set_git_root(path.clone()).await;

        let stored = daemon.git_root.read().await;
        assert_eq!(stored.as_ref(), Some(&path));
    }

    #[tokio::test]
    async fn test_last_run_time_tracking() {
        let daemon = BackgroundDaemon::new();

        // Initially no last run times
        assert!(daemon.last_run_time("test").await.is_none());

        // After clearing (which does nothing for non-existent)
        daemon.clear_last_run_time("test").await;
        assert!(daemon.last_run_time("test").await.is_none());

        // Clear all doesn't panic
        daemon.clear_all_last_run_times().await;
        assert!(daemon.all_last_run_times().await.is_empty());
    }

    #[tokio::test]
    async fn test_daemon_run_cancels_immediately() {
        let daemon = BackgroundDaemon::new();

        let cancel_token = CancellationToken::new();
        cancel_token.cancel();

        // Should exit immediately due to cancellation
        let result = timeout(Duration::from_millis(100), daemon.run(cancel_token.clone())).await;

        assert!(result.is_ok(), "Daemon should exit immediately on cancel");
        assert!(result.unwrap().is_ok(), "Daemon run should return Ok");
    }

    #[tokio::test]
    async fn test_daemon_run_cancels_after_delay() {
        let daemon = BackgroundDaemon::new();
        let cancel_token = CancellationToken::new();

        // Cancel after a short delay
        let token_clone = cancel_token.clone();
        tokio::spawn(async move {
            sleep(Duration::from_millis(50)).await;
            token_clone.cancel();
        });

        // Should exit after the delay
        let result = timeout(Duration::from_secs(1), daemon.run(cancel_token)).await;

        assert!(result.is_ok(), "Daemon should exit within timeout");
        assert!(result.unwrap().is_ok(), "Daemon run should return Ok");
    }

    #[tokio::test]
    async fn test_tick_without_components_returns_error() {
        let daemon = BackgroundDaemon::new();

        // Should fail without git_root, store, or config
        let result = daemon.tick().await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Git root not set"));
    }

    #[tokio::test]
    async fn test_check_if_due_without_last_run() {
        let temp_dir = TempDir::new().expect("tempdir should be created");
        let db_path = temp_dir.path().join("test.db");
        let store = velor_automations::AutomationStore::open(&db_path)
            .await
            .expect("store should open");

        let daemon = BackgroundDaemon::new();
        let last_runs = HashMap::new();

        // Create an automation that runs every second (for testing)
        let automation = Automation {
            name: "test-every-second".to_string(),
            description: "Test automation".to_string(),
            schedule: "0 * * * * *".to_string(), // Every minute actually
            timezone: "UTC".to_string(),
            prompt: "test".to_string(),
            enabled: true,
            vars: std::collections::BTreeMap::new(),
            catch_up: CatchUpPolicy::Skip,
            max_catch_up: 10,
            timeout_seconds: None,
            notify_on_success: false,
            notify_on_failure: false,
        };

        // Without a last run, it should check against "now" which is in the past
        let result = daemon.check_if_due(&automation, &last_runs, &store).await;

        // The result depends on current time vs schedule
        // We just verify it doesn't error
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_tick_with_no_automations_directory() {
        let temp_dir = TempDir::new().expect("tempdir should be created");
        let db_path = temp_dir.path().join("test.db");

        let daemon = BackgroundDaemon::new();
        daemon.set_git_root(temp_dir.path().to_path_buf()).await;

        let store = velor_automations::AutomationStore::open(&db_path)
            .await
            .expect("store should open");
        daemon.set_automation_store(store).await;

        let config = velor_core::FileConfig::default();
        daemon.set_config(config).await;

        // Should succeed even with no automations directory
        let result = daemon.tick().await;

        assert!(
            result.is_ok(),
            "Tick should succeed without automations directory"
        );
    }

    #[tokio::test]
    async fn test_tick_with_empty_automations() {
        let temp_dir = TempDir::new().expect("tempdir should be created");
        let db_path = temp_dir.path().join("test.db");
        let automations_dir = temp_dir.path().join(".velor/automations.d");

        // Create empty automations directory
        tokio::fs::create_dir_all(&automations_dir)
            .await
            .expect("directory should be created");

        let daemon = BackgroundDaemon::new();
        daemon.set_git_root(temp_dir.path().to_path_buf()).await;

        let store = velor_automations::AutomationStore::open(&db_path)
            .await
            .expect("store should open");
        daemon.set_automation_store(store).await;

        let config = velor_core::FileConfig::default();
        daemon.set_config(config).await;

        // Should succeed with no automations
        let result = daemon.tick().await;

        assert!(result.is_ok(), "Tick should succeed with empty automations");
    }

    #[tokio::test]
    async fn test_tick_with_disabled_automation() {
        let temp_dir = TempDir::new().expect("tempdir should be created");
        let db_path = temp_dir.path().join("test.db");
        let automations_dir = temp_dir.path().join(".velor/automations.d");

        tokio::fs::create_dir_all(&automations_dir)
            .await
            .expect("directory should be created");

        // Create a disabled automation
        let automation_path = automations_dir.join("disabled.toml");
        let content = r#"
name = "disabled"
description = "Disabled automation"
schedule = "0 * * * * *"
timezone = "UTC"
prompt = "test"
enabled = false
"#;
        tokio::fs::write(&automation_path, content)
            .await
            .expect("automation should be written");

        let daemon = BackgroundDaemon::new();
        daemon.set_git_root(temp_dir.path().to_path_buf()).await;

        let store = velor_automations::AutomationStore::open(&db_path)
            .await
            .expect("store should open");
        daemon.set_automation_store(store).await;

        let config = velor_core::FileConfig::default();
        daemon.set_config(config).await;

        // Should succeed without running disabled automation
        let result = daemon.tick().await;

        assert!(
            result.is_ok(),
            "Tick should succeed with disabled automation"
        );
    }

    #[tokio::test]
    async fn test_all_last_run_times() {
        let daemon = BackgroundDaemon::new();
        let _now = Utc::now();

        // Initially empty
        assert!(daemon.all_last_run_times().await.is_empty());

        // We can't directly set times without going through tick()
        // So we just verify the method works
        let times = daemon.all_last_run_times().await;
        assert!(times.is_empty());
    }

    #[tokio::test]
    async fn test_clear_all_last_run_times() {
        let daemon = BackgroundDaemon::new();

        // Clearing empty should not panic
        daemon.clear_all_last_run_times().await;

        // Still empty
        assert!(daemon.all_last_run_times().await.is_empty());
    }
}

#[cfg(test)]
mod proptest_tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn test_builder_preserves_values(
            binary in "[a-z]{1,20}",
            interval_secs in 1u64..3600,
            max_conc in 1u32..100,
        ) {
            let daemon = BackgroundDaemon::new()
                .with_velor_binary(binary.clone())
                .with_tick_interval(Duration::from_secs(interval_secs))
                .with_max_concurrent(max_conc);

            prop_assert_eq!(daemon.velor_binary, binary);
            prop_assert_eq!(daemon.tick_interval, Duration::from_secs(interval_secs));
            prop_assert_eq!(daemon.max_concurrent, max_conc);
        }

        #[test]
        fn test_tick_interval_reasonable(
            secs in 10u64..3600,
        ) {
            let daemon = BackgroundDaemon::new()
                .with_tick_interval(Duration::from_secs(secs));

            // Verify the interval is stored correctly
            prop_assert_eq!(daemon.tick_interval, Duration::from_secs(secs));
        }
    }
}
