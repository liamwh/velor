# Implementation Plan: Auto-Negotiation Mode for `velor plan`

## Summary

Add an auto-negotiation mode to the `velor plan` command where a user-provided plan file is iteratively refined through a review loop between claude-glm (codebase agent) and ChatGPT 5.2 (reviewer). The loop continues until ChatGPT gives a perfection score >= 0.9, then sends a system notification.

---

## User Requirements (Clarified)

1. **Plan input**: User provides an existing plan file to refine (not generated from specs)
2. **ChatGPT integration**: Use the `rig` framework for ChatGPT API calls
3. **Questions handling**: Stop and wait for user input when claude-glm asks questions
4. **Termination**: Loop until ChatGPT score >= 0.9
5. **Notification**: Send system notification on completion

---

## Phase 1: Dependencies

### Add to `Cargo.toml`

```toml
[dependencies]
# ... existing dependencies ...
rig = "0.6"           # AI agent framework for ChatGPT integration
notify-rust = "4"     # Cross-platform system notifications
```

---

## Phase 2: Configuration

### Modify `src/config.rs` - `PlanConfig` struct (lines 40-70)

Add new fields:

```rust
pub struct PlanConfig {
    // ... existing fields ...

    // === NEGOTIATION MODE FIELDS ===
    /// ChatGPT API key environment variable.
    pub chatgpt_api_key_env: String,

    /// ChatGPT model to use for reviews (default: "gpt-4o" - use latest available).
    pub chatgpt_model: String,

    /// ChatGPT base URL (optional, for custom endpoints).
    pub chatgpt_base_url: Option<String>,

    /// Perfection score threshold (0.0 to 1.0) to stop negotiation.
    pub negotiation_score_threshold: f64,

    /// Maximum negotiation iterations (prevents infinite loops).
    pub negotiation_max_iterations: u32,

    /// Claude binary to use for plan refinement.
    pub claude_binary: String,
}
```

### Update `PlanConfig::default()` (lines 60-70)

```rust
impl Default for PlanConfig {
    fn default() -> Self {
        Self {
            // ... existing defaults ...
            chatgpt_api_key_env: "CHATGPT_API_KEY".to_string(),
            chatgpt_model: "gpt-4o".to_string(),  // Use latest available model
            chatgpt_base_url: None,
            negotiation_score_threshold: 0.9,
            negotiation_max_iterations: 5,
            claude_binary: "claude-glm".to_string(),
        }
    }
}
```

---

## Phase 3: CLI Arguments

### Modify `src/main.rs` - `PlanArgs` struct (around line 114)

Add new arguments:

```rust
#[derive(Debug, Args)]
struct PlanArgs {
    // ... existing fields ...

    /// Path to the plan file to negotiate/refine.
    #[arg(long)]
    plan_file: Option<PathBuf>,

    /// Enable negotiation mode with ChatGPT review.
    #[arg(long, action = ArgAction::SetTrue)]
    negotiate: bool,

    /// ChatGPT API key (overrides environment variable).
    #[arg(long)]
    chatgpt_api_key: Option<String>,

    /// ChatGPT model to use.
    #[arg(long)]
    chatgpt_model: Option<String>,

    /// Perfection score threshold (0.0 to 1.0).
    #[arg(long)]
    score_threshold: Option<f64>,

    /// Maximum negotiation iterations.
    #[arg(long)]
    negotiation_max_iterations: Option<u32>,
}
```

---

## Phase 4: New Negotiation Module

### Create `src/negotiation.rs`

Key structures and functions:

```rust
use rig::providers::openai;
use rig::agent::Agent;
use color_eyre::eyre::{Result, WrapErr};

/// Configuration for the negotiation loop.
pub struct NegotiationConfig {
    pub plan_file_path: PathBuf,
    pub chatgpt_api_key: String,
    pub chatgpt_model: String,
    pub chatgpt_base_url: Option<String>,
    pub score_threshold: f64,
    pub max_iterations: u32,
    pub claude_binary: String,
}

/// Result from a single ChatGPT review.
pub struct ReviewResult {
    pub score: f64,
    pub feedback: String,
    pub suggestions: Vec<String>,
}

/// Final negotiation result.
pub struct NegotiationResult {
    pub plan: String,
    pub iterations: u32,
    pub final_score: f64,
    pub review_history: Vec<ReviewResult>,
}

/// Main negotiation loop.
#[tracing::instrument(level = "debug", ret, err, skip(config))]
pub fn run_negotiation_loop(config: &NegotiationConfig) -> Result<NegotiationResult> {
    // Load initial plan from file
    let mut current_plan = std::fs::read_to_string(&config.plan_file_path)
        .wrap_err_with(|| format!("failed to read plan file: {}", config.plan_file_path.display()))?;

    let mut history = Vec::new();

    for iteration in 1..=config.max_iterations {
        println!("\n────────────────────────────────────────");
        println!("🔄 Negotiation iteration {}/{}", iteration, config.max_iterations);

        // Review current plan with ChatGPT using rig
        let review = review_plan_with_chatgpt(config, &current_plan)?;

        println!("📊 Perfection score: {:.2}", review.score);

        if review.score >= config.score_threshold {
            println!("✅ Plan meets quality threshold!");
            send_system_notification("Plan Negotiation Complete",
                &format!("Completed {} iterations with score: {:.2}", iteration, review.score))?;
            return Ok(NegotiationResult {
                plan: current_plan,
                iterations: iteration,
                final_score: review.score,
                review_history: history,
            });
        }

        println!("💬 Feedback: {}", truncate(&review.feedback, 200));

        // Refine plan with claude-glm, passing feedback
        current_plan = refine_plan_with_claude(config, &current_plan, &review.feedback)?;

        history.push(review);
    }

    // Max iterations reached
    let final_score = history.last().map(|r| r.score).unwrap_or(0.0);
    send_system_notification("Plan Negotiation Complete",
        &format!("Reached max iterations with score: {:.2}", final_score))?;

    Ok(NegotiationResult {
        plan: current_plan,
        iterations: config.max_iterations,
        final_score,
        review_history: history,
    })
}

/// Reviews a plan using ChatGPT via rig framework.
fn review_plan_with_chatgpt(config: &NegotiationConfig, plan: &str) -> Result<ReviewResult> {
    let prompt = build_review_prompt(plan);

    // Use rig's OpenAI provider
    let client = if let Some(base_url) = &config.chatgpt_base_url {
        openai::Client::new(&config.chatgpt_api_key)
            .with_base_url(base_url)
    } else {
        openai::Client::new(&config.chatgpt_api_key)
    };

    let mut agent = client
        .agent(&config.chatgpt_model)
        .prompt("You are an expert technical reviewer. Always respond with valid JSON containing score (0-1), feedback (string), and suggestions (array of strings).")
        .build();

    let response = agent.prompt(&prompt).await?;

    // Parse structured JSON response
    let review: serde_json::Value = serde_json::from_str(&response)?;
    let score = review["score"].as_f64().unwrap_or(0.0).clamp(0.0, 1.0);
    let feedback = review["feedback"].as_str().unwrap_or("No feedback").to_string();
    let suggestions = review["suggestions"]
        .as_array()
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();

    Ok(ReviewResult { score, feedback, suggestions })
}

/// Refines a plan using claude-glm with feedback from ChatGPT.
fn refine_plan_with_claude(config: &NegotiationConfig, plan: &str, feedback: &str) -> Result<String> {
    let prompt = build_refinement_prompt(plan, feedback);

    let mut child = std::process::Command::new(&config.claude_binary)
        .args(["plan", "--permission-mode", "acceptEdits"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;

    // Write prompt to stdin
    let mut stdin = child.stdin.take()?;
    std::io::Write::write_all(&mut stdin, prompt.as_bytes())?;
    drop(stdin);

    // Wait for output
    let output = child.wait_with_output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Check if claude-glm is asking questions
        if contains_questions(&stderr) || contains_questions(&String::from_utf8_lossy(&output.stdout)) {
            println!("\n❓ claude-glm has questions. Please review and provide input:");
            println!("{}", stderr);
            // Wait for user to respond - this is interactive
            // For now, return what we have
            return Ok(String::from_utf8_lossy(&output.stdout).to_string());
        }
        return Err(color_eyre::eyre::eyre!("claude-glm failed: {}", stderr));
    }

    Ok(String::from_utf8(output.stdout)?)
}

/// Sends a system notification using notify-rust.
fn send_system_notification(title: &str, body: &str) -> Result<()> {
    notify_rust::Notification::new()
        .summary(title)
        .body(body)
        .show()
        .wrap_err("failed to send system notification")?;
    Ok(())
}

fn build_review_prompt(plan: &str) -> String {
    format!(
        "Review the following implementation plan and provide:\n\
        1. A perfection score from 0.0 to 1.0 (0.9+ is excellent)\n\
        2. Detailed feedback on what could be improved\n\
        3. Specific suggestions for improvement\n\n\
        Respond ONLY with valid JSON: {{\"score\": 0.0-1.0, \"feedback\": \"string\", \"suggestions\": [\"string1\", \"string2\"]}}\n\n\
        Plan to review:\n\
        {}",
        plan
    )
}

fn build_refinement_prompt(plan: &str, feedback: &str) -> String {
    format!(
        "Please refine the following plan based on the reviewer's feedback.\n\
        Keep the good parts and address the concerns raised.\n\n\
        Current plan:\n{}\n\n\
        Reviewer feedback:\n{}",
        plan, feedback
    )
}

fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len.saturating_sub(3)])
    }
}

fn contains_questions(s: &str) -> bool {
    s.contains('?') || s.to_lowercase().contains("question")
}
```

---

## Phase 5: Integration

### Modify `src/main.rs`

1. Add module declaration (line ~17):
```rust
mod negotiation;
```

2. Update `run_plan()` function (lines 312-385) to handle negotiation mode:

```rust
fn run_plan(args: PlanArgs, home_cfg: FileConfig, git_root: PathBuf) -> Result<()> {
    // ... existing config loading ...

    if args.negotiate {
        // Negotiation mode
        let plan_file = args.plan_file.ok_or_else(|| {
            color_eyre::eyre::eyre!("--plan-file is required when using --negotiate")
        })?;

        let chatgpt_api_key = args.chatgpt_api_key
            .unwrap_or_else(|| std::env::var(&plan_cfg.chatgpt_api_key_env)
                .wrap_err_with(|| format!("ChatGPT API key not found. Set {} environment variable or use --chatgpt-api-key",
                    plan_cfg.chatgpt_api_key_env))?);

        let config = negotiation::NegotiationConfig {
            plan_file_path: plan_file,
            chatgpt_api_key,
            chatgpt_model: args.chatgpt_model.unwrap_or_else(|| plan_cfg.chatgpt_model.clone()),
            chatgpt_base_url: plan_cfg.chatgpt_base_url,
            score_threshold: args.score_threshold.unwrap_or(plan_cfg.negotiation_score_threshold),
            max_iterations: args.negotiation_max_iterations.unwrap_or(plan_cfg.negotiation_max_iterations),
            claude_binary: plan_cfg.claude_binary.clone(),
        };

        let result = negotiation::run_negotiation_loop(&config)?;

        println!("\n════════════════════════════════════════");
        println!("📋 Final Refined Plan");
        println!("════════════════════════════════════════\n");
        println!("{}", result.plan);
        println!("════════════════════════════════════════");
        println!("✅ Negotiation complete! {} iterations, final score: {:.2}",
            result.iterations, result.final_score);

        Ok(())
    } else {
        // Existing plan generation path
        // ... existing code ...
    }
}
```

---

## Phase 6: Verification

### Testing Steps

1. **Dependency test**:
   ```bash
   cargo check -q
   ```

2. **Config test** - Create test config:
   ```toml
   [plan]
   chatgpt_api_key_env = "CHATGPT_API_KEY"
   chatgpt_model = "gpt-4o"
   negotiation_score_threshold = 0.9
   negotiation_max_iterations = 5
   claude_binary = "claude-glm"
   ```

3. **Dry run test** (if supported):
   ```bash
   cargo run -- plan --negotiate --plan-file test-plan.md --dry-run
   ```

4. **Integration test**:
   ```bash
   # Create a test plan file
   cat > test-plan.md << 'EOF'
   # Test Plan
   This is a test plan for negotiation.
   EOF

   # Run with API keys
   CHATGPT_API_KEY=sk-... cargo run -- plan --negotiate --plan-file test-plan.md
   ```

5. **Verify notification**:
   - After negotiation completes, check for macOS notification

---

## Critical Files to Modify

| File | Lines | Changes |
|------|-------|---------|
| `Cargo.toml` | Dependencies | Add `rig` and `notify-rust` |
| `src/config.rs` | 40-70 | Add negotiation fields to `PlanConfig` |
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
openai_api_key_env = "OPENAI_API_KEY"
openai_model = "gpt-4o"

# Negotiation mode settings
chatgpt_api_key_env = "CHATGPT_API_KEY"
chatgpt_model = "gpt-4o"
negotiation_score_threshold = 0.9
negotiation_max_iterations = 5
claude_binary = "claude-glm"
```

---

## Usage Examples

```bash
# Basic negotiation mode
velor plan --negotiate --plan-file my-plan.md

# With custom settings
velor plan --negotiate \
  --plan-file my-plan.md \
  --score-threshold 0.95 \
  --negotiation-max-iterations 10

# With API key override
CHATGPT_API_KEY=sk-xxx velor plan --negotiate --plan-file my-plan.md
```

---

## Notes

1. **rig framework**: Uses `rig::providers::openai` for ChatGPT integration with async support
2. **Async consideration**: Since rig is async, may need to add `tokio` runtime or use blocking wrapper
3. **Questions handling**: When claude-glm asks questions, the function pauses - full interactive support may need additional refinement
4. **Error handling**: All API failures wrapped with `color_eyre` for detailed error reports
