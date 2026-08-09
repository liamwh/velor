//! Automations command handlers for velor.
//!
//! This module provides CLI commands for managing and running scheduled automations.
//! Supports dual-location discovery: global (XDG_CONFIG_HOME/velor/automations/)
//! and project-specific ({repo}/.velor/automations/).

use crate::core::config::FileConfig;
use clap::Args;
use color_eyre::eyre::WrapErr;
use color_eyre::eyre::eyre;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use velor_automations::{
    Automation, AutomationCache, AutomationEntry, AutomationRunner, AutomationStore, CatchUpPolicy,
    ProjectEntry, ProjectRegistry, merge_automation_vars,
};
use velor_core::prompts::PromptCache;

pub mod launchd;

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
    List {
        /// Show disabled automations
        #[arg(long)]
        all: bool,
    },

    /// Validate automation definitions
    Validate,

    /// Run an automation immediately (bypassing schedule)
    Run {
        /// Name of the automation to run
        name: String,
        /// Force run even if disabled
        #[arg(long)]
        force: bool,
    },

    /// Show automation status and recent runs
    Status {
        /// Optional automation name to filter by
        name: Option<String>,
    },

    /// Run one tick of the scheduler (for use with external schedulers like launchd/cron)
    Tick {},

    /// Start the automation daemon (runs continuously)
    Daemon {
        /// Override the default tick interval in seconds (default: 60)
        #[arg(long)]
        tick_interval_secs: Option<u64>,
    },

    /// Install launchd service
    Install {
        /// Tick interval in seconds
        #[arg(long)]
        interval: Option<u64>,
    },

    /// Uninstall launchd service
    Uninstall,

    /// Show launchd service status
    ServiceStatus,
}

/// Gets the XDG config home directory for velor.
///
/// Returns `XDG_CONFIG_HOME/velor` or `~/.config/velor` as fallback.
#[must_use]
pub fn get_xdg_config_home() -> PathBuf {
    // Try XDG_CONFIG_HOME first
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        return PathBuf::from(xdg).join("velor");
    }

    // Fallback to ~/.config/velor
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".config").join("velor");
    }

    // Last resort: try USERPROFILE (Windows)
    if let Ok(home) = std::env::var("USERPROFILE") {
        return PathBuf::from(home).join(".velor");
    }

    // If all else fails, use current directory
    PathBuf::from(".velor")
}

/// Runs the `automations list` subcommand.
///
/// Lists all configured automations with their schedules and status.
/// Uses AutomationCache for dual-location discovery (global + project).
#[tracing::instrument(level = "debug", ret, err, fields(git_root = %git_root.display()))]
pub async fn run_list(
    all: bool,
    home_cfg: FileConfig,
    git_root: PathBuf,
) -> color_eyre::eyre::Result<()> {
    let home_dir = get_xdg_config_home();
    let repo_dir = Some(git_root.join(".velor"));

    let cache = AutomationCache::new(home_dir.clone(), repo_dir);
    let automations = cache.list_all().await?;

    // Filter by enabled unless --all
    let automations: Vec<_> = automations
        .into_iter()
        .filter(|e| all || e.automation.enabled)
        .collect();

    let total_count = automations.len();

    if automations.is_empty() {
        println!(
            "No {}automations configured.",
            if all { "" } else { "enabled " }
        );
        println!("Global: {}/automations/", home_dir.display());
        println!("Project: {}/.velor/automations/", git_root.display());
        return Ok(());
    }

    println!("════════════════════════════════════════");
    println!("📋 Configured Automations");
    println!("════════════════════════════════════════\n");

    for entry in &automations {
        let (source_icon, source_label) = match entry.source {
            velor_automations::AutomationSource::Global => ("🌍", "global"),
            velor_automations::AutomationSource::Project => ("📁", "project"),
            velor_automations::AutomationSource::Legacy => ("⚠️ ", "legacy"),
        };
        println!(
            "{} {} ({})",
            source_icon, entry.automation.name, source_label
        );
        println!("  Description: {}", entry.automation.description);
        println!("  Schedule: {}", entry.automation.schedule_raw);
        println!(
            "  Timezone: {}",
            entry.automation.timezone.iana_name().unwrap_or("unknown")
        );
        println!(
            "  Status: {}",
            if entry.automation.enabled {
                "✅ Enabled"
            } else {
                "❌ Disabled"
            }
        );
        if !entry.automation.enabled && !all {
            println!("  Hint: Use --all to show disabled automations");
        }
        println!();
    }

    println!("Total: {} automation(s)", total_count);

    Ok(())
}

/// Runs the `automations validate` subcommand.
///
/// Validates all automation definitions with detailed checks including
/// prompt resolution and warnings for legacy format.
#[tracing::instrument(level = "debug", ret, err, fields(git_root = %git_root.display()))]
pub async fn run_validate(home_cfg: FileConfig, git_root: PathBuf) -> color_eyre::eyre::Result<()> {
    let home_dir = get_xdg_config_home();
    let repo_dir = Some(git_root.join(".velor"));

    let cache = AutomationCache::new(home_dir.clone(), repo_dir.clone());
    let automations = cache.list_all().await?;

    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    // Create prompt cache once
    let prompt_cache = PromptCache::new(home_dir, repo_dir);

    let total_count = automations.len();
    println!("🔍 Validating {} automation(s)...", total_count);

    for entry in &automations {
        let source_label = match entry.source {
            velor_automations::AutomationSource::Global => "global",
            velor_automations::AutomationSource::Project => "project",
            velor_automations::AutomationSource::Legacy => "legacy",
        };
        println!("  Checking: {} ({})", entry.automation.name, source_label);

        // Check catch_up consistency
        if entry.automation.catch_up != CatchUpPolicy::Skip && entry.automation.max_catch_up == 0 {
            warnings.push((
                format!(
                    "'{}': catch_up enabled but max_catch_up is 0",
                    entry.automation.name
                ),
                entry.path.clone(),
            ));
        }

        // Resolve prompt to check it exists
        match entry
            .automation
            .prompt_source
            .resolve(
                &prompt_cache,
                &get_xdg_config_home(),
                Some(&git_root.join(".velor")),
            )
            .await
        {
            Ok(_) => {}
            Err(e) => errors.push((
                format!("'{}': {}", entry.automation.name, e),
                entry.path.clone(),
            )),
        }

        // Warn about legacy format
        if entry.source == velor_automations::AutomationSource::Legacy {
            warnings.push((
                format!("'{}': Legacy .velor/automations.d/ format detected. Migrate to .velor/automations/", entry.automation.name),
                entry.path.clone(),
            ));
        }
    }

    // Report results
    if errors.is_empty() && warnings.is_empty() {
        println!("✅ All {} automation(s) are valid!", total_count);
    } else {
        for (msg, path) in &warnings {
            println!("⚠️  Warning: {} ({})", msg, path.display());
        }
        for (msg, path) in &errors {
            println!("❌ Error: {} ({})", msg, path.display());
        }
        if !errors.is_empty() {
            return Err(eyre!("Validation failed with {} error(s)", errors.len()));
        }
    }

    Ok(())
}

/// Runs the `automations run` subcommand.
///
/// Executes a single automation immediately, bypassing its schedule.
/// Supports --force flag to run disabled automations.
#[tracing::instrument(level = "debug", ret, err, fields(git_root = %git_root.display(), name = %name))]
pub async fn run_run(
    name: String,
    force: bool,
    home_cfg: FileConfig,
    git_root: PathBuf,
) -> color_eyre::eyre::Result<()> {
    let home_dir = get_xdg_config_home();
    let repo_dir = Some(git_root.join(".velor"));

    let cache = AutomationCache::new(home_dir.clone(), repo_dir);
    let entry = cache.get_by_name(&name).await?;

    if !entry.automation.enabled && !force {
        println!(
            "⚠️  Automation '{}' is disabled. Use --force to run anyway.",
            name
        );
        return Ok(());
    }

    println!("🚀 Running automation '{}'...", name);

    // Load config for variable merging
    let config_path = FileConfig::default_config_path(&git_root);
    let repo_cfg = FileConfig::load_if_exists(&config_path)
        .wrap_err_with(|| format!("failed to load config at {}", config_path.display()))?
        .unwrap_or_default();
    let merged_cfg = FileConfig::merge(home_cfg.clone(), repo_cfg.clone());

    // Resolve binary to absolute path for launchd compatibility
    let binary_path = merged_cfg
        .resolve_binary_path()
        .wrap_err("Failed to resolve binary path")?;

    let auto_cfg = merged_cfg.automations;

    // Merge variables with built-ins (home -> repo -> automation -> built-ins)
    let cwd = std::env::current_dir()?;
    let merged_vars = merge_automation_vars(
        entry.automation.vars.clone(),
        repo_cfg.vars.clone(),
        home_cfg.vars.clone(),
        &git_root,
        &cwd,
    );

    // Resolve prompt
    let prompt_cache = PromptCache::new(home_dir, Some(git_root.join(".velor")));
    let prompt_content = entry
        .automation
        .prompt_source
        .resolve(
            &prompt_cache,
            &get_xdg_config_home(),
            Some(&git_root.join(".velor")),
        )
        .await?;

    // Open state database with automatic migration from legacy automations.db
    let db_path = git_root.join(&auto_cfg.state_db_path);
    let store = AutomationStore::open_with_migration(&db_path).await?;

    // Create runner
    let runner = AutomationRunner::new(
        store.clone(),
        auto_cfg.max_concurrent,
        &git_root,
        binary_path,
        auto_cfg.max_output_bytes,
    );

    // Use current time as scheduled_for (since we're running immediately)
    let scheduled_for = jiff::Timestamp::now();

    // Get the cancel token
    let (_cancel_handler, cancel_token) = crate::cancellation::CancellationHandler::new();

    // Convert AutomationFile to legacy Automation for runner compatibility
    let automation = Automation {
        name: entry.automation.name.clone(),
        description: entry.automation.description.clone(),
        schedule: entry.automation.schedule_raw.clone(),
        timezone: entry
            .automation
            .timezone
            .iana_name()
            .unwrap_or("UTC")
            .to_string(),
        prompt: prompt_content.clone(), // Use resolved prompt content
        enabled: entry.automation.enabled,
        vars: merged_vars,
        catch_up: entry.automation.catch_up,
        max_catch_up: entry.automation.max_catch_up,
        timeout_seconds: entry.automation.timeout_seconds,
        notify_on_success: entry.automation.notify_on_success,
        notify_on_failure: entry.automation.notify_on_failure,
    };

    // Run the automation
    let result = runner
        .run_automation(&automation, scheduled_for, &cancel_token)
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

    let store = AutomationStore::open_with_migration(&db_path).await?;
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
            run.scheduled_for.strftime("%Y-%m-%d %H:%M:%S UTC")
        );
        println!(
            "  Started: {}",
            run.started_at.strftime("%Y-%m-%d %H:%M:%S UTC")
        );
        if let Some(completed) = run.completed_at {
            println!(
                "  Completed: {}",
                completed.strftime("%Y-%m-%d %H:%M:%S UTC")
            );
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

/// Runs the `automations tick` subcommand.
///
/// Executes a single tick of the scheduler - runs all scheduled automations once and exits.
/// This is designed for use with external schedulers like launchd (macOS) or cron (Linux).
///
/// Supports multi-repo automation discovery via ProjectRegistry. If the registry is empty,
/// falls back to legacy single-repo mode using the current directory.
#[tracing::instrument(level = "debug", ret, err)]
pub async fn run_tick(home_cfg: FileConfig, _git_root: PathBuf) -> color_eyre::eyre::Result<()> {
    use fs2::FileExt;

    // Acquire single-instance lock (prefer XDG_RUNTIME_DIR, fall back to XDG_STATE_HOME)
    let lock_dir = dirs::runtime_dir()
        .or_else(dirs::state_dir)
        .unwrap_or_else(|| {
            dirs::home_dir()
                .map(|h| h.join(".local/state"))
                .unwrap_or_else(|| PathBuf::from("/tmp"))
        });
    let lock_dir = lock_dir.join("velor");
    std::fs::create_dir_all(&lock_dir)
        .wrap_err_with(|| format!("Failed to create lock directory: {}", lock_dir.display()))?;

    let lock_path = lock_dir.join("automations.lock");
    let lock_file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&lock_path)
        .wrap_err_with(|| format!("Failed to open lock file: {}", lock_path.display()))?;

    if lock_file.try_lock_exclusive().is_err() {
        tracing::info!("Tick already running, exiting");
        return Ok(());
    }

    // Load registry
    let registry = ProjectRegistry::load()
        .await
        .wrap_err("Failed to load project registry")?;

    // Collect projects to process (use BTreeMap for stable ordering)
    let projects: std::collections::BTreeMap<String, ProjectEntry> =
        if registry.enabled_projects().is_empty() {
            // Backwards compatibility: if empty, use current directory
            let cwd = std::env::current_dir().wrap_err("Failed to get current directory")?;
            let git_root = velor_core::git::discover_git_root(&cwd).unwrap_or(cwd.clone());

            eprintln!("Warning: No projects registered. Running in legacy mode.");
            eprintln!("Run 'vel project add .' to enable multi-repo support.");

            let mut projects = std::collections::BTreeMap::new();
            projects.insert(
                "current".to_string(),
                velor_automations::ProjectEntry {
                    id: "current".to_string(),
                    path: git_root,
                    enabled: true,
                },
            );
            projects
        } else {
            // Convert registry Vec to BTreeMap by ID for stable ordering
            registry
                .list()
                .iter()
                .filter(|p| p.enabled)
                .map(|p| (p.id.clone(), p.clone()))
                .collect()
        };

    let now = jiff::Timestamp::now();
    let project_count = projects.len();
    tracing::info!(
        "Tick started at {} for {} project(s)",
        now.strftime("%Y-%m-%d %H:%M:%S UTC"),
        project_count
    );

    // Track errors across all projects
    let had_errors = Arc::new(AtomicBool::new(false));

    // Load global config once
    let global_cfg_path =
        FileConfig::home_config_path().wrap_err("Failed to determine home config path")?;
    let global_cfg = FileConfig::load_if_exists(&global_cfg_path)?.unwrap_or_default();

    // Process each project (PATH-EXPLICIT, no set_current_dir)
    for (id, project) in &projects {
        tracing::debug!("Processing project: {}", id);

        let project_result = process_project_tick(id, project, &global_cfg, &home_cfg, now).await;

        if let Err(e) = project_result {
            had_errors.store(true, Ordering::Relaxed);
            eprintln!("❌ Error processing project '{}': {}", id, e);
        }
    }

    // Single summary line (only if errors occurred)
    if had_errors.load(Ordering::Relaxed) {
        eprintln!("⚠️  Tick completed with errors");
    } else {
        tracing::info!("Tick completed successfully");
    }

    // Lock file is released when it goes out of scope
    Ok(())
}

/// Processes automations for a single project during tick.
///
/// This function handles all the per-project logic including:
/// - Loading project-specific config
/// - Discovering automations from global and project sources
/// - Running due automations
///
/// Uses path-explicit execution (no `set_current_dir`) to ensure safety in multi-repo scenarios.
#[tracing::instrument(level = "debug", ret, err, skip_all, fields(id = %id))]
async fn process_project_tick(
    id: &str,
    project: &velor_automations::ProjectEntry,
    global_cfg: &FileConfig,
    home_cfg: &FileConfig,
    now: jiff::Timestamp,
) -> color_eyre::eyre::Result<()> {
    let git_root = &project.path;

    // Load repo config
    let config_path = FileConfig::default_config_path(git_root);
    let repo_cfg = FileConfig::load_if_exists(&config_path)
        .wrap_err_with(|| format!("Failed to load config at {}", config_path.display()))?
        .unwrap_or_default();

    // Merge configs: global -> repo
    let merged_cfg = FileConfig::merge(global_cfg.clone(), repo_cfg);

    // Resolve binary to absolute path for launchd compatibility
    let binary_path = merged_cfg
        .resolve_binary_path()
        .wrap_err_with(|| format!("Failed to resolve binary path for project '{}'", id))?;

    let auto_cfg = merged_cfg.automations;

    let home_dir = get_xdg_config_home();
    let repo_dir = Some(git_root.join(".velor"));
    let db_path = git_root.join(&auto_cfg.state_db_path);

    // Load automations using cache for this project
    let cache = AutomationCache::new(home_dir.clone(), repo_dir);
    let automations = cache
        .get()
        .await
        .wrap_err_with(|| format!("Failed to load automations for project '{}'", id))?;

    let enabled_automations: Vec<_> = automations
        .into_values()
        .filter(|e| e.automation.enabled)
        .collect();

    if enabled_automations.is_empty() {
        tracing::debug!("No enabled automations for project '{}'", id);
        return Ok(());
    }

    // Open state database for this project with automatic migration from legacy automations.db
    let store = AutomationStore::open_with_migration(&db_path)
        .await
        .wrap_err_with(|| format!("Failed to open state database at {}", db_path.display()))?;

    // Create runner for this project
    let runner = AutomationRunner::new(
        store.clone(),
        auto_cfg.max_concurrent,
        git_root,
        binary_path,
        auto_cfg.max_output_bytes,
    );

    // Create cancellation handler
    let (_cancel_handler, cancel_token) = crate::cancellation::CancellationHandler::new();

    tracing::debug!(
        "Processing {} automation(s) for project '{}'",
        enabled_automations.len(),
        id
    );

    // Process all automations for this project
    process_automations_tick(
        &enabled_automations,
        &runner,
        &cancel_token,
        home_cfg,
        git_root,
        now,
    )
    .await
    .wrap_err_with(|| format!("Failed to process automations for project '{}'", id))?;

    Ok(())
}

/// Runs the `automations daemon` subcommand.
///
/// Starts the automation daemon which continuously monitors and runs scheduled automations.
/// Uses AutomationCache for dual-location discovery.
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

    let merged_cfg = FileConfig::merge(home_cfg.clone(), repo_cfg);

    // Resolve binary to absolute path for launchd compatibility
    let binary_path = merged_cfg
        .resolve_binary_path()
        .wrap_err("Failed to resolve binary path")?;

    let auto_cfg = merged_cfg.automations;

    let home_dir = get_xdg_config_home();
    let repo_dir = Some(git_root.join(".velor"));
    let db_path = git_root.join(&auto_cfg.state_db_path);

    let tick_interval = std::time::Duration::from_secs(tick_interval_secs.unwrap_or(60));

    println!("🔄 Starting Velor Automations Daemon");
    println!("   Tick interval: {:?}", tick_interval);
    println!("   Global automations: {}/automations/", home_dir.display());
    println!(
        "   Project automations: {}/.velor/automations/",
        git_root.display()
    );
    println!("   State database: {}", db_path.display());
    println!();

    // Load automations using cache
    let cache = AutomationCache::new(home_dir, repo_dir);
    let automations = cache.get().await?;
    let enabled_automations: Vec<_> = automations
        .into_values()
        .filter(|e| e.automation.enabled)
        .collect();

    if enabled_automations.is_empty() {
        println!("⚠️  No enabled automations found. Exiting.");
        return Ok(());
    }

    println!(
        "📋 Loaded {} enabled automation(s):",
        enabled_automations.len()
    );
    for entry in &enabled_automations {
        println!(
            "  - {} ({})",
            entry.automation.name, entry.automation.schedule_raw
        );
    }
    println!();

    // Open state database with automatic migration from legacy automations.db
    let store = AutomationStore::open_with_migration(&db_path).await?;

    // Create runner
    let runner = AutomationRunner::new(
        store,
        auto_cfg.max_concurrent,
        &git_root,
        binary_path,
        auto_cfg.max_output_bytes,
    );

    // Create cancellation handler for graceful shutdown
    let (cancel_handler, cancel_token) = crate::cancellation::CancellationHandler::new();

    println!("✅ Daemon started. Press Ctrl+C twice to stop.");
    println!("════════════════════════════════════════\n");

    // Main daemon loop
    let mut tick_count = 0u64;
    loop {
        // Check for force cancellation (Ctrl+C twice).
        if cancel_handler.is_cancelled() {
            println!("\n🛑 Force stop by user (Ctrl+C twice)");
            break;
        }

        // Check for a stop-after-iteration request (programmatic; the daemon
        // has no TUI `s` key, so this is only set by explicit API calls).
        if cancel_handler.stop_after_iteration_requested() {
            println!("\n⚠️  Stop requested. Stopping after current tick...");
            break;
        }

        tick_count += 1;
        let now = jiff::Timestamp::now();

        println!(
            "🕐 Tick #{} at {}",
            tick_count,
            now.strftime("%Y-%m-%d %H:%M:%S UTC")
        );

        // Process all automations using shared tick logic
        if let Err(e) = process_automations_tick(
            &enabled_automations,
            &runner,
            &cancel_token,
            &home_cfg,
            &git_root,
            now,
        )
        .await
        {
            println!("  ❌ Error processing tick: {}", e);
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

/// Processes a single tick for all enabled automations.
///
/// This is shared logic used by both `run_tick()` (single execution) and `run_daemon()` (continuous loop).
#[tracing::instrument(level = "debug", ret, err, skip_all)]
async fn process_automations_tick(
    enabled_automations: &[AutomationEntry],
    runner: &AutomationRunner,
    cancel_token: &tokio_util::sync::CancellationToken,
    home_cfg: &FileConfig,
    git_root: &Path,
    now: jiff::Timestamp,
) -> color_eyre::eyre::Result<()> {
    for entry in enabled_automations {
        // Get last run time for this automation
        let last_run = get_last_run_time(runner.store(), &entry.automation.name, now).await;

        // Calculate next scheduled time (timezone is already Tz)
        match velor_automations::Scheduler::new(
            &entry.automation.schedule_raw,
            entry.automation.timezone.clone(),
        ) {
            Ok(scheduler) => {
                // Determine which runs to execute based on catch-up policy
                let runs_to_execute = match entry.automation.catch_up {
                    velor_automations::CatchUpPolicy::Skip => {
                        // Only run if the next scheduled time has passed
                        let next_run = scheduler.next_after(last_run);
                        if next_run <= now {
                            vec![next_run]
                        } else {
                            vec![]
                        }
                    }
                    velor_automations::CatchUpPolicy::RunOnce => {
                        // Run once if any runs were missed
                        let missed = scheduler.missed_runs_since(last_run, now, u32::MAX);
                        if missed.is_empty() {
                            vec![]
                        } else {
                            // Run only the first missed schedule
                            vec![missed[0]]
                        }
                    }
                    velor_automations::CatchUpPolicy::RunAll => {
                        // Run all missed schedules up to max_catch_up
                        scheduler.missed_runs_since(last_run, now, entry.automation.max_catch_up)
                    }
                };

                if runs_to_execute.is_empty() {
                    tracing::debug!(
                        "  ⏸️  Skipping '{}' (next run at {})",
                        entry.automation.name,
                        scheduler.next_after(last_run).strftime("%Y-%m-%d %H:%M:%S")
                    );
                    continue;
                }

                println!(
                    "  ▶️  Running '{}' {} time(s) (catch-up: {:?})",
                    entry.automation.name,
                    runs_to_execute.len(),
                    entry.automation.catch_up
                );

                // Resolve prompt for this automation
                let home_dir = get_xdg_config_home();
                let repo_dir = Some(git_root.join(".velor"));
                let prompt_cache = PromptCache::new(home_dir, repo_dir);
                let prompt_content = match entry
                    .automation
                    .prompt_source
                    .resolve(
                        &prompt_cache,
                        &get_xdg_config_home(),
                        Some(&git_root.join(".velor")),
                    )
                    .await
                {
                    Ok(p) => p,
                    Err(e) => {
                        println!("        ❌ Failed to resolve prompt: {}", e);
                        continue;
                    }
                };

                // Load config for variable merging
                let cwd = std::env::current_dir()?;
                let config_path = FileConfig::default_config_path(git_root);
                let repo_cfg = FileConfig::load_if_exists(&config_path)?.unwrap_or_default();
                let merged_cfg = FileConfig::merge(home_cfg.clone(), repo_cfg);
                let merged_vars = merge_automation_vars(
                    entry.automation.vars.clone(),
                    merged_cfg.vars.clone(),
                    home_cfg.vars.clone(),
                    git_root,
                    &cwd,
                );

                // Convert AutomationFile to legacy Automation
                let automation = Automation {
                    name: entry.automation.name.clone(),
                    description: entry.automation.description.clone(),
                    schedule: entry.automation.schedule_raw.clone(),
                    timezone: entry
                        .automation
                        .timezone
                        .iana_name()
                        .unwrap_or("UTC")
                        .to_string(),
                    prompt: prompt_content,
                    enabled: entry.automation.enabled,
                    vars: merged_vars,
                    catch_up: entry.automation.catch_up,
                    max_catch_up: entry.automation.max_catch_up,
                    timeout_seconds: entry.automation.timeout_seconds,
                    notify_on_success: entry.automation.notify_on_success,
                    notify_on_failure: entry.automation.notify_on_failure,
                };

                // Execute each scheduled run
                for scheduled_for in runs_to_execute {
                    println!(
                        "      - Scheduled for {}",
                        scheduled_for.strftime("%H:%M:%S")
                    );

                    match runner
                        .run_automation(&automation, scheduled_for, cancel_token)
                        .await
                    {
                        Ok(result) => {
                            let status_icon = match result.status {
                                velor_automations::AutomationRunStatus::Completed => "✅",
                                velor_automations::AutomationRunStatus::Failed => "❌",
                                velor_automations::AutomationRunStatus::Cancelled => "⚠️",
                                _ => "⏳",
                            };
                            println!("        {} Status: {}", status_icon, result.status.as_str());
                        }
                        Err(e) => {
                            println!("        ❌ Error: {}", e);
                        }
                    }
                }
            }
            Err(e) => {
                println!(
                    "  ❌ Invalid schedule for '{}': {}",
                    entry.automation.name, e
                );
            }
        }
    }

    Ok(())
}

/// Gets the last run time for an automation.
///
/// Returns the time of the most recent run, or the current time if no runs exist.
#[tracing::instrument(level = "trace", ret, fields(automation_name = %automation_name))]
async fn get_last_run_time(
    store: &AutomationStore,
    automation_name: &str,
    default: jiff::Timestamp,
) -> jiff::Timestamp {
    // Query the store for the most recent run
    match store.get_runs(Some(automation_name), 1).await {
        Ok(runs) if !runs.is_empty() => {
            // Return the started_at time of the most recent run
            runs[0].started_at
        }
        _ => default,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_automations_command_exists() {
        // This test ensures the command types compile correctly
        let _ = AutomationsArgs {
            command: AutomationsCommand::List { all: false },
        };
    }

    #[test]
    fn test_get_xdg_config_home() {
        let path = get_xdg_config_home();
        // Just verify it returns a non-empty path
        assert!(!path.as_os_str().is_empty());
    }
}
