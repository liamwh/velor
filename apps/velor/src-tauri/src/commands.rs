//! Tauri command handlers for the Velor GUI.
//!
//! This module provides all the commands that can be invoked from the frontend
//! via Tauri's IPC mechanism. Commands are organized by category:
//! - Config: Configuration management
//! - Execution: Agent execution control
//! - Automation: Scheduled task management
//! - Notification: Notification testing
//! - System: System utilities

use color_eyre::eyre::WrapErr;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use tauri::{Emitter, State};
use tracing::{error, info, instrument, warn};

use velor_automations::{Automation, CatchUpPolicy, load_automations};
use velor_core::{
    AgentEvent, AgentRunner, ExecutionActivity, ExecutionActivityKind, ExecutionConfig,
    ExecutionEvent, ExecutionId, ExecutionMetrics, ExecutionRecord, ExecutionState, FileConfig,
    PromptDef, build_notifiers, render_template,
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
    /// Execution start timestamp.
    pub started_at: String,
    /// Execution completion timestamp (terminal states only).
    pub completed_at: Option<String>,
    /// Final error message if any.
    pub error: Option<String>,
    /// Optional user-assigned session name.
    pub name: Option<String>,
    /// Whether the session is pinned.
    pub pinned: bool,
    /// Project path associated with this execution.
    pub project_path: Option<String>,
}

/// Request payload for starting an execution.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct StartExecutionRequest {
    /// Execution configuration.
    pub config: UiExecutionConfig,
}

/// Frontend/API execution config shape.
///
/// This API model is intentionally decoupled from `velor_core::ExecutionConfig`
/// so the backend can apply config defaults and evolve independently.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct UiExecutionConfig {
    /// Name of the prompt template.
    pub prompt_name: String,
    /// Template variables from the UI.
    #[serde(default)]
    pub vars: BTreeMap<String, serde_json::Value>,
    /// Optional max iterations override.
    pub max_iterations: Option<u32>,
    /// Optional max retries override (reserved for retry loop integration).
    #[allow(dead_code)]
    pub max_retries: Option<u32>,
    /// Optional completion token override.
    pub complete_token: Option<String>,
    /// Optional provider binary override.
    pub binary: Option<String>,
    /// Optional permission mode override.
    pub permission_mode: Option<String>,
    /// Optional execution working directory.
    pub cwd: Option<String>,
    /// Optional rules enabled override.
    pub rules_enabled: Option<bool>,
    /// Optional rules directory override.
    pub rules_dir: Option<String>,
}

/// Response payload for start execution.
#[derive(Debug, Clone, serde::Serialize)]
pub struct StartExecutionResponse {
    /// Execution ID.
    pub execution_id: String,
    /// Initial state.
    pub state: String,
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

// ============================================================================
// Plan Types
// ============================================================================

/// Information about a discovered spec file.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SpecFileInfo {
    /// The file name (without extension).
    pub name: String,
    /// The full file path.
    pub path: String,
    /// The file content.
    pub content: String,
}

/// Request to generate a plan.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct GeneratePlanRequest {
    /// Path to the specs directory.
    pub specs_dir: Option<String>,
    /// OpenAI API key (if not using environment variable).
    pub api_key: Option<String>,
    /// OpenAI model to use.
    pub model: Option<String>,
    /// Optional custom OpenAI base URL.
    pub base_url: Option<String>,
    /// Whether to use dry run (no API calls).
    pub dry_run: Option<bool>,
}

/// Helper to extract timestamp from an event.
fn event_timestamp(event: &ExecutionEvent) -> chrono::DateTime<chrono::Utc> {
    match event {
        ExecutionEvent::StateChanged { timestamp, .. }
        | ExecutionEvent::OutputChunk { timestamp, .. }
        | ExecutionEvent::Error { timestamp, .. }
        | ExecutionEvent::IterationCompleted { timestamp, .. }
        | ExecutionEvent::MetricsUpdated { timestamp, .. }
        | ExecutionEvent::Activity { timestamp, .. } => *timestamp,
    }
}

/// Converts a UI variable value into the string representation expected by templates.
fn ui_var_to_string(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Null => String::new(),
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => value.to_string(),
    }
}

/// Resolves the effective execution binary from merged defaults + request override.
fn resolve_execution_binary(merged: &FileConfig, requested: Option<String>) -> String {
    if let Some(binary) = requested
        && !binary.trim().is_empty()
    {
        return binary;
    }

    let defaults_binary = merged.defaults.binary.clone();
    if merged.defaults.provider == velor_core::AgentProvider::Codex
        && defaults_binary == "claude-glm"
    {
        "codex".to_string()
    } else {
        defaults_binary
    }
}

/// Builds a core execution config from the API request and merged defaults.
fn build_execution_config(
    merged: &FileConfig,
    request: UiExecutionConfig,
    default_cwd: &str,
) -> ExecutionConfig {
    let template_vars = request
        .vars
        .into_iter()
        .map(|(k, v)| (k, ui_var_to_string(&v)))
        .collect::<BTreeMap<_, _>>();

    let binary = resolve_execution_binary(merged, request.binary);
    let permission_mode = request
        .permission_mode
        .or_else(|| merged.defaults.permission_mode.clone())
        .unwrap_or_else(|| "acceptEdits".to_string());
    let complete_token = request
        .complete_token
        .or_else(|| merged.defaults.complete_token.clone())
        .unwrap_or_else(|| "<promise>COMPLETE</promise>".to_string());
    let max_iterations = request
        .max_iterations
        .or(merged.defaults.iterations)
        .unwrap_or(1000);
    let cwd = request
        .cwd
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| default_cwd.to_string());
    let rules_enabled = request.rules_enabled.unwrap_or(merged.rules.enabled);
    let rules_dir = request
        .rules_dir
        .unwrap_or_else(|| merged.rules.directory.clone());

    ExecutionConfig::new(request.prompt_name)
        .with_template_vars(template_vars)
        .with_max_iterations(max_iterations)
        .with_complete_token(complete_token)
        .with_binary(binary)
        .with_permission_mode(permission_mode)
        .with_cwd(cwd)
        .with_rules(rules_enabled, rules_dir)
}

/// Converts an execution record into an API response model.
fn to_execution_status_response(
    record: &ExecutionRecord,
    is_active: bool,
    is_cancelled: bool,
) -> ExecutionStatusResponse {
    ExecutionStatusResponse {
        id: record.id.to_string(),
        state: format!("{:?}", record.state),
        prompt_name: record.config.prompt_name.clone(),
        iteration: record.metrics.iteration,
        is_active,
        is_cancelled,
        events: record.events.clone(),
        metrics: record.metrics.clone(),
        started_at: record.started_at.to_rfc3339(),
        completed_at: record.ended_at.map(|t| t.to_rfc3339()),
        error: record.error.clone(),
        name: record.name.clone(),
        pinned: record.pinned,
        project_path: record.project_path.clone(),
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
#[tauri::command]
#[instrument(skip(state, request), level = "debug")]
pub async fn start_execution(
    app: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
    request: StartExecutionRequest,
) -> CommandResult<StartExecutionResponse> {
    let merged = state.merged_config().await;
    let default_cwd = state
        .git_root()
        .await
        .map(|p| p.to_string_lossy().to_string())
        .or_else(|| {
            std::env::current_dir()
                .ok()
                .map(|p| p.to_string_lossy().to_string())
        })
        .unwrap_or_else(|| ".".to_string());
    let config = build_execution_config(&merged, request.config, &default_cwd);

    let id = state
        .start_execution(config.clone())
        .await
        .map_err(|e| format!("Failed to start execution: {}", e))?;

    let project_path = state
        .git_root()
        .await
        .map(|p| p.to_string_lossy().to_string());

    let initial = state
        .update_execution_record(&id, |record| {
            record.project_path = project_path.clone();
            record.set_state(ExecutionState::Pending);
        })
        .await
        .map_err(|e| format!("Failed to initialize execution record: {}", e))?
        .ok_or_else(|| "Execution not found immediately after creation".to_string())?;

    let payload = ExecutionEventPayload {
        execution: initial.clone(),
    };
    let _ = app.emit("velor://execution_started", payload);

    let state_arc = Arc::clone(state.inner());
    let app_clone = app.clone();
    let id_clone = id.clone();
    let id_for_task = id.clone();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build();
        match runtime {
            Ok(rt) => {
                if let Err(e) = rt.block_on(run_execution_task(app_clone, state_arc, id_clone)) {
                    error!(execution_id = %id_for_task, error = %e, "Execution task crashed");
                }
            }
            Err(e) => {
                error!(
                    execution_id = %id_for_task,
                    error = %e,
                    "Failed to create execution runtime"
                );
            }
        }
    });

    info!(id = %id, "Execution started");
    Ok(StartExecutionResponse {
        execution_id: id.to_string(),
        state: "pending".to_string(),
    })
}

/// Event payload for execution lifecycle updates.
#[derive(Debug, Clone, serde::Serialize)]
struct ExecutionEventPayload {
    execution: ExecutionRecord,
}

/// Background worker for running one execution and streaming updates.
async fn run_execution_task(
    app: tauri::AppHandle,
    state: Arc<AppState>,
    execution_id: ExecutionId,
) -> CommandResult<()> {
    let Some(record) = state.get_execution_record(&execution_id).await else {
        return Err(format!(
            "Execution {} disappeared before task start",
            execution_id
        ));
    };

    state
        .update_execution_record(&execution_id, |r| r.set_state(ExecutionState::Rendering))
        .await
        .map_err(|e| format!("Failed to set Rendering state: {e}"))?;
    emit_execution_update(&app, "velor://execution_updated", &state, &execution_id).await;

    let merged = state.merged_config().await;
    let git_root = state.git_root().await;
    let template = resolve_prompt_template_for_gui(&merged, &record.config, git_root.as_deref())
        .await
        .map_err(|e| {
            format!(
                "Failed to resolve prompt '{}': {e}",
                record.config.prompt_name
            )
        })?;

    let mut vars = merged.vars.clone();
    for (key, value) in &record.config.template_vars {
        vars.insert(key.clone(), value.clone());
    }
    vars.insert("iteration".to_string(), "1".to_string());
    vars.insert("cwd".to_string(), record.config.cwd.clone());

    let rendered_prompt = render_template(&template, &vars)
        .map_err(|e| format!("Failed to render execution prompt: {e}"))?;

    state
        .update_execution_record(&execution_id, |r| r.set_state(ExecutionState::Running))
        .await
        .map_err(|e| format!("Failed to set Running state: {e}"))?;
    emit_execution_update(&app, "velor://execution_updated", &state, &execution_id).await;

    let mut binary = record.config.binary.clone();
    if merged.defaults.provider == velor_core::AgentProvider::Codex && binary == "claude-glm" {
        binary = "codex".to_string();
    }
    if merged.defaults.provider == velor_core::AgentProvider::Omp && binary == "claude-glm" {
        binary = "omp".to_string();
    }

    let runner = AgentRunner::from_config(
        merged.defaults.provider,
        merged.defaults.protocol,
        merged.defaults.acp.clone(),
        merged.defaults.codex.clone(),
        merged.defaults.omp.clone(),
    );

    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
    let state_for_events = Arc::clone(&state);
    let app_for_events = app.clone();
    let event_execution_id = execution_id.clone();
    let provider_name = if merged.defaults.provider == velor_core::AgentProvider::Codex {
        "codex"
    } else if merged.defaults.provider == velor_core::AgentProvider::Omp {
        "omp"
    } else {
        "claude"
    }
    .to_string();

    let event_task = tokio::spawn(async move {
        while let Some(event) = event_rx.recv().await {
            if let Err(err) = apply_agent_event(
                &state_for_events,
                &event_execution_id,
                provider_name.as_str(),
                event,
            )
            .await
            {
                error!(
                    execution_id = %event_execution_id,
                    error = %err,
                    "Failed to apply agent event"
                );
                continue;
            }
            emit_execution_update(
                &app_for_events,
                "velor://execution_updated",
                &state_for_events,
                &event_execution_id,
            )
            .await;
        }
    });

    let event_tx_for_runner = event_tx.clone();
    let attempt_timeouts = velor_core::execution_service::supervisor::ProcessTimeouts {
        startup: None,
        stdin_write: None,
        idle: merged.defaults.idle_timeout.map(|t| t.get()),
        total: merged.defaults.attempt_timeout.map(|t| t.get()),
        termination_grace: merged
            .defaults
            .termination_grace
            .map(|t| t.get())
            .unwrap_or_else(|| std::time::Duration::from_secs(5)),
    };
    let run_result = runner
        .run_with_events(
            &binary,
            &record.config.permission_mode,
            &rendered_prompt,
            &record.config.prompt_name,
            Path::new(&record.config.cwd),
            &[],
            attempt_timeouts,
            tokio_util::sync::CancellationToken::new(),
            move |event| {
                let _ = event_tx_for_runner.send(event);
            },
        )
        .await;

    drop(event_tx);
    if let Err(e) = event_task.await {
        error!(
            execution_id = %execution_id,
            error = %e,
            "Execution event task join error"
        );
    }

    let was_cancelled = state
        .get_execution(&execution_id)
        .await
        .map(|e| e.is_cancelled())
        .unwrap_or(false);

    match run_result {
        Ok(result) => {
            if was_cancelled {
                state
                    .update_execution_record(&execution_id, |r| {
                        r.set_state(ExecutionState::Cancelled);
                    })
                    .await
                    .map_err(|e| format!("Failed to set Cancelled state: {e}"))?;
                emit_execution_update(&app, "velor://execution_failed", &state, &execution_id)
                    .await;
            } else {
                state
                    .update_execution_record(&execution_id, |r| {
                        if r.output.is_empty() {
                            r.append_output(&result.stdout);
                        }
                        r.set_state(ExecutionState::Completed);
                    })
                    .await
                    .map_err(|e| format!("Failed to set Completed state: {e}"))?;
                emit_execution_update(&app, "velor://execution_completed", &state, &execution_id)
                    .await;
            }
        }
        Err(err) => {
            state
                .update_execution_record(&execution_id, |r| {
                    r.record_error(err.to_string(), false);
                    r.set_state(ExecutionState::Failed);
                })
                .await
                .map_err(|e| format!("Failed to set Failed state: {e}"))?;
            emit_execution_update(&app, "velor://execution_failed", &state, &execution_id).await;
        }
    }

    state
        .finish_execution(&execution_id)
        .await
        .map_err(|e| format!("Failed to finalize execution: {e}"))?;

    Ok(())
}

/// Resolves the prompt template for GUI execution.
async fn resolve_prompt_template_for_gui(
    merged: &FileConfig,
    config: &ExecutionConfig,
    git_root: Option<&Path>,
) -> color_eyre::eyre::Result<String> {
    if merged.prompts_config.enabled {
        let home_dir = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .wrap_err("failed to determine home directory")?;
        let home_velor_dir = std::path::PathBuf::from(home_dir).join(".velor");
        let repo_velor_dir = git_root.map(|g| g.join(".velor"));
        let prompt_cache = velor_core::prompts::PromptCache::new(home_velor_dir, repo_velor_dir);

        if let Ok(prompt) = prompt_cache.get_by_name(&config.prompt_name).await {
            return Ok(prompt.content);
        }
    }

    let prompt_def = merged
        .prompts
        .get(&config.prompt_name)
        .ok_or_else(|| color_eyre::eyre::eyre!("prompt '{}' not found", config.prompt_name))?;

    let template = match prompt_def {
        PromptDef::Inline(s) => s.clone(),
        PromptDef::Table { template, .. } => template.clone(),
        PromptDef::File { path, .. } => {
            let Some(root) = git_root else {
                return Err(color_eyre::eyre::eyre!(
                    "cannot resolve file prompt '{}' without git root",
                    path
                ));
            };
            let full = root
                .join(".velor")
                .join(&merged.prompts_config.directory)
                .join(path);
            tokio::fs::read_to_string(&full)
                .await
                .wrap_err_with(|| format!("failed to read prompt file {}", full.display()))?
        }
    };

    Ok(template)
}

/// Applies a streamed provider event to an execution record.
async fn apply_agent_event(
    state: &Arc<AppState>,
    execution_id: &ExecutionId,
    provider: &str,
    event: AgentEvent,
) -> CommandResult<()> {
    state
        .update_execution_record(execution_id, |record| match event {
            AgentEvent::Status { message } => record.record_activity(ExecutionActivity {
                provider: provider.to_string(),
                kind: ExecutionActivityKind::Status,
                summary: message,
                detail: None,
                success: None,
            }),
            AgentEvent::TextDelta { text } => record.append_output(&text),
            AgentEvent::Thinking { text } => record.record_activity(ExecutionActivity {
                provider: provider.to_string(),
                kind: ExecutionActivityKind::Status,
                summary: "thinking".to_string(),
                detail: Some(text),
                success: None,
            }),
            AgentEvent::ToolCall { tool, detail, .. } => {
                record.record_activity(ExecutionActivity {
                    provider: provider.to_string(),
                    kind: ExecutionActivityKind::ToolCall,
                    summary: tool,
                    detail: Some(detail),
                    success: None,
                })
            }
            AgentEvent::ToolResult {
                tool,
                detail,
                success,
            } => record.record_activity(ExecutionActivity {
                provider: provider.to_string(),
                kind: ExecutionActivityKind::ToolResult,
                summary: tool,
                detail: Some(detail),
                success,
            }),
            AgentEvent::FileEdit { edit } => record.record_activity(ExecutionActivity {
                provider: provider.to_string(),
                kind: ExecutionActivityKind::ToolCall,
                summary: "file edit".to_string(),
                detail: Some(format!(
                    "{} ({} line{})",
                    edit.path,
                    edit.diff_line_count(),
                    if edit.diff_line_count() == 1 { "" } else { "s" }
                )),
                success: None,
            }),
            AgentEvent::Usage {
                input_tokens,
                output_tokens,
                ..
            } => {
                let mut metrics = record.metrics.clone();
                let total = input_tokens.unwrap_or(0) + output_tokens.unwrap_or(0);
                if total > 0 {
                    metrics.total_tokens = Some(total);
                    record.update_metrics(metrics);
                }
                record.record_activity(ExecutionActivity {
                    provider: provider.to_string(),
                    kind: ExecutionActivityKind::Usage,
                    summary: "token usage".to_string(),
                    detail: Some(format!(
                        "input={}, output={}",
                        input_tokens.unwrap_or(0),
                        output_tokens.unwrap_or(0)
                    )),
                    success: None,
                });
            }
            AgentEvent::Error { message } => {
                record.record_error(message.clone(), false);
                record.record_activity(ExecutionActivity {
                    provider: provider.to_string(),
                    kind: ExecutionActivityKind::Provider,
                    summary: "provider error".to_string(),
                    detail: Some(message),
                    success: Some(false),
                });
            }
        })
        .await
        .map_err(|e| format!("Failed to apply event: {e}"))?;
    Ok(())
}

/// Emits one execution update event by name if the record is still active.
async fn emit_execution_update(
    app: &tauri::AppHandle,
    event_name: &str,
    state: &Arc<AppState>,
    execution_id: &ExecutionId,
) {
    if let Some(record) = state.get_execution_record(execution_id).await {
        let _ = app.emit(event_name, ExecutionEventPayload { execution: record });
    }
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
        return Ok(Some(to_execution_status_response(
            &active.record,
            true,
            active.is_cancelled(),
        )));
    }

    // Check history
    let history = state.execution_history().await;
    if let Some(record) = history.iter().find(|r| r.id.to_string() == id) {
        return Ok(Some(to_execution_status_response(record, false, false)));
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
        .map(|a| to_execution_status_response(&a.record, true, a.is_cancelled()))
        .collect();

    all.extend(
        history
            .into_iter()
            .map(|r| to_execution_status_response(&r, false, false)),
    );

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

// ============================================================================
// Plan Commands
// ============================================================================

/// Discovers and returns all spec files from the specs directory.
///
/// # Arguments
///
/// * `specs_dir` - Optional path to specs directory. If not provided, uses
///   `{git_root}/specs/`.
///
/// Returns a list of spec files with their names, paths, and contents.
#[tauri::command]
#[instrument(skip(state), level = "debug")]
pub async fn discover_specs(
    state: State<'_, Arc<AppState>>,
    specs_dir: Option<String>,
) -> CommandResult<Vec<SpecFileInfo>> {
    let git_root = state.git_root().await.ok_or("No git root set")?;

    let specs_path = match specs_dir {
        Some(dir) => std::path::PathBuf::from(dir),
        None => git_root.join("specs"),
    };

    if !specs_path.exists() {
        return Err(format!(
            "Specs directory not found: {}",
            specs_path.display()
        ));
    }

    let mut specs = Vec::new();

    let mut dir_reader = tokio::fs::read_dir(&specs_path)
        .await
        .map_err(|e| format!("Failed to read specs directory: {}", e))?;

    while let Some(entry) = dir_reader
        .next_entry()
        .await
        .map_err(|e| format!("Failed to read directory entry: {}", e))?
    {
        let path = entry.path();

        // Only process .md files
        if path.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }

        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| format!("Invalid spec file name: {}", path.display()))?
            .to_string();

        let content = tokio::fs::read_to_string(&path)
            .await
            .map_err(|e| format!("Failed to read spec file {}: {}", path.display(), e))?;

        specs.push(SpecFileInfo {
            name,
            path: path.to_string_lossy().to_string(),
            content,
        });
    }

    // Sort by name for deterministic ordering
    specs.sort_by(|a, b| a.name.cmp(&b.name));

    info!(count = specs.len(), "Specs discovered");
    Ok(specs)
}

/// Builds a plan prompt from the given spec files.
///
/// This is useful for dry-run mode to see what prompt would be sent
/// without actually calling the API.
///
/// # Arguments
///
/// * `specs` - List of spec files to include in the prompt.
#[tauri::command]
#[instrument(level = "debug")]
pub async fn build_plan_prompt(specs: Vec<SpecFileInfo>) -> CommandResult<String> {
    let mut prompt = String::from(
        "# Plan Generation Request\n\n\
        You are an expert software architect. Please review the following specification(s) \
        and generate a detailed implementation plan.\n\n\
        The plan should:\n\
        1. Break down the work into clear, actionable tasks\n\
        2. Identify dependencies between tasks\n\
        3. Suggest an optimal execution order\n\
        4. Note any potential risks or technical challenges\n\
        5. Reference the specific spec files being addressed\n\n",
    );

    if specs.is_empty() {
        prompt
            .push_str("WARNING: No spec files were found. Please verify the specs directory.\n\n");
    } else {
        prompt.push_str("## Specifications\n\n");
        for spec in &specs {
            prompt.push_str(&format!("### {} ({})\n\n", spec.name, spec.path));
            prompt.push_str(&spec.content);
            prompt.push_str("\n\n");
        }
    }

    prompt.push_str(
        "## Output Format\n\n\
        Please output the implementation plan in markdown format with:\n\
        - Clear task headings with task numbers\n\
        - Dependencies between tasks clearly marked\n\
        - Estimated complexity for each task (Low/Medium/High)\n\
        - Risk assessment where applicable\n\n\
        Begin your response with the plan directly.",
    );

    Ok(prompt)
}

/// Generates an implementation plan using OpenAI.
///
/// # Arguments
///
/// * `request` - The plan generation request.
///
/// Returns the generated plan content.
#[tauri::command]
#[instrument(skip(state, request), level = "debug")]
pub async fn generate_plan(
    state: State<'_, Arc<AppState>>,
    request: GeneratePlanRequest,
) -> CommandResult<String> {
    // Get API key from request or environment
    let api_key = match request.api_key {
        Some(ref key) if !key.is_empty() => key.clone(),
        _ => std::env::var("OPENAI_API_KEY")
            .map_err(|_| "OpenAI API key not found. Set OPENAI_API_KEY environment variable or provide api_key in request.".to_string())?,
    };

    // Validate API key
    if api_key.is_empty() {
        return Err(
            "OpenAI API key is empty. Set OPENAI_API_KEY environment variable.".to_string(),
        );
    }

    // Discover specs
    let specs = discover_specs(state, request.specs_dir.clone()).await?;

    if specs.is_empty() {
        return Err(
            "No spec files found in specs directory. Please create .md spec files first."
                .to_string(),
        );
    }

    // Build prompt
    let prompt = build_plan_prompt(specs).await?;

    tracing::debug!(prompt_len = prompt.len(), "Prepared plan generation prompt");

    // Handle dry run
    if request.dry_run.unwrap_or(false) {
        return Ok(format!(
            "# Plan Generation Prompt (Dry Run)\n\n{}\n\n✅ Dry run complete. No API call was made.",
            prompt
        ));
    }

    // Build HTTP client
    let client = reqwest::Client::builder()
        .build()
        .map_err(|e| format!("Failed to build HTTP client: {}", e))?;

    // Determine model and base URL
    let model = request.model.unwrap_or_else(|| "gpt-4o".to_string());
    let base_url = request
        .base_url
        .unwrap_or_else(|| "https://api.openai.com/v1/chat/completions".to_string());

    // Build API request
    let response = client
        .post(&base_url)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({
            "model": model,
            "messages": [
                {
                    "role": "user",
                    "content": prompt
                }
            ],
            "temperature": 0.7,
        }))
        .send()
        .await
        .map_err(|e| format!("Failed to send request to OpenAI API: {}", e))?;

    if !response.status().is_success() {
        let status = response.status();
        let error_body = response
            .text()
            .await
            .unwrap_or_else(|_| "unable to read error body".to_string());
        return Err(format!(
            "OpenAI API request failed with status {}: {}",
            status, error_body
        ));
    }

    let response_json: serde_json::Value = response
        .json()
        .await
        .map_err(|e| format!("Failed to parse OpenAI API response as JSON: {}", e))?;

    let content = response_json["choices"][0]["message"]["content"]
        .as_str()
        .ok_or("OpenAI API response missing content field")?
        .to_string();

    info!("Plan generated successfully");
    Ok(content)
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
            started_at: chrono::Utc::now().to_rfc3339(),
            completed_at: None,
            error: None,
            name: None,
            pinned: false,
            project_path: None,
        };

        let json = serde_json::to_string(&response);
        assert!(
            json.is_ok(),
            "ExecutionStatusResponse should serialize to JSON"
        );
    }

    #[test]
    fn test_build_execution_config_uses_merged_defaults() {
        let mut merged = FileConfig::default();
        merged.defaults.provider = velor_core::AgentProvider::Codex;
        merged.defaults.binary = "claude-glm".to_string();
        merged.defaults.permission_mode = Some("allow".to_string());
        merged.defaults.complete_token = Some("<DONE>".to_string());
        merged.defaults.iterations = Some(17);
        merged.rules.enabled = true;
        merged.rules.directory = ".agents/rules".to_string();

        let mut vars = BTreeMap::new();
        vars.insert("text".to_string(), serde_json::json!("hello"));
        vars.insert("num".to_string(), serde_json::json!(42));
        vars.insert("flag".to_string(), serde_json::json!(true));
        vars.insert("obj".to_string(), serde_json::json!({"k":"v"}));

        let request = UiExecutionConfig {
            prompt_name: "build".to_string(),
            vars,
            max_iterations: None,
            max_retries: None,
            complete_token: None,
            binary: None,
            permission_mode: None,
            cwd: None,
            rules_enabled: None,
            rules_dir: None,
        };

        let config = build_execution_config(&merged, request, "/tmp/work");

        assert_eq!(config.prompt_name, "build");
        assert_eq!(config.binary, "codex");
        assert_eq!(config.permission_mode, "allow");
        assert_eq!(config.complete_token, "<DONE>");
        assert_eq!(config.max_iterations, 17);
        assert_eq!(config.cwd, "/tmp/work");
        assert!(config.rules_enabled);
        assert_eq!(config.rules_dir, ".agents/rules");
        assert_eq!(config.template_vars.get("text"), Some(&"hello".to_string()));
        assert_eq!(config.template_vars.get("num"), Some(&"42".to_string()));
        assert_eq!(config.template_vars.get("flag"), Some(&"true".to_string()));
        assert_eq!(
            config.template_vars.get("obj"),
            Some(&"{\"k\":\"v\"}".to_string())
        );
    }

    #[test]
    fn test_build_execution_config_respects_request_overrides() {
        let mut merged = FileConfig::default();
        merged.defaults.binary = "claude-glm".to_string();
        merged.defaults.permission_mode = Some("acceptEdits".to_string());
        merged.defaults.complete_token = Some("<DEFAULT>".to_string());

        let request = UiExecutionConfig {
            prompt_name: "override".to_string(),
            vars: BTreeMap::new(),
            max_iterations: Some(3),
            max_retries: Some(2),
            complete_token: Some("<LOCAL>".to_string()),
            binary: Some("custom-agent".to_string()),
            permission_mode: Some("manual".to_string()),
            cwd: Some("/tmp/custom".to_string()),
            rules_enabled: Some(false),
            rules_dir: Some(".rules/custom".to_string()),
        };

        let config = build_execution_config(&merged, request, "/tmp/work");

        assert_eq!(config.binary, "custom-agent");
        assert_eq!(config.permission_mode, "manual");
        assert_eq!(config.complete_token, "<LOCAL>");
        assert_eq!(config.max_iterations, 3);
        assert_eq!(config.cwd, "/tmp/custom");
        assert!(!config.rules_enabled);
        assert_eq!(config.rules_dir, ".rules/custom");
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

    // ========================================================================
    // Plan Command Tests
    // ========================================================================

    #[test]
    fn test_spec_file_info_serialization() {
        let spec = SpecFileInfo {
            name: "auth".to_string(),
            path: "/specs/auth.md".to_string(),
            content: "# Auth Spec\n\nImplement authentication.".to_string(),
        };

        let json = serde_json::to_string(&spec);
        assert!(json.is_ok(), "SpecFileInfo should serialize to JSON");

        let deserialized: SpecFileInfo = serde_json::from_str(&json.unwrap())
            .expect("SpecFileInfo should deserialize from JSON");
        assert_eq!(deserialized.name, "auth");
        assert_eq!(deserialized.path, "/specs/auth.md");
        assert!(deserialized.content.contains("authentication"));
    }

    #[test]
    fn test_spec_file_info_deserialization() {
        let json = r##"{
            "name": "database",
            "path": "/specs/database.md",
            "content": "# Database Spec"
        }"##;

        let spec: SpecFileInfo =
            serde_json::from_str(json).expect("SpecFileInfo should deserialize from JSON");

        assert_eq!(spec.name, "database");
        assert_eq!(spec.path, "/specs/database.md");
        assert_eq!(spec.content, "# Database Spec");
    }

    #[test]
    fn test_generate_plan_request_deserialization() {
        let json = r#"{
            "specs_dir": "/custom/specs",
            "model": "gpt-4",
            "dry_run": true
        }"#;

        let request: GeneratePlanRequest =
            serde_json::from_str(json).expect("GeneratePlanRequest should deserialize from JSON");

        assert_eq!(request.specs_dir, Some("/custom/specs".to_string()));
        assert_eq!(request.model, Some("gpt-4".to_string()));
        assert_eq!(request.dry_run, Some(true));
        assert_eq!(request.api_key, None);
        assert_eq!(request.base_url, None);
    }

    #[test]
    fn test_generate_plan_request_defaults() {
        let json = r#"{}"#;

        let request: GeneratePlanRequest = serde_json::from_str(json)
            .expect("GeneratePlanRequest should deserialize with defaults");

        assert_eq!(request.specs_dir, None);
        assert_eq!(request.model, None);
        assert_eq!(request.dry_run, None);
        assert_eq!(request.api_key, None);
        assert_eq!(request.base_url, None);
    }

    #[tokio::test]
    async fn test_build_plan_prompt_empty_specs() {
        let specs = vec![];
        let prompt = build_plan_prompt(specs).await.expect("should build prompt");

        assert!(
            prompt.contains("WARNING: No spec files were found"),
            "Prompt should warn about empty specs"
        );
    }

    #[tokio::test]
    async fn test_build_plan_prompt_with_specs() {
        let specs = vec![
            SpecFileInfo {
                name: "auth".to_string(),
                path: "/specs/auth.md".to_string(),
                content: "# Auth\n\nImplement OAuth2.".to_string(),
            },
            SpecFileInfo {
                name: "database".to_string(),
                path: "/specs/database.md".to_string(),
                content: "# Database\n\nUse PostgreSQL.".to_string(),
            },
        ];

        let prompt = build_plan_prompt(specs).await.expect("should build prompt");

        assert!(
            prompt.contains("## Specifications"),
            "Prompt should contain Specifications section"
        );
        assert!(
            prompt.contains("auth.md"),
            "Prompt should mention auth spec file"
        );
        assert!(
            prompt.contains("database.md"),
            "Prompt should mention database spec file"
        );
        assert!(
            prompt.contains("Implement OAuth2"),
            "Prompt should contain auth content"
        );
        assert!(
            prompt.contains("Use PostgreSQL"),
            "Prompt should contain database content"
        );
        assert!(
            prompt.contains("## Output Format"),
            "Prompt should contain Output Format section"
        );
    }

    #[tokio::test]
    async fn test_build_plan_prompt_includes_instructions() {
        let specs = vec![SpecFileInfo {
            name: "test".to_string(),
            path: "/specs/test.md".to_string(),
            content: "# Test".to_string(),
        }];

        let prompt = build_plan_prompt(specs).await.expect("should build prompt");

        assert!(
            prompt.contains("Plan Generation Request"),
            "Prompt should contain title"
        );
        assert!(
            prompt.contains("expert software architect"),
            "Prompt should set the role"
        );
        assert!(
            prompt.contains("actionable tasks"),
            "Prompt should ask for actionable tasks"
        );
        assert!(
            prompt.contains("Dependencies between tasks"),
            "Prompt should ask for dependencies"
        );
        assert!(
            prompt.contains("Risk assessment"),
            "Prompt should ask for risk assessment"
        );
    }
}
