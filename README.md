# Velor Agent CLI

Velor is a Rust-based toolchain for running autonomous coding agents (Claude and Codex) with template-based prompts, iterative execution, and crash recovery.

## Features

- **Template-based prompts** - Use MiniJinja templates with variable substitution
- **Iterative execution** - Run agents until completion with automatic retry logic
- **Crash recovery** - Preserve conversation context across failures
- **Provider abstraction** - Switch between Claude and Codex with clean provider-level config
- **Multiple protocols** - Support for Claude subprocess and ACP (Agent Client Protocol)
- **Configuration management** - TOML-based config with home and project-level overrides
- **Notifications** - Telegram and macOS notification support
- **Telegram control plane** - `vel serve` long-polling runtime for Telegram text/photo -> configurable runner profiles

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
# Agent provider: "claude" or "codex"
provider = "claude"

# Provider binary to invoke
binary = "claude-glm"

# Permission mode for Claude Code
permission_mode = "acceptEdits"

# Protocol to use: "subprocess" or "acp"
protocol = "subprocess"

[defaults.codex]
full_auto = true
sandbox = "workspace-write"
skip_git_repo_check = false
progress_cursor = false

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

## Telegram Runner Server

Velor includes a first-class server mode in the main CLI binary:

```bash
vel serve
```

By default, Telegram-triggered runner executions use `~/git` as the working directory. Use `--cwd` to override.
Replying to a bot run message in Telegram continues the same underlying runner session (native resume).

For full architecture, security, setup, and operational guidance, see:

- [`docs/codex-telegram-server.md`](docs/codex-telegram-server.md)

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

## Shell Completion

Velor provides shell completion for the `--prompt` argument and all subcommands. The completion is **dynamic** - it reads your current prompts at runtime, so it always stays accurate.

### Zsh (First-class Support)

Zsh has full support with dynamic prompt completion:

```bash
# Option 1: Eval completion (simplest)
# Add to ~/.zshrc
eval "$(velor completion --shell zsh)"

# Option 2: Source from file (more robust)
# Add to ~/.zshrc
mkdir -p ~/.zsh/completion
velor completion --shell zsh > ~/.zsh/completion/_velor
fpath=(~/.zsh/completion $fpath)
autoload -U compinit && compinit
```

After reloading your shell, press `<TAB>` after `--prompt` to see available prompts:

```bash
velor once --prompt <TAB>
# Shows: default, refactor, debug, etc.
```

### Other Shells

For Bash, Fish, Elvish, and PowerShell:

```bash
# Generate completion script
velor completion --shell bash > ~/.local/share/bash-completion/completions/velor
velor completion --shell fish > ~/.config/fish/completions/velor.fish
velor completion --shell elvish > ~/.elvish/lib/velor.elv
velor completion --shell powershell > ~/.config/powershell/velor.ps1
```

### Fuzzy Finding (Optional)

For enhanced prompt selection with fzf, add to your `~/.zshrc`:

```bash
_velor_fzf_prompt() {
    local prompt=$(velor internal complete-prompts | fzf --prompt="Select prompt: ")
    if [[ -n "$prompt" ]]; then
        LBUFFER+="$prompt"
        zle reset-prompt
    fi
}
zle -N _velor_fzf_prompt
bindkey '^g' _velor_fzf_prompt  # Ctrl+G to open fzf selector
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
