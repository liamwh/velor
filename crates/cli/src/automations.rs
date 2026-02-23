//! Automations command handlers for velor.
//!
//! This module provides CLI commands for managing and running scheduled automations.

use crate::config::FileConfig;
use clap::Args;
use color_eyre::eyre::WrapErr;
use std::path::PathBuf;

use velor_automations::{AutomationRunner, AutomationStore, load_automations};

/// Arguments for the `automations` subcommand.
#[derive(Debug, Args)]
pub struct AutomationsArgs {
    #[command(subcommand)]
    pub command: AutomationsCommand,
}

/// Automations subcommands.
#[derive(Debug, clap::Subcommand)]
pub enum AutomationsCommand {
    /// List all configured automations
    List,

    /// Validate automation definitions
    Validate,

    /// Run an automation immediately (bypassing schedule)
    Run {
        /// Name of the automation to run
        name: String,
    },

    /// Show automation status and recent runs
    Status {
        /// Optional automation name to filter by
        name: Option<String>,
    },

    /// Start the automation daemon (runs continuously)
    Daemon {
        /// Override the default tick interval in seconds (default: 60)
        #[arg(long)]
        tick_interval_secs: Option<u64>,
    },
}

/// Runs the `automations list` subcommand.
///
/// Lists all configured automations with their schedules and status.
#[tracing::instrument(level = "debug", ret, err, fields(git_root = %git_root.display()))]
pub async fn run_list(home_cfg: FileConfig, git_root: PathBuf) -> color_eyre::eyre::Result<()> {
    let config_path = FileConfig::default_config_path(&git_root);
    let repo_cfg = FileConfig::load_if_exists(&config_path)
        .wrap_err_with(|| format!("failed to load config at {}", config_path.display()))?
        .unwrap_or_default();

    let merged_cfg = FileConfig::merge(home_cfg, repo_cfg);
    let auto_cfg = merged_cfg.automations;

    let automations_dir = git_root.join(&auto_cfg.automations_dir);

    println!(
        "📋 Loading automations from {}...",
        automations_dir.display()
    );

    let automations = load_automations(&automations_dir).await?;

    if automations.is_empty() {
        println!("No automations configured.");
        println!("Create automation files in {}/", automations_dir.display());
        return Ok(());
    }

    println!("════════════════════════════════════════");
    println!("📋 Configured Automations");
    println!("════════════════════════════════════════\n");

    for auto in &automations {
        println!("Name: {}", auto.name);
        println!("  Description: {}", auto.description);
        println!("  Schedule: {}", auto.schedule);
        println!("  Timezone: {}", auto.timezone);
        println!("  Prompt: {}", auto.prompt);
        println!(
            "  Status: {}",
            if auto.enabled {
                "✅ Enabled"
            } else {
                "❌ Disabled"
            }
        );
        println!("  Catch-up policy: {:?}", auto.catch_up);
        if !auto.vars.is_empty() {
            println!("  Variables:");
            for (key, value) in &auto.vars {
                println!("    {} = {}", key, value);
            }
        }
        println!();
    }

    println!("Total: {} automation(s)", automations.len());

    Ok(())
}

/// Runs the `automations validate` subcommand.
///
/// Validates all automation definitions without executing them.
#[tracing::instrument(level = "debug", ret, err, fields(git_root = %git_root.display()))]
pub async fn run_validate(home_cfg: FileConfig, git_root: PathBuf) -> color_eyre::eyre::Result<()> {
    let config_path = FileConfig::default_config_path(&git_root);
    let repo_cfg = FileConfig::load_if_exists(&config_path)
        .wrap_err_with(|| format!("failed to load config at {}", config_path.display()))?
        .unwrap_or_default();

    let merged_cfg = FileConfig::merge(home_cfg, repo_cfg);
    let auto_cfg = merged_cfg.automations;

    let automations_dir = git_root.join(&auto_cfg.automations_dir);

    println!(
        "🔍 Validating automations in {}...",
        automations_dir.display()
    );

    match load_automations(&automations_dir).await {
        Ok(automations) => {
            println!("✅ All {} automation(s) are valid!", automations.len());
            for auto in &automations {
                println!("  - {} ({})", auto.name, auto.schedule);
            }
            Ok(())
        }
        Err(e) => {
            println!("❌ Validation failed:");
            Err(e)
        }
    }
}

/// Runs the `automations run` subcommand.
///
/// Executes a single automation immediately, bypassing its schedule.
#[tracing::instrument(level = "debug", ret, err, fields(git_root = %git_root.display(), name = %name))]
pub async fn run_run(
    name: String,
    home_cfg: FileConfig,
    git_root: PathBuf,
) -> color_eyre::eyre::Result<()> {
    let config_path = FileConfig::default_config_path(&git_root);
    let repo_cfg = FileConfig::load_if_exists(&config_path)
        .wrap_err_with(|| format!("failed to load config at {}", config_path.display()))?
        .unwrap_or_default();

    let merged_cfg = FileConfig::merge(home_cfg, repo_cfg);
    let auto_cfg = merged_cfg.automations;

    let automations_dir = git_root.join(&auto_cfg.automations_dir);
    let automations = load_automations(&automations_dir).await?;

    let automation = automations
        .iter()
        .find(|a| a.name == name)
        .ok_or_else(|| color_eyre::eyre::eyre!("automation '{}' not found", name))?;

    if !automation.enabled {
        println!("⚠️  Automation '{}' is disabled", name);
        return Ok(());
    }

    println!("🚀 Running automation '{}'...", name);

    // Open state database
    let db_path = git_root.join(&auto_cfg.state_db_path);
    let store = AutomationStore::open(&db_path).await?;

    // Create runner
    let runner = AutomationRunner::new(
        store,
        auto_cfg.max_concurrent,
        &git_root,
        merged_cfg.defaults.binary,
        auto_cfg.max_output_bytes,
    );

    // Use current time as scheduled_for (since we're running immediately)
    let scheduled_for = chrono::Utc::now();

    // Get the cancel token
    let (_cancel_handler, cancel_token) = crate::cancellation::CancellationHandler::new();

    // Run the automation
    let result = runner
        .run_automation(automation, scheduled_for, &cancel_token)
        .await?;

    match result.status {
        velor_automations::AutomationRunStatus::Completed => {
            println!("✅ Automation '{}' completed successfully", name);
            println!("   Iterations: {}", result.iterations_completed);
        }
        velor_automations::AutomationRunStatus::Failed => {
            println!("❌ Automation '{}' failed", name);
            if let Some(error) = result.error {
                println!("   Error: {}", error);
            }
        }
        velor_automations::AutomationRunStatus::Cancelled => {
            println!("⚠️  Automation '{}' was cancelled", name);
        }
        _ => {
            println!(
                "⚠️  Automation '{}' ended with status: {:?}",
                name, result.status
            );
        }
    }

    Ok(())
}

/// Runs the `automations status` subcommand.
///
/// Shows recent execution history for automations.
#[tracing::instrument(level = "debug", ret, err, fields(git_root = %git_root.display()))]
pub async fn run_status(
    name: Option<String>,
    home_cfg: FileConfig,
    git_root: PathBuf,
) -> color_eyre::eyre::Result<()> {
    let config_path = FileConfig::default_config_path(&git_root);
    let repo_cfg = FileConfig::load_if_exists(&config_path)
        .wrap_err_with(|| format!("failed to load config at {}", config_path.display()))?
        .unwrap_or_default();

    let merged_cfg = FileConfig::merge(home_cfg, repo_cfg);
    let auto_cfg = merged_cfg.automations;

    let db_path = git_root.join(&auto_cfg.state_db_path);

    if !db_path.exists() {
        println!(
            "No automation state database found at {}",
            db_path.display()
        );
        println!("Automations have not been run yet.");
        return Ok(());
    }

    let store = AutomationStore::open(&db_path).await?;
    let limit = 20;

    let runs = store.get_runs(name.as_deref(), limit).await?;

    if runs.is_empty() {
        println!(
            "No recent runs{}",
            name.as_ref()
                .map(|n| format!(" for automation '{}'", n))
                .unwrap_or_default()
        );
        return Ok(());
    }

    println!("════════════════════════════════════════");
    println!("📊 Recent Automation Runs");
    println!("════════════════════════════════════════\n");

    for run in runs {
        let status_icon = match run.status {
            velor_automations::AutomationRunStatus::Pending => "⏳",
            velor_automations::AutomationRunStatus::Running => "🔄",
            velor_automations::AutomationRunStatus::Completed => "✅",
            velor_automations::AutomationRunStatus::Failed => "❌",
            velor_automations::AutomationRunStatus::Cancelled => "⚠️",
        };

        println!("{} Run ID: {}", status_icon, run.id);
        println!("  Automation: {}", run.automation_name);
        println!(
            "  Scheduled: {}",
            run.scheduled_for.format("%Y-%m-%d %H:%M:%S UTC")
        );
        println!(
            "  Started: {}",
            run.started_at.format("%Y-%m-%d %H:%M:%S UTC")
        );
        if let Some(completed) = run.completed_at {
            println!("  Completed: {}", completed.format("%Y-%m-%d %H:%M:%S UTC"));
        }
        println!("  Status: {}", run.status.as_str());
        println!("  Iterations: {}", run.iterations_completed);
        if let Some(duration_ms) = run.duration_ms {
            println!("  Duration: {:.2}s", duration_ms as f64 / 1000.0);
        }
        if let Some(exit_code) = run.exit_code {
            println!("  Exit Code: {}", exit_code);
        }
        if let Some(ref output) = run.output {
            let preview = if output.len() > 200 {
                format!("{}...", &output[..200])
            } else {
                output.clone()
            };
            println!("  Output: {}", preview);
        }
        if let Some(ref error) = run.error {
            println!("  Error: {}", error);
        }
        println!();
    }

    Ok(())
}

/// Runs the `automations daemon` subcommand.
///
/// Starts the automation daemon which continuously monitors and runs scheduled automations.
#[tracing::instrument(level = "debug", ret, err, fields(git_root = %git_root.display()))]
pub async fn run_daemon(
    tick_interval_secs: Option<u64>,
    home_cfg: FileConfig,
    git_root: PathBuf,
) -> color_eyre::eyre::Result<()> {
    let config_path = FileConfig::default_config_path(&git_root);
    let repo_cfg = FileConfig::load_if_exists(&config_path)
        .wrap_err_with(|| format!("failed to load config at {}", config_path.display()))?
        .unwrap_or_default();

    let merged_cfg = FileConfig::merge(home_cfg, repo_cfg);
    let auto_cfg = merged_cfg.automations;

    let automations_dir = git_root.join(&auto_cfg.automations_dir);
    let db_path = git_root.join(&auto_cfg.state_db_path);

    let tick_interval = std::time::Duration::from_secs(tick_interval_secs.unwrap_or(60));

    println!("🔄 Starting Velor Automations Daemon");
    println!("   Tick interval: {:?}", tick_interval);
    println!("   Automations dir: {}", automations_dir.display());
    println!("   State database: {}", db_path.display());
    println!();

    // Load automations
    let automations = load_automations(&automations_dir).await?;
    let enabled_automations: Vec<_> = automations.into_iter().filter(|a| a.enabled).collect();

    if enabled_automations.is_empty() {
        println!("⚠️  No enabled automations found. Exiting.");
        return Ok(());
    }

    println!(
        "📋 Loaded {} enabled automation(s):",
        enabled_automations.len()
    );
    for auto in &enabled_automations {
        println!("  - {} ({})", auto.name, auto.schedule);
    }
    println!();

    // Open state database
    let store = AutomationStore::open(&db_path).await?;

    // Create runner
    let runner = AutomationRunner::new(
        store,
        auto_cfg.max_concurrent,
        &git_root,
        merged_cfg.defaults.binary,
        auto_cfg.max_output_bytes,
    );

    // Create cancellation handler for graceful shutdown
    let (cancel_handler, cancel_token) = crate::cancellation::CancellationHandler::new();

    println!("✅ Daemon started. Press Ctrl+C to stop gracefully.");
    println!("════════════════════════════════════════\n");

    // Main daemon loop
    let mut tick_count = 0u64;
    loop {
        // Check for force cancellation (Ctrl+C twice)
        if cancel_handler.is_cancelled() {
            println!("\n🛑 Force quit by user (Ctrl+C twice)");
            break;
        }

        // Check for graceful shutdown request (Ctrl+C once)
        if cancel_handler.graceful_shutdown_requested() {
            println!("\n⚠️  Graceful shutdown requested. Stopping after current tick...");
            break;
        }

        tick_count += 1;
        let now = chrono::Utc::now();

        println!(
            "🕐 Tick #{} at {}",
            tick_count,
            now.format("%Y-%m-%d %H:%M:%S UTC")
        );

        // Check each automation
        for automation in &enabled_automations {
            // Get last run time for this automation
            let last_run = get_last_run_time(&automation.name, now).await;

            // Calculate next scheduled time
            let timezone = automation
                .timezone
                .parse::<chrono_tz::Tz>()
                .unwrap_or(chrono_tz::UTC);

            match velor_automations::Scheduler::new(&automation.schedule, timezone) {
                Ok(scheduler) => {
                    let next_run = scheduler.next_after(last_run);

                    if next_run <= now {
                        println!(
                            "  ▶️  Running '{}' (scheduled for {})",
                            automation.name,
                            next_run.format("%H:%M:%S")
                        );

                        // Run the automation
                        match runner
                            .run_automation(automation, next_run, &cancel_token)
                            .await
                        {
                            Ok(result) => {
                                let status_icon = match result.status {
                                    velor_automations::AutomationRunStatus::Completed => "✅",
                                    velor_automations::AutomationRunStatus::Failed => "❌",
                                    velor_automations::AutomationRunStatus::Cancelled => "⚠️",
                                    _ => "⏳",
                                };
                                println!(
                                    "      {} Status: {}",
                                    status_icon,
                                    result.status.as_str()
                                );
                            }
                            Err(e) => {
                                println!("      ❌ Error: {}", e);
                            }
                        }
                    } else {
                        tracing::debug!(
                            "  ⏸️  Skipping '{}' (next run at {})",
                            automation.name,
                            next_run.format("%Y-%m-%d %H:%M:%S")
                        );
                    }
                }
                Err(e) => {
                    println!("  ❌ Invalid schedule for '{}': {}", automation.name, e);
                }
            }
        }

        println!();

        // Wait for next tick or cancellation
        tokio::select! {
            _ = tokio::time::sleep(tick_interval) => {}
            _ = cancel_handler.token().cancelled() => {
                println!("\n🛑 Cancel signal received, stopping...");
                break;
            }
        }
    }

    println!("👋 Daemon stopped.");

    Ok(())
}

/// Gets the last run time for an automation.
///
/// Returns the time of the most recent run, or the current time if no runs exist.
async fn get_last_run_time(
    _automation_name: &str,
    default: chrono::DateTime<chrono::Utc>,
) -> chrono::DateTime<chrono::Utc> {
    // For now, return a simple default
    // In a full implementation, this would query the store
    default
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_automations_command_exists() {
        // This test ensures the command types compile correctly
        let _ = AutomationsArgs {
            command: AutomationsCommand::List,
        };
    }
}
