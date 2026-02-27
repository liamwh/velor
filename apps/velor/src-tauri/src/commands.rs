//! Tauri command handlers for the Velor GUI.
//!
//! This module provides all the commands that can be invoked from the frontend
//! via Tauri's IPC mechanism. Commands are organized by category:
//! - Config: Configuration management
//! - Execution: Agent execution control
//! - Automation: Scheduled task management
//! - Notification: Notification testing
//! - System: System utilities

use std::collections::BTreeMap;
use std::sync::Arc;
use tauri::State;
use tracing::{error, info, instrument, warn};

use velor_automations::{Automation, CatchUpPolicy, load_automations};
use velor_core::{
    ExecutionConfig, ExecutionEvent, ExecutionId, ExecutionMetrics, ExecutionRecord, FileConfig,
    build_notifiers,
};

use crate::state::AppState;
use crate::unified_store::SessionStats;

/// Result type for commands that can fail.
pub type CommandResult<T> = Result<T, String>;

/// Configuration response with merged, home, and repo configs.
///
/// Both the parsed config objects and pre-serialized TOML strings are provided.
/// The TOML strings should be used for display purposes to ensure proper
/// serialization of nested structures.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ConfigResponse {
    /// The merged (effective) configuration.
    pub merged: FileConfig,
    /// Pre-serialized TOML for the merged config.
    pub merged_toml: String,
    /// The home configuration (from ~/.velor/velor.toml).
    pub home: Option<FileConfig>,
    /// Pre-serialized TOML for the home config.
    pub home_toml: Option<String>,
    /// The repo configuration (from {git_root}/.velor/velor.toml).
    pub repo: Option<FileConfig>,
    /// Pre-serialized TOML for the repo config.
    pub repo_toml: Option<String>,
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

/// Request to create a new automation.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct CreateAutomationRequest {
    /// Unique name of the automation.
    pub name: String,
    /// Human-readable description.
    pub description: Option<String>,
    /// Cron schedule expression (6-field).
    pub schedule: String,
    /// Timezone for the schedule.
    pub timezone: Option<String>,
    /// Prompt template name.
    pub prompt: String,
    /// Whether this automation is enabled.
    pub enabled: Option<bool>,
    /// Variables to pass to the prompt.
    pub vars: Option<BTreeMap<String, String>>,
    /// Policy for handling missed runs.
    pub catch_up: Option<CatchUpPolicy>,
    /// Maximum number of catch-up runs.
    pub max_catch_up: Option<u32>,
    /// Timeout in seconds.
    pub timeout_seconds: Option<u64>,
    /// Send notification on success.
    pub notify_on_success: Option<bool>,
    /// Send notification on failure.
    pub notify_on_failure: Option<bool>,
}

/// Request to update an existing automation.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct UpdateAutomationRequest {
    /// Current automation name (used as identifier).
    pub current_name: String,
    /// New unique name of the automation.
    pub name: Option<String>,
    /// Human-readable description.
    pub description: Option<String>,
    /// Cron schedule expression (6-field).
    pub schedule: Option<String>,
    /// Timezone for the schedule.
    pub timezone: Option<String>,
    /// Prompt template name.
    pub prompt: Option<String>,
    /// Whether this automation is enabled.
    pub enabled: Option<bool>,
    /// Variables to pass to the prompt.
    pub vars: Option<BTreeMap<String, String>>,
    /// Policy for handling missed runs.
    pub catch_up: Option<CatchUpPolicy>,
    /// Maximum number of catch-up runs.
    pub max_catch_up: Option<u32>,
    /// Timeout in seconds.
    pub timeout_seconds: Option<u64>,
    /// Send notification on success.
    pub notify_on_success: Option<bool>,
    /// Send notification on failure.
    pub notify_on_failure: Option<bool>,
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
/// which values come from which source. TOML strings are pre-serialized by the
/// backend to ensure proper handling of nested structures.
#[tauri::command]
#[instrument(skip(state), level = "debug")]
pub async fn get_config(state: State<'_, Arc<AppState>>) -> CommandResult<ConfigResponse> {
    let merged = state.merged_config().await;
    let home = state.home_config().await;
    let repo = state.repo_config().await;

    // Serialize configs to TOML strings for display
    let merged_toml = toml::to_string_pretty(&merged)
        .map_err(|e| format!("Failed to serialize merged config to TOML: {e}"))?;
    let home_toml = home
        .as_ref()
        .map(toml::to_string_pretty)
        .transpose()
        .map_err(|e| format!("Failed to serialize home config to TOML: {e}"))?;
    let repo_toml = repo
        .as_ref()
        .map(toml::to_string_pretty)
        .transpose()
        .map_err(|e| format!("Failed to serialize repo config to TOML: {e}"))?;

    Ok(ConfigResponse {
        merged,
        merged_toml,
        home,
        home_toml,
        repo,
        repo_toml,
    })
}

/// Returns only the home configuration as a TOML string.
#[tauri::command]
#[instrument(skip(state), level = "debug")]
pub async fn get_home_config(state: State<'_, Arc<AppState>>) -> CommandResult<Option<String>> {
    let config = state.home_config().await;
    match config {
        Some(c) => {
            let toml = toml::to_string_pretty(&c)
                .map_err(|e| format!("Failed to serialize home config to TOML: {e}"))?;
            Ok(Some(toml))
        }
        None => Ok(None),
    }
}

/// Returns only the repo configuration as a TOML string.
#[tauri::command]
#[instrument(skip(state), level = "debug")]
pub async fn get_repo_config(state: State<'_, Arc<AppState>>) -> CommandResult<Option<String>> {
    let config = state.repo_config().await;
    match config {
        Some(c) => {
            let toml = toml::to_string_pretty(&c)
                .map_err(|e| format!("Failed to serialize repo config to TOML: {e}"))?;
            Ok(Some(toml))
        }
        None => Ok(None),
    }
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
        let a_time = a.events.last().map(event_timestamp).unwrap_or_default();
        let b_time = b.events.last().map(event_timestamp).unwrap_or_default();
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
    let store = state.store().await;
    let (recent_runs, last_run_status) = if let Some(store) = store {
        match store.get_automation_runs(Some(&name), 100).await {
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
) -> CommandResult<Vec<crate::unified_store::AutomationRun>> {
    let store = state.store().await.ok_or("Store not initialized")?;

    let limit = limit.unwrap_or(50);
    let runs = store
        .get_automation_runs(Some(&name), limit)
        .await
        .map_err(|e| format!("Failed to get automation runs: {}", e))?;

    Ok(runs)
}

/// Creates a new automation.
///
/// # Arguments
///
/// * `request` - The automation creation request.
#[tauri::command]
#[instrument(skip(state, request), level = "debug")]
pub async fn create_automation(
    state: State<'_, Arc<AppState>>,
    request: CreateAutomationRequest,
) -> CommandResult<()> {
    let git_root = state.git_root().await.ok_or("No git root set")?;

    let merged = state.merged_config().await;
    let automations_dir = git_root.join(&merged.automations.automations_dir);

    // Ensure automations directory exists
    tokio::fs::create_dir_all(&automations_dir)
        .await
        .map_err(|e| format!("Failed to create automations directory: {}", e))?;

    let file_path = automations_dir.join(format!("{}.toml", request.name));

    // Check if automation already exists
    if file_path.exists() {
        return Err(format!("Automation '{}' already exists", request.name));
    }

    // Build automation from request
    let automation = Automation {
        name: request.name.clone(),
        description: request.description.unwrap_or_default(),
        schedule: request.schedule,
        timezone: request
            .timezone
            .unwrap_or_else(|| merged.automations.default_timezone.clone()),
        prompt: request.prompt,
        enabled: request.enabled.unwrap_or(true),
        vars: request.vars.unwrap_or_default(),
        catch_up: request.catch_up.unwrap_or_default(),
        max_catch_up: request.max_catch_up.unwrap_or(10),
        timeout_seconds: request.timeout_seconds,
        notify_on_success: request.notify_on_success.unwrap_or(true),
        notify_on_failure: request.notify_on_failure.unwrap_or(true),
    };

    // Serialize and write
    let toml_str = toml::to_string_pretty(&automation)
        .map_err(|e| format!("Failed to serialize automation: {}", e))?;

    tokio::fs::write(&file_path, toml_str)
        .await
        .map_err(|e| format!("Failed to write automation file: {}", e))?;

    info!(name = %request.name, "Automation created");
    Ok(())
}

/// Updates an existing automation.
///
/// # Arguments
///
/// * `request` - The automation update request.
#[tauri::command]
#[instrument(skip(state, request), level = "debug")]
pub async fn update_automation(
    state: State<'_, Arc<AppState>>,
    request: UpdateAutomationRequest,
) -> CommandResult<()> {
    let git_root = state.git_root().await.ok_or("No git root set")?;

    let merged = state.merged_config().await;
    let automations_dir = git_root.join(&merged.automations.automations_dir);

    let file_path = automations_dir.join(format!("{}.toml", request.current_name));

    // Read existing file
    let content = tokio::fs::read_to_string(&file_path)
        .await
        .map_err(|e| format!("Failed to read automation file: {}", e))?;

    // Parse existing automation
    let mut automation: Automation =
        toml::from_str(&content).map_err(|e| format!("Failed to parse automation file: {}", e))?;

    // Apply updates
    if let Some(name) = request.name {
        automation.name = name;
    }
    if let Some(description) = request.description {
        automation.description = description;
    }
    if let Some(schedule) = request.schedule {
        automation.schedule = schedule;
    }
    if let Some(timezone) = request.timezone {
        automation.timezone = timezone;
    }
    if let Some(prompt) = request.prompt {
        automation.prompt = prompt;
    }
    if let Some(enabled) = request.enabled {
        automation.enabled = enabled;
    }
    if let Some(vars) = request.vars {
        automation.vars = vars;
    }
    if let Some(catch_up) = request.catch_up {
        automation.catch_up = catch_up;
    }
    if let Some(max_catch_up) = request.max_catch_up {
        automation.max_catch_up = max_catch_up;
    }
    if let Some(timeout_seconds) = request.timeout_seconds {
        automation.timeout_seconds = Some(timeout_seconds);
    }
    if let Some(notify_on_success) = request.notify_on_success {
        automation.notify_on_success = notify_on_success;
    }
    if let Some(notify_on_failure) = request.notify_on_failure {
        automation.notify_on_failure = notify_on_failure;
    }

    // If name changed, rename the file
    let new_file_path = if automation.name != request.current_name {
        automations_dir.join(format!("{}.toml", automation.name))
    } else {
        file_path.clone()
    };

    // Serialize and write
    let toml_str = toml::to_string_pretty(&automation)
        .map_err(|e| format!("Failed to serialize automation: {}", e))?;

    // Remove old file if renamed
    if new_file_path != file_path {
        tokio::fs::remove_file(&file_path)
            .await
            .map_err(|e| format!("Failed to remove old automation file: {}", e))?;
    }

    tokio::fs::write(&new_file_path, toml_str)
        .await
        .map_err(|e| format!("Failed to write automation file: {}", e))?;

    info!(
        old_name = %request.current_name,
        new_name = %automation.name,
        "Automation updated"
    );
    Ok(())
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
/// - Store is not initialized
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

    // Create AutomationStore from the unified database for the daemon runner
    let velor_dir = git_root.join(".velor");
    let db_path = velor_dir.join("velor.db");
    let automation_store = velor_automations::AutomationStore::open(&db_path)
        .await
        .map_err(|e| format!("Failed to open automation store: {}", e))?;

    let config = state.merged_config().await;

    // Configure the daemon
    let daemon = state.daemon();
    daemon.set_git_root(git_root).await;
    daemon.set_automation_store(automation_store).await;
    daemon.set_config(config).await;

    // Create cancel token
    let cancel_token = tokio_util::sync::CancellationToken::new();
    state
        .set_daemon_cancel_token(Some(cancel_token.clone()))
        .await;

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

// ============================================================================
// Session Commands
// ============================================================================

/// Lists execution sessions from the persistent store.
///
/// # Arguments
///
/// * `limit` - Maximum number of sessions to return (default: 50).
/// * `offset` - Number of sessions to skip for pagination (default: 0).
///
/// Returns sessions ordered by start time, most recent first.
#[tauri::command]
#[instrument(skip(state), level = "debug")]
pub async fn list_sessions(
    state: State<'_, Arc<AppState>>,
    limit: Option<u32>,
    offset: Option<u32>,
) -> CommandResult<Vec<ExecutionRecord>> {
    let store = state.store().await.ok_or("Store not initialized")?;

    let limit = limit.unwrap_or(50);
    let offset = offset.unwrap_or(0);

    let sessions = store
        .list_sessions(limit, offset)
        .await
        .map_err(|e| format!("Failed to list sessions: {}", e))?;

    info!(limit, offset, count = sessions.len(), "Sessions listed");
    Ok(sessions)
}

/// Gets a specific execution session by ID.
///
/// # Arguments
///
/// * `id` - The session ID to retrieve.
///
/// Returns the session record if found, or null if not found.
#[tauri::command]
#[instrument(skip(state), level = "debug")]
pub async fn get_session(
    state: State<'_, Arc<AppState>>,
    id: String,
) -> CommandResult<Option<ExecutionRecord>> {
    let store = state.store().await.ok_or("Store not initialized")?;

    let session = store
        .get_session(&id)
        .await
        .map_err(|e| format!("Failed to get session: {}", e))?;

    Ok(session)
}

/// Deletes an execution session from the persistent store.
///
/// This operation is idempotent - deleting a non-existent session
/// will succeed without error.
///
/// # Arguments
///
/// * `id` - The session ID to delete.
#[tauri::command]
#[instrument(skip(state), level = "debug")]
pub async fn delete_session(state: State<'_, Arc<AppState>>, id: String) -> CommandResult<()> {
    let store = state.store().await.ok_or("Store not initialized")?;

    store
        .delete_session(&id)
        .await
        .map_err(|e| format!("Failed to delete session: {}", e))?;

    info!(id, "Session deleted");
    Ok(())
}

/// Gets statistics about execution sessions.
///
/// Returns counts of total, completed, failed, cancelled, and active sessions.
#[tauri::command]
#[instrument(skip(state), level = "debug")]
pub async fn get_session_stats(state: State<'_, Arc<AppState>>) -> CommandResult<SessionStats> {
    let store = state.store().await.ok_or("Store not initialized")?;

    let stats = store
        .get_session_stats()
        .await
        .map_err(|e| format!("Failed to get session stats: {}", e))?;

    Ok(stats)
}

/// Renames a session.
///
/// # Arguments
///
/// * `id` - The session ID to rename.
/// * `name` - The new name for the session (null to clear and use default).
#[tauri::command]
#[instrument(skip(state), level = "debug")]
pub async fn rename_session(
    state: State<'_, Arc<AppState>>,
    id: String,
    name: Option<String>,
) -> CommandResult<()> {
    let store = state.store().await.ok_or("Store not initialized")?;

    store
        .rename_session(&id, name)
        .await
        .map_err(|e| format!("Failed to rename session: {}", e))?;

    info!(id, "Session renamed");
    Ok(())
}

/// Toggles the pinned status of a session.
///
/// # Arguments
///
/// * `id` - The session ID to toggle pin status for.
///
/// Returns the new pinned state (true if pinned, false if unpinned).
#[tauri::command]
#[instrument(skip(state), level = "debug")]
pub async fn toggle_session_pin(
    state: State<'_, Arc<AppState>>,
    id: String,
) -> CommandResult<bool> {
    let store = state.store().await.ok_or("Store not initialized")?;

    let pinned = store
        .toggle_session_pin(&id)
        .await
        .map_err(|e| format!("Failed to toggle session pin: {}", e))?;

    info!(id, pinned, "Session pin toggled");
    Ok(pinned)
}

// ============================================================================
// Project Commands
// ============================================================================

/// Lists all projects with their session counts and metadata.
#[tauri::command]
#[instrument(skip(state), level = "debug")]
pub async fn list_projects(
    state: State<'_, Arc<AppState>>,
) -> CommandResult<Vec<crate::unified_store::Project>> {
    let store = state.store().await.ok_or("Store not initialized")?;

    let projects = store
        .list_projects()
        .await
        .map_err(|e| format!("Failed to list projects: {}", e))?;

    Ok(projects)
}

/// Hides a project from the sidebar.
///
/// # Arguments
///
/// * `path` - The project path to hide.
#[tauri::command]
#[instrument(skip(state), level = "debug")]
pub async fn hide_project(state: State<'_, Arc<AppState>>, path: String) -> CommandResult<()> {
    let store = state.store().await.ok_or("Store not initialized")?;

    store
        .hide_project(&path)
        .await
        .map_err(|e| format!("Failed to hide project: {}", e))?;

    info!(path, "Project hidden");
    Ok(())
}

/// Shows a hidden project in the sidebar.
///
/// # Arguments
///
/// * `path` - The project path to show.
#[tauri::command]
#[instrument(skip(state), level = "debug")]
pub async fn show_project(state: State<'_, Arc<AppState>>, path: String) -> CommandResult<()> {
    let store = state.store().await.ok_or("Store not initialized")?;

    store
        .show_project(&path)
        .await
        .map_err(|e| format!("Failed to show project: {}", e))?;

    info!(path, "Project shown");
    Ok(())
}

/// Renames a project.
///
/// # Arguments
///
/// * `path` - The project path to rename.
/// * `display_name` - The new display name.
#[tauri::command]
#[instrument(skip(state), level = "debug")]
pub async fn rename_project(
    state: State<'_, Arc<AppState>>,
    path: String,
    display_name: String,
) -> CommandResult<()> {
    let store = state.store().await.ok_or("Store not initialized")?;

    store
        .rename_project(&path, display_name.clone())
        .await
        .map_err(|e| format!("Failed to rename project: {}", e))?;

    info!(path, display_name, "Project renamed");
    Ok(())
}

/// Reorders projects by updating their sort order.
///
/// # Arguments
///
/// * `paths` - Ordered list of project paths (first appears at top).
#[tauri::command]
#[instrument(skip(state), level = "debug")]
pub async fn reorder_projects(
    state: State<'_, Arc<AppState>>,
    paths: Vec<String>,
) -> CommandResult<()> {
    let store = state.store().await.ok_or("Store not initialized")?;

    store
        .reorder_projects(paths)
        .await
        .map_err(|e| format!("Failed to reorder projects: {}", e))?;

    info!("Projects reordered");
    Ok(())
}

// ============================================================================
// Automation Management Commands
// ============================================================================

/// Deletes an automation by name.
///
/// This operation removes the automation file from disk. It is idempotent -
/// attempting to delete a non-existent automation will succeed without error.
///
/// # Arguments
///
/// * `name` - The name of the automation to delete.
#[tauri::command]
#[instrument(skip(state), level = "debug")]
pub async fn delete_automation(state: State<'_, Arc<AppState>>, name: String) -> CommandResult<()> {
    let git_root = state.git_root().await.ok_or("No git root set")?;

    let merged = state.merged_config().await;
    let automations_dir = git_root.join(&merged.automations.automations_dir);
    let file_path = automations_dir.join(format!("{}.toml", name));

    // Delete the file if it exists (idempotent)
    if file_path.exists() {
        tokio::fs::remove_file(&file_path)
            .await
            .map_err(|e| format!("Failed to delete automation file: {}", e))?;
        info!(name, "Automation deleted");
    } else {
        info!(name, "Automation file not found, nothing to delete");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_response_serialization() {
        let merged = FileConfig::default();
        let merged_toml = toml::to_string_pretty(&merged).expect("default config should serialize");
        let response = ConfigResponse {
            merged,
            merged_toml,
            home: None,
            home_toml: None,
            repo: None,
            repo_toml: None,
        };

        let json = serde_json::to_string(&response);
        assert!(json.is_ok(), "ConfigResponse should serialize to JSON");
    }

    /// Tests that TOML serialization handles nested structures correctly.
    ///
    /// This test verifies the fix for the "[object Object]" bug where the
    /// frontend's custom tomlStringify function couldn't handle deeply nested
    /// objects like the `prompts` section.
    #[test]
    fn test_toml_serialization_handles_nested_structures() {
        use velor_core::PromptDef;

        // Create a config with nested structures
        let mut prompts = BTreeMap::new();
        prompts.insert(
            "test-prompt".to_string(),
            PromptDef::Table {
                template: "Hello {{name}}".to_string(),
                complete_token: Some("<DONE>".to_string()),
            },
        );
        prompts.insert(
            "another-prompt".to_string(),
            PromptDef::Inline("Goodbye {{name}}".to_string()),
        );

        let config = FileConfig {
            vars: {
                let mut vars = BTreeMap::new();
                vars.insert("name".to_string(), "world".to_string());
                vars
            },
            prompts,
            ..FileConfig::default()
        };

        // Serialize to TOML
        let toml_string = toml::to_string_pretty(&config);
        assert!(toml_string.is_ok(), "Config should serialize to TOML");

        let toml = toml_string.unwrap();

        // Verify nested structures are properly serialized
        assert!(
            toml.contains("[prompts.test-prompt]"),
            "TOML should contain nested prompt section"
        );
        assert!(
            toml.contains("template = \"Hello {{name}}\""),
            "TOML should contain prompt template"
        );
        assert!(
            toml.contains("complete_token = \"<DONE>\""),
            "TOML should contain complete_token"
        );
        assert!(
            !toml.contains("[object Object]"),
            "TOML should not contain '[object Object]'"
        );

        // Verify we can deserialize back
        let deserialized: Result<FileConfig, _> = toml::from_str(&toml);
        assert!(
            deserialized.is_ok(),
            "TOML should deserialize back to FileConfig"
        );
        let deserialized = deserialized.unwrap();
        assert_eq!(
            deserialized.prompts.len(),
            2,
            "Deserialized config should have 2 prompts"
        );
        assert_eq!(
            deserialized.prompts["test-prompt"].template(),
            "Hello {{name}}",
            "Deserialized prompt template should match"
        );
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
        assert!(
            json.is_ok(),
            "ExecutionStatusResponse should serialize to JSON"
        );
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
        assert!(json.is_ok(), "AutomationDetail should serialize to JSON");
    }

    #[test]
    fn test_session_stats_serialization() {
        let stats = SessionStats {
            total: 100,
            completed: 75,
            failed: 15,
            cancelled: 5,
            active: 5,
        };

        let json = serde_json::to_string(&stats);
        assert!(json.is_ok(), "SessionStats should serialize to JSON");

        let json_str = json.unwrap();
        assert!(
            json_str.contains("\"total\":100"),
            "JSON should contain total count"
        );
        assert!(
            json_str.contains("\"completed\":75"),
            "JSON should contain completed count"
        );
    }

    #[test]
    fn test_create_automation_request_deserialization() {
        let json = r#"{
            "name": "test-automation",
            "description": "Test description",
            "schedule": "0 0 * * * *",
            "prompt": "test-prompt",
            "enabled": true
        }"#;

        let request: CreateAutomationRequest = serde_json::from_str(json)
            .expect("CreateAutomationRequest should deserialize from JSON");

        assert_eq!(request.name, "test-automation");
        assert_eq!(request.description, Some("Test description".to_string()));
        assert_eq!(request.schedule, "0 0 * * * *");
        assert_eq!(request.prompt, "test-prompt");
        assert_eq!(request.enabled, Some(true));
    }

    #[test]
    fn test_update_automation_request_deserialization() {
        let json = r#"{
            "current_name": "old-name",
            "name": "new-name",
            "enabled": false
        }"#;

        let request: UpdateAutomationRequest = serde_json::from_str(json)
            .expect("UpdateAutomationRequest should deserialize from JSON");

        assert_eq!(request.current_name, "old-name");
        assert_eq!(request.name, Some("new-name".to_string()));
        assert_eq!(request.enabled, Some(false));
    }

    #[test]
    fn test_session_stats_default() {
        let stats = SessionStats::default();

        assert_eq!(stats.total, 0, "Default total should be 0");
        assert_eq!(stats.completed, 0, "Default completed should be 0");
        assert_eq!(stats.failed, 0, "Default failed should be 0");
        assert_eq!(stats.cancelled, 0, "Default cancelled should be 0");
        assert_eq!(stats.active, 0, "Default active should be 0");
    }
}
