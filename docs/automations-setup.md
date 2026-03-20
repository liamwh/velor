# Velor Automations Setup Guide

## Overview

Velor Automations is a multi-repo automation system that runs scheduled tasks using Claude AI agents. It uses a single launchd service that can manage automations across multiple git repositories.

## Quick Start

### 1. Install the launchd service

```bash
# Build and install
cd /Users/liam/git/velor
cargo build --release -p velor-cli
cp target/release/vel ~/bin/vel

# (macOS only) Sign the binary to prevent "killed" errors
codesign --force --deep -s - ~/bin/vel

# Set up environment variables for API keys
cat > ~/.config/velor/.env << EOF
# API Keys for automations
ZAI_API_KEY=your_key_here
CONTEXT7_API_KEY=your_key_here
EOF

# Install the launchd service (runs every 60 seconds by default)
vel automations install --interval 60

# Verify it's running
vel automations service-status
```

### 2. Register a project

```bash
# Register the current repository
vel project add .

# Or register a specific path
vel project add ~/git/my-project --id my-project

# List registered projects
vel project list
```

### 3. Create an automation

Automations are defined as TOML files in either:
- **Global**: `~/.config/velor/automations/*.toml` (runs for all projects)
- **Project**: `{repo}/.velor/automations/*.toml` (runs only for that project)

Example automation (`{repo}/.velor/automations/daily-summary.toml`):

```toml
description = "Generate a daily summary of recent changes"
schedule = "0 0 9 * * *"  # 9 AM UTC every day
timezone = "UTC"

prompt = """You are generating a daily summary of this repository.

Variables available:
- iteration: {{iteration}}
- git_root: {{git_root}}
- cwd: {{cwd}}
- timestamp: {{timestamp}}

Please:
1. Check the last 24 hours of git commits
2. Summarize significant changes
3. Write the summary to {{git_root}}/docs/daily-summary.md"""

enabled = true
catch_up = "skip"
max_catch_up = 0
timeout_seconds = 300
notify_on_success = false
notify_on_failure = false
```

### 4. Verify and manage automations

```bash
# List all automations
vel automations list

# List including disabled automations
vel automations list --all

# Validate automation definitions
vel automations validate

# Run an automation immediately
vel automations run daily-summary

# Check run history
vel automations status
vel automations status --name daily-summary
```

## Schedule Format

Automations use cron-like syntax with 6 fields:

```
┌───────────── second (0-59)
│ ┌───────────── minute (0-59)
│ │ ┌───────────── hour (0-23)
│ │ │ ┌───────────── day of month (1-31)
│ │ │ │ ┌───────────── month (1-12)
│ │ │ │ │ ┌───────────── day of week (0-6, 0 = Sunday)
│ │ │ │ │ │
* * * * * *
```

Examples:
- `0 * * * * *` - Every minute
- `0 */5 * * * *` - Every 5 minutes
- `0 0 9 * * *` - Every day at 9 AM
- `0 0 9 * * 1-5` - Every weekday at 9 AM
- `0 30 14 * * 1` - Every Monday at 2:30 PM

## Project Management

```bash
# Add a project
vel project add <path> [--id <identifier>]

# List all projects
vel project list

# Disable a project temporarily
vel project disable <project-id>

# Enable a disabled project
vel project enable <project-id>

# Remove a project
vel project remove <project-id>
```

## Launchd Service Management

```bash
# Install service with custom interval (seconds)
vel automations install --interval 30

# Check service status and recent logs
vel automations service-status

# Uninstall the service
vel automations uninstall
```

## Configuration

### Global Configuration

Located at `~/.config/velor/velor.toml`:

```toml
[vars]
api_key = "your-api-key"
default_model = "claude-sonnet-4-6"

[defaults]
binary = "claude-glm"

[automations]
state_db_path = ".velor/velor.db"
max_concurrent = 3
max_output_bytes = 1048576
```

### Repository Configuration

Located at `{repo}/.velor/velor.toml` (overrides global):

```toml
[vars]
project_specific_var = "value"

[automations]
# Override settings for this repository
max_concurrent = 5
```

## Variables Available in Automations

| Variable | Description |
|----------|-------------|
| `{{now}}` | Current timestamp in RFC3339 format |
| `{{iteration}}` | Current iteration number (for multi-step automations) |
| `{{git_root}}` | Git repository root path |
| `{{cwd}}` | Current working directory |
| `{{repo}}` | Repository name (derived from git_root) |
| `{{branch}}` | Current git branch name |
| Any `[vars]` from config | Custom variables from global or repo config |

## Catch-up Policies

When the automation system hasn't run for a while, you can control how missed runs are handled:

| Policy | Description |
|--------|-------------|
| `skip` | Only run if the next scheduled time has passed |
| `run_once` | Run once if any runs were missed |
| `run_all` | Run all missed schedules up to `max_catch_up` |

```toml
catch_up = "run_all"
max_catch_up = 10  # Maximum number of missed runs to execute
```

## Setting Up Automations for a Different Repository

To add a new repository to the automation system:

```bash
# 1. Navigate to the repository
cd ~/git/my-other-project

# 2. Register it with velor
vel project add .

# 3. Create the automations directory
mkdir -p .velor/automations

# 4. Create an automation file
cat > .velor/automations/my-task.toml << 'EOF'
description = "My automation task"
schedule = "0 0 * * * *"
prompt = "Do something useful"
enabled = true
EOF

# 5. Verify the automation
vel automations list

# 6. Wait for the next tick (or run manually)
vel automations run my-task
```

## Environment Variables

The launchd service needs access to API keys and other environment variables. These are loaded from `~/.config/velor/.env`:

```bash
# Create or edit ~/.config/velor/.env
cat > ~/.config/velor/.env << EOF
# API Keys (required for Claude AI)
ZAI_API_KEY=your_key_here
CONTEXT7_API_KEY=your_key_here
OPENAI_API_KEY=your_key_here

# Custom variables
MY_CUSTOM_VAR=value
EOF

# Reinstall the service to pick up changes
vel automations uninstall
vel automations install --interval 60
```

**Note**: Environment variables are only loaded when the launchd service is installed. After modifying `.env`, you must reinstall the service.

### Environment Variable Precedence

Variables from `.env` are set in the launchd environment and available to all automations. You can also use:
- **Global config vars**: `~/.config/velor/velor.toml` `[vars]` section
- **Repo config vars**: `{repo}/.velor/velor.toml` `[vars]` section
- **Automation vars**: `{repo}/.velor/automations/{name}.toml` `vars` table

These are merged with `.env` variables taking lowest precedence.

## Troubleshooting

### Automation not running

1. Check the service status:
   ```bash
   vel automations service-status
   ```

2. Check recent logs:
   ```bash
   tail ~/Library/Logs/velor/automations.log
   ```

3. Check error logs:
   ```bash
   tail ~/Library/Logs/velor/automations.error.log
   ```

4. Verify the project is registered and enabled:
   ```bash
   vel project list
   ```

5. Validate the automation definition:
   ```bash
   vel automations validate
   ```

### Automation failing

1. Run the automation manually to see errors:
   ```bash
   vel automations run <automation-name>
   ```

2. Check the run history:
   ```bash
   vel automations status --name <automation-name>
   ```

### Multiple projects not being processed

1. Verify all projects are registered:
   ```bash
   vel project list
   ```

2. Check that projects are enabled:
   ```bash
   vel project list
   # Look for ✅ (enabled) vs ❌ (disabled)
   ```

3. Check the logs to see which projects are being processed:
   ```bash
   tail ~/Library/Logs/velor/automations.log | grep "project"
   ```

### Binary not found or "killed" error (macOS)

If the binary gets "killed" when run from PATH, you need to code sign it:

```bash
# Sign the binary
codesign --force --deep -s - ~/bin/vel

# Reinstall the service
vel automations uninstall
vel automations install --interval 60
```

The `install` command now does this automatically, but if you manually copy the binary you may need to sign it.

### Environment variables not available

If automations fail with "API_KEY not set" errors:

1. Check that `~/.config/velor/.env` exists and contains your keys:
   ```bash
   cat ~/.config/velor/.env
   ```

2. Reinstall the service to pick up environment changes:
   ```bash
   vel automations uninstall
   vel automations install --interval 60
   ```

3. Verify the launchd plist includes the variables:
   ```bash
   grep -A 10 "EnvironmentVariables" ~/Library/LaunchAgents/com.liamwh.velor.plist
   ```

## File Locations

| File | Location |
|------|----------|
| Project registry | `~/.config/velor/projects.toml` |
| Global config | `~/.config/velor/velor.toml` |
| Environment variables | `~/.config/velor/.env` |
| Global automations | `~/.config/velor/automations/*.toml` |
| Launchd plist | `~/Library/LaunchAgents/com.liamwh.velor.plist` |
| Service logs | `~/Library/Logs/velor/automations.log` |
| Error logs | `~/Library/Logs/velor/automations.error.log` |
| State database | `{repo}/.velor/velor.db` |
| Lock file | `~/Library/velor/automations.lock` |

## Architecture

```
launchd (system scheduler)
  └─ Runs every 60 seconds
      └─ vel automations tick
          ├─ Acquires lock (exits if already running)
          ├─ Loads project registry
          ├─ For each enabled project:
          │   ├─ Load repo config
          │   ├─ Discover automations (global + project)
          │   └─ Run due automations
          └─ Releases lock
```

The system is designed to be:
- **Multi-repo**: One service handles all repositories
- **Idempotent**: Safe to run manually while service is active
- **Path-explicit**: No `cd` commands, all paths are explicit
- **Single-instance**: Lock file prevents overlapping runs
