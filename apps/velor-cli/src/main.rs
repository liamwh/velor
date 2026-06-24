//! Velor Agent CLI (velor)
//!
//! A command-line interface for running autonomous coding agents.
//! Supports template-based prompts, variable substitution, and iterative execution.

use chrono::Utc;
use clap::{ArgAction, Args, Parser, Subcommand};
use color_eyre::eyre::WrapErr;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

// CLI-specific modules (not in velor-core)
mod automations;
mod cancellation;
mod completion;
mod plan;
mod projects;
mod run_logger;
mod serve;
mod streaming_tui;
mod tui;
mod vault;

// Re-export from velor-core
use velor_core as core;

use cancellation::CancellationHandler;

use automations::AutomationsArgs;
use plan::{PlanRunConfig, run_plan_generation};
use projects::ProjectArgs;
use serve::ServeArgs;

use core::{
    agent::{AgentRunner, require_agent_on_path},
    config::FileConfig,
    notification::{
        NotificationPayload, RunStatus, build_notifiers, send_notifications, should_notify,
    },
    prompts::PromptCache,
    retry::{BackoffPolicy, ConversationHistory, RetryConfig, RetryError},
    rules::{
        RulesCache, RulesState, build_follow_up_prompt_delta, get_rules_by_names, inject_rules,
        select_rules,
    },
};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

/// Version string with embedded git info (hash + dirty status + branch).
const VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("VELOR_GIT_HASH"),
    "-",
    env!("VELOR_GIT_DIRTY"),
    ")",
);

/// Velor Agent CLI - Run autonomous coding agents.
#[derive(Debug, Parser)]
#[command(name = "velor", version = VERSION, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Run a single agent invocation
    Once(OnceArgs),

    /// Run multiple iterations until complete or max iterations reached
    Auto(AutoArgs),

    /// Initialise a new repository with a .velor directory and velor.toml config
    Init,

    /// Generate an implementation plan from spec files using OpenAI
    Plan(PlanArgs),

    /// Send a test notification to verify notification configuration
    TestNotification,

    /// Run Telegram listener and execute incoming runner-profile requests
    Serve(ServeArgs),

    /// Manage and run scheduled automations
    Automations(AutomationsArgs),

    /// Manage project registry for multi-repo automation discovery
    Project(ProjectArgs),

    /// Manage encrypted secrets vault
    Vault(vault::VaultArgs),

    /// Generate shell completion script
    Completion(CompletionArgs),

    /// Hidden internal commands for developer tooling
    #[command(hide = true)]
    Internal(InternalArgs),
}

/// Internal command arguments.
#[derive(Debug, Args)]
struct InternalArgs {
    #[command(subcommand)]
    command: InternalCommands,
}

/// Internal commands for development tooling and shell completion.
#[derive(Debug, Subcommand)]
enum InternalCommands {
    /// Output available prompt names for shell completion (newline-delimited).
    ///
    /// Prints one prompt name per line, sorted alphabetically.
    /// Outputs nothing on failure (graceful degradation for shell completion).
    CompletePrompts,
}

/// Completion command arguments.
#[derive(Debug, Args)]
struct CompletionArgs {
    /// Shell type for completion script.
    ///
    /// Supported shells: bash, zsh, fish, elvish, powershell, nushell.
    #[arg(short, long, value_name = "SHELL")]
    shell: completion::Shell,
}

/// Arguments common to both subcommands
#[derive(Debug, Args)]
struct CommonArgs {
    /// Override config path (defaults to {git_root}/.velor/velor.toml).
    #[arg(short, long)]
    config: Option<std::path::PathBuf>,

    /// Prompt name from TOML: [prompts.<name>].
    #[arg(long)]
    prompt: Option<String>,

    /// Inline template string (takes precedence over --prompt).
    #[arg(long)]
    prompt_text: Option<String>,

    /// Permission mode passed to the provider when supported (e.g. acceptEdits).
    #[arg(long)]
    permission_mode: Option<String>,

    /// PRD file path (available as {{prd_path}}).
    #[arg(long)]
    prd_path: Option<String>,

    /// Progress file path (available as {{progress_path}}).
    #[arg(long)]
    progress_path: Option<String>,

    /// Override completion token (defaults to <promise>COMPLETE</promise>).
    #[arg(long)]
    complete_token: Option<String>,

    /// Provide/override template variables. Can also use --key=value directly.
    #[arg(long = "set", value_parser = parse_kv, action = ArgAction::Append)]
    set_vars: Vec<(String, String)>,

    /// Override the agent binary to use (e.g. "claude", "claude-glm", or "codex")
    #[arg(short, long, visible_alias = "bin", global = true)]
    pub binary: Option<String>,

    /// Print the final rendered prompt and exit (no agent call).
    #[arg(long, action = ArgAction::SetTrue)]
    dry_run: bool,

    /// Emit a sanitised invocation diagnostic + replay manifest (JSON) and the
    /// replay command, then run normally. No secrets are printed.
    #[arg(long, action = ArgAction::SetTrue)]
    diagnose: bool,

    /// Append additional instructions to the final rendered prompt.
    #[arg(long)]
    append: Option<String>,
}

/// Arguments for the `once` subcommand
#[derive(Debug, Args)]
struct OnceArgs {
    #[command(flatten)]
    common: CommonArgs,
}

/// Arguments for the `auto` subcommand
#[derive(Debug, Args)]
struct AutoArgs {
    #[command(flatten)]
    common: CommonArgs,

    /// Iterations for auto-mode (available as {{iterations}}).
    #[arg(long)]
    iterations: Option<u32>,

    /// Maximum retry attempts per iteration (default: 5).
    #[arg(long)]
    max_retries: Option<u32>,

    /// Base backoff in milliseconds for exponential backoff (default: 100).
    #[arg(long)]
    base_backoff_ms: Option<u64>,

    /// Disable notifications for this run.
    #[arg(long)]
    no_notify: bool,

    /// Disable the TUI; print agent output to stdout instead.
    #[arg(long)]
    no_tui: bool,
}

/// Arguments for the `plan` subcommand
#[derive(Debug, Args)]
struct PlanArgs {
    /// Override config path (defaults to {git_root}/.velor/velor.toml).
    #[arg(short, long)]
    config: Option<std::path::PathBuf>,

    /// Specs directory path (relative to git root).
    #[arg(long)]
    specs_dir: Option<String>,

    /// Maximum refinement iterations.
    #[arg(long)]
    max_iterations: Option<u32>,

    /// OpenAI API key (overrides environment variable).
    #[arg(long)]
    openai_api_key: Option<String>,

    /// OpenAI model to use.
    #[arg(long)]
    openai_model: Option<String>,

    /// OpenAI base URL (for custom endpoints).
    #[arg(long)]
    openai_base_url: Option<String>,

    /// Print the plan prompt without calling the API.
    #[arg(long, action = ArgAction::SetTrue)]
    dry_run: bool,
}

/// Parses a key=value pair for the `--set` argument.
fn parse_kv(s: &str) -> Result<(String, String), String> {
    let Some((k, v)) = s.split_once('=') else {
        return Err("expected key=value".to_string());
    };
    let k = k.trim();
    let v = v.trim();
    if k.is_empty() {
        return Err("key must not be empty".to_string());
    }
    Ok((k.to_string(), v.to_string()))
}

/// Known clap flags that should NOT be treated as variable overrides.
const KNOWN_FLAGS: &[&str] = &[
    "config",
    "prompt",
    "prompt-text",
    "permission-mode",
    "prd-path",
    "progress-path",
    "complete-token",
    "set",
    "dry-run",
    "iterations",
    "max-retries",
    "base-backoff-ms",
    "no-tui",
    "specs-dir",
    "max-iterations",
    "openai-api-key",
    "openai-model",
    "openai-base-url",
    "binary",
    "bin",
    "cwd",
    "poll-timeout-secs",
    "poll-limit",
    "include-backlog",
    "trigger-prefix",
];

/// Checks if a string is a valid variable name (lowercase, underscores).
fn is_valid_var_name(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        None => false,
        Some(c) if c == '_' || c.is_ascii_lowercase() => {
            chars.all(|c| c == '_' || c.is_ascii_lowercase() || c.is_ascii_digit())
        }
        _ => false,
    }
}

/// Extracts variable overrides from raw CLI arguments.
/// Returns (extracted_overrides, remaining_args_for_clap).
fn extract_var_overrides(
    args: impl IntoIterator<Item = String>,
) -> (Vec<(String, String)>, Vec<String>) {
    let mut overrides = Vec::new();
    let mut remaining = Vec::new();

    for arg in args {
        if let Some(stripped) = arg.strip_prefix("--")
            && let Some((key, value)) = stripped.split_once('=')
            && is_valid_var_name(key)
            && !KNOWN_FLAGS.contains(&key)
        {
            overrides.push((key.to_string(), value.to_string()));
            continue;
        }
        remaining.push(arg);
    }
    (overrides, remaining)
}

/// Merges explicit --set vars with extracted var overrides.
/// Explicit --set takes precedence over direct --key=value.
fn merge_cli_vars(
    explicit_set: &[(String, String)],
    extracted: &[(String, String)],
) -> Vec<(String, String)> {
    let mut result = extracted.to_vec();
    for (k, v) in explicit_set {
        result.retain(|(ek, _)| ek != k);
        result.push((k.clone(), v.clone()));
    }
    result
}

/// Default velor.toml configuration template.
const DEFAULT_VELOR_TOML: &str = r#"# Velor Agent CLI Configuration
#
# This config provides defaults and templates for running autonomous Claude agents.
# Customise the values below to suit your project's needs.

[defaults]
# Agent provider: "claude" or "codex"
provider = "claude"

# Default permission mode for Claude (accepts edit suggestions automatically)
permission_mode = "acceptEdits"

# Claude binary to use (e.g. "claude", "claude-glm", etc.)
binary = "claude-glm"

# Default progress file for tracking work
progress_path = ".velor/progress.md"

# Default iterations for auto-mode
iterations = 1000

# Default prompt name
prompt = "once"

# Completion token that signals plan completion
complete_token = "<promise>COMPLETE</promise>"

# Codex-specific defaults (used when provider = "codex")
[defaults.codex]
full_auto = true
sandbox = "danger-full-access"
skip_git_repo_check = true
progress_cursor = false
# Optional: low|medium|high|xhigh
# model_reasoning_effort = "high"

# Global variables available to all prompt templates
[vars]
# Project metadata - customise these for your project
project_name = "My Project"
repo_name = "my-repo"

# Tech stack - customise for your project
# frontend = "SvelteKit 5 + Skeleton.dev UI"
# backend = "Rust (Axum + utoipa + SeaORM)"
# database = "PostgreSQL 16+ with pgvector"

# Important file paths - customise for your project
# pin = "specs/README.md"  # Pinned information to the start of each agent
# implementation_plan = "docs/plans/my-plan.md"
# rust_rules_file = ".cursor/rules/read-this-if-reading-or-writing-rust.mdc"

# Development commands - customise for your project
# test_rust_cmd = "cargo test"
# check_cmd = "cargo check"
# lint_rust_cmd = "cargo clippy"

# Named prompt templates
[prompts]
# Single-shot prompt for one-off tasks
once = """
1. study {{pin}}.
2. study {{implementation_plan}} and select the most important task to do next. Work ONLY on that ONE task.
3. study {{rust_rules_file}}
IMPORTANT:
- Author property tests, snapshot tests, and/or unit tests (whichever combination you think is best)
- After making the changes run the tests
- When the tests pass and {{check_cmd}} runs without errors or warnings, commit (no push) the changes.
- Update the implementation plan when the task is done.
- Update any other spec files if necessary.

If, while implementing the feature, you notice the plan is complete, output exactly this text: {{complete_token}}
"""

# Auto-mode prompt for iterative development with implement-plan
implement-plan = """
1. study {{pin}}.
2. study {{implementation_plan}} and select the most important task to do next. Work ONLY on that ONE task. This should be the one YOU decide has the highest priority, not necessarily the first thing you see.
3. study {{rust_rules_file}}
4. If, while implementing the feature, you notice the entire plan is complete, with all best-practices followed from end to end, double-check the entire plan's implementation for any possible improvements (don't trust the progress file, double-check everything), and if it's really completely and perfectly implemented, then output exactly this text: "{{complete_token}}". Otherwise, DO NOT OUTPUT {{complete_token}}, but instead just stop.
IMPORTANT:
- Author property tests, snapshot tests, and/or unit tests (whichever combination of the 3 you think is best)
- After making the changes to the files run the tests
- When the task is done, tests pass and {{check_cmd}} runs without errors or warnings, create a detailed commit (don't push) your work (only).
- Update any other spec files if necessary.
- Leave a summary of your progress at {{implementation_plan}}.progress.md.
"""

[conversation_db]
path = ".velor/conversations.db"
encrypt_content = false
encryption_key_env = "VELOR_CONVERSATIONS_KEY"
retention_days = 0

[plan]
specs_dir = "specs"
plan_max_iterations = 10
openai_api_key_env = "OPENAI_API_KEY"
openai_model = "gpt-4o"

[rules]
enabled = false
directory = ".agents/rules"
max_mid_iteration_injections = 2
# Enable intelligent rule selection via ACP
intelligent_selection = false
intelligent_selection_max_rules = 5
"#;

/// Runs the `init` subcommand to initialise a new repository with velor config.
///
/// Creates the `.velor` directory and a default `velor.toml` configuration file.
/// Automatically migrates existing `agent-cli.toml` to `velor.toml` if found.
#[tracing::instrument(level = "debug", ret, err, fields(git_root = %git_root.display()))]
async fn run_init(git_root: std::path::PathBuf) -> color_eyre::eyre::Result<()> {
    let velor_dir = git_root.join(".velor");
    let config_path = velor_dir.join("velor.toml");
    let old_config_path = velor_dir.join("agent-cli.toml");

    // Check if .velor directory already exists
    if velor_dir.exists() {
        // Check if velor.toml already exists
        if config_path.exists() {
            println!("⚠️  .velor/velor.toml already exists. Exiting.");
            return Ok(());
        }

        // Check if agent-cli.toml exists and migrate it
        if old_config_path.exists() {
            println!("📋 Found existing agent-cli.toml, renaming to velor.toml...");
            std::fs::rename(&old_config_path, &config_path).wrap_err_with(|| {
                format!(
                    "failed to rename {} to {}",
                    old_config_path.display(),
                    config_path.display()
                )
            })?;
            println!("📝 Migrated agent-cli.toml to velor.toml");
            println!("✅ Velor initialised successfully!");
            return Ok(());
        }

        // Directory exists but no config - that's fine, we'll create the config
    } else {
        // Create .velor directory
        std::fs::create_dir_all(&velor_dir).wrap_err_with(|| {
            format!(
                "failed to create .velor directory at {}",
                velor_dir.display()
            )
        })?;
        println!("📁 Created .velor directory");
    }

    // Write default velor.toml
    std::fs::write(&config_path, DEFAULT_VELOR_TOML)
        .wrap_err_with(|| format!("failed to write velor.toml to {}", config_path.display()))?;

    println!("📝 Created velor.toml configuration file");
    println!("✅ Velor initialised successfully!");
    println!(
        "📖 Edit {}/.velor/velor.toml to customise for your project.",
        git_root.display()
    );
    Ok(())
}

/// Runs the `project` subcommand to manage the project registry.
///
/// This command provides a way to register, list, enable, and disable
/// projects for multi-repo automation discovery.
#[tracing::instrument(level = "debug", ret, err)]
async fn run_project(args: ProjectArgs) -> color_eyre::eyre::Result<()> {
    projects::run_project(args).await
}

/// Runs the `internal complete-prompts` subcommand for shell completion.
///
/// This handler outputs available prompt names for shell completion.
/// On failure, it silently exits with no output (graceful degradation).
#[tracing::instrument(level = "debug", ret, err)]
async fn run_internal_complete_prompts(
    home_cfg: FileConfig,
    git_root: std::path::PathBuf,
) -> color_eyre::eyre::Result<()> {
    // Attempt to load config and discover prompts
    let result: color_eyre::eyre::Result<Vec<String>> = async {
        // Load git repo config (optional, may not exist)
        let config_path = FileConfig::default_config_path(&git_root);
        let repo_cfg = FileConfig::load_if_exists(&config_path)
            .wrap_err_with(|| format!("failed to load config at {}", config_path.display()))?
            .unwrap_or_default();

        // Merge: home config as base, repo config as overlay
        let file_cfg = FileConfig::merge(home_cfg, repo_cfg);

        core::prompts::discovery::discover_prompt_names(Some(&git_root), &file_cfg)
            .await
            .map_err(|e| color_eyre::eyre::eyre!("prompt discovery failed: {e}"))
    }
    .await;

    // Output: one name per line, or nothing on failure
    // Shell completion expects quiet degradation
    match result {
        Ok(names) => {
            for name in names {
                println!("{name}");
            }
        }
        Err(e) => {
            // Silently exit with no output
            // Shell completion will simply show no options
            tracing::debug!("Completion failed: {e}");
        }
    }

    Ok(())
}

/// Runs the `plan` subcommand to generate an implementation plan from spec files.
///
/// # Errors
///
/// Returns an error if spec files are not found, API key is missing, or the API call fails.
#[tracing::instrument(level = "debug", ret, err, fields(git_root = %git_root.display()))]
async fn run_plan(
    args: PlanArgs,
    home_cfg: FileConfig,
    git_root: std::path::PathBuf,
) -> color_eyre::eyre::Result<()> {
    // Load git repo config (optional, may not exist)
    let config_path = args
        .config
        .unwrap_or_else(|| FileConfig::default_config_path(&git_root));
    let repo_cfg = FileConfig::load_if_exists(&config_path)
        .wrap_err_with(|| format!("failed to load config at {}", config_path.display()))?
        .unwrap_or_default();

    // Merge: home config as base, repo config as overlay
    let file_cfg = FileConfig::merge(home_cfg, repo_cfg);

    let plan_cfg = file_cfg.plan;

    // Resolve specs directory
    let specs_dir = args.specs_dir.unwrap_or_else(|| plan_cfg.specs_dir.clone());

    let specs_dir = git_root.join(&specs_dir);

    // Resolve max iterations
    let max_iterations = args.max_iterations.unwrap_or(plan_cfg.plan_max_iterations);

    // Resolve OpenAI API key
    let api_key_env = plan_cfg.openai_api_key_env.clone();
    let api_key = if let Some(key) = args.openai_api_key {
        key
    } else {
        std::env::var(&api_key_env).wrap_err_with(|| {
            format!(
                "OpenAI API key not found. Set the {} environment variable or use --openai-api-key.",
                api_key_env
            )
        })?
    };

    // Resolve OpenAI model
    let openai_model = args
        .openai_model
        .unwrap_or_else(|| plan_cfg.openai_model.clone());

    // Resolve OpenAI base URL
    let openai_base_url = args.openai_base_url.or(plan_cfg.openai_base_url);

    let config = PlanRunConfig {
        specs_dir,
        max_iterations,
        api_key,
        model: openai_model,
        base_url: openai_base_url,
        dry_run: args.dry_run,
    };

    println!(
        "🔍 Scanning for spec files in {}...",
        config.specs_dir.display()
    );
    println!("🤖 Generating plan with {}...\n", config.model);

    let result = run_plan_generation(&config)?;

    println!("════════════════════════════════════════");
    println!("📋 Generated Plan");
    println!("════════════════════════════════════════\n");
    println!("{}", result.content);
    println!("════════════════════════════════════════");
    println!("✅ Plan generation complete!");

    Ok(())
}

/// Runs the `test-notification` subcommand to verify notification configuration.
#[tracing::instrument(level = "debug", ret, err, fields(git_root = %git_root.display()))]
async fn run_test_notification(
    home_cfg: FileConfig,
    git_root: std::path::PathBuf,
) -> color_eyre::eyre::Result<()> {
    // Load git repo config (optional, may not exist)
    let config_path = FileConfig::default_config_path(&git_root);
    let repo_cfg = FileConfig::load_if_exists(&config_path)
        .wrap_err_with(|| format!("failed to load config at {}", config_path.display()))?
        .unwrap_or_default();

    // Merge: home config as base, repo config as overlay
    let merged_cfg = FileConfig::merge(home_cfg, repo_cfg);

    let notifiers = build_notifiers(&merged_cfg.notifications)?;

    if notifiers.is_empty() {
        println!(
            "No notifications enabled. Configure [notifications.telegram] or [notifications.macos] in velor.toml"
        );
        return Ok(());
    }

    let payload = NotificationPayload {
        mode: "test",
        iterations_completed: 1,
        max_iterations: 1,
        duration: std::time::Duration::from_secs(0),
        status: RunStatus::Completed,
        output_preview: Some("This is a test notification from velor.".to_string()),
        prompt_name: "test-notification".to_string(),
    };

    println!(
        "Sending test notification via: {}",
        notifiers
            .iter()
            .map(|n| n.name())
            .collect::<Vec<_>>()
            .join(", ")
    );

    send_notifications(&notifiers, &payload);

    println!("Test notification sent!");
    Ok(())
}

/// Runs the `automations` subcommand.
///
/// Dispatches to the appropriate automations subcommand handler.
#[tracing::instrument(level = "debug", ret, err, fields(git_root = %git_root.display()))]
async fn run_automations(
    args: AutomationsArgs,
    home_cfg: FileConfig,
    git_root: std::path::PathBuf,
) -> color_eyre::eyre::Result<()> {
    use automations::AutomationsCommand;

    match args.command {
        AutomationsCommand::List { all } => automations::run_list(all, home_cfg, git_root).await,
        AutomationsCommand::Validate => automations::run_validate(home_cfg, git_root).await,
        AutomationsCommand::Run { name, force } => {
            automations::run_run(name, force, home_cfg, git_root).await
        }
        AutomationsCommand::Status { name } => {
            automations::run_status(name, home_cfg, git_root).await
        }
        AutomationsCommand::Tick {} => automations::run_tick(home_cfg, git_root).await,
        AutomationsCommand::Daemon { tick_interval_secs } => {
            automations::run_daemon(tick_interval_secs, home_cfg, git_root).await
        }
        AutomationsCommand::Install { interval } => {
            automations::launchd::run_install(interval).await
        }
        AutomationsCommand::Uninstall => automations::launchd::run_uninstall().await,
        AutomationsCommand::ServiceStatus => automations::launchd::run_status().await,
    }
}

#[tokio::main]
async fn main() -> color_eyre::eyre::Result<()> {
    // Initialize tracing subscriber for logging.
    // Suppress noisy library warnings (tui_markdown HTML-not-supported spam).
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into())
                .add_directive("tui_markdown=error".parse().unwrap_or_default()),
        )
        .init();

    // Install color-eyre for better error reports
    color_eyre::install()?;

    // Create cancellation handler for two-stage shutdown
    let (cancel_handler, _cancel_token) = CancellationHandler::new();

    // Pre-parse to extract --key=value variable overrides
    let raw_args: Vec<String> = std::env::args().collect();
    let (var_overrides, remaining_args) = extract_var_overrides(raw_args);

    // Parse with clap using filtered arguments
    let cli = Cli::parse_from(remaining_args);

    let cwd = std::env::current_dir().wrap_err("failed to get current directory")?;
    let git_root = core::git::discover_git_root(&cwd).wrap_err("failed to discover git root")?;

    // Load .env file from git root (if exists)
    core::git::load_dotenv_from_git_root(&git_root)
        .wrap_err("failed to load .env from git root")?;

    // Load home config (optional, may not exist)
    let home_cfg = FileConfig::load_if_exists(&FileConfig::home_config_path()?)
        .wrap_err("failed to load home config")?
        .unwrap_or_default();

    match cli.command {
        Some(Commands::Once(args)) => {
            run_once(
                args,
                home_cfg,
                git_root,
                cwd,
                &var_overrides,
                cancel_handler,
            )
            .await
        }
        Some(Commands::Auto(args)) => {
            run_auto(
                args,
                home_cfg,
                git_root,
                cwd,
                &var_overrides,
                cancel_handler,
            )
            .await
        }
        Some(Commands::Init) => run_init(git_root).await,
        Some(Commands::Plan(args)) => run_plan(args, home_cfg, git_root).await,
        Some(Commands::TestNotification) => run_test_notification(home_cfg, git_root).await,
        Some(Commands::Serve(args)) => serve::run_serve(args, home_cfg, git_root, cwd).await,
        Some(Commands::Automations(args)) => run_automations(args, home_cfg, git_root).await,
        Some(Commands::Project(args)) => run_project(args).await,
        Some(Commands::Vault(args)) => vault::run(args.command, Some(git_root)).await,
        Some(Commands::Completion(args)) => {
            completion::generate_completion(args.shell)?;
            Ok(())
        }
        Some(Commands::Internal(args)) => match args.command {
            InternalCommands::CompletePrompts => {
                run_internal_complete_prompts(home_cfg, git_root).await
            }
        },
        None => run_interactive_menu(home_cfg, git_root, cwd).await,
    }
}

/// Runs the interactive TUI menu when no subcommand is provided.
#[tracing::instrument(level = "debug", ret, err, fields(git_root = %git_root.display()))]
async fn run_interactive_menu(
    home_cfg: FileConfig,
    git_root: std::path::PathBuf,
    cwd: std::path::PathBuf,
) -> color_eyre::eyre::Result<()> {
    use tui::MenuChoice;

    // Create cancellation handler for interactive menu
    let (cancel_handler, __cancel_token) = CancellationHandler::new();

    let choice = tui::run_menu()?;

    match choice {
        MenuChoice::Once => {
            run_once(
                OnceArgs {
                    common: CommonArgs {
                        config: None,
                        prompt: None,
                        prompt_text: None,
                        permission_mode: None,
                        prd_path: None,
                        progress_path: None,
                        complete_token: None,
                        binary: None,
                        set_vars: vec![],
                        dry_run: false,
                        diagnose: false,
                        append: None,
                    },
                },
                home_cfg,
                git_root,
                cwd,
                &[],
                cancel_handler,
            )
            .await
        }
        MenuChoice::Auto => {
            run_auto(
                AutoArgs {
                    common: CommonArgs {
                        config: None,
                        prompt: None,
                        prompt_text: None,
                        permission_mode: None,
                        prd_path: None,
                        progress_path: None,
                        complete_token: None,
                        binary: None,
                        set_vars: vec![],
                        dry_run: false,
                        diagnose: false,
                        append: None,
                    },
                    iterations: None,
                    max_retries: None,
                    base_backoff_ms: None,
                    no_notify: false,
                    no_tui: false,
                },
                home_cfg,
                git_root,
                cwd,
                &[],
                cancel_handler,
            )
            .await
        }
        MenuChoice::Init => run_init(git_root).await,
        MenuChoice::Plan => {
            run_plan(
                PlanArgs {
                    config: None,
                    specs_dir: None,
                    max_iterations: None,
                    openai_api_key: None,
                    openai_model: None,
                    openai_base_url: None,
                    dry_run: false,
                },
                home_cfg,
                git_root,
            )
            .await
        }
        MenuChoice::Automations => {
            // When selected from TUI, show the list of automations
            automations::run_list(false, home_cfg, git_root).await
        }
        MenuChoice::Quit => Ok(()),
    }
}

/// The execution mode for the CLI (internal use for runtime variables).
#[derive(Debug, Clone, Copy)]
enum RunMode {
    /// Run a single Claude invocation.
    Once,
    /// Run multiple iterations until complete or max iterations reached.
    Auto,
}

/// Finalises a rendered prompt with optional extra user instructions.
///
/// # Arguments
///
/// * `prompt` - The base rendered prompt (possibly with rules already injected)
/// * `append_text` - Optional user text to append as a new section
///
/// # Returns
///
/// The original prompt if append_text is None/empty/whitespace,
/// otherwise the prompt with a new "Additional instructions" section appended.
///
/// # Behaviour
///
/// - Trims surrounding whitespace from append_text
/// - Ignores empty-after-trim values (treats as None)
/// - Preserves internal newlines in multi-line input
/// - Adds a clear section header "## ADDITIONAL INSTRUCTIONS" for legibility in --dry-run
fn finalize_prompt(prompt: &str, append_text: Option<&str>) -> String {
    let Some(text) = append_text.map(str::trim).filter(|s| !s.is_empty()) else {
        return prompt.to_owned();
    };

    format!("{prompt}\n\n## ADDITIONAL INSTRUCTIONS\n\n{text}")
}

/// Resolves the effective agent binary for the selected provider.
///
/// If Codex is selected and the binary was not explicitly overridden, this
/// falls back to `codex` instead of the Claude default binary.
fn resolve_agent_binary(common: &CommonArgs, defaults: &core::config::Defaults) -> String {
    if let Some(binary) = common.binary.clone() {
        return binary;
    }

    if defaults.provider == core::config::AgentProvider::Codex && defaults.binary == "claude-glm" {
        return "codex".to_string();
    }

    defaults.binary.clone()
}

/// Runs the `once` subcommand.
#[tracing::instrument(level = "debug", ret, err, fields(git_root = %git_root.display(), cwd = %cwd.display()))]
async fn run_once(
    args: OnceArgs,
    home_cfg: FileConfig,
    git_root: std::path::PathBuf,
    cwd: std::path::PathBuf,
    extracted_overrides: &[(String, String)],
    cancel_handler: CancellationHandler,
) -> color_eyre::eyre::Result<()> {
    let common = args.common;

    // Load git repo config (optional, may not exist)
    let config_path = common
        .config
        .clone()
        .unwrap_or_else(|| FileConfig::default_config_path(&git_root));
    let repo_cfg = FileConfig::load_if_exists(&config_path)
        .wrap_err_with(|| format!("failed to load config at {}", config_path.display()))?
        .unwrap_or_default();

    // Merge: home config as base, repo config as overlay
    let file_cfg = FileConfig::merge(home_cfg, repo_cfg);

    let permission_mode = common
        .permission_mode
        .clone()
        .or_else(|| file_cfg.defaults.permission_mode.clone())
        .unwrap_or_else(|| "acceptEdits".to_string());
    let binary = resolve_agent_binary(&common, &file_cfg.defaults);

    let prd_path = common
        .prd_path
        .clone()
        .or_else(|| file_cfg.defaults.prd_path.clone())
        .unwrap_or_else(|| "plans/prd.json".to_string());

    let progress_path = common
        .progress_path
        .clone()
        .or_else(|| file_cfg.defaults.progress_path.clone())
        .unwrap_or_else(|| "progress.txt".to_string());

    let prompt_name = common
        .prompt
        .clone()
        .or_else(|| file_cfg.defaults.prompt.clone())
        .unwrap_or_else(|| "once".to_string());

    let complete_token = common
        .complete_token
        .clone()
        .or_else(|| file_cfg.defaults.complete_token.clone())
        .unwrap_or_else(|| "<promise>COMPLETE</promise>".to_string());

    let runtime_vars = build_runtime_vars(
        &git_root,
        &cwd,
        &prd_path,
        &progress_path,
        1, // iterations is always 1 for once mode
        &permission_mode,
        &prompt_name,
        &complete_token,
        &RunMode::Once,
        &config_path,
    );

    let cli_vars = merge_cli_vars(&common.set_vars, extracted_overrides);
    let vars = core::template::merge_vars(&file_cfg.vars, &cli_vars, &runtime_vars);

    // Initialize prompt cache for file-based prompts
    let home_dir = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .wrap_err("failed to determine home directory")?;
    let home_velor_dir = std::path::PathBuf::from(home_dir).join(".velor");
    let repo_velor_dir = git_root.join(".velor");
    let prompt_cache = PromptCache::new(home_velor_dir, Some(repo_velor_dir));

    let template_str =
        resolve_prompt_template(&common, &file_cfg, &prompt_name, &prompt_cache).await?;

    let rendered = core::template::render_template(&template_str, &vars)?;

    // Load and inject rules if enabled
    tracing::info!("Rules enabled in config: {}", file_cfg.rules.enabled);
    let prompt_with_rules = if file_cfg.rules.enabled {
        let rules_cache = RulesCache::new(git_root.clone(), file_cfg.rules.directory.clone());
        tracing::info!(
            "Loading rules from: {}/{}",
            git_root.display(),
            file_cfg.rules.directory
        );
        match rules_cache.get().await {
            Ok(rules_set) => {
                let state = RulesState::new();
                let selected = select_rules(&rules_set, &state);
                inject_rules(&rendered, &selected.rules)
            }
            Err(e) => {
                tracing::warn!("Failed to load rules: {e}. Proceeding without rules.");
                rendered.clone()
            }
        }
    } else {
        rendered.clone()
    };

    // Finalise prompt with user instructions (--append)
    let final_prompt = finalize_prompt(&prompt_with_rules, common.append.as_deref());

    if common.dry_run {
        println!("{final_prompt}");
        return Ok(());
    }

    require_agent_on_path(&binary)?;

    println!("Running {} with prompt '{prompt_name}'...", binary);

    // Create agent runner based on configured protocol
    let runner = AgentRunner::from_config(
        file_cfg.defaults.provider,
        file_cfg.defaults.protocol,
        file_cfg.defaults.acp.clone(),
        file_cfg.defaults.codex.clone(),
    );

    if common.diagnose {
        emit_diagnostic(&binary, &permission_mode, &final_prompt, &cwd)?;
    }

    // Run the agent (no callback for streaming output in this mode)
    runner
        .run(
            &binary,
            &permission_mode,
            &final_prompt,
            &prompt_name,
            &cwd,
            process_timeouts_from_defaults(&file_cfg.defaults),
            CancellationToken::new(),
        )
        .await?;
    Ok(())
}

/// Emits a sanitised invocation diagnostic (JSON) + a replay manifest to stderr,
/// for comparing Velor's invocation against a direct manual run. No secrets.
fn emit_diagnostic(
    binary: &str,
    permission_mode: &str,
    prompt: &str,
    cwd: &Path,
) -> color_eyre::eyre::Result<()> {
    use core::execution_service::diagnostics::{InvocationRecord, ReplayManifest};
    let env = std::collections::BTreeMap::new();
    let record =
        InvocationRecord::derive(binary, prompt.as_bytes(), Some(cwd), None, None, 0, &env);
    eprintln!("{}", serde_json::to_string_pretty(&record)?);
    // Standard Claude stream-json flags (mirrors the Claude adapter); the prompt
    // is written out-of-line so no secret lands in the replay command.
    let args = vec![
        "--permission-mode".to_string(),
        permission_mode.to_string(),
        "--dangerously-skip-permissions".to_string(),
        "-p".to_string(),
        "--verbose".to_string(),
        "--input-format".to_string(),
        "text".to_string(),
        "--output-format".to_string(),
        "stream-json".to_string(),
        "--include-partial-messages".to_string(),
    ];
    let prompt_path = std::env::temp_dir().join(format!(
        "velor-diagnose-{}.txt",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    ));
    let manifest = ReplayManifest::build(
        binary,
        &args,
        Some(cwd),
        prompt.as_bytes(),
        &prompt_path,
        &env,
    )?;
    eprintln!("{}", serde_json::to_string_pretty(&manifest)?);
    eprintln!("# replay:\n{}", manifest.replay_command());
    Ok(())
}

/// Builds per-attempt process deadlines from config (idle/total/grace), with a
/// 5 s termination grace default. Used to bound agent invocations so a hung
/// provider request cannot block forever.
fn process_timeouts_from_defaults(
    d: &core::config::Defaults,
) -> core::execution_service::supervisor::ProcessTimeouts {
    core::execution_service::supervisor::ProcessTimeouts {
        startup: None,
        stdin_write: None,
        idle: d.idle_timeout.map(|t| t.get()),
        total: d.attempt_timeout.map(|t| t.get()),
        termination_grace: d
            .termination_grace
            .map(|t| t.get())
            .unwrap_or_else(|| std::time::Duration::from_secs(5)),
    }
}

/// Runs the `auto` subcommand.
#[tracing::instrument(level = "debug", ret, err, fields(git_root = %git_root.display(), cwd = %cwd.display()))]
async fn run_auto(
    args: AutoArgs,
    home_cfg: FileConfig,
    git_root: std::path::PathBuf,
    cwd: std::path::PathBuf,
    extracted_overrides: &[(String, String)],
    cancel_handler: CancellationHandler,
) -> color_eyre::eyre::Result<()> {
    let common = args.common;

    // Load git repo config (optional, may not exist)
    let config_path = common
        .config
        .clone()
        .unwrap_or_else(|| FileConfig::default_config_path(&git_root));
    let repo_cfg = FileConfig::load_if_exists(&config_path)
        .wrap_err_with(|| format!("failed to load config at {}", config_path.display()))?
        .unwrap_or_default();

    // Merge: home config as base, repo config as overlay
    let file_cfg = FileConfig::merge(home_cfg, repo_cfg);

    let permission_mode = common
        .permission_mode
        .clone()
        .or_else(|| file_cfg.defaults.permission_mode.clone())
        .unwrap_or_else(|| "acceptEdits".to_string());
    let binary = resolve_agent_binary(&common, &file_cfg.defaults);

    let prd_path = common
        .prd_path
        .clone()
        .or_else(|| file_cfg.defaults.prd_path.clone())
        .unwrap_or_else(|| "plans/prd.json".to_string());

    let progress_path = common
        .progress_path
        .clone()
        .or_else(|| file_cfg.defaults.progress_path.clone())
        .unwrap_or_else(|| "progress.txt".to_string());

    let iterations = args
        .iterations
        .or(file_cfg.defaults.iterations)
        .unwrap_or(1000);

    let prompt_name = common
        .prompt
        .clone()
        .or_else(|| file_cfg.defaults.prompt.clone())
        .unwrap_or_else(|| "auto".to_string());

    let complete_token = common
        .complete_token
        .clone()
        .or_else(|| file_cfg.defaults.complete_token.clone())
        .unwrap_or_else(|| "<promise>COMPLETE</promise>".to_string());

    let runtime_vars = build_runtime_vars(
        &git_root,
        &cwd,
        &prd_path,
        &progress_path,
        iterations,
        &permission_mode,
        &prompt_name,
        &complete_token,
        &RunMode::Auto,
        &config_path,
    );

    let cli_vars = merge_cli_vars(&common.set_vars, extracted_overrides);
    let vars = core::template::merge_vars(&file_cfg.vars, &cli_vars, &runtime_vars);

    // Initialize prompt cache for file-based prompts
    let home_dir = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .wrap_err("failed to determine home directory")?;
    let home_velor_dir = std::path::PathBuf::from(home_dir).join(".velor");
    let repo_velor_dir = git_root.join(".velor");
    let prompt_cache = PromptCache::new(home_velor_dir, Some(repo_velor_dir));

    let template_str =
        resolve_prompt_template(&common, &file_cfg, &prompt_name, &prompt_cache).await?;

    if common.dry_run {
        // Render once with iteration=1 for dry run
        let mut vars = vars.clone();
        vars.insert("iteration".to_string(), "1".to_string());
        let rendered = core::template::render_template(&template_str, &vars)?;
        println!("{rendered}");
        return Ok(());
    }

    require_agent_on_path(&binary)?;

    // Load retry configuration
    let max_retries = args
        .max_retries
        .or(Some(file_cfg.defaults.max_retries))
        .unwrap_or(5);

    let base_backoff_ms = args
        .base_backoff_ms
        .or(Some(file_cfg.defaults.base_backoff_ms as u64))
        .unwrap_or(100);

    let absolute_timeout_ms = file_cfg.defaults.absolute_timeout_ms as u64;

    let retry_config = RetryConfig {
        max_retries,
        base_backoff_ms,
        max_backoff_ms: base_backoff_ms * 16, // legacy cap; delays now come from BackoffPolicy
        absolute_timeout_ms,
    };

    // Build the backoff policy from config (human-readable durations), falling
    // back to the built-in multi-second defaults.
    let defaults_d = BackoffPolicy::default();
    let backoff_policy = BackoffPolicy {
        initial: file_cfg
            .defaults
            .initial_backoff
            .map(|d| d.get())
            .unwrap_or(defaults_d.initial),
        max: file_cfg
            .defaults
            .max_backoff
            .map(|d| d.get())
            .unwrap_or(defaults_d.max),
        floor: file_cfg
            .defaults
            .backoff_floor
            .map(|d| d.get())
            .unwrap_or(defaults_d.floor),
        max_attempts: max_retries,
        multiplier: defaults_d.multiplier,
    };
    let attempt_timeouts = process_timeouts_from_defaults(&file_cfg.defaults);

    println!(
        "🔄 Running auto mode with prompt '{prompt_name}' (max {iterations} iterations, max {max_retries} retries per iteration)..."
    );

    let no_notify = args.no_notify;

    // Clone acp config before using it (since AgentRunner takes ownership)
    let acp_config = file_cfg.defaults.acp.clone();

    // Create agent runner based on configured protocol
    let runner = AgentRunner::from_config(
        file_cfg.defaults.provider,
        file_cfg.defaults.protocol,
        acp_config.clone(),
        file_cfg.defaults.codex.clone(),
    );
    tracing::info!("Runner created: {:?}", runner);

    // Load rules if enabled for auto mode
    tracing::info!("Rules enabled in config: {}", file_cfg.rules.enabled);
    let rules_cache = if file_cfg.rules.enabled {
        tracing::info!(
            "Creating rules cache for {}/{}",
            git_root.display(),
            file_cfg.rules.directory
        );
        Some(RulesCache::new(
            git_root.clone(),
            file_cfg.rules.directory.clone(),
        ))
    } else {
        tracing::info!("Rules not enabled, skipping cache creation");
        None
    };
    let rules_set = if let Some(cache) = rules_cache {
        tracing::info!("Fetching rules from cache...");
        match cache.get().await {
            Ok(rules) => {
                tracing::info!(
                    "Rules loaded successfully: {} total rules",
                    rules.total_count()
                );
                Some(rules)
            }
            Err(e) => {
                tracing::warn!("Failed to load rules: {e}. Proceeding without rules.");
                None
            }
        }
    } else {
        tracing::info!("No rules cache, proceeding without rules");
        None
    };
    // Convert Option<RulesSet> to Option<&RulesSet> for passing
    let rules_set_ref = rules_set.as_ref();
    tracing::info!("rules_set_ref.is_some(): {}", rules_set_ref.is_some());

    // Structured JSONL run logger.
    let logger = std::sync::Arc::new(run_logger::RunLogger::new(&git_root, &prompt_name));

    // Streaming TUI: shows agent events with timestamps in an alternate screen.
    // When --no-tui, fall back to plain stdout printing (no TUI task).
    let no_tui = args.no_tui;
    let (tui_tx, tui_rx) = tokio::sync::mpsc::channel::<streaming_tui::TuiMessage>(256);
    // Tell the TUI the log file path (for the 'l' key).
    let _ = tui_tx.try_send(streaming_tui::TuiMessage::SetLogPath(
        logger.path().to_string_lossy().to_string(),
    ));
    let tui_cancel = cancel_handler.token().clone();
    let tui_task = if no_tui {
        None
    } else {
        Some(tokio::spawn(streaming_tui::run_streaming_tui(
            tui_rx, tui_cancel,
        )))
    };
    let tui_ref = if no_tui { None } else { Some(&tui_tx) };

    let result = run_auto_loop(
        &runner,
        &binary,
        &permission_mode,
        &template_str,
        &vars,
        &complete_token,
        &prompt_name,
        &retry_config,
        &backoff_policy,
        attempt_timeouts,
        &cwd,
        iterations,
        &cancel_handler,
        rules_set_ref,
        &git_root,
        &file_cfg.defaults.acp,
        &file_cfg.rules,
        common.append.as_deref(),
        tui_ref,
        &logger,
    )
    .await;

    // Restore terminal (TUI exits when sender is dropped).
    drop(tui_tx);
    if let Some(task) = tui_task {
        let _ = task.await;
    }

    // Print the log file path so the user can find it.
    println!("📄 Log file: {}", logger.path().display());

    // Log final outcome.
    match &result {
        Ok(r) => logger.log_outcome(
            &format!("{:?}", r.status),
            r.iterations_completed,
            r.duration.as_secs(),
        ),
        Err(_) => logger.log_outcome("error", 0, 0),
    }

    // Handle result and send notifications
    match result {
        Ok(auto_result) => {
            // Send notification if enabled — but skip when the user manually
            // cancelled (Ctrl+C); they already know.
            if !no_notify
                && auto_result.status != RunStatus::Cancelled
                && should_notify(auto_result.status, &file_cfg.notifications)
            {
                match build_notifiers(&file_cfg.notifications) {
                    Ok(notifiers) => {
                        if !notifiers.is_empty() {
                            let payload = NotificationPayload {
                                mode: "auto",
                                iterations_completed: auto_result.iterations_completed,
                                max_iterations: auto_result.max_iterations,
                                duration: auto_result.duration,
                                status: auto_result.status,
                                output_preview: Some(auto_result.output.clone()),
                                prompt_name: prompt_name.clone(),
                            };
                            send_notifications(&notifiers, &payload);
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Failed to build notifiers: {e}");
                    }
                }
            }

            // Print final status message
            match auto_result.status {
                RunStatus::Completed => {
                    println!(
                        "✅ Run completed successfully after {} iteration(s).",
                        auto_result.iterations_completed
                    );
                }
                RunStatus::MaxIterationsReached => {
                    println!(
                        "⚠️  Reached maximum iterations ({}) without completion.",
                        auto_result.max_iterations
                    );
                }
                _ => {}
            }

            Ok(())
        }
        Err(e) => {
            // Send failure notification if enabled
            if !no_notify
                && file_cfg.notifications.enabled
                && file_cfg.notifications.notify_on_failure
                && let Ok(notifiers) = build_notifiers(&file_cfg.notifications)
                && !notifiers.is_empty()
            {
                let payload = NotificationPayload {
                    mode: "auto",
                    iterations_completed: 0, // We don't know which iteration failed
                    max_iterations: iterations,
                    duration: std::time::Duration::ZERO,
                    status: RunStatus::Failed,
                    output_preview: Some(e.to_string()),
                    prompt_name: prompt_name.clone(),
                };
                send_notifications(&notifiers, &payload);
            }
            Err(e)
        }
    }
}

/// Resolves the prompt template string from CLI args, config, or file-based prompts.
///
/// # Errors
///
/// Returns an error if the named prompt is not found in the config or file-based prompts.
#[tracing::instrument(level = "debug", ret, err, fields(prompt_name))]
async fn resolve_prompt_template(
    args: &CommonArgs,
    cfg: &FileConfig,
    prompt_name: &str,
    prompt_cache: &PromptCache,
) -> color_eyre::eyre::Result<String> {
    // CLI --prompt-text takes precedence
    if let Some(s) = args.prompt_text.clone() {
        return Ok(s);
    }

    // Check if prompts are enabled in config
    if cfg.prompts_config.enabled {
        // Try to load from file-based prompts first (highest priority for file prompts)
        if let Ok(prompt) = prompt_cache.get_by_name(prompt_name).await {
            tracing::debug!("Loaded prompt '{}' from file-based prompts", prompt_name);
            return Ok(prompt.content);
        }
    }

    // Fall back to config prompts
    let prompt_def = cfg.prompts.get(prompt_name).ok_or_else(|| {
        let available: Vec<String> = cfg.prompts.keys().cloned().collect();
        color_eyre::eyre::eyre!(
            "prompt '{prompt_name}' not found in config or file-based prompts. Available config prompts: {available:?}"
        )
    })?;

    // For File variant, load from prompt cache
    if prompt_def.is_file() {
        let path = prompt_def.template();
        // Extract just the filename from the path (for error messages)
        let filename = Path::new(path)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(path);

        // Try to load from cache
        match prompt_cache.get_by_name(filename).await {
            Ok(prompt) => {
                tracing::debug!("Loaded file-based prompt '{}' from: {}", prompt_name, path);
                return Ok(prompt.content);
            }
            Err(e) => {
                return Err(color_eyre::eyre::eyre!(
                    "file-based prompt '{prompt_name}' not found at path '{path}': {e}"
                ));
            }
        }
    }

    Ok(prompt_def.template().to_string())
}

/// Builds runtime variables available to templates.
#[allow(clippy::too_many_arguments)]
#[tracing::instrument(level = "trace", ret, fields(git_root = %git_root.display(), cwd = %cwd.display(), prompt_name, mode = ?mode))]
fn build_runtime_vars(
    git_root: &Path,
    cwd: &Path,
    prd_path: &str,
    progress_path: &str,
    iterations: u32,
    permission_mode: &str,
    prompt_name: &str,
    complete_token: &str,
    mode: &RunMode,
    config_path: &Path,
) -> Vec<(String, String)> {
    let now = Utc::now().to_rfc3339();
    let mode_str = match mode {
        RunMode::Once => "once",
        RunMode::Auto => "auto",
    };

    vec![
        ("git_root".to_string(), git_root.display().to_string()),
        ("cwd".to_string(), cwd.display().to_string()),
        ("prd_path".to_string(), prd_path.to_string()),
        ("progress_path".to_string(), progress_path.to_string()),
        ("iterations".to_string(), iterations.to_string()),
        ("permission_mode".to_string(), permission_mode.to_string()),
        ("prompt".to_string(), prompt_name.to_string()),
        ("complete_token".to_string(), complete_token.to_string()),
        ("mode".to_string(), mode_str.to_string()),
        ("now".to_string(), now),
        ("config_path".to_string(), config_path.display().to_string()),
    ]
}

/// Runs a single auto-mode iteration with multi-turn rule injection.
///
/// This function implements Phase 3 of the rules system: glob-based rule activation
/// with mid-iteration injection. When the agent reads files matching glob patterns,
/// new rules are injected via follow-up prompts within the same iteration.
///
/// # Arguments
///
/// * `session` - Active ACP session for multi-turn communication
/// * `prompt` - The rendered prompt for this iteration
/// * `rules_set` - All discovered rules
/// * `state` - Persistent rules state across iterations
/// * `config` - Rules configuration (max_mid_iteration_injections)
/// * `iteration` - Current iteration number (for logging)
/// * `intelligent_rules` - Optional intelligently selected rules for this iteration
///
/// # Errors
///
/// Returns an error if a turn fails or ACP communication fails.
#[allow(clippy::too_many_arguments)]
#[tracing::instrument(level = "debug", ret, err)]
async fn run_auto_iteration_with_session(
    session: &mut core::acp::AcpSession,
    prompt: &str,
    rules_set: &core::rules::RulesSet,
    state: &Arc<Mutex<RulesState>>,
    config: &core::config::RulesConfig,
    iteration: u32,
    intelligent_rules: Option<&[core::rules::Rule]>,
    append: Option<&str>,
) -> color_eyre::eyre::Result<String> {
    let mut injections = 0u32;
    let max = config.max_mid_iteration_injections;
    let mut all_output = String::new();
    let mut all_files_read = Vec::new();
    // files_delta will be initialized from the first turn result
    let mut files_delta;

    // Select rules for initial prompt (always_apply, glob-based, and intelligent rules)
    let initial_rules = {
        let state_guard = state.lock().await;
        core::rules::select_rules_with_intelligent(
            rules_set,
            &state_guard,
            intelligent_rules,
            config.intelligent_selection_max_rules,
        )
        // Lock dropped here
    };

    // TURN A: Initial prompt with always-apply rules
    let base_prompt = inject_rules(prompt, &initial_rules.rules);

    // Finalise with user instructions (--append)
    // Applied every iteration, including initial prompt
    let prompt_with_rules = finalize_prompt(&base_prompt, append);

    tracing::debug!(
        "Iteration {}: sending initial prompt with {} rules",
        iteration,
        initial_rules.rules.len()
    );

    let turn_result = session
        .run_turn(
            &prompt_with_rules,
            &format!("iteration_{}_turn_a", iteration),
        )
        .await?;

    all_output.push_str(&turn_result.output);
    all_files_read.extend(turn_result.files_read.clone());
    files_delta = turn_result.files_read;

    // Debug logging to understand what files were read
    tracing::info!(
        "Turn A completed: files_read={}, output_len={}",
        files_delta.len(),
        turn_result.output.len()
    );
    if !files_delta.is_empty() {
        tracing::info!("Files read in Turn A: {:?}", files_delta);
    }

    // Update state with files read
    {
        let mut state_guard = state.lock().await;
        for file in &files_delta {
            state_guard.record_file_read(file.clone());
        }
        // Mark initial rules as injected
        for rule in &initial_rules.rules {
            state_guard.mark_injected(rule.name().to_string());
        }
        // Lock dropped
    }

    // Multi-turn loop for glob-based rule injection
    loop {
        // Check for new glob matches using current delta (with detailed tracing)
        let new_rules_with_files = {
            let state_guard = state.lock().await;
            core::rules::check_new_glob_matches_with_tracing(
                rules_set,
                &files_delta,
                state_guard.injected_rules(),
            )
            // Lock dropped
        };

        let new_rule_names: Vec<_> = new_rules_with_files
            .iter()
            .map(|(name, _files)| name.clone())
            .collect();

        if new_rule_names.is_empty() || injections >= max {
            tracing::debug!(
                "Iteration {}: ending multi-turn (new_rules={}, injections={}/{})",
                iteration,
                new_rule_names.len(),
                injections,
                max
            );
            break;
        }

        // Log detailed information about what triggered each rule
        for (rule_name, files) in &new_rules_with_files {
            tracing::info!(
                "📋 Mid-iteration: Injecting rule '{}' (triggered by files: {})",
                rule_name,
                files.join(", ")
            );
        }

        // Fetch rule contents for formatting
        let new_rules = get_rules_by_names(rules_set, &new_rule_names);

        // Mark new rules as injected
        {
            let mut state_guard = state.lock().await;
            for name in &new_rule_names {
                state_guard.mark_injected(name.clone());
            }
            // Lock dropped
        }

        // TURN B, C, etc.: Follow-up prompt with new rules
        let follow_up = build_follow_up_prompt_delta(&files_delta, &new_rules);
        let turn_name = format!("iteration_{}_turn_{}", iteration, injections + 2);

        let turn_result = session.run_turn(&follow_up, &turn_name).await?;

        all_output.push_str(&turn_result.output);
        all_files_read.extend(turn_result.files_read.clone());
        files_delta = turn_result.files_read;

        // Update state with new files read
        {
            let mut state_guard = state.lock().await;
            for file in &files_delta {
                state_guard.record_file_read(file.clone());
            }
            // Lock dropped
        }

        injections += 1;
    }

    Ok(all_output)
}

/// Runs the auto-mode loop until completion or max iterations.
///
/// Includes crash resilience with exponential backoff retries and context preservation.
/// For ACP mode with rules enabled, supports multi-turn per iteration for glob-based rule injection.
///
/// # Errors
///
/// Returns an error if a Claude invocation fails after all retries.
#[allow(clippy::too_many_arguments)]
#[tracing::instrument(level = "debug", ret, skip(template_str, base_vars, logger))]
async fn run_auto_loop(
    runner: &AgentRunner,
    binary: &str,
    permission_mode: &str,
    template_str: &str,
    base_vars: &BTreeMap<String, String>,
    complete_token: &str,
    prompt_name: &str,
    retry_config: &RetryConfig,
    backoff_policy: &BackoffPolicy,
    timeouts: core::execution_service::supervisor::ProcessTimeouts,
    cwd: &std::path::Path,
    iterations: u32,
    cancel_handler: &CancellationHandler,
    rules_set: Option<&core::rules::RulesSet>,
    git_root: &std::path::Path,
    acp_config: &core::config::AcpConfig,
    rules_config: &core::config::RulesConfig,
    append: Option<&str>,
    tui_tx: Option<&tokio::sync::mpsc::Sender<streaming_tui::TuiMessage>>,
    logger: &std::sync::Arc<run_logger::RunLogger>,
) -> color_eyre::eyre::Result<AutoLoopResult> {
    let start_time = std::time::Instant::now();
    let mut current_iteration = 1u32;
    let mut history = ConversationHistory::new();
    let mut final_output = String::new();
    let mut previous_iteration_completed = false;

    // Create persistent RulesState across iterations for glob-based rule tracking
    let rules_state = Arc::new(Mutex::new(RulesState::new()));
    let use_acp_session = rules_set.is_some() && runner.is_acp() && rules_config.enabled;

    while current_iteration <= iterations {
        // Check for force cancellation at the start of each iteration
        if cancel_handler.is_cancelled() {
            println!("\n🛑 Force quit by user (Ctrl+C twice)");
            return Ok(AutoLoopResult {
                status: RunStatus::Cancelled,
                iterations_completed: current_iteration - 1,
                max_iterations: iterations,
                duration: start_time.elapsed(),
                output: final_output,
            });
        }

        // Check for graceful shutdown request - stop after this iteration completes
        let should_stop_after_this = cancel_handler.graceful_shutdown_requested();

        println!("🔁 Iteration {current_iteration}/{iterations}");
        if should_stop_after_this {
            println!("⚠️  Stopping after this iteration (graceful shutdown requested)");
        }
        println!("────────────────────────────────────────");

        // Render prompt with context if crash recovery is active
        let rendered_prompt = if !history.is_empty() {
            let context = history.get_previous_context();
            let mut vars = base_vars.clone();
            vars.insert("iteration".to_string(), current_iteration.to_string());
            vars.insert("crash_recovery_context".to_string(), context);
            vars.insert(
                "previous_iteration".to_string(),
                (current_iteration - 1).to_string(),
            );

            core::template::render_template(template_str, &vars).wrap_err_with(|| {
                format!("failed to render template for iteration {current_iteration}")
            })?
        } else {
            let mut vars = base_vars.clone();
            vars.insert("iteration".to_string(), current_iteration.to_string());

            core::template::render_template(template_str, &vars).wrap_err_with(|| {
                format!("failed to render template for iteration {current_iteration}")
            })?
        };

        // Execute iteration with appropriate mode
        let iteration_output = if use_acp_session {
            // ACP mode with rules enabled: use multi-turn session
            match run_auto_iteration_acp(
                binary,
                acp_config,
                &rendered_prompt,
                rules_set.unwrap(), // Safe to unwrap because we checked use_acp_session
                &rules_state,
                rules_config,
                current_iteration,
                cwd,
                append,
            )
            .await
            {
                Ok(output) => output,
                Err(e) => {
                    // Treat as retryable error
                    let msg = format!("ACP iteration failed: {e}");
                    println!("⚠️  All retries exhausted for iteration {current_iteration}");
                    println!("📝 Preserving context for crash recovery...");
                    println!("💡 The iteration will be retried with previous context prepended.");

                    history.add(
                        current_iteration,
                        &rendered_prompt,
                        &format!("<FAILED: {msg}>"),
                    );
                    continue; // Retry same iteration
                }
            }
        } else {
            // Subprocess mode or rules disabled: use traditional single-shot
            let prompt_with_rules = if let Some(rules_set) = rules_set {
                let state = rules_state.lock().await;
                let selected = select_rules(rules_set, &state);
                inject_rules(&rendered_prompt, &selected.rules)
            } else {
                rendered_prompt.clone()
            };

            // Finalise prompt with user instructions (--append)
            // Applied every iteration to ensure instructions persist
            let final_prompt = finalize_prompt(&prompt_with_rules, append);

            // Show the prompt: TUI modal (press p) or stdout.
            if let Some(tx) = tui_tx {
                let _ = tx.try_send(streaming_tui::TuiMessage::SetPrompt(final_prompt.clone()));
            } else {
                println!("📋 Prompt:\n{final_prompt}");
                println!("────────────────────────────────────────");
            }

            // Execute with retry logic
            let retry_result = execute_with_retry(
                runner,
                binary,
                permission_mode,
                &final_prompt,
                prompt_name,
                current_iteration,
                retry_config,
                backoff_policy,
                timeouts.clone(),
                cwd,
                cancel_handler.token(),
                tui_tx,
                logger,
            )
            .await;

            match retry_result {
                Ok(result) => result.stdout,
                Err(RetryError::Permanent(e)) => {
                    return Err(color_eyre::eyre::eyre!(
                        "permanent failure on iteration {current_iteration}: {e}"
                    ));
                }
                Err(RetryError::TimeoutExceeded(e)) => {
                    return Err(color_eyre::eyre::eyre!(
                        "timeout exceeded on iteration {current_iteration}: {e}"
                    ));
                }
                Err(RetryError::Cancelled) => {
                    println!("\n🛑 Cancelled by user during iteration {current_iteration}");
                    return Ok(AutoLoopResult {
                        status: RunStatus::Cancelled,
                        iterations_completed: current_iteration - 1,
                        max_iterations: iterations,
                        duration: start_time.elapsed(),
                        output: final_output,
                    });
                }
                Err(RetryError::Retryable(e)) => {
                    println!("⚠️  All retries exhausted for iteration {current_iteration}");
                    println!("📝 Preserving context for crash recovery...");
                    println!("💡 The iteration will be retried with previous context prepended.");

                    history.add(
                        current_iteration,
                        &rendered_prompt,
                        &format!("<FAILED: {e}>"),
                    );
                    continue; // Retry same iteration
                }
            }
        };

        // Store final output for notification preview
        final_output = iteration_output.clone();

        // Success - clear history and continue
        if !history.is_empty() {
            let failure_reasons = history.get_failure_reasons();
            if failure_reasons.is_empty() {
                println!("✅ Crash recovery successful for iteration {current_iteration}");
            } else {
                println!(
                    "✅ Crash recovery successful for iteration {current_iteration} (previous failure: {})",
                    failure_reasons.join(", ")
                );
            }
        }
        history.clear();

        if iteration_output.contains(complete_token) {
            if previous_iteration_completed {
                println!("✅ Completion token seen in consecutive iterations, exiting.");
                return Ok(AutoLoopResult {
                    status: RunStatus::Completed,
                    iterations_completed: current_iteration,
                    max_iterations: iterations,
                    duration: start_time.elapsed(),
                    output: final_output,
                });
            } else {
                println!(
                    "⏳ Completion token found - one more consecutive iteration needed to stop."
                );
                previous_iteration_completed = true;
            }
        } else {
            previous_iteration_completed = false;
        }

        // Check if graceful shutdown was requested - stop after current iteration
        let should_stop_after_this = cancel_handler.graceful_shutdown_requested();
        if should_stop_after_this {
            println!("✅ Graceful shutdown: stopping after iteration {current_iteration}");
            return Ok(AutoLoopResult {
                status: RunStatus::Cancelled,
                iterations_completed: current_iteration,
                max_iterations: iterations,
                duration: start_time.elapsed(),
                output: final_output,
            });
        }

        current_iteration += 1;
    }

    // Ran all iterations without completion token
    Ok(AutoLoopResult {
        status: RunStatus::MaxIterationsReached,
        iterations_completed: current_iteration - 1,
        max_iterations: iterations,
        duration: start_time.elapsed(),
        output: final_output,
    })
}

/// Runs a single auto-mode iteration with ACP session for multi-turn rule injection.
///
/// This is a helper function that creates an ACP session, runs the iteration with
/// multi-turn rule injection, and closes the session. If intelligent selection is
/// enabled, it first creates a separate session to select relevant rules.
///
/// # Arguments
///
/// * `binary` - Path to the ACP adapter binary
/// * `acp_config` - ACP configuration
/// * `prompt` - The rendered prompt for this iteration
/// * `rules_set` - All discovered rules
/// * `state` - Persistent rules state across iterations
/// * `config` - Rules configuration
/// * `iteration` - Current iteration number
/// * `cwd` - Current working directory
///
/// # Errors
///
/// Returns an error if session creation, turn execution, intelligent selection, or session close fails.
#[allow(clippy::too_many_arguments)]
async fn run_auto_iteration_acp(
    binary: &str,
    acp_config: &core::config::AcpConfig,
    prompt: &str,
    rules_set: &core::rules::RulesSet,
    state: &Arc<Mutex<RulesState>>,
    config: &core::config::RulesConfig,
    iteration: u32,
    cwd: &Path,
    append: Option<&str>,
) -> color_eyre::eyre::Result<String> {
    // Step 1: Intelligent rule selection (if enabled)
    let intelligent_rules = if config.intelligent_selection && !rules_set.intelligent.is_empty() {
        tracing::info!(
            "Iteration {}: Running intelligent selection for {} rules",
            iteration,
            rules_set.intelligent.len()
        );

        match select_intelligent_rules_acp(
            binary,
            acp_config,
            &rules_set.intelligent,
            prompt,
            config.intelligent_selection_max_rules,
            cwd,
        )
        .await
        {
            Ok(rules) => {
                tracing::info!(
                    "Iteration {}: Selected {} intelligent rules: {:?}",
                    iteration,
                    rules.len(),
                    rules.iter().map(|r| &r.name).collect::<Vec<_>>()
                );
                Some(rules)
            }
            Err(e) => {
                tracing::warn!(
                    "Iteration {}: Intelligent selection failed: {e}. Proceeding without intelligent rules.",
                    iteration
                );
                None
            }
        }
    } else {
        None
    };

    // Step 2: Create ACP session for this iteration
    let mut session = core::acp::AcpSession::new(binary, acp_config, cwd).await?;

    // Step 3: Run iteration with multi-turn rule injection
    let output = run_auto_iteration_with_session(
        &mut session,
        prompt,
        rules_set,
        state,
        config,
        iteration,
        intelligent_rules.as_deref(),
        append,
    )
    .await?;

    // Step 4: Close the session
    session.close().await?;

    Ok(output)
}

/// Performs intelligent rule selection using a separate ACP session.
///
/// This function creates a short-lived ACP session to ask the agent which
/// rules are relevant for the current task. The selected rules are then
/// returned for use in the main iteration.
///
/// # Arguments
///
/// * `binary` - Path to the ACP adapter binary
/// * `acp_config` - ACP configuration
/// * `intelligent_rules` - Rules that are candidates for intelligent selection
/// * `task_preview` - Preview of the current task
/// * `max_rules` - Maximum number of rules to select
/// * `cwd` - Current working directory
///
/// # Errors
///
/// Returns an error if session creation, prompt sending, or response parsing fails.
#[allow(clippy::too_many_arguments)]
async fn select_intelligent_rules_acp(
    binary: &str,
    acp_config: &core::config::AcpConfig,
    intelligent_rules: &[core::rules::Rule],
    task_preview: &str,
    max_rules: usize,
    cwd: &Path,
) -> color_eyre::eyre::Result<Vec<core::rules::Rule>> {
    use core::rules::{build_intelligent_selection_prompt, parse_intelligent_selection_response};
    use std::collections::HashSet;

    if intelligent_rules.is_empty() {
        return Ok(Vec::new());
    }

    // Build allowed names set for validation
    let allowed_names: HashSet<_> = intelligent_rules.iter().map(|r| r.name.as_str()).collect();

    // Build the selection prompt
    let selection_prompt = build_intelligent_selection_prompt(intelligent_rules, task_preview);

    tracing::debug!("Sending intelligent selection prompt...");

    // Create a short-lived session for selection
    let mut session = core::acp::AcpSession::new(binary, acp_config, cwd).await?;

    // Run the selection prompt
    let turn_result = session
        .run_turn(&selection_prompt, "intelligent_selection")
        .await?;

    // Close the selection session
    session.close().await?;

    // Parse the response
    let selected_names = parse_intelligent_selection_response(&turn_result.output, &allowed_names)
        .wrap_err("Failed to parse intelligent selection response")?;

    // Cap the number of rules and map names to Rule objects
    let selected: Vec<_> = intelligent_rules
        .iter()
        .filter(|r| selected_names.contains(&r.name))
        .take(max_rules)
        .cloned()
        .collect();

    tracing::debug!("Intelligent selection returned {} rules", selected.len());

    Ok(selected)
}

/// Result of running the auto loop.
#[derive(Debug)]
struct AutoLoopResult {
    /// Final status of the run.
    status: RunStatus,
    /// Number of iterations completed.
    iterations_completed: u32,
    /// Maximum iterations allowed.
    max_iterations: u32,
    /// Total duration of the run.
    duration: std::time::Duration,
    /// Final output from Claude.
    output: String,
}

/// Executes Claude with exponential backoff retry logic.
///
/// # Errors
///
/// Returns `RetryError::Permanent` for non-retryable errors.
/// Returns `RetryError::Retryable` when all retries are exhausted.
/// Returns `RetryError::TimeoutExceeded` when the absolute timeout is exceeded.
#[tracing::instrument(level = "debug", ret, err, skip(runner, prompt, logger))]
#[allow(clippy::too_many_arguments)]
async fn execute_with_retry(
    runner: &AgentRunner,
    binary: &str,
    permission_mode: &str,
    prompt: &str,
    prompt_name: &str,
    iteration: u32,
    config: &RetryConfig,
    policy: &BackoffPolicy,
    timeouts: core::execution_service::supervisor::ProcessTimeouts,
    cwd: &std::path::Path,
    cancel_token: &CancellationToken,
    tui_tx: Option<&tokio::sync::mpsc::Sender<streaming_tui::TuiMessage>>,
    logger: &std::sync::Arc<run_logger::RunLogger>,
) -> Result<core::agent::ClaudeRunResult, RetryError> {
    let mut last_error = String::new();
    let mut last_floor: Option<std::time::Duration> = None;
    let retry_start = std::time::Instant::now();
    let mut jitter = core::retry::SystemJitter;

    for attempt in 1..=config.max_retries {
        // Check for cancellation before each attempt
        if cancel_token.is_cancelled() {
            return Err(RetryError::Cancelled);
        }

        // Check absolute timeout before each retry attempt
        if retry_start.elapsed().as_millis() > config.absolute_timeout_ms as u128 {
            return Err(RetryError::TimeoutExceeded(format!(
                "absolute timeout of {}ms exceeded",
                config.absolute_timeout_ms
            )));
        }
        if attempt > 1 {
            let delay = policy.delay(attempt, &mut jitter, last_floor);
            let secs = delay.as_secs_f64();
            println!(
                "⏳ Retrying attempt {attempt}/{} for iteration {iteration} after {secs:.1}s...",
                config.max_retries
            );
            tokio::time::sleep(delay).await;

            // Check again after sleep
            if cancel_token.is_cancelled() {
                return Err(RetryError::Cancelled);
            }

            println!(
                "🔄 Retry attempt {attempt}/{} for iteration {iteration}...",
                config.max_retries
            );
        }

        match runner
            .run_with_events(
                binary,
                permission_mode,
                prompt,
                prompt_name,
                cwd,
                &[],
                timeouts.clone(),
                cancel_token.clone(),
                {
                    let tui_tx = tui_tx.map(|tx| tx.clone());
                    let logger = std::sync::Arc::clone(logger);
                    move |event: core::agent::AgentEvent| {
                        logger.log_agent_event(&event);
                        match &tui_tx {
                            Some(tx) => {
                                if let Some(entry) = streaming_tui::agent_event_to_tui(&event) {
                                    let _ = tx.try_send(streaming_tui::TuiMessage::Entry(entry));
                                }
                            }
                            None => {
                                use std::io::Write;
                                match &event {
                                    core::agent::AgentEvent::TextDelta { text } => {
                                        print!("{text}");
                                        let _ = std::io::stdout().flush();
                                    }
                                    core::agent::AgentEvent::ToolCall { tool, detail, .. } => {
                                        println!("🔧 {tool}: {detail}");
                                    }
                                    core::agent::AgentEvent::ToolResult {
                                        detail, success, ..
                                    } => {
                                        let prefix = if success == &Some(false) {
                                            "⚠️"
                                        } else {
                                            "✅"
                                        };
                                        println!("{prefix} {detail}");
                                    }
                                    core::agent::AgentEvent::Status { message } => {
                                        if !message.starts_with("session: ")
                                            && !message.starts_with("thread started: ")
                                        {
                                            println!("ℹ️ {message}");
                                        }
                                    }
                                    core::agent::AgentEvent::Error { message } => {
                                        eprintln!("❌ {message}");
                                    }
                                    core::agent::AgentEvent::Usage { .. } => {}
                                }
                            }
                        }
                    }
                },
            )
            .await
        {
            Ok(result) => {
                println!(); // newline after streamed text
                if attempt > 1 {
                    println!("✅ Retry {attempt} succeeded for iteration {iteration}");
                }
                return Ok(result);
            }
            Err(e) => {
                last_error = e.to_string();
                // Capture the per-class floor (overload ~5s, rate-limit Retry-After,
                // connection-reset ~2s) so the next backoff honours it.
                last_floor = e.retryability().floor();

                // Typed classification: provider/process errors decide retryability
                // structurally, not by string matching.
                if !e.retryability().is_retryable() {
                    tracing::error!("permanent error detected on iteration {iteration}: {e}");
                    logger.log_permanent_failure(attempt, &e.to_string());
                    return Err(RetryError::Permanent(e.to_string()));
                }

                tracing::warn!(
                    "retryable error on iteration {iteration}, attempt {attempt}/{}: {e}",
                    config.max_retries
                );
                logger.log_retry(
                    attempt,
                    config.max_retries,
                    0.0,
                    &e.to_string(),
                    &format!("{:?}", e.retryability()),
                );
            }
        }
    }

    Err(RetryError::Retryable(format!(
        "failed after {} retries: {}",
        config.max_retries, last_error
    )))
}

#[cfg(test)]
mod var_override_tests {
    use super::*;

    #[test]
    fn test_is_valid_var_name() {
        assert!(is_valid_var_name("foo"));
        assert!(is_valid_var_name("implementation_plan_2"));
        assert!(is_valid_var_name("_private"));
        assert!(is_valid_var_name("a"));
        assert!(is_valid_var_name("abc123"));
        assert!(!is_valid_var_name("Foo")); // uppercase
        assert!(!is_valid_var_name("foo-bar")); // hyphen
        assert!(!is_valid_var_name("123foo")); // starts with digit
        assert!(!is_valid_var_name("")); // empty
        assert!(!is_valid_var_name("fooBar")); // camelCase
    }

    #[test]
    fn test_extract_var_overrides_basic() {
        let args = vec![
            "velor".to_string(),
            "auto".to_string(),
            "--prompt".to_string(),
            "foo".to_string(),
            "--implementation_plan_2=docs/plan.md".to_string(),
        ];
        let (overrides, remaining) = extract_var_overrides(args);

        assert_eq!(
            overrides,
            vec![(
                "implementation_plan_2".to_string(),
                "docs/plan.md".to_string()
            )]
        );
        assert_eq!(remaining, vec!["velor", "auto", "--prompt", "foo"]);
    }

    #[test]
    fn test_known_flags_not_extracted() {
        let args = vec!["velor".to_string(), "--prompt=foo".to_string()];
        let (overrides, remaining) = extract_var_overrides(args);

        assert!(overrides.is_empty());
        assert_eq!(remaining, vec!["velor", "--prompt=foo"]);
    }

    #[test]
    fn test_merge_cli_vars_explicit_wins() {
        let explicit = vec![("key".to_string(), "explicit".to_string())];
        let extracted = vec![("key".to_string(), "extracted".to_string())];
        let merged = merge_cli_vars(&explicit, &extracted);

        assert_eq!(merged, vec![("key".to_string(), "explicit".to_string())]);
    }

    #[test]
    fn test_merge_cli_vars_both_preserved() {
        let explicit = vec![("key1".to_string(), "explicit".to_string())];
        let extracted = vec![("key2".to_string(), "extracted".to_string())];
        let merged = merge_cli_vars(&explicit, &extracted);

        assert_eq!(merged.len(), 2);
        assert!(merged.contains(&("key1".to_string(), "explicit".to_string())));
        assert!(merged.contains(&("key2".to_string(), "extracted".to_string())));
    }

    #[test]
    fn test_extract_multiple_overrides() {
        let args = vec![
            "velor".to_string(),
            "auto".to_string(),
            "--foo=bar".to_string(),
            "--baz=qux".to_string(),
            "--prompt=test".to_string(),
        ];
        let (overrides, remaining) = extract_var_overrides(args);

        assert_eq!(overrides.len(), 2);
        assert!(overrides.contains(&("foo".to_string(), "bar".to_string())));
        assert!(overrides.contains(&("baz".to_string(), "qux".to_string())));
        assert_eq!(remaining, vec!["velor", "auto", "--prompt=test"]);
    }

    #[test]
    fn test_all_known_flags_not_extracted() {
        // Test all known flags are not extracted as variable overrides
        for flag in KNOWN_FLAGS {
            let args = vec!["velor".to_string(), format!("--{flag}=some_value")];
            let (overrides, remaining) = extract_var_overrides(args);

            assert!(
                overrides.is_empty(),
                "Flag {} should not be extracted as override",
                flag
            );
            assert_eq!(remaining.len(), 2);
            assert_eq!(remaining[1], format!("--{flag}=some_value"));
        }
    }
}

#[cfg(test)]
mod var_override_proptests {
    use super::*;
    use proptest::prelude::*;

    /// Generate valid variable names: lowercase letters, underscores, digits (not starting with digit)
    fn valid_var_name_strategy() -> impl Strategy<Value = String> {
        "[a-z_][a-z0-9_]{0,20}"
    }

    /// Generate any string for values
    fn any_value_strategy() -> impl Strategy<Value = String> {
        ".*"
    }

    proptest! {
        #[test]
        fn test_is_valid_var_name_accepts_valid_names(name in valid_var_name_strategy()) {
            prop_assert!(is_valid_var_name(&name));
        }

        #[test]
        fn test_is_valid_var_name_rejects_uppercase(
            prefix in "[a-z_]*",
            upper in "[A-Z]",
            suffix in "[a-z0-9_]*"
        ) {
            let name = format!("{prefix}{upper}{suffix}");
            prop_assert!(!is_valid_var_name(&name));
        }

        #[test]
        fn test_is_valid_var_name_rejects_leading_digit(
            digit in "[0-9]",
            rest in "[a-z0-9_]*"
        ) {
            let name = format!("{digit}{rest}");
            prop_assert!(!is_valid_var_name(&name));
        }

        #[test]
        fn test_is_valid_var_name_rejects_hyphen(
            prefix in "[a-z_]+",
            suffix in "[a-z0-9_]+"
        ) {
            let name = format!("{prefix}-{suffix}");
            prop_assert!(!is_valid_var_name(&name));
        }

        #[test]
        fn test_extract_var_overrides_preserves_non_override_args(
            program in "prog[a-z]{0,5}",
            subcmd in "cmd[a-z]{0,5}",
            flag in "flag[a-z]{0,5}",
            value in "[a-z]{1,5}"
        ) {
            // Skip if flag happens to be a known flag
            prop_assume!(!KNOWN_FLAGS.contains(&flag.as_str()));

            let args = vec![
                program.clone(),
                subcmd.clone(),
                format!("--{flag}={value}"),
            ];
            let (overrides, remaining) = extract_var_overrides(args);

            // Should extract as variable override
            prop_assert_eq!(overrides.len(), 1);
            prop_assert_eq!(&overrides[0], &(flag.clone(), value));

            // Program and subcommand should remain
            prop_assert!(remaining.contains(&program));
            prop_assert!(remaining.contains(&subcmd));
        }

        #[test]
        fn test_merge_cli_vars_explicit_always_wins(
            key in valid_var_name_strategy(),
            explicit_val in any_value_strategy(),
            extracted_val in any_value_strategy()
        ) {
            // Skip if values are the same (would be indistinguishable)
            prop_assume!(explicit_val != extracted_val);

            let explicit = vec![(key.clone(), explicit_val.clone())];
            let extracted = vec![(key.clone(), extracted_val)];
            let merged = merge_cli_vars(&explicit, &extracted);

            prop_assert_eq!(merged.len(), 1);
            prop_assert_eq!(&merged[0], &(key, explicit_val));
        }

        #[test]
        fn test_merge_cli_vars_preserves_unique_keys(
            key1 in valid_var_name_strategy(),
            key2 in valid_var_name_strategy(),
            val1 in any_value_strategy(),
            val2 in any_value_strategy()
        ) {
            // Skip if keys are the same
            prop_assume!(key1 != key2);

            let explicit = vec![(key1.clone(), val1.clone())];
            let extracted = vec![(key2.clone(), val2.clone())];
            let merged = merge_cli_vars(&explicit, &extracted);

            prop_assert_eq!(merged.len(), 2);
            prop_assert!(merged.contains(&(key1, val1)));
            prop_assert!(merged.contains(&(key2, val2)));
        }

        #[test]
        fn test_extract_var_overrides_roundtrip(
            vars in prop::collection::vec(
                (valid_var_name_strategy(), any_value_strategy()),
                0..5
            )
        ) {
            // Build args from vars
            let mut args: Vec<String> = vec!["velor".to_string(), "auto".to_string()];
            for (key, value) in &vars {
                args.push(format!("--{key}={value}"));
            }

            let (overrides, _remaining) = extract_var_overrides(args);

            // All vars should be extracted
            prop_assert_eq!(overrides.len(), vars.len());
            for (key, value) in &vars {
                prop_assert!(overrides.contains(&(key.clone(), value.clone())));
            }
        }
    }
}

#[cfg(test)]
mod finalize_prompt_tests {
    use super::*;

    #[test]
    fn test_finalize_prompt_none_returns_original() {
        let base = "Hello world";
        assert_eq!(finalize_prompt(base, None), base);
    }

    #[test]
    fn test_finalize_prompt_empty_returns_original() {
        let base = "Hello world";
        assert_eq!(finalize_prompt(base, Some("")), base);
        assert_eq!(finalize_prompt(base, Some("   ")), base);
        assert_eq!(finalize_prompt(base, Some("\n\n\t")), base);
    }

    #[test]
    fn test_finalize_prompt_single_line() {
        let base = "Base prompt";
        let result = finalize_prompt(base, Some("extra instruction"));
        assert!(result.contains("Base prompt"));
        assert!(result.contains("## ADDITIONAL INSTRUCTIONS"));
        assert!(result.contains("extra instruction"));
    }

    #[test]
    fn test_finalize_prompt_multiline() {
        let base = "Base prompt";
        let append = "first line\nsecond line\nthird line";
        let result = finalize_prompt(base, Some(append));
        assert!(result.contains("first line"));
        assert!(result.contains("second line"));
        assert!(result.contains("third line"));
    }

    #[test]
    fn test_finalize_prompt_preserves_internal_newlines() {
        let base = "Base prompt";
        let append = "line1\n\nline2";
        let result = finalize_prompt(base, Some(append));
        assert!(result.contains("line1\n\nline2"));
    }
}
