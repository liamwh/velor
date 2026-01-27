//! Velor Agent CLI (velor)
//!
//! A command-line interface for running autonomous agents with Claude AI.
//! Supports template-based prompts, variable substitution, and iterative execution.

use chrono::Utc;
use clap::{ArgAction, Args, Parser, Subcommand};
use color_eyre::eyre::WrapErr;
use std::collections::BTreeMap;
use std::path::Path;

mod claude;
mod config;
mod git;
mod retry;
mod template;

use claude::{require_claude_on_path, run_claude};
use config::FileConfig;
use retry::{ConversationHistory, RetryConfig, RetryError};

/// Velor Agent CLI - Run autonomous agents with Claude AI.
#[derive(Debug, Parser)]
#[command(name = "velor", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Run a single Claude invocation
    Once(OnceArgs),

    /// Run multiple iterations until complete or max iterations reached
    Auto(AutoArgs),

    /// Initialise a new repository with a .velor directory and velor.toml config
    Init,
}

/// Arguments common to both subcommands
#[derive(Debug, Args)]
struct CommonArgs {
    /// Override config path (defaults to {git_root}/.velor/agent-cli.toml).
    #[arg(long)]
    config: Option<std::path::PathBuf>,

    /// Prompt name from TOML: [prompts.<name>].
    #[arg(long)]
    prompt: Option<String>,

    /// Inline template string (takes precedence over --prompt).
    #[arg(long)]
    prompt_text: Option<String>,

    /// Permission mode passed to Claude (e.g. acceptEdits).
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

    /// Provide/override template variables (repeatable): --set key=value.
    #[arg(long = "set", value_parser = parse_kv, action = ArgAction::Append)]
    set_vars: Vec<(String, String)>,

    /// Print the final rendered prompt and exit (no Claude call).
    #[arg(long, action = ArgAction::SetTrue)]
    dry_run: bool,
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

/// Default velor.toml configuration template.
const DEFAULT_VELOR_TOML: &str = r#"# Velor Agent CLI Configuration
#
# This config provides defaults and templates for running autonomous Claude agents.
# Customise the values below to suit your project's needs.

[defaults]
# Default permission mode for Claude (accepts edit suggestions automatically)
permission_mode = "acceptEdits"

# Claude binary to use (e.g. "claude", "claude-glm", etc.)
binary = "claude-glm"

# Default progress file for tracking work
progress_path = ".velor/progress.md"

# Default iterations for auto-mode
iterations = 25

# Default prompt name
prompt = "once"

# Completion token that signals plan completion
complete_token = "<promise>COMPLETE</promise>"

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

# Auto-mode prompt for iterative development with auto-implement-plan
auto-implement-plan = """
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
encryption_key_env = "VELO R_CONVERSATIONS_KEY"
retention_days = 0

[plan]
specs_dir = "specs"
plan_max_iterations = 10
openai_api_key_env = "OPENAI_API_KEY"
openai_model = "gpt-4o"
"#;

/// Runs the `init` subcommand to initialise a new repository with velor config.
///
/// Creates the `.velor` directory and a default `velor.toml` configuration file.
/// Automatically migrates existing `agent-cli.toml` to `velor.toml` if found.
fn run_init(git_root: std::path::PathBuf) -> color_eyre::eyre::Result<()> {
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

fn main() -> color_eyre::eyre::Result<()> {
    // Install color-eyre for better error reports
    color_eyre::install()?;

    let cli = Cli::parse();
    let cwd = std::env::current_dir().wrap_err("failed to get current directory")?;
    let git_root = git::discover_git_root(&cwd).wrap_err("failed to discover git root")?;

    // Load home config (optional, may not exist)
    let home_cfg = FileConfig::load_if_exists(&FileConfig::home_config_path()?)
        .wrap_err("failed to load home config")?
        .unwrap_or_default();

    match cli.command {
        Commands::Once(args) => run_once(args, home_cfg, git_root, cwd),
        Commands::Auto(args) => run_auto(args, home_cfg, git_root, cwd),
        Commands::Init => run_init(git_root),
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

/// Runs the `once` subcommand.
fn run_once(
    args: OnceArgs,
    home_cfg: FileConfig,
    git_root: std::path::PathBuf,
    cwd: std::path::PathBuf,
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
    let binary = file_cfg.defaults.binary.clone();

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

    let vars = template::merge_vars(&file_cfg.vars, &common.set_vars, &runtime_vars);

    let template_str = resolve_prompt_template(&common, &file_cfg, &prompt_name)?;

    let rendered = template::render_template(&template_str, &vars)?;

    if common.dry_run {
        println!("{rendered}");
        return Ok(());
    }

    require_claude_on_path(&binary)?;

    println!("Running Claude with prompt '{prompt_name}'...");
    run_claude(&binary, &permission_mode, &rendered, &prompt_name)?;
    Ok(())
}

/// Runs the `auto` subcommand.
fn run_auto(
    args: AutoArgs,
    home_cfg: FileConfig,
    git_root: std::path::PathBuf,
    cwd: std::path::PathBuf,
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
    let binary = file_cfg.defaults.binary.clone();

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
        .unwrap_or(10);

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

    let vars = template::merge_vars(&file_cfg.vars, &common.set_vars, &runtime_vars);

    let template_str = resolve_prompt_template(&common, &file_cfg, &prompt_name)?;

    if common.dry_run {
        // Render once with iteration=1 for dry run
        let mut vars = vars.clone();
        vars.insert("iteration".to_string(), "1".to_string());
        let rendered = template::render_template(&template_str, &vars)?;
        println!("{rendered}");
        return Ok(());
    }

    require_claude_on_path(&binary)?;

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
        max_backoff_ms: base_backoff_ms * 16, // Cap at 16x base (5 retries: 100, 200, 400, 800, 1600ms)
        absolute_timeout_ms,
    };

    println!(
        "🔄 Running auto mode with prompt '{prompt_name}' (max {iterations} iterations, max {max_retries} retries per iteration)..."
    );
    run_auto_loop(
        iterations,
        &binary,
        &permission_mode,
        &template_str,
        &vars,
        &complete_token,
        &prompt_name,
        &retry_config,
    )
}

/// Resolves the prompt template string from CLI args or config.
///
/// # Errors
///
/// Returns an error if the named prompt is not found in the config.
fn resolve_prompt_template(
    args: &CommonArgs,
    cfg: &FileConfig,
    prompt_name: &str,
) -> color_eyre::eyre::Result<String> {
    if let Some(s) = args.prompt_text.clone() {
        return Ok(s);
    }

    let prompt_def = cfg.prompts.get(prompt_name).ok_or_else(|| {
        let available: Vec<String> = cfg.prompts.keys().cloned().collect();
        color_eyre::eyre::eyre!(
            "prompt '{prompt_name}' not found in config. Available prompts: {available:?}"
        )
    })?;

    Ok(prompt_def.template().to_string())
}

/// Builds runtime variables available to templates.
#[allow(clippy::too_many_arguments)]
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

/// Runs the auto-mode loop until completion or max iterations.
///
/// Includes crash resilience with exponential backoff retries and context preservation.
///
/// # Errors
///
/// Returns an error if a Claude invocation fails after all retries.
#[allow(clippy::too_many_arguments)]
#[tracing::instrument(level = "debug", ret)]
fn run_auto_loop(
    iterations: u32,
    binary: &str,
    permission_mode: &str,
    template_str: &str,
    base_vars: &BTreeMap<String, String>,
    complete_token: &str,
    prompt_name: &str,
    retry_config: &RetryConfig,
) -> color_eyre::eyre::Result<()> {
    let mut current_iteration = 1u32;
    let mut history = ConversationHistory::new();

    while current_iteration <= iterations {
        println!("🔁 Iteration {current_iteration}/{iterations}");
        println!("────────────────────────────────────────");

        // Render prompt with context if crash recovery is active
        let prompt_with_context = if !history.is_empty() {
            let context = history.get_previous_context();
            let mut vars = base_vars.clone();
            vars.insert("iteration".to_string(), current_iteration.to_string());
            vars.insert("crash_recovery_context".to_string(), context);
            vars.insert(
                "previous_iteration".to_string(),
                (current_iteration - 1).to_string(),
            );

            template::render_template(template_str, &vars).wrap_err_with(|| {
                format!("failed to render template for iteration {current_iteration}")
            })?
        } else {
            let mut vars = base_vars.clone();
            vars.insert("iteration".to_string(), current_iteration.to_string());

            template::render_template(template_str, &vars).wrap_err_with(|| {
                format!("failed to render template for iteration {current_iteration}")
            })?
        };

        println!("📋 Prompt:\n{prompt_with_context}");
        println!("────────────────────────────────────────");

        // Execute with retry logic
        let retry_result = execute_with_retry(
            binary,
            permission_mode,
            &prompt_with_context,
            prompt_name,
            current_iteration,
            retry_config,
        );

        match retry_result {
            Ok(result) => {
                // Success - clear history and continue
                if !history.is_empty() {
                    println!("✅ Crash recovery successful for iteration {current_iteration}");
                }
                history.clear();

                if result.stdout.contains(complete_token) {
                    println!("✅ PRD complete, exiting.");
                    return Ok(());
                }

                current_iteration += 1;
            }
            Err(RetryError::Permanent(e)) => {
                // Permanent failure - give up
                return Err(color_eyre::eyre::eyre!(
                    "permanent failure on iteration {current_iteration}: {e}"
                ));
            }
            Err(RetryError::TimeoutExceeded(e)) => {
                // Timeout exceeded - give up
                return Err(color_eyre::eyre::eyre!(
                    "timeout exceeded on iteration {current_iteration}: {e}"
                ));
            }
            Err(RetryError::Retryable(e)) => {
                // All retries exhausted - preserve context and retry same iteration
                println!("⚠️  All retries exhausted for iteration {current_iteration}");
                println!("📝 Preserving context for crash recovery...");
                println!("💡 The iteration will be retried with previous context prepended.");

                // Add failed attempt to history for context
                history.add(
                    current_iteration,
                    &prompt_with_context,
                    &format!("<FAILED: {e}>"),
                );

                // Continue loop without incrementing iteration
                // This will retry with context prepended
            }
        }
    }

    Ok(())
}

/// Executes Claude with exponential backoff retry logic.
///
/// # Errors
///
/// Returns `RetryError::Permanent` for non-retryable errors.
/// Returns `RetryError::Retryable` when all retries are exhausted.
/// Returns `RetryError::TimeoutExceeded` when the absolute timeout is exceeded.
#[tracing::instrument(level = "debug", ret, err)]
fn execute_with_retry(
    binary: &str,
    permission_mode: &str,
    prompt: &str,
    prompt_name: &str,
    iteration: u32,
    config: &RetryConfig,
) -> Result<claude::ClaudeRunResult, RetryError> {
    let mut last_error = String::new();
    let retry_start = std::time::Instant::now();

    for attempt in 1..=config.max_retries {
        // Check absolute timeout before each retry attempt
        if retry_start.elapsed().as_millis() > config.absolute_timeout_ms as u128 {
            return Err(RetryError::TimeoutExceeded(format!(
                "absolute timeout of {}ms exceeded",
                config.absolute_timeout_ms
            )));
        }
        if attempt > 1 {
            let delay =
                retry::calculate_backoff(attempt, config.base_backoff_ms, config.max_backoff_ms);
            println!(
                "⏳ Waiting {}ms before retry {}...",
                delay.as_millis(),
                attempt
            );
            std::thread::sleep(delay);
            println!(
                "🔄 Retry attempt {attempt}/{} for iteration {iteration}...",
                config.max_retries
            );
        }

        match run_claude(binary, permission_mode, prompt, prompt_name) {
            Ok(result) => {
                if attempt > 1 {
                    println!("✅ Retry {attempt} succeeded for iteration {iteration}");
                }
                return Ok(result);
            }
            Err(e) => {
                last_error = e.to_string();

                // Classify error as retryable or permanent
                if retry::is_permanent_error(&e) {
                    tracing::error!("permanent error detected on iteration {iteration}: {e}");
                    return Err(RetryError::Permanent(e.to_string()));
                }

                tracing::warn!(
                    "retryable error on iteration {iteration}, attempt {attempt}/{}: {e}",
                    config.max_retries
                );
            }
        }
    }

    Err(RetryError::Retryable(format!(
        "failed after {} retries: {}",
        config.max_retries, last_error
    )))
}
