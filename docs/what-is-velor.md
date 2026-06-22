# What is Velor?

Velor is a Rust-based orchestration toolchain for autonomous coding agents. It lets you define, schedule, and run AI agents (Claude Code, OpenAI Codex) against your codebase using template-based prompts, with built-in crash recovery, notifications, and a Telegram control plane.

## The problem it solves

Tools like Claude Code and Codex are powerful, but running them autonomously — across multiple repos, on a schedule, or triggered from your phone — requires glue that doesn't exist out of the box. Velor is that glue.

## How it works

1. **Write a prompt template** in `velor.toml` using MiniJinja syntax. Templates have access to built-in variables (`cwd`, `git_root`, `iteration`) and any custom vars you define.
2. **Run it** via `vel once` (single shot) or `vel auto` (loop until a completion token appears in the output). Auto mode retries on failure with exponential backoff and preserves conversation context across crashes.
3. **Velor spawns the agent binary** (e.g. `claude` or `codex`) as a subprocess, streams its output to your terminal, and watches for the completion token.

## Key capabilities

| Capability | What it does |
|---|---|
| **Template-based prompts** | MiniJinja templates with variable substitution, defined in TOML config |
| **Iterative execution** | Auto-mode loops until a `<promise>COMPLETE</promise>` token appears |
| **Crash recovery** | On failure, retries with exponential backoff; preserves conversation history so the agent can resume |
| **Multiple providers** | Claude Code (subprocess) and Codex (subprocess), plus ACP protocol support |
| **Project rules** | `.agents/rules/*.mdc` files inject context-aware instructions into prompts (like Cursor rules) |
| **Notifications** | Telegram and macOS push notifications on run completion/failure |
| **Telegram control plane** | `vel serve` runs a long-polling Telegram bot — send text or photos from your phone, Velor dispatches an agent run and replies with results |
| **Automations** | Cron-scheduled recurring agent runs with catch-up logic, SQLite-backed run history, and multi-repo project registry |
| **Secrets vault** | Encrypted secrets storage (XChaCha20-Poly1305) with OS keyring integration (macOS Keychain, Linux Secret Service) for API keys and tokens |
| **TUI** | Interactive terminal menu when invoked without a subcommand (ratatui-based) |
| **Desktop app** | Tauri-based GUI for managing sessions and runs |
| **Shell completion** | Dynamic Zsh/Bash/Fish completion that reads your current prompt names |

## The workspace

Velor is organised as a Cargo workspace with three libraries and two applications:

- **`crates/velor-core`** — agent runners, config loading, template rendering, retry logic, notifications, rules engine
- **`crates/automations`** — cron scheduler, run store (SQLite), project registry
- **`crates/velor-vault`** — encrypted secrets vault with OS keyring backends
- **`apps/velor-cli`** — the `vel` / `velor` CLI binary (`vel once`, `vel auto`, `vel serve`, etc.)
- **`apps/velor`** — Tauri desktop application

## Typical workflows

**Local iterative coding:**
```bash
vel auto --prompt refactor
```
Runs the `refactor` prompt template in a loop until the agent emits the completion token. Crashes are retried automatically.

**Trigger an agent from Telegram:**
```bash
vel serve
```
Starts a Telegram bot. Send a message to the bot, and Velor runs the matching runner profile, streaming results back to the chat. Reply to a run message to continue the session.

**Scheduled automations:**
```bash
vel automations run --all
```
Runs all due cron-scheduled automations across registered projects, with catch-up for missed runs.

## Configuration

Config lives in TOML files at two levels:
- **Global:** `~/.velor/velor.toml`
- **Project:** `{git_root}/.velor/velor.toml`

Project config overrides global config. CLI flags override both. Secrets come from the encrypted vault or environment variables — never from config files.
