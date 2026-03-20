# Plan: Add `--append` CLI Flag for Prompt Augmentation

## Context

Add a `--append` CLI flag that appends additional user instructions to the final rendered prompt. This allows users to dynamically augment any prompt with extra context, constraints, or instructions without modifying templates or config files.

**Key design decision:** This establishes a single prompt-finalisation step in the pipeline, making future augmentations (--prepend, --context-file, etc.) easier to add without duplicating logic.

**Ordering invariant:** `--append` is applied **after** rule injection so the user's ad hoc instruction is placed at the very end of the rendered prompt with maximum local salience.

**Multi-turn behaviour:** In auto mode, `--append` is applied to **every iteration**, not just the initial prompt. This ensures the additional instructions remain present throughout the entire workflow.

## Implementation

### 1. Add `append` field to `CommonArgs` (main.rs:117-158)

```rust
#[derive(Debug, Args)]
struct CommonArgs {
    // ... existing fields ...

    /// Append additional instructions to the final rendered prompt.
    #[arg(long)]
    append: Option<String>,
}
```

### 2. Create central `finalize_prompt()` helper function (main.rs)

Add after existing helper functions (around line 500+):

```rust
/// Finalises a rendered prompt with optional extra user instructions.
///
/// # Arguments
/// * `prompt` - The base rendered prompt (possibly with rules already injected)
/// * `append_text` - Optional user text to append as a new section
///
/// # Returns
/// The original prompt if append_text is None/empty/whitespace,
/// otherwise the prompt with a new "Additional instructions" section appended.
///
/// # Behaviour
/// - Trims surrounding whitespace from append_text
/// - Ignores empty-after-trim values (treats as None)
/// - Preserves internal newlines in multi-line input
/// - Adds a clear section header "## ADDITIONAL INSTRUCTIONS" for legibility in --dry-run
fn finalize_prompt(prompt: &str, append_text: Option<&str>) -> String {
    let Some(text) = append_text.map(str::trim).filter(|s| !s.is_empty()) else {
        return prompt.to_owned();
    };

    format!(
        "{prompt}\n\n## ADDITIONAL INSTRUCTIONS\n\n{text}"
    )
}
```

### 3. Modify `run_once()` function (main.rs:965-986)

Replace lines 963-986 with:

```rust
    // Inject rules if enabled
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

    require_claude_on_path(&binary)?;

    println!("Running Claude with prompt '{prompt_name}'...");

    let runner = AgentRunner::from_config(file_cfg.defaults.protocol, file_cfg.defaults.acp);

    runner
        .run(
            &binary,
            &permission_mode,
            &final_prompt,
            &prompt_name,
            &cwd,
        )
        .await?;
```

### 4. Modify `run_auto_loop()` function (main.rs:1626-1646)

Find the subprocess/single-shot mode section (around line 1628) and replace:

```rust
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
            let final_prompt = finalize_prompt(&prompt_with_rules, common.append.as_deref());

            println!("📋 Prompt:\n{final_prompt}");
            println!("────────────────────────────────────────");

            // ... use final_prompt instead of prompt_with_rules ...
```

### 5. Modify `run_auto_iteration_with_session()` for multi-turn mode (main.rs:1397)

Replace line 1397 and surrounding context:

```rust
    // TURN A: Initial prompt with always-apply rules
    let base_prompt = inject_rules(prompt, &initial_rules.rules);

    // Finalise with user instructions (--append)
    // Applied every iteration, including initial prompt
    let prompt_with_rules = finalize_prompt(&base_prompt, common.append.as_deref());

    tracing::debug!(
        "Iteration {}: sending initial prompt with {} rules",
        iteration,
        initial_rules.rules.len()
    );
```

## Tests

### Unit Tests for `finalize_prompt()`

Add test module in main.rs (or in a separate tests module):

```rust
#[cfg(test)]
mod tests {
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
```

### Integration Tests

Test scenarios to verify manually:
1. **No append** - `velor once --prompt test` (baseline)
2. **Single-line append** - `velor once --prompt test --append "be careful with errors"`
3. **Multi-line append** - `velor once --prompt test --append "first line\nsecond line"`
4. **Empty append** - `velor once --prompt test --append ""` (should behave as no append)
5. **Whitespace-only append** - `velor once --prompt test --append "   "` (should behave as no append)
6. **Dry-run shows section** - `velor once --prompt test --append "test" --dry-run` (verify "## ADDITIONAL INSTRUCTIONS" header)
7. **Auto mode applies every iteration** - `velor auto --prompt test --append "focus on performance"` (verify in logs)
8. **Combined with rules** - verify ordering: base prompt → rules → ADDITIONAL INSTRUCTIONS

## Verification

Run cargo check and tests:

```bash
cargo check -q
cargo test finalize_prompt
```

Manual verification:

```bash
# Should show additional instructions section
velor once --prompt test --append "focus on error handling" --dry-run

# Should apply append every iteration (check logs for each iteration)
velor auto --prompt test --append "be thorough" --iterations 3

# Should handle empty/whitespace gracefully
velor once --prompt test --append "" --dry-run | grep -q "ADDITIONAL INSTRUCTIONS" && echo "FAIL: empty showed" || echo "PASS"
```

## Files Modified

- `/Users/liam/git/velor/apps/velor-cli/src/main.rs` (5 changes)
  - Add `append` field to `CommonArgs` struct (line ~158)
  - Add `finalize_prompt()` helper function (line ~500)
  - Add unit tests for `finalize_prompt()` (in test module)
  - Modify `run_once()` to use `finalize_prompt()` (lines ~964, ~982)
  - Modify `run_auto_loop()` to use `finalize_prompt()` (line ~1634)
  - Modify `run_auto_iteration_with_session()` for multi-turn mode (line ~1397)

## Future Extensibility

This change establishes a single prompt-finalisation step. Future augmentations can be added to `finalize_prompt()` without duplicating logic across execution modes:

- `--prepend` for instructions before the main prompt
- `--context-file` for injecting file contents
- `--system-note` for meta-level guidance
