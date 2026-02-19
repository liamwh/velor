# Velor Agent CLI

Velor is a Rust-based command-line tool for running autonomous AI agents with Claude AI. It uses template-based prompts with variable substitution and supports iterative execution with crash recovery.

## Features

- **Template-based prompts** - Use MiniJinja templates with variable substitution
- **Iterative execution** - Run agents until completion with automatic retry logic
- **Crash recovery** - Preserve conversation context across failures
- **Multiple protocols** - Support for both subprocess spawning and ACP (Agent Client Protocol)
- **Configuration management** - TOML-based config with home and project-level overrides
- **Notifications** - Telegram and macOS notification support

## Installation

```bash
# From source
cargo install --path .

# Or using just
just install
```

## Quick Start

1. **Initialize in your project:**

```bash
velor init
```

This creates `.velor/velor.toml` with default configuration.

2. **Configure your prompt** (`.velor/velor.toml`):

```toml
[prompts.default]
complete_token = "<promise>COMPLETE</promise>"
prompt = '''
You are a helpful coding assistant.
Current directory: {{ cwd }}
Git root: {{ git_root }}

{{ task }}
'''

[vars]
task = "Help me refactor this code"
```

3. **Run the agent:**

```bash
# Single-shot execution
velor once

# Iterative execution (until complete token is found)
velor auto
```

## Configuration

Configuration is loaded from multiple sources (highest precedence first):

1. CLI `--set` overrides
2. Project config: `{git_root}/.velor/velor.toml`
3. Home config: `~/.velor/velor.toml`
4. Built-in defaults

### Basic Configuration

```toml
[defaults]
# Claude binary to invoke
binary = "claude-glm"

# Permission mode for Claude Code
permission_mode = "acceptEdits"

# Protocol to use: "subprocess" or "acp"
protocol = "subprocess"

[vars]
# Default variables available in templates
project_name = "my-project"
```

### ACP Protocol Configuration

Velor supports the [Agent Client Protocol (ACP)](https://github.com/zed-industries/agent-client-protocol), a standardized JSON-RPC protocol for AI agents.

#### Why ACP?

- **Standardization** - Common protocol across different AI agents
- **Better session management** - Native support for conversation persistence
- **Structured communication** - Type-safe messages instead of parsing JSON
- **Future compatibility** - Works with any ACP-compliant agent

#### Setup ACP

1. **Install the ACP adapter:**

```bash
npm install -g @zed-industries/claude-agent-acp
```

2. **Configure Velor for ACP:**

```toml
[defaults]
protocol = "acp"
binary = "claude-agent-acp"

# ACP-specific options
[defaults.acp]
# Environment variable containing Anthropic API key
api_key_env = "ANTHROPIC_API_KEY"

# Permission handling: "allow" or "deny"
permission_mode = "allow"

# Keep adapter process alive between prompts (recommended)
persist_adapter = true
```

3. **Set your API key:**

```bash
export ANTHROPIC_API_KEY=sk-ant-...
```

4. **Run with ACP:**

```bash
velor auto
```

#### ACP Permission Modes

| Mode | Behavior |
|------|----------|
| `allow` | Automatically grant all permission requests |
| `deny` | Automatically deny all permission requests |

#### ACP Features

The Velor ACP client implements the following agent methods:

- **Filesystem** - `fs/read_text_file` for read-only file access
- **Permissions** - `session/request_permission` for permission handling
- **Notifications** - `session/notification` for streaming output

Terminal subsystem and write operations are not implemented in the MVP.

## Templates

Templates use [MiniJinja](https://github.com/Pallets/jinja2-like-engine-for-Rust) syntax with access to all variables:

### Built-in Variables

| Variable | Description |
|----------|-------------|
| `{{ cwd }}` | Current working directory |
| `{{ git_root }}` | Git repository root |
| `{{ iteration }}` | Current iteration number (auto mode) |
| `{{ var_name }}` | Any custom variable from `[vars]` section |

### Example Template

```toml
[prompts.refactor]
prompt = '''
You are a code refactoring expert.

Project: {{ project_name }}
Directory: {{ cwd }}

Task: {{ refactor_task }}
Iterations: {{ iteration }}

Please analyze and refactor the code following best practices.
'''
complete_token = "<DONE>"

[vars]
project_name = "my-app"
refactor_task = "Simplify the authentication module"
```

## Notifications

Velor can send notifications when runs complete, reach max iterations, or fail.

### Telegram Notifications

```toml
[notifications]
enabled = true
notify_on_success = true
notify_on_max_iterations = true
notify_on_failure = true
output_preview_chars = 500

[notifications.telegram]
enabled = true
bot_token_env = "TELEGRAM_BOT_TOKEN"
chat_id = "-1001234567890"
parse_mode = "MarkdownV2"
```

### macOS Notifications

```toml
[notifications.macos]
enabled = true
sound = "default"  # "Basso", "Sosumi", etc.
```

### Test Notifications

```bash
velor test-notification
```

## CLI Usage

```bash
# Show help
velor --help

# Single-shot execution
velor once [--prompt-name NAME] [--set key=value...]

# Iterative execution
velor auto [--prompt-name NAME] [--max-iterations N] [--set key=value...]

# Dry run (show rendered prompt without executing)
velor once --dry-run

# Initialize config
velor init

# Test notifications
velor test-notification
```

## Development

```bash
# Run checks (fmt, clippy, tests)
just check

# Run tests
just test

# Build
just build

# Install to ~/bin
just install

# Edit config
just edit-config

# Show available prompts
just show-prompts
```

## Architecture

```
┌─────────────────┐                    ┌──────────────────────┐
│     Velor       │   ACP Protocol     │  claude-agent-acp    │
│  (ACP Client)   │◄──────────────────►│  (ACP Agent/Server)  │
│                 │   stdio            │                      │
└─────────────────┘                    └──────────────────────┘
                                                │
                                                ▼
                                       ┌──────────────────┐
                                       │   Claude Agent   │
                                       │      SDK         │
                                       └──────────────────┘
```

### Module Structure

- **src/main.rs** - CLI entry point with clap-derived argument parsing
- **src/config.rs** - TOML configuration loading with precedence
- **src/template.rs** - MiniJinja template rendering
- **src/claude.rs** - Interface to Claude CLI binary and AgentRunner abstraction
- **src/acp.rs** - ACP client implementation
- **src/git.rs** - Git repository root discovery
- **src/retry.rs** - Exponential backoff retry logic
- **src/notification.rs** - Notification system (Telegram, macOS)

## License

UNLICENSED
