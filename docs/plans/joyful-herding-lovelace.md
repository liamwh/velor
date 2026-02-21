# Plan: Add `-c` Short Alias for `--config` CLI Option

## Context

The `velor` CLI already supports `--config` to specify a custom config file location. This plan adds the short alias `-c` for convenience and consistency with common CLI conventions.

## Implementation

### 1. Update `CommonArgs` in `src/main.rs`

**Location**: `src/main.rs:61`

Change the clap attribute from `#[arg(long)]` to `#[arg(short, long)]`:

```rust
/// Override config path (defaults to {git_root}/.velor/velor.toml).
#[arg(short, long)]
config: Option<std::path::PathBuf>,
```

### 2. Update `PlanArgs` in `src/main.rs`

**Location**: `src/main.rs:135`

Apply the same change:

```rust
#[arg(short, long)]
config: Option<std::path::PathBuf>,
```

### 3. Update Help Text (Optional)

**Location**: `src/main.rs:60`

The current help text mentions `agent-cli.toml` but should reference `velor.toml` for consistency:

```rust
/// Override config path (defaults to {git_root}/.velor/velor.toml).
```

## Verification

Test that `-c` works equivalently to `--config`:

```bash
# Test short alias
velor -c /path/to/custom/config.toml once

# Test that it overrides default config loading
velor -c ~/.velor/velor.toml once --dry-run

# Test with plan subcommand
velor -c /path/to/config.toml plan
```

Run existing tests to ensure no regressions:

```bash
just test
```
