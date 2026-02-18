# Implementation Plan: Auto-Negotiation Mode for `velor plan`

## Summary

Add an auto-negotiation mode to the `velor plan` command where a user-provided plan file is iteratively refined through a review loop between claude-glm (refiner) and OpenAI (reviewer). The loop continues until the reviewer gives a perfection score >= 0.9, then sends a system notification.

---

## Key Changes from Original Plan

Based on extensive feedback, this plan addresses:

1. **Uses OpenAI naming** (not "ChatGPT API key") - reuses existing `openai_api_key_env`, `openai_model`, `openai_base_url` fields
2. **Fully async with tokio** - uses `tokio::fs`, `tokio::process`, `tokio::time::timeout` (no blocking I/O in async context)
3. **Consistent history/scoring** - reviews pushed immediately, final review done at end
4. **First-class outcomes** - `NegotiationOutcome` enum with proper variants, not heuristic-based
5. **Structured JSON from both parties** - OpenAI reviewer and claude-glm refiner both emit structured JSON
6. **Split write flags** - `write_final_plan` and `write_iteration_backups` are independent
7. **Robust JSON parsing** - multi-stage fallback with proper first-{ to last-} extraction
8. **Best-effort notifications** - failures logged as warnings, not fatal
9. **Distinct exit codes** - `2` for needs input, `1` for errors, `0` for success
10. **Configuration validation** - score threshold and max iterations validated at startup
11. **No secrets in CLI flags** - API key from environment variable only

---

## Phase 1: Dependencies

### Add to `Cargo.toml`

```toml
[dependencies]
# ... existing dependencies ...
rig-core = "0.6"       # AI agent framework for OpenAI integration
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }  # Async runtime
notify-rust = "4"      # Cross-platform system notifications
```

**Note**: Using `rig-core` (formerly `rig`) provides a cleaner async API for OpenAI integration with built-in structured output support.

---

## Phase 2: Configuration (Minimal Changes)

### `src/config.rs` - Extend `PlanConfig` struct (lines 40-70)

The existing `PlanConfig` already has the OpenAI fields we need (`openai_api_key_env`, `openai_model`, `openai_base_url`). We only need to add negotiation-specific fields:

```rust
/// Configuration for the plan subcommand.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct PlanConfig {
    /// Directory for spec files (relative to git root).
    pub specs_dir: String,

    /// Maximum review iterations.
    pub plan_max_iterations: u32,

    /// Environment variable name for OpenAI API key.
    pub openai_api_key_env: String,

    /// OpenAI model to use for reviews.
    pub openai_model: String,

    /// OpenAI base URL (optional, for custom endpoints).
    pub openai_base_url: Option<String>,

    // === NEGOTIATION MODE FIELDS (NEW) ===
    /// Perfection score threshold (0.0 to 1.0) to stop negotiation.
    pub negotiation_score_threshold: f64,

    /// Maximum negotiation iterations (prevents infinite loops).
    pub negotiation_max_iterations: u32,

    /// Claude binary to use for plan refinement.
    pub claude_binary: String,

    /// Write final plan back to original file on completion.
    pub negotiation_write_final_plan: bool,

    /// Write per-iteration backup files.
    pub negotiation_write_iteration_backups: bool,
}
```

### Update `PlanConfig::default()` (lines 60-70)

```rust
impl Default for PlanConfig {
    fn default() -> Self {
        Self {
            specs_dir: "specs".to_string(),
            plan_max_iterations: 10,
            openai_api_key_env: "OPENAI_API_KEY".to_string(),
            openai_model: "gpt-4o".to_string(),
            openai_base_url: None,
            // New negotiation defaults
            negotiation_score_threshold: 0.9,
            negotiation_max_iterations: 5,
            claude_binary: "claude-glm".to_string(),
            negotiation_write_final_plan: true,
            negotiation_write_iteration_backups: true,
        }
    }
}
```

---

## Phase 3: CLI Arguments

### Modify `src/main.rs` - `PlanArgs` struct (around line 114)

```rust
/// Arguments for the `plan` subcommand
#[derive(Debug, Args)]
struct PlanArgs {
    /// Override config path (defaults to {git_root}/.velor/velor.toml).
    #[arg(long)]
    config: Option<std::path::PathBuf>,

    /// Specs directory path (relative to git root).
    #[arg(long)]
    specs_dir: Option<String>,

    /// Maximum refinement iterations.
    #[arg(long)]
    max_iterations: Option<u32>,

    /// OpenAI model to use (overrides config).
    #[arg(long)]
    openai_model: Option<String>,

    /// OpenAI base URL (for custom endpoints, overrides config).
    #[arg(long)]
    openai_base_url: Option<String>,

    /// Print the plan prompt without calling the API.
    #[arg(long, action = ArgAction::SetTrue)]
    dry_run: bool,

    // === NEGOTIATION MODE FLAGS (NEW) ===
    /// Path to plan file to refine (enables negotiation mode).
    #[arg(long, alias = "file")]
    plan_file: Option<PathBuf>,

    /// Enable review-based negotiation loop (requires --plan-file).
    #[arg(long, alias = "review")]
    negotiate: bool,

    /// Perfection score threshold (0.0 to 1.0, overrides config).
    #[arg(long)]
    score_threshold: Option<f64>,

    /// Maximum negotiation iterations (overrides config).
    #[arg(long)]
    negotiation_max_iterations: Option<u32>,
}
```

**Note**: No `--openai-api-key` flag is added - secrets should come from environment variables only (per security best practices).

---

## Phase 4: Negotiation Module

### Create `src/negotiation.rs`

```rust
//! Plan negotiation module.
//!
//! Implements iterative plan refinement through review loop between OpenAI (reviewer)
//! and claude-glm (refiner). Continues until score threshold is met or max iterations.

use color_eyre::eyre::{Result, WrapErr};
use rig::providers::openai;
use serde::Deserialize;
use std::path::PathBuf;
use tracing::{debug, instrument, warn};

/// Configuration for the negotiation loop.
#[derive(Debug, Clone)]
pub struct NegotiationConfig {
    pub plan_file_path: PathBuf,
    pub openai_api_key: String,
    pub openai_model: String,
    pub openai_base_url: Option<String>,
    pub score_threshold: f64,
    pub max_iterations: u32,
    pub claude_binary: String,
    pub write_final_plan: bool,
    pub write_iteration_backups: bool,
}

/// Result from a single OpenAI review.
#[derive(Debug, Clone, Deserialize)]
pub struct ReviewResult {
    pub score: f64,
    pub feedback: String,
    #[serde(default)]
    pub blocking_issues: Vec<String>,
    #[serde(default)]
    pub suggestions: Vec<String>,
}

/// Structured response from claude-glm refiner.
#[derive(Debug, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum ClaudeRefineResponse {
    Refined { plan: String },
    NeedsUserInput { questions: Vec<String>, plan_so_far: String },
}

/// Outcome of the negotiation loop.
#[derive(Debug)]
pub enum NegotiationOutcome {
    Done {
        plan: String,
        iterations: u32,
        final_score: f64,
        review_history: Vec<ReviewResult>,
    },
    NeedsUserInput {
        plan: String,
        iterations: u32,
        last_review: ReviewResult,
        questions: String,
    },
}

/// Main negotiation loop (async).
///
/// # Errors
///
/// Returns an error if API calls fail, files cannot be read/written, or
/// the reviewer returns invalid responses.
pub async fn run_negotiation_loop(config: &NegotiationConfig) -> Result<NegotiationOutcome> {
    let mut current_plan = tokio::fs::read_to_string(&config.plan_file_path)
        .await
        .wrap_err_with(|| format!("failed to read plan file: {}", config.plan_file_path.display()))?;

    let mut history: Vec<ReviewResult> = Vec::new();

    for iteration in 1..=config.max_iterations {
        let review = review_plan_with_openai(config, &current_plan).await?;
        history.push(review.clone());

        print_iteration_status(iteration, config.max_iterations, &review);

        if review.score >= config.score_threshold {
            if config.write_final_plan {
                tokio::fs::write(&config.plan_file_path, &current_plan)
                    .await
                    .wrap_err("failed to write refined plan back to file")?;
            }

            send_system_notification(
                "Plan Negotiation Complete",
                &format!("Completed {iteration} iterations with score: {:.2}", review.score),
            );

            return Ok(NegotiationOutcome::Done {
                plan: current_plan,
                iterations: iteration,
                final_score: review.score,
                review_history: history,
            });
        }

        let refinement = refine_plan_with_claude(config, &current_plan, &review).await?;

        match refinement {
            ClaudeRefineResponse::Refined { plan } => {
                current_plan = plan;

                if config.write_iteration_backups {
                    let backup_path = iteration_backup_path(&config.plan_file_path, iteration);
                    tokio::fs::write(&backup_path, &current_plan)
                        .await
                        .wrap_err_with(|| format!("failed to write iteration backup: {}", backup_path.display()))?;
                }
            }
            ClaudeRefineResponse::NeedsUserInput { questions, plan_so_far } => {
                send_system_notification(
                    "Plan Negotiation Needs Input",
                    &format!("Stopped at iteration {iteration} (review score: {:.2})", review.score),
                );

                let questions_text = if questions.is_empty() {
                    "claude-glm requested input but provided no specific questions.".to_string()
                } else {
                    questions.iter().enumerate().map(|(i, q)| format!("{}. {}\n", i + 1, q)).collect()
                };

                return Ok(NegotiationOutcome::NeedsUserInput {
                    plan: plan_so_far,
                    iterations: iteration,
                    last_review: review,
                    questions: questions_text,
                });
            }
        }
    }

    // Final review to ensure score corresponds to the final plan text.
    let final_review = review_plan_with_openai(config, &current_plan).await?;
    history.push(final_review.clone());

    if config.write_final_plan {
        tokio::fs::write(&config.plan_file_path, &current_plan)
            .await
            .wrap_err("failed to write refined plan back to file")?;
    }

    send_system_notification(
        "Plan Negotiation Complete",
        &format!(
            "Reached max iterations ({}). Final score: {:.2}",
            config.max_iterations, final_review.score
        ),
    );

    Ok(NegotiationOutcome::Done {
        plan: current_plan,
        iterations: config.max_iterations,
        final_score: final_review.score,
        review_history: history,
    })
}

/// Reviews a plan using OpenAI API via rig.
async fn review_plan_with_openai(config: &NegotiationConfig, plan: &str) -> Result<ReviewResult> {
    let prompt = build_review_prompt(plan);

    // Build OpenAI client with optional custom base URL
    let client = if let Some(base_url) = &config.openai_base_url {
        openai::Client::new(&config.openai_api_key).with_base_url(base_url)
    } else {
        openai::Client::new(&config.openai_api_key)
    };

    // Build agent with deterministic settings for consistent JSON output
    let agent = client
        .agent(&config.openai_model)
        .temperature(0.0)
        .prompt("You are an expert technical reviewer. Always respond with valid JSON containing all required fields.")
        .build();

    // Send prompt and get response
    let response = agent
        .prompt(&prompt)
        .await
        .wrap_err("failed to get response from OpenAI")?;

    parse_review_response(&response)
}

/// Parses OpenAI response with robust JSON handling.
fn parse_review_response(response_text: &str) -> Result<ReviewResult> {
    // First try: direct parse (response is clean JSON)
    let parsed = serde_json::from_str::<ReviewResult>(response_text);

    if let Ok(review) = parsed {
        debug!(score = review.score, feedback_len = review.feedback.len(), "parsed review (direct)");
        return Ok(review);
    }

    // Second try: strip code fences and parse
    let stripped = response_text
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();

    if let Ok(review) = serde_json::from_str::<ReviewResult>(stripped) {
        debug!(score = review.score, feedback_len = review.feedback.len(), "parsed review (stripped fences)");
        return Ok(review);
    }

    // Third try: find first { and last } for "best effort" extraction
    if let (Some(first_brace), Some(last_brace)) = (stripped.find('{'), stripped.rfind('}')) {
        if last_brace > first_brace {
            let extracted = &stripped[first_brace..=last_brace];
            if let Ok(review) = serde_json::from_str::<ReviewResult>(extracted) {
                debug!(score = review.score, feedback_len = review.feedback.len(), "parsed review (extracted)");
                return Ok(review);
            }
        }
    }

    // All attempts failed
    Err(color_eyre::eyre::eyre!(
        "failed to parse review response as JSON. Response: {}",
        response_text.chars().take(500).collect::<String>()
    ))
}

/// Refines a plan using claude-glm with structured JSON output.
async fn refine_plan_with_claude(
    config: &NegotiationConfig,
    plan: &str,
    review: &ReviewResult,
) -> Result<ClaudeRefineResponse> {
    let prompt = build_refinement_prompt(plan, review);

    // Use tokio::process for async subprocess handling
    let mut child = tokio::process::Command::new(&config.claude_binary)
        .args(["plan", "--permission-mode", "acceptEdits"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;

    // Write prompt to stdin
    {
        let mut stdin = child.stdin.take().ok_or_else(|| {
            color_eyre::eyre::eyre!("failed to get stdin handle for claude-glm")
        })?;
        tokio::io::AsyncWriteExt::write_all(&mut stdin, prompt.as_bytes()).await?;
    }

    // Wait for output with timeout (5 minutes)
    let timeout_duration = std::time::Duration::from_secs(300);
    let output = tokio::time::timeout(timeout_duration, child.wait_with_output())
        .await
        .map_err(|_| color_eyre::eyre::eyre!("claude-glm timed out after 5 minutes"))?
        .wrap_err("failed to wait for claude-glm")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(color_eyre::eyre::eyre!("claude-glm failed: {}", stderr));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Parse structured JSON response from claude-glm
    parse_claude_response(&stdout)
}

/// Parses structured JSON response from claude-glm.
fn parse_claude_response(response: &str) -> Result<ClaudeRefineResponse> {
    // Try direct parse first
    if let Ok(parsed) = serde_json::from_str::<ClaudeRefineResponse>(response) {
        return Ok(parsed);
    }

    // Strip code fences and try again
    let stripped = response
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();

    // Find JSON object bounds
    let json_str = if let (Some(first), Some(last)) = (stripped.find('{'), stripped.rfind('}')) {
        &stripped[first..=last]
    } else {
        stripped
    };

    serde_json::from_str::<ClaudeRefineResponse>(json_str).wrap_err_with(|| {
        format!("failed to parse claude-glm response. Response: {}", response.chars().take(500).collect::<String>())
    })
}

/// Sends a system notification using notify-rust (best-effort).
///
/// Notification failures are logged but do not fail the operation.
fn send_system_notification(title: &str, body: &str) {
    if let Err(e) = notify_rust::Notification::new()
        .summary(title)
        .body(body)
        .show()
    {
        warn!("failed to send system notification: {}", e);
        // Continue - notification failure is non-fatal
    }
}

fn build_review_prompt(plan: &str) -> String {
    format!(
        r#"Review the following implementation plan and provide:

1. A perfection score from 0.0 to 1.0:
   - 0.9+: Excellent, ready to implement
   - 0.7-0.9: Good with minor improvements needed
   - 0.5-0.7: Adequate but needs significant improvements
   - <0.5: Major issues, needs substantial revision

2. Blocking issues: Specific problems that MUST be addressed before implementation
   (e.g., missing critical steps, incorrect dependencies, security issues)

3. Non-blocking suggestions: Improvements that would enhance the plan

4. Detailed feedback explaining the score

Rubric:
- Completeness: All phases, tasks, dependencies, and verification steps included
- Correctness: Accurate crates, APIs, file paths, and technical details
- Safety/Security: Proper secrets handling, failure modes considered
- Clarity: Steps are executable and unambiguous

Respond ONLY with valid JSON (all fields required):
{{"score": 0.0-1.0, "feedback": "string", "blocking_issues": ["string1", "string2"], "suggestions": ["string1", "string2"]}}

Plan to review:
{}"#,
        plan
    )
}

fn build_refinement_prompt(plan: &str, review: &ReviewResult) -> String {
    let mut prompt = format!(
        "Please refine the following plan based on the reviewer's feedback.\n\n\
        Keep the good parts and address the concerns raised.\n\n\
        Current plan:\n{}\n\n\
        Reviewer feedback (score: {:.2}):\n{}\n",
        plan, review.score, review.feedback
    );

    if !review.blocking_issues.is_empty() {
        prompt.push_str("\nBlocking issues to address:\n");
        for (i, issue) in review.blocking_issues.iter().enumerate() {
            prompt.push_str(&format!("{}. {}\n", i + 1, issue));
        }
    }

    if !review.suggestions.is_empty() {
        prompt.push_str("\nSuggestions to consider:\n");
        for (i, suggestion) in review.suggestions.iter().enumerate() {
            prompt.push_str(&format!("{}. {}\n", i + 1, suggestion));
        }
    }

    prompt.push_str(
        "\n\nIMPORTANT: Output ONLY valid JSON with this exact structure:\n\
        {\"status\": \"refined\", \"plan\": \"your refined plan in markdown\"}\n\n\
        If you need user input before proceeding, use:\n\
        {\"status\": \"needs_user_input\", \"questions\": [\"question1\", \"question2\"], \"plan_so_far\": \"work so far\"}",
    );

    prompt
}

fn print_iteration_status(iteration: u32, max: u32, review: &ReviewResult) {
    println!("\n────────────────────────────────────────");
    println!("🔄 Negotiation iteration {}/{max}", iteration);
    println!("📊 Perfection score: {:.2}", review.score);
    if !review.blocking_issues.is_empty() {
        println!("⚠️  Blocking issues: {}", review.blocking_issues.len());
    }
    println!("💬 Feedback: {}", truncate(&review.feedback, 300));
}

fn iteration_backup_path(plan_path: &PathBuf, iteration: u32) -> PathBuf {
    let file_name = plan_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("plan.md");
    let backup_name = format!("{file_name}.negotiated.iter-{iteration:02}.md");

    let mut out = plan_path.clone();
    out.set_file_name(backup_name);
    out
}

fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        return s.to_string();
    }
    let cut = max_len.saturating_sub(3);
    format!("{}...", &s[..cut])
}

/// Validates negotiation configuration before starting the loop.
fn validate_negotiation_config(config: &NegotiationConfig) -> Result<()> {
    if !(0.0..=1.0).contains(&config.score_threshold) {
        return Err(color_eyre::eyre::eyre!(
            "score_threshold must be between 0.0 and 1.0, got {}",
            config.score_threshold
        ));
    }

    if config.max_iterations < 1 {
        return Err(color_eyre::eyre::eyre!(
            "max_iterations must be at least 1, got {}",
            config.max_iterations
        ));
    }

    if config.claude_binary.is_empty() {
        return Err(color_eyre::eyre::eyre!("claude_binary cannot be empty"));
    }

    Ok(())
}
```

---

## Phase 5: Integration

### Modify `src/main.rs`

1. Add module declaration (line ~17):
```rust
mod negotiation;
```

2. Update `run_plan()` function (lines 312-385):

```rust
/// Runs the `plan` subcommand.
#[tracing::instrument(level = "debug", ret, err, fields(git_root = %git_root.display()))]
fn run_plan(
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

    // Check if negotiation mode is requested
    if args.negotiate {
        let plan_file = args.plan_file.ok_or_else(|| {
            color_eyre::eyre::eyre!("--plan-file is required when using --negotiate")
        })?;

        // Resolve OpenAI API key from environment (no CLI flag for security)
        let api_key = std::env::var(&plan_cfg.openai_api_key_env).wrap_err_with(|| {
            format!(
                "OpenAI API key not found. Set the {} environment variable.",
                plan_cfg.openai_api_key_env
            )
        })?;

        let config = negotiation::NegotiationConfig {
            plan_file_path: plan_file,
            openai_api_key: api_key,
            openai_model: args.openai_model.unwrap_or_else(|| plan_cfg.openai_model.clone()),
            openai_base_url: args.openai_base_url.or(plan_cfg.openai_base_url),
            score_threshold: args.score_threshold.unwrap_or(plan_cfg.negotiation_score_threshold),
            max_iterations: args.negotiation_max_iterations.unwrap_or(plan_cfg.negotiation_max_iterations),
            claude_binary: plan_cfg.claude_binary.clone(),
            write_final_plan: plan_cfg.negotiation_write_final_plan,
            write_iteration_backups: plan_cfg.negotiation_write_iteration_backups,
        };

        // Validate configuration
        validate_negotiation_config(&config)?;

        // Create tokio runtime for async negotiation
        let rt = tokio::runtime::Runtime::new()
            .wrap_err("failed to create tokio runtime")?;

        let outcome = rt.block_on(negotiation::run_negotiation_loop(&config))?;

        match outcome {
            negotiation::NegotiationOutcome::Done { plan, iterations, final_score, .. } => {
                println!("\n════════════════════════════════════════");
                println!("📋 Final Refined Plan");
                println!("════════════════════════════════════════\n");
                println!("{}", plan);
                println!("════════════════════════════════════════");
                println!("✅ Negotiation complete! {} iterations, final score: {:.2}",
                    iterations, final_score);
            }
            negotiation::NegotiationOutcome::NeedsUserInput { questions, iterations, last_review, .. } => {
                println!("\n════════════════════════════════════════");
                println!("❓ Negotiation Paused: User Input Required");
                println!("════════════════════════════════════════\n");
                println!("Iteration: {}/{}", iterations, config.max_iterations);
                println!("Current score: {:.2}", last_review.score);
                println!("\nQuestions/Clarifications needed:\n");
                println!("{}", questions);
                println!("\n💡 Please address the questions above and re-run the negotiation.");
                std::process::exit(2); // Exit code 2 = needs user input
            }
        }

        return Ok(());
    }

    // Original plan generation path (unchanged)
    let specs_dir = args.specs_dir.unwrap_or_else(|| plan_cfg.specs_dir.clone());
    let specs_dir = git_root.join(&specs_dir);
    let max_iterations = args.max_iterations.unwrap_or(plan_cfg.plan_max_iterations);
    let api_key_env = plan_cfg.openai_api_key_env.clone();
    let api_key = std::env::var(&api_key_env).wrap_err_with(|| {
        format!(
            "OpenAI API key not found. Set the {} environment variable.",
            api_key_env
        )
    })?;
    let openai_model = args
        .openai_model
        .unwrap_or_else(|| plan_cfg.openai_model.clone());
    let openai_base_url = args.openai_base_url.or(plan_cfg.openai_base_url);

    let config = plan::PlanRunConfig {
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

    let result = plan::run_plan_generation(&config)?;

    println!("════════════════════════════════════════");
    println!("📋 Generated Plan");
    println!("════════════════════════════════════════\n");
    println!("{}", result.content);
    println!("════════════════════════════════════════");
    println!("✅ Plan generation complete!");

    Ok(())
}
```

3. Update `run_interactive_menu()` to include negotiation args for Plan (line ~460):

```rust
MenuChoice::Plan => run_plan(
    PlanArgs {
        config: None,
        specs_dir: None,
        max_iterations: None,
        openai_model: None,
        openai_base_url: None,
        dry_run: false,
        plan_file: None,
        negotiate: false,
        score_threshold: None,
        negotiation_max_iterations: None,
    },
    home_cfg,
    git_root,
),
```

---

## Phase 6: Verification

### Testing Steps

1. **Dependency check**:
   ```bash
   cargo check -q
   ```

2. **Config test** - Add to `.velor/velor.toml`:
   ```toml
   [plan]
   # Existing settings
   specs_dir = "specs"
   openai_api_key_env = "OPENAI_API_KEY"
   openai_model = "gpt-4o"

   # Negotiation mode settings
   negotiation_score_threshold = 0.9
   negotiation_max_iterations = 5
   claude_binary = "claude-glm"
   write_iteration_backups = true
   ```

3. **Unit tests** - Add tests to `src/negotiation.rs`:
   - `test_parse_review_response_valid_json`
   - `test_parse_review_response_with_code_fences`
   - `test_detect_user_questions`
   - `test_build_review_prompt`
   - `test_build_refinement_prompt`

4. **Integration test**:
   ```bash
   # Create a test plan file
   cat > test-plan.md << 'EOF'
   # Test Plan
   This is a test plan for negotiation.
   EOF

   # Run with environment variable (not CLI flag)
   OPENAI_API_KEY=sk-... cargo run -- plan --negotiate --plan-file test-plan.md
   ```

5. **Verify notification**:
   - After negotiation completes, check for system notification
   - Test both success and timeout scenarios

---

## Critical Files to Modify

| File | Lines | Changes |
|------|-------|---------|
| `Cargo.toml` | Dependencies | Add `rig-core`, `tokio`, and `notify-rust` |
| `src/config.rs` | 40-70 | Extend `PlanConfig` with negotiation fields (split write flags) |
| `src/main.rs` | ~17 | Add `mod negotiation;` |
| `src/main.rs` | ~114+ | Extend `PlanArgs` with new CLI args |
| `src/main.rs` | 312-385 | Update `run_plan()` to handle negotiation |
| `src/negotiation.rs` | NEW | Create entire negotiation module |

---

## Configuration Template

Add to `.velor/velor.toml`:

```toml
[plan]
# Existing settings
specs_dir = "specs"
plan_max_iterations = 10
openai_api_key_env = "OPENAI_API_KEY"
openai_model = "gpt-4o"

# Negotiation mode settings (new)
negotiation_score_threshold = 0.9
negotiation_max_iterations = 5
claude_binary = "claude-glm"
negotiation_write_final_plan = true
negotiation_write_iteration_backups = true
```

---

## Usage Examples

```bash
# Basic negotiation mode (refines existing plan)
velor plan --negotiate --plan-file my-plan.md

# With custom settings
velor plan --negotiate \
  --plan-file my-plan.md \
  --score-threshold 0.95 \
  --negotiation-max-iterations 10

# With API key via environment variable (secure)
OPENAI_API_KEY=sk-xxx velor plan --negotiate --plan-file my-plan.md

# Existing plan generation (unchanged)
velor plan  # Generates from specs in specs/
```

---

## Exit Codes

- `0`: Negotiation completed successfully
- `1`: Error occurred (API failure, file error, invalid response, etc.)
- `2`: Negotiation paused awaiting user input

---

## Notes

1. **rig-core framework**: Uses `rig::providers::openai` for clean async OpenAI integration with temperature=0 for deterministic JSON output
2. **Fully async**: Uses `tokio::fs`, `tokio::process`, and `tokio::time::timeout` throughout to avoid blocking the async runtime
3. **Structured output**: Both OpenAI reviewer and claude-glm refiner return structured JSON, eliminating heuristic-based detection
4. **Write flags**: Separate `negotiation_write_final_plan` and `negotiation_write_iteration_backups` config for independent control
5. **Notifications**: Best-effort via `notify-rust`; failures are logged warnings, not fatal errors
6. **Exit codes**: `0` = success, `1` = error, `2` = needs user input (for automation/scripting)
7. **Validation**: `score_threshold` clamped to [0.0, 1.0], `max_iterations` must be >= 1
8. **Security**: No `--openai-api-key` CLI flag; API key from environment variable only
9. **Timeout**: claude-glm subprocess has 5-minute timeout via `tokio::time::timeout`
