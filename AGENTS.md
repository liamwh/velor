# AGENTS.md

## Project Overview

Velor Agent CLI is a Rust-based command-line tool for running autonomous AI agents with Claude AI. It uses template-based prompts with variable substitution and supports iterative execution with crash recovery.

## Common Commands

### Development
- `just check` - Run all checks (fmt, clippy, svelte-check)

### Configuration
- Global config: `~/.velor/velor.toml`
- Project config: `{git_root}/.velor/velor.toml`

## Architecture

### Module Structure
- **src/main.rs** - CLI entry point with clap-derived argument parsing, command dispatch, and the auto-mode loop with retry logic
- **src/config.rs** - TOML configuration loading with precedence: CLI args → repo config → home config → defaults
- **src/template.rs** - MiniJinja template rendering with strict undefined behavior
- **src/claude.rs** - Interface to Claude CLI binary, spawns subprocess with stream-json output parsing
- **src/git.rs** - Git repository root discovery via `git rev-parse` or directory tree walking
- **src/retry.rs** - Exponential backoff retry logic, conversation history for crash recovery, error classification
- **src/notification.rs** - Notification system supporting Telegram and macOS; fire-and-forget delivery with error logging

### Configuration Precedence
Variables are merged from multiple sources with the following precedence (highest to lowest):
1. CLI `--set` overrides
2. Runtime variables (iteration, git_root, cwd, etc.)
3. File config variables (`[vars]` section)

For defaults, repo config overrides home config.

### Prompt Templates
Templates are defined in `[prompts.<name>]` sections of `velor.toml`. Both inline strings and table formats (with optional `complete_token` override) are supported. Templates use MiniJinja syntax with access to all variables.

### Auto-Mode Loop
The auto-mode runs iterations with crash resilience:
- Each iteration renders the template with current variables (including `{{iteration}}`)
- On failure, retries with exponential backoff (max 5 retries by default)
- After all retries exhausted, preserves conversation context and retries same iteration
- Successful iterations clear history and advance to next iteration
- Exits when output contains the completion token (`<promise>COMPLETE</promise>` by default)

### Claude Interface
The CLI spawns the configured binary (default: `claude-glm`) with:
- `--permission-mode` (default: `acceptEdits`)
- `--input-format text --output-format stream-json --include-partial-messages`
- Stdin: the rendered prompt
- Stdout: parsed for text chunks and streamed to console
- Stderr: streamed directly

## Rust Guidelines (from .agents/rules/rust.mdc)

- Use `thiserror` for library crates, `color-eyre` for binary crates (this is a binary)
- Use `tracing::instrument` macro for function instrumentation
- Always include messages in assert macros
- Prefer types over primitives (Newtypes for IDs/units, Enums for state machines)
- Return early for error conditions
- TEST EVERYTHING with unit or property tests (use `proptest` for critical algorithms)
- Never use `anyhow`
- Always run `cargo check -q` before concluding work
- Use fully qualified paths for imports when possible
- Document all public items

### Configuration and Secrets
- Secrets MUST come from environment variables only
- Non-sensitive config from TOML files
- Validate all required values at startup (fail fast)
- Use `secrecy::SecretString` for credentials
- Use `veil` for redacting sensitive fields in Debug output

### UI and Design
- Bun instead of npm
