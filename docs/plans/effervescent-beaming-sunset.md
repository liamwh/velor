# Add `--binary` CLI Argument

## Context

Currently, the Claude binary path can only be set via TOML configuration (`binary = "claude-glm"` in the `[defaults]` section). Users want the ability to override this from the CLI for temporary use cases like testing different Claude versions or binaries without editing config files.

## Implementation Plan

### 1. Add CLI Argument to `CommonArgs` (src/main.rs)

Add a new field to the `CommonArgs` struct around line 86:

```rust
/// Override the Claude binary to use (e.g. "claude" or "claude-glm")
#[arg(short, long, global = true)]
pub binary: Option<String>,
```

### 2. Update `KNOWN_FLAGS` (src/main.rs)

Add `"binary"` to the `KNOWN_FLAGS` array (around line 160-178) to exclude it from being parsed as a template variable:

```rust
const KNOWN_FLAGS: &[&str] = &[
    "config", "prompt", "prompt-text", "permission-mode", "prd-path",
    "progress-path", "complete-token", "set", "dry-run", "iterations",
    "max-retries", "base-backoff-ms", "specs-dir", "max-iterations",
    "openai-api-key", "openai-model", "openai-base-url",
    "binary",  // <-- Add this
];
```

### 3. Resolve Binary Value with Precedence (src/main.rs)

Update both `run_once` (around line 592) and `run_auto` (around line 678) to use the CLI override pattern:

```rust
let binary = common
    .binary
    .clone()
    .unwrap_or_else(|| file_cfg.defaults.binary.clone());
```

### Critical Files

- `src/main.rs` - CLI argument definitions and command dispatch
  - Lines ~86: Add `binary: Option<String>` to `CommonArgs`
  - Lines ~160-178: Add `"binary"` to `KNOWN_FLAGS`
  - Lines ~592, ~678: Update binary resolution in `run_once` and `run_auto`

## Verification

1. Run `cargo check -q` to verify compilation
2. Test CLI override: `velor --binary claude once --prompt example`
3. Test short flag: `velor -b claude once --prompt example`
4. Test without flag (uses config default): `velor once --prompt example`
5. Test with auto mode: `velor --binary claude auto --prompt example`
6. Run `just check` to ensure fmt, clippy, and tests pass
