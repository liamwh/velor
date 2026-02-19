# AGENTS.md

## Project Overview

Velor Agent CLI is a Rust-based command-line tool for running autonomous AI agents with Claude AI. It uses template-based prompts with variable substitution and supports iterative execution with crash recovery.

## Common Commands

### Development
- `just check` - Run all checks (fmt, clippy, tests)
- `cargo check -q` - Quick compile check (always use quiet flag)
- `just test` or `cargo nextest run` - Run tests (always use nextest, never `cargo test`)
- `just fmt` - Format code
- `just lint` - Run clippy
- `just build` - Build the binary
- `just build-release` - Build release binary
- `just install` - Install to ~/bin

### CLI Usage
- `velor once` - Single-shot Claude invocation
- `velor auto` - Iterative execution until completion
- `velor init` - Initialize repository with `.velor/velor.toml` config
- `velor test-notification` - Send test notification to verify configuration
- `--dry-run` - Show rendered prompt without executing

### Configuration
- Global config: `~/.velor/velor.toml`
- Project config: `{git_root}/.velor/velor.toml`
- `just edit-config` - Open project config in `$EDITOR`
- `just show-prompts` - List available prompt templates
- `just test-notification` - Test notification configuration

### Notifications

Velor can send notifications when runs complete, reach max iterations, or fail. Supports Telegram and macOS Notification Center.

#### Configuration

Add a `[notifications]` section to your `velor.toml`:

```toml
[notifications]
enabled = true
notify_on_success = true
notify_on_max_iterations = true
notify_on_failure = true
output_preview_chars = 500

# Telegram notifications
[notifications.telegram]
enabled = true
bot_token_env = "TELEGRAM_BOT_TOKEN"  # Environment variable with bot token
chat_id = "-1001234567890"             # Target chat/group ID
api_base_url = "https://api.telegram.org"  # Optional: for proxies
parse_mode = "MarkdownV2"              # Optional: "MarkdownV2" or "Html"

# macOS notifications (works on macOS only)
[notifications.macos]
enabled = true
sound = "default"  # Optional: "default", "Basso", "Sosumi", etc.
```

#### Testing

Run `velor test-notification` to verify your configuration:
- Builds notifiers from merged home + repo config
- Sends a test notification via all enabled channels
- Shows which notifiers are being used

#### Telegram Setup

1. Create a bot via [@BotFather](https://t.me/botfather) and get the token
2. Get your chat ID (message [@userinfobot](https://t.me/userinfobot))
3. Set environment variable: `export TELEGRAM_BOT_TOKEN="your-token"`
4. Add `[notifications.telegram]` to your config with `chat_id`
5. Run `velor test-notification` to verify

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
