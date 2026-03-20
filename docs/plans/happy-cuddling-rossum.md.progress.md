# File-Based Prompt Support Implementation Progress

## Summary

Successfully implemented file-based prompt support for Velor, enabling prompts to be defined as markdown files in `.velor/prompts/` directories instead of only inline TOML configuration.

## Completed Tasks

### 1. Created `crates/velor-core/src/prompts.rs` module ✅
- Implemented `Prompt` struct with name, description, complete_token, and content fields
- Implemented `PromptFrontmatter` struct for YAML frontmatter parsing
- Implemented `PromptCache` struct with home and repo directory support
- Implemented `parse_prompt_file()` for parsing `.md` files with YAML frontmatter
- Implemented `discover_prompts()` for loading prompts from directories
- Implemented `split_frontmatter()` for parsing YAML frontmatter (reused from rules.rs pattern)
- Added constants: `MAX_PROMPT_FILE_SIZE: 100KB`, `MAX_TOTAL_PROMPTS: 50`
- Added comprehensive unit tests and property tests

### 2. Updated `PromptDef` enum in `crates/velor-core/src/config.rs` ✅
- Added `File { path: String, complete_token: Option<String> }` variant
- Updated `template()` method to handle File variant (returns path)
- Updated `complete_token()` method to handle File variant
- Added `is_file()` helper method

### 3. Added `PromptsConfig` struct and integrated into `FileConfig` ✅
- Created `PromptsConfig` struct with `enabled: bool` and `directory: String` fields
- Default: enabled=true, directory="prompts"
- Added `prompts_config: PromptsConfig` field to `FileConfig`
- Updated `FileConfig::merge()` to include `prompts_config`

### 4. Updated `crates/velor-core/src/lib.rs` exports ✅
- Added `pub mod prompts;`
- Re-exported `PromptsConfig` type
- Updated module documentation to include prompts module

### 5. Updated `resolve_prompt_template()` in `apps/velor-cli/src/main.rs` ✅
- Made function async to support file I/O
- Added `prompt_cache: &PromptCache` parameter
- Implemented resolution order:
  1. CLI `--prompt-text` override (existing)
  2. File-based prompts from cache (if enabled)
  3. Inline/config prompts (existing)
  4. For `PromptDef::File` variants, load from cache
- Updated error messages to include file-based prompts

### 6. Initialized `PromptCache` in CLI ✅
- Added `PromptCache` import to main.rs
- Initialized `PromptCache` in both `run_once()` and `run_auto()` functions
- Home directory: `~/.velor`
- Repo directory: `{git_root}/.velor`
- Passed cache to `resolve_prompt_template()` calls

### 7. Fixed all test cases ✅
- Fixed `FileConfig` test initializations to include `prompts_config` field
- Fixed `FileConfig::merge()` to include `prompts_config`
- All 382 tests pass

## File Changes Summary

| File | Changes |
|------|---------|
| `crates/velor-core/src/prompts.rs` | **NEW** - Complete prompts module with cache, parsing, discovery |
| `crates/velor-core/src/config.rs` | Added `PromptDef::File` variant, `PromptsConfig` struct, updated `FileConfig` |
| `crates/velor-core/src/lib.rs` | Added prompts module export and re-exports |
| `apps/velor-cli/src/main.rs` | Added PromptCache import, initialized cache, updated `resolve_prompt_template()` |

## Directory Structure

```
~/.velor/
├── velor.toml
└── prompts/              # Home prompts (base)
    ├── common.md
    └── debug.md

{git_root}/.velor/
├── velor.toml
└── prompts/              # Repo prompts (override)
    ├── project-task.md
    └── common.md         # Overrides home/common.md
```

## Prompt File Format

```markdown
---
description: "A human-readable description of this prompt"
complete_token: "<promise>DONE</promise>"
---

# Optional markdown heading

This is the prompt template content. Variables work as usual:
- Use {{git_root}} for the repository root
- Use {{iteration}} for the current iteration
- Any custom variables from [vars] section
```

## Backward Compatibility

- ✅ Existing inline prompts continue to work unchanged
- ✅ Existing TOML table prompts continue to work
- ✅ File-based prompts are opt-in (requires creating `.md` files)
- ✅ Mixed configuration (inline + file) supported

## Verification

- ✅ All unit tests pass
- ✅ All property tests pass
- ✅ `cargo check` passes
- ✅ `clippy` passes (only test code `unwrap()` warnings, which are acceptable)
- ✅ 382 tests passed

## Next Steps (if continuing implementation)

1. Add integration tests for file-based prompts
2. Add manual testing scenarios to verify the feature works end-to-end
3. Consider adding a `velor prompts list` command to show available prompts
4. Consider adding a `velor prompts validate` command to verify prompt files
