# Progress: Add `--append` CLI Flag for Prompt Augmentation

## Current Status: COMPLETE

## Commit SHA

- **1032fbd** - feat(cli): add --append flag for prompt augmentation

## What Changed

### 1. Added `append` field to `CommonArgs` struct (line 159 in main.rs)
- Added `append: Option<String>` field to hold the optional user instructions
- Updated TUI struct initializations at lines 786 and 810 to include `append: None`

### 2. Created `finalize_prompt()` helper function (lines 860-883 in main.rs)
- Takes a base prompt and optional append text
- Trims surrounding whitespace from append_text
- Ignores empty-after-trim values (treats as None)
- Preserves internal newlines in multi-line input
- Adds "## ADDITIONAL INSTRUCTIONS" section header for visibility

### 3. Added unit tests for `finalize_prompt()` (lines 2149-2205 in main.rs)
- `test_finalize_prompt_none_returns_original` - verifies None returns original
- `test_finalize_prompt_empty_returns_original` - verifies empty/whitespace returns original
- `test_finalize_prompt_single_line` - verifies single-line append works
- `test_finalize_prompt_multiline` - verifies multiline append works
- `test_finalize_prompt_preserves_internal_newlines` - verifies internal newlines preserved
- All 5 tests pass

### 4. Modified `run_once()` function (line 998 in main.rs)
- Added `finalize_prompt()` call after rule injection
- Uses `final_prompt` in dry-run and agent runner

### 5. Modified `run_auto_loop()` function
- Added `append: Option<&str>` parameter to function signature (line 1577)
- Passes `common.append.as_deref()` from `run_auto()` (line 1216)
- Adds `finalize_prompt()` call in subprocess/single-shot mode (line 1677)

### 6. Modified `run_auto_iteration_acp()` function
- Added `append: Option<&str>` parameter to function signature (line 1819)
- Passes `append` from `run_auto_loop()` (line 1646)

### 7. Modified `run_auto_iteration_with_session()` for multi-turn mode
- Added `append: Option<&str>` parameter to function signature (line 1414)
- Passes `append` from `run_auto_iteration_acp()` (line 1878)
- Added `finalize_prompt()` call after initial rule injection (line 1444)

## Verification

- `cargo check -q` passes
- `cargo test finalize_prompt` - All 5 unit tests pass
- `cargo test` - All new tests pass (pre-existing failure in prompt_discovery is unrelated)

## Next Steps

- Test the `--append` flag manually with various scenarios:
  - `velor once --prompt test --append "extra instructions" --dry-run`
  - `velor auto --prompt test --append "focus on performance" --iterations 3`
  - Verify empty/whitespace handling
  - Verify multi-line append works correctly

## Technical Notes

- The `--append` flag is applied AFTER rule injection so user instructions appear at the very end of the rendered prompt
- In auto mode, `--append` is applied to EVERY iteration, not just the initial prompt
- This establishes a single prompt-finalisation step in the pipeline, making future augmentations (--prepend, --context-file, etc.) easier to add

## No Open Questions or Blockers
