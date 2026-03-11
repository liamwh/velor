# Progress Handoff: Tab Autocomplete for --prompt Argument

## Most Recent Commit

SHA: (to be added after commit)

## What Changed

### Phase 1: Shared Prompt Discovery in velor-core ✅ COMPLETED

**File: `crates/velor-core/src/prompts.rs`**
- Added `pub mod discovery` module with:
  - `discover_prompt_names(git_root, cfg)` - Public async function that returns sorted prompt names from all sources
  - `scan_prompt_dir(dir)` - Private helper to scan a directory for `.md` files
  - `PromptSource` enum - Private enum for tracking prompt origin (Config, HomeFile, RepoFile)
  - `PromptDiscoveryError` enum - Public error type with `NotFound` and `Io` variants

**Precedence handling:**
- Layer 3 (lowest): Config prompts from `FileConfig.prompts`
- Layer 2: Home file prompts from `~/.velor/prompts/*.md`
- Layer 1 (highest): Repo file prompts from `{git_root}/.velor/prompts/*.md`

**Key design decisions:**
- Returns `BTreeMap`-sorted names (alphabetically sorted)
- Case-insensitive `.md` extension matching
- Graceful degradation: missing directories are not errors
- `NotFound` is distinguished from other errors for proper handling
- Uses `thiserror` for error types (library crate)

**Dependencies added:**
- `crates/velor-core/Cargo.toml`: Added `dirs` and `thiserror` workspace dependencies

**File: `crates/velor-core/tests/prompt_discovery.rs`**
- Created comprehensive test suite with 10 tests:
  - `test_empty_when_no_sources` - Empty result when no sources exist
  - `test_config_prompts_only` - Only config prompts returned
  - `test_alphabetic_sorting` - Results are alphabetically sorted
  - `test_missing_directory_returns_empty` - Missing dirs handled gracefully
  - `test_non_md_files_ignored` - Only `.md` files processed (case-insensitive)
  - `test_case_insensitive_extension` - `.MD`, `.Md` recognized
  - `test_only_file_stems_returned` - Returns filenames without `.md` extension
  - Serial tests (using `serial_test`):
    - `test_repo_prompts_override_home` - Repo takes precedence over home
    - `test_shadowing_semantics` - Full 3-layer precedence test
    - `test_no_git_root_uses_home_and_config_only` - Behavior outside git repo

**Dependencies added:**
- `Cargo.toml` (workspace): Added `serial_test = "3"`
- `crates/velor-core/Cargo.toml`: Added `serial_test` to dev-dependencies

## What's Next

### Phase 2: Internal Hidden Subcommand
**File: `apps/velor-cli/src/main.rs`**
- Add `Internal(InternalCommands)` to the `Commands` enum (with `#[command(hide = true)]`)
- Create `InternalCommands` enum with `CompletePrompts` variant
- Implement `handle_internal_complete_prompts()` handler with graceful degradation
- Update command matching in `main()` to handle internal commands

This is the most logical next step because it builds directly on the `discover_prompt_names()` function just implemented.

## Remaining Phases

- **Phase 3**: Custom Zsh Completion (`apps/velor-cli/src/completion.rs` - NEW FILE)
- **Phase 4**: Completion Command (add `Completion` command to main.rs)
- **Phase 5**: Zsh Installation (documentation only)
- **Phase 6**: Comprehensive Tests (Phase 6 tests already integrated into Phase 1)

## Technical Notes

- The `discover_prompt_names()` function uses `dirs::home_dir()` which respects the `HOME` environment variable
- Tests that manipulate `HOME` use `serial_test` to prevent race conditions
- The function is async and designed for fast execution (< 50ms target) for shell completion performance
- Error handling uses `thiserror` as per library crate guidelines in `.agents/rules/rust.mdc`

## Verification Steps Completed

1. ✅ `cargo test -p velor-core --test prompt_discovery` - All 10 tests pass
2. ✅ `cargo clippy -p velor-core --all-targets` - No warnings
3. ✅ Dependencies properly added to workspace and crate

## Verification Steps Remaining (for next phase)

1. `velor internal complete-prompts` - Should output newline-delimited prompt names
2. Test error handling: Ensure the command exits silently (no stderr) on failure
