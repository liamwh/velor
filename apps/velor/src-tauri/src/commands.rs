//! Tauri command handlers for the Velor GUI.
//!
//! This module provides all the commands that can be invoked from the frontend
//! via Tauri's IPC mechanism. Commands are organized by category:
//! - Config: Configuration management
//! - Execution: Agent execution control
//! - Automation: Scheduled task management
//! - Notification: Notification testing
//! - System: System utilities

use std::sync::Arc;
use tauri::State;
use tracing::{error, info, instrument, warn};

use velor_automations::{Automation, load_automations};
use velor_core::{
    ExecutionConfig, ExecutionEvent, ExecutionId, ExecutionMetrics, FileConfig, build_notifiers,
};

use crate::state::AppState;

/// Result type for commands that can fail.
pub type CommandResult<T> = Result<T, String>;

/// Configuration response with merged, home, and repo configs.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ConfigResponse {
    /// The merged (effective) configuration.
    pub merged: FileConfig,
    /// The home configuration (from ~/.velor/velor.toml).
    pub home: Option<FileConfig>,
    /// The repo configuration (from {git_root}/.velor/velor.toml).
    pub repo: Option<FileConfig>,
}

/// Execution status response.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ExecutionStatusResponse {
    /// The execution ID.
    pub id: String,
    /// The current state.
    pub state: String,
    /// The prompt name being executed.
    pub prompt_name: String,
    /// The number of iterations completed.
    pub iteration: u32,
    /// Whether this execution is active.
    pub is_active: bool,
    /// Whether this execution has been cancelled.
    pub is_cancelled: bool,
    /// All events from this execution.
    pub events: Vec<velor_core::ExecutionEvent>,
    /// The metrics.
    pub metrics: ExecutionMetrics,
}

/// Automation detail response.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AutomationDetail {
    /// The automation definition.
    pub automation: Automation,
    /// Whether the automation file exists on disk.
    pub exists: bool,
    /// Number of runs in the last 24 hours.
    pub recent_runs: u32,
    /// Last run status if any.
    pub last_run_status: Option<String>,
}

/// Helper to extract timestamp from an event.
fn event_timestamp(event: &ExecutionEvent) -> chrono::DateTime<chrono::Utc> {
    match event {
        ExecutionEvent::StateChanged { timestamp, .. }
        | ExecutionEvent::OutputChunk { timestamp, .. }
        | ExecutionEvent::Error { timestamp, .. }
        | ExecutionEvent::IterationCompleted { timestamp, .. }
        | ExecutionEvent::MetricsUpdated { timestamp, .. } => *timestamp,
    }
}

// ============================================================================
// Config Commands
// ============================================================================

/// Returns the merged configuration along with home and repo configs separately.
///
/// This allows the frontend to display the effective config while also showing
/// which values come from which source.
#[tauri::command]
#[instrument(skip(state), level = "debug")]
pub async fn get_config(state: State<'_, Arc<AppState>>) -> CommandResult<ConfigResponse> {
    let merged = state.merged_config().await;
    let home = state.home_config().await;
    let repo = state.repo_config().await;

    Ok(ConfigResponse { merged, home, repo })
}

/// Returns only the home configuration.
#[tauri::command]
#[instrument(skip(state), level = "debug")]
pub async fn get_home_config(state: State<'_, Arc<AppState>>) -> CommandResult<Option<FileConfig>> {
    Ok(state.home_config().await)
}

/// Returns only the repo configuration.
#[tauri::command]
#[instrument(skip(state), level = "debug")]
pub async fn get_repo_config(state: State<'_, Arc<AppState>>) -> CommandResult<Option<FileConfig>> {
    Ok(state.repo_config().await)
}

/// Saves configuration to the specified path.
///
/// # Arguments
///
/// * `config` - The configuration to save.
/// * `scope` - Either "home" for ~/.velor/velor.toml or "repo" for {git_root}/.velor/velor.toml.
#[tauri::command]
#[instrument(skip(state, config), level = "debug")]
pub async fn save_config(
    state: State<'_, Arc<AppState>>,
    config: FileConfig,
    scope: String,
) -> CommandResult<()> {
    let path = match scope.as_str() {
        "home" => FileConfig::home_config_path()
            .map_err(|e| format!("Failed to get home config path: {}", e))?,
        "repo" => {
            let git_root = state
                .git_root()
                .await
                .ok_or("No git root set, cannot save repo config")?;
            git_root.join(".velor").join("velor.toml")
        }
        _ => {
            return Err(format!(
                "Invalid scope: {}. Expected 'home' or 'repo'",
                scope
            ));
        }
    };

    state
        .save_config(&config, &path)
        .await
        .map_err(|e| format!("Failed to save config: {}", e))?;

    // Reload configs after saving
    state
        .load_configs(None, None)
        .await
        .map_err(|e| format!("Failed to reload configs: {}", e))?;

    info!(?scope, ?path, "Configuration saved and reloaded");
    Ok(())
}

// ============================================================================
// Execution Commands
// ============================================================================

/// Starts a new agent execution.
///
/// # Arguments
///
/// * `prompt_name` - The name of the prompt template to use.
/// * `vars` - Optional variables to override/add to the template.
/// * `max_iterations` - Optional maximum iterations override.
///
/// Returns the execution ID.
#[tauri::command]
#[instrument(skip(state), level = "debug")]
pub async fn start_execution(
    state: State<'_, Arc<AppState>>,
    prompt_name: String,
    vars: Option<std::collections::BTreeMap<String, String>>,
    max_iterations: Option<u32>,
) -> CommandResult<String> {
    let mut config = ExecutionConfig::new(prompt_name);

    if let Some(v) = vars {
        config.template_vars.extend(v);
    }

    if let Some(max_iter) = max_iterations {
        config.max_iterations = max_iter;
    }

    let id = state
        .start_execution(config)
        .await
        .map_err(|e| format!("Failed to start execution: {}", e))?;

    info!(id = %id, "Execution started");
    Ok(id.to_string())
}

/// Cancels a running execution.
///
/// # Arguments
///
/// * `id` - The execution ID to cancel.
///
/// Returns true if the execution was found and cancelled.
#[tauri::command]
#[instrument(skip(state), level = "debug")]
pub async fn cancel_execution(state: State<'_, Arc<AppState>>, id: String) -> CommandResult<bool> {
    let execution_id = ExecutionId::from_string(id.clone());
    let cancelled = state
        .cancel_execution(&execution_id)
        .await
        .map_err(|e| format!("Failed to cancel execution: {}", e))?;

    if cancelled {
        info!(id, "Execution cancelled");
    } else {
        warn!(id, "Execution not found for cancellation");
    }

    Ok(cancelled)
}

/// Returns the status of a specific execution.
///
/// # Arguments
///
/// * `id` - The execution ID to query.
#[tauri::command]
#[instrument(skip(state), level = "debug")]
pub async fn get_execution_status(
    state: State<'_, Arc<AppState>>,
    id: String,
) -> CommandResult<Option<ExecutionStatusResponse>> {
    let execution_id = ExecutionId::from_string(id.clone());

    // Try active executions first
    if let Some(active) = state.get_execution(&execution_id).await {
        return Ok(Some(ExecutionStatusResponse {
            id: active.record.id.to_string(),
            state: format!("{:?}", active.record.state),
            prompt_name: active.record.config.prompt_name.clone(),
            iteration: active.record.metrics.iteration,
            is_active: true,
            is_cancelled: active.is_cancelled(),
            events: active.record.events.clone(),
            metrics: active.record.metrics.clone(),
        }));
    }

    // Check history
    let history = state.execution_history().await;
    if let Some(record) = history.iter().find(|r| r.id.to_string() == id) {
        return Ok(Some(ExecutionStatusResponse {
            id: record.id.to_string(),
            state: format!("{:?}", record.state),
            prompt_name: record.config.prompt_name.clone(),
            iteration: record.metrics.iteration,
            is_active: false,
            is_cancelled: false,
            events: record.events.clone(),
            metrics: record.metrics.clone(),
        }));
    }

    Ok(None)
}

/// Returns the execution history (most recent first).
///
/// # Arguments
///
/// * `limit` - Maximum number of records to return.
#[tauri::command]
#[instrument(skip(state), level = "debug")]
pub async fn get_execution_history(
    state: State<'_, Arc<AppState>>,
    limit: Option<usize>,
) -> CommandResult<Vec<ExecutionStatusResponse>> {
    let history = state.execution_history().await;
    let active = state.active_executions().await;

    let mut all: Vec<ExecutionStatusResponse> = active
        .into_iter()
        .map(|a| ExecutionStatusResponse {
            id: a.record.id.to_string(),
            state: format!("{:?}", a.record.state),
            prompt_name: a.record.config.prompt_name.clone(),
            iteration: a.record.metrics.iteration,
            is_active: true,
            is_cancelled: a.is_cancelled(),
            events: a.record.events.clone(),
            metrics: a.record.metrics.clone(),
        })
        .collect();

    all.extend(history.into_iter().map(|r| ExecutionStatusResponse {
        id: r.id.to_string(),
        state: format!("{:?}", r.state),
        prompt_name: r.config.prompt_name.clone(),
        iteration: r.metrics.iteration,
        is_active: false,
        is_cancelled: false,
        events: r.events.clone(),
        metrics: r.metrics.clone(),
    }));

    // Sort by event timestamp (most recent first) and apply limit
    all.sort_by(|a, b| {
        let a_time = a
            .events
            .last()
            .map(|e| event_timestamp(e))
            .unwrap_or_default();
        let b_time = b
            .events
            .last()
            .map(|e| event_timestamp(e))
            .unwrap_or_default();
        b_time.cmp(&a_time)
    });

    if let Some(limit) = limit {
        all.truncate(limit);
    }

    Ok(all)
}

// ============================================================================
// Automation Commands
// ============================================================================

/// Lists all available automations from the automations directory.
#[tauri::command]
#[instrument(skip(state), level = "debug")]
pub async fn list_automations(state: State<'_, Arc<AppState>>) -> CommandResult<Vec<Automation>> {
    let git_root = state.git_root().await.ok_or("No git root set")?;

    let merged = state.merged_config().await;
    let automations_dir = git_root.join(&merged.automations.automations_dir);

    let automations = load_automations(&automations_dir)
        .await
        .map_err(|e| format!("Failed to load automations: {}", e))?;

    Ok(automations)
}

/// Returns details about a specific automation.
///
/// # Arguments
///
/// * `name` - The name of the automation.
#[tauri::command]
#[instrument(skip(state), level = "debug")]
pub async fn get_automation(
    state: State<'_, Arc<AppState>>,
    name: String,
) -> CommandResult<Option<AutomationDetail>> {
    let git_root = state.git_root().await.ok_or("No git root set")?;

    let merged = state.merged_config().await;
    let automations_dir = git_root.join(&merged.automations.automations_dir);

    // Load all automations
    let automations = load_automations(&automations_dir)
        .await
        .map_err(|e| format!("Failed to load automations: {}", e))?;

    let automation = automations.into_iter().find(|a| a.name == name);

    let automation = match automation {
        Some(a) => a,
        None => return Ok(None),
    };

    // Check if file exists
    let file_path = automations_dir.join(format!("{}.toml", name));
    let exists = file_path.exists();

    // Get recent runs from store
    let store = state.automation_store().await;
    let (recent_runs, last_run_status) = if let Some(store) = store {
        match store.get_runs(Some(&name), 100).await {
            Ok(runs) => {
                let recent = runs
                    .iter()
                    .filter(|r| r.started_at + chrono::Duration::hours(24) > chrono::Utc::now())
                    .count() as u32;

                let last_status = runs.first().map(|r| r.status.as_str().to_string());
                (recent, last_status)
            }
            Err(_) => (0, None),
        }
    } else {
        (0, None)
    };

    Ok(Some(AutomationDetail {
        automation,
        exists,
        recent_runs,
        last_run_status,
    }))
}

/// Toggles an automation's enabled state.
///
/// # Arguments
///
/// * `name` - The name of the automation.
/// * `enabled` - Whether to enable or disable the automation.
#[tauri::command]
#[instrument(skip(state), level = "debug")]
pub async fn toggle_automation(
    state: State<'_, Arc<AppState>>,
    name: String,
    enabled: bool,
) -> CommandResult<()> {
    let git_root = state.git_root().await.ok_or("No git root set")?;

    let merged = state.merged_config().await;
    let automations_dir = git_root.join(&merged.automations.automations_dir);
    let file_path = automations_dir.join(format!("{}.toml", name));

    // Read existing file
    let content = tokio::fs::read_to_string(&file_path)
        .await
        .map_err(|e| format!("Failed to read automation file: {}", e))?;

    // Parse, update, and serialize
    let mut automation: Automation =
        toml::from_str(&content).map_err(|e| format!("Failed to parse automation file: {}", e))?;

    automation.enabled = enabled;

    let toml_str = toml::to_string_pretty(&automation)
        .map_err(|e| format!("Failed to serialize automation: {}", e))?;

    // Write back
    tokio::fs::write(&file_path, toml_str)
        .await
        .map_err(|e| format!("Failed to write automation file: {}", e))?;

    info!(name, enabled, "Automation toggled");
    Ok(())
}

/// Runs an automation immediately, ignoring its schedule.
///
/// # Arguments
///
/// * `name` - The name of the automation to run.
///
/// Note: This is a placeholder. The actual execution logic will be
/// implemented when the daemon and runner are integrated.
#[tauri::command]
#[instrument(skip(state), level = "debug")]
pub async fn run_automation_now(
    state: State<'_, Arc<AppState>>,
    name: String,
) -> CommandResult<String> {
    // Load the automation
    let git_root = state.git_root().await.ok_or("No git root set")?;

    let merged = state.merged_config().await;
    let automations_dir = git_root.join(&merged.automations.automations_dir);

    let automations = load_automations(&automations_dir)
        .await
        .map_err(|e| format!("Failed to load automations: {}", e))?;

    let automation = automations
        .into_iter()
        .find(|a| a.name == name)
        .ok_or(format!("Automation '{}' not found", name))?;

    if !automation.enabled {
        return Err(format!("Automation '{}' is disabled", name));
    }

    // For now, create a standard execution
    // TODO: Integrate with the runner when daemon is implemented
    let mut config = ExecutionConfig::new(automation.prompt.clone());
    config.template_vars.extend(automation.vars.clone());

    let id = state
        .start_execution(config)
        .await
        .map_err(|e| format!("Failed to start execution: {}", e))?;

    info!(name, id = %id, "Automation run started");
    Ok(id.to_string())
}

/// Returns the run history for an automation.
///
/// # Arguments
///
/// * `name` - The name of the automation.
/// * `limit` - Maximum number of runs to return.
#[tauri::command]
#[instrument(skip(state), level = "debug")]
pub async fn get_automation_runs(
    state: State<'_, Arc<AppState>>,
    name: String,
    limit: Option<u32>,
) -> CommandResult<Vec<velor_automations::AutomationRun>> {
    let store = state
        .automation_store()
        .await
        .ok_or("Automation store not initialized")?;

    let limit = limit.unwrap_or(50);
    let runs = store
        .get_runs(Some(&name), limit)
        .await
        .map_err(|e| format!("Failed to get automation runs: {}", e))?;

    Ok(runs)
}

/// Starts the background daemon for scheduled automations.
///
/// The daemon will run in a background task, periodically checking for
/// automations that are due to run based on their cron schedules.
///
/// # Errors
///
/// Returns an error if:
/// - The daemon is already running
/// - Git root is not configured
/// - Automation store is not initialized
#[tauri::command]
#[instrument(skip(state), level = "debug")]
pub async fn start_daemon(state: State<'_, Arc<AppState>>) -> CommandResult<()> {
    // Check if daemon is already running
    if state.is_daemon_running().await {
        return Err("Daemon is already running".to_string());
    }

    // Check required components
    let git_root = state
        .git_root()
        .await
        .ok_or("Git root not configured. Cannot start daemon.")?;

    let automation_store = state
        .automation_store()
        .await
        .ok_or("Automation store not initialized. Cannot start daemon.")?;

    let config = state.merged_config().await;

    // Configure the daemon
    let daemon = state.daemon();
    daemon.set_git_root(git_root).await;
    daemon.set_automation_store(automation_store).await;
    daemon.set_config(config).await;

    // Create cancel token
    let cancel_token = tokio_util::sync::CancellationToken::new();
    state.set_daemon_cancel_token(Some(cancel_token.clone())).await;

    // Clone the inner Arc for the spawned task
    let state_inner = Arc::clone(&state);
    let daemon_clone = Arc::clone(&daemon);

    // Spawn daemon task in background
    tokio::spawn(async move {
        info!("Background daemon task started");

        let result = daemon_clone.run(cancel_token.clone()).await;

        // Mark daemon as not running
        state_inner.set_daemon_running(false).await;
        state_inner.set_daemon_cancel_token(None).await;

        match result {
            Ok(_) => {
                info!("Background daemon stopped gracefully");
            }
            Err(e) => {
                error!("Background daemon stopped with error: {}", e);
            }
        }
    });

    // Mark daemon as running
    state.set_daemon_running(true).await;

    info!("Daemon started successfully");
    Ok(())
}

/// Stops the background daemon.
///
/// This will signal the daemon to stop after the current tick completes.
/// The daemon may take up to one tick interval to fully stop.
///
/// # Errors
///
/// Returns an error if the daemon is not running.
#[tauri::command]
#[instrument(skip(state), level = "debug")]
pub async fn stop_daemon(state: State<'_, Arc<AppState>>) -> CommandResult<()> {
    // Check if daemon is running
    if !state.is_daemon_running().await {
        return Err("Daemon is not running".to_string());
    }

    // Cancel the daemon
    if let Some(cancel_token) = state.daemon_cancel_token().await {
        cancel_token.cancel();
        info!("Daemon cancel signal sent");
    } else {
        return Err("Daemon cancel token not found".to_string());
    }

    // Note: We don't immediately set daemon_running to false
    // The daemon task will set it when it actually stops
    info!("Daemon stop requested");
    Ok(())
}

// ============================================================================
// Notification Commands
// ============================================================================

/// Sends a test notification to verify the notification configuration.
#[tauri::command]
#[instrument(skip(state), level = "debug")]
pub async fn test_notification(state: State<'_, Arc<AppState>>) -> CommandResult<()> {
    let config = state.merged_config().await;

    if !config.notifications.enabled {
        return Err("Notifications are disabled in configuration".to_string());
    }

    let notifiers = build_notifiers(&config.notifications)
        .map_err(|e| format!("Failed to build notifiers: {}", e))?;

    let payload = velor_core::NotificationPayload {
        mode: "test",
        iterations_completed: 1,
        max_iterations: 10,
        duration: std::time::Duration::from_secs(1),
        status: velor_core::RunStatus::Completed,
        output_preview: Some("This is a test notification from Velor GUI.".to_string()),
        prompt_name: "test-notification".to_string(),
    };

    for notifier in &notifiers {
        if let Err(e) = notifier.notify(&payload) {
            error!("Failed to send test notification: {}", e);
        }
    }

    info!("Test notification sent");
    Ok(())
}

// ============================================================================
// System Commands
// ============================================================================

/// Discovers and returns the git root directory for the current path.
///
/// If called from the frontend, this will use the current working directory.
#[tauri::command]
#[instrument(level = "debug")]
pub async fn discover_git_root() -> CommandResult<Option<String>> {
    let cwd =
        std::env::current_dir().map_err(|e| format!("Failed to get current directory: {}", e))?;

    match velor_core::git::discover_git_root(&cwd) {
        Ok(path) => Ok(Some(path.to_string_lossy().to_string())),
        Err(_) => Ok(None),
    }
}

/// Checks if a binary is available on the system PATH.
///
/// # Arguments
///
/// * `binary` - The name of the binary to check.
#[tauri::command]
#[instrument(level = "debug")]
pub async fn check_binary_available(binary: String) -> CommandResult<bool> {
    Ok(which::which(&binary).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_response_serialization() {
        let response = ConfigResponse {
            merged: FileConfig::default(),
            home: None,
            repo: None,
        };

        let json = serde_json::to_string(&response);
        assert!(json.is_ok());
    }

    #[test]
    fn test_execution_status_response_serialization() {
        let response = ExecutionStatusResponse {
            id: "test-id".to_string(),
            state: "Running".to_string(),
            prompt_name: "test-prompt".to_string(),
            iteration: 1,
            is_active: true,
            is_cancelled: false,
            events: vec![],
            metrics: ExecutionMetrics::default(),
        };

        let json = serde_json::to_string(&response);
        assert!(json.is_ok());
    }

    #[test]
    fn test_automation_detail_serialization() {
        let detail = AutomationDetail {
            automation: Automation {
                name: "test".to_string(),
                description: "Test automation".to_string(),
                schedule: "0 0 * * * *".to_string(),
                timezone: "UTC".to_string(),
                prompt: "test-prompt".to_string(),
                enabled: true,
                vars: std::collections::BTreeMap::new(),
                catch_up: velor_automations::CatchUpPolicy::Skip,
                max_catch_up: 0,
                timeout_seconds: None,
                notify_on_success: true,
                notify_on_failure: true,
            },
            exists: true,
            recent_runs: 5,
            last_run_status: Some("Completed".to_string()),
        };

        let json = serde_json::to_string(&detail);
        assert!(json.is_ok());
    }
}
