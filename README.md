<p align="center">
  <img src="branding/social/readme-banner-1600x480.png" alt="Velor" width="880">
</p>

<h1 align="center">Velor</h1>

<p align="center">
  <strong>A personal harness for running coding agents autonomously — from your terminal, on a schedule, or from your phone.</strong>
</p>

<p align="center">
  <a href="#what-is-velor">Overview</a> · <a href="#quick-start">Quick Start</a> · <a href="#the-tui">The TUI</a> · <a href="#telegram-control-plane">Telegram</a> · <a href="#automations">Automations</a> · <a href="#workspace-layout">Workspace</a> · <a href="#status">Status</a> · <a href="#license">License</a>
</p>

---

> [!WARNING]
> **Velor is experimental software.** It is published as-is, without stability guarantees. The streaming TUI is the most mature surface; the Telegram control plane, automations scheduler, and Tauri desktop app work but are rougher. Expect sharp edges, config churn, and behaviour changes between commits. See [Status](#status) for where the project stands.

## What is Velor

Velor is a Rust toolchain for orchestrating autonomous coding agents — [Claude Code](https://code.claude.com/) and [OpenAI Codex](https://developers.openai.com/codex/) — around *your* workflow instead of a vendor's UI.

The core idea is a simple, inspectable loop:

1. **Write a prompt template** in TOML ([MiniJinja](https://github.com/mitsuhiko/minijinja) syntax) with access to built-in variables (`cwd`, `git_root`, `iteration`) and any custom vars you define.
2. **Run it** — `vel once` for a single shot, `vel auto` to loop until the agent emits a completion token. Failures retry with exponential backoff, and conversation history is preserved across crashes so the agent resumes instead of starting over.
3. **Velor supervises the agent binary** as a subprocess (or over [ACP](https://github.com/zed-industries/agent-client-protocol)), streams output, injects project rules, watches for the completion token, and notifies you when it's done.

On top of that loop sit the things that made it a daily driver rather than a script: a streaming TUI you can steer mid-run, a Telegram bot for driving agents from your phone, a cron scheduler with catch-up, and an encrypted secrets vault.

## Features

| Capability | What it does |
|---|---|
| **Template prompts** | MiniJinja templates with variable substitution, defined per project in TOML |
| **Iterative execution** | Auto-mode loops until a `<promise>COMPLETE</promise>` token appears |
| **Crash recovery** | Exponential-backoff retries that preserve conversation context across failures |
| **Multiple providers** | Claude Code and Codex via subprocess, plus Agent Client Protocol support |
| **Streaming TUI** | Live transcript with thinking, text, and file diffs; steer or follow up mid-run |
| **Telegram control plane** | `vel serve` — send text or photos from your phone, Velor runs the agent and streams results back |
| **Automations** | Cron-scheduled recurring runs with catch-up logic and SQLite-backed history |
| **Secrets vault** | Encrypted storage (XChaCha20-Poly1305) with macOS Keychain / Linux Secret Service backends |
| **Project rules** | `.agents/rules/*.mdc` files injected into prompts by glob, like Cursor rules |
| **Notifications** | Telegram and macOS notifications on completion, max iterations, or failure |
| **Shell completion** | Dynamic Zsh/Bash/Fish/Elvish/PowerShell completion that reads your live prompt names |

## Quick start

**Prerequisites:** a working `claude` or `codex` CLI on your `PATH`, authenticated.

```sh
cargo install --git https://github.com/liamwh/velor
```

Initialise a project config:

```sh
cd your-project
vel init          # creates .velor/velor.toml
```

Define a prompt in `.velor/velor.toml`:

```toml
[prompts.refactor]
prompt = '''
You are working in {{ cwd }} (git root: {{ git_root }}).
Iteration: {{ iteration }}

Task: {{ task }}

When the task is fully complete, output exactly: {{ complete_token }}
'''
complete_token = "<promise>COMPLETE</promise>"

[vars]
task = "Simplify the authentication module"
```

Run it:

```sh
vel once              # single shot
vel auto              # loop until the completion token
vel                   # no subcommand → interactive TUI
```

## The TUI

Running `vel` without a subcommand opens a ratatui-based streaming interface — the surface Velor is most mature as.

- **Live transcript** — thinking, text, tool activity, and syntax-highlighted file-edit diffs stream in as the agent works, with bounded memory so a runaway run can't exhaust RAM
- **Mid-run steering** — press `i` to interrupt and redirect the agent, `f` to queue a follow-up message; both apply to the live session
- **Provider visibility** — `m` shows the active provider/binary/model; the title bar tracks the in-progress task
- **Discoverable keys** — `?` opens a searchable (`/`) keybinding reference
- **Scrollback and copy** — scroll the transcript, copy it to the clipboard for sharing or post-mortems

## Providers and protocols

```toml
[defaults]
provider = "claude"          # or "codex"
binary = "claude"
permission_mode = "acceptEdits"
protocol = "subprocess"      # or "acp"
```

The ACP path speaks the Agent Client Protocol over stdio via an adapter such as `@zed-industries/claude-agent-acp`, giving structured sessions instead of parsing subprocess JSON. Permission handling, session persistence, and capability detection differ per runner and are documented in [`docs/what-is-velor.md`](docs/what-is-velor.md).

## Telegram control plane

`vel serve` runs a long-polling Telegram bot as a control plane for your agents:

- Send text or photos to the bot; Velor dispatches the configured runner profile and streams progress back by editing a single message
- Route by prefix — e.g. `opus: fix the flaky test` vs `codex: …` pick different models/providers from your runner table
- **Reply to a run message to continue that session** — native resume, context intact
- Result formatting is configurable (`compact`/`standard`/`verbose`/`raw`); raw execution logs are retained under `.velor/serve-run-logs`

Full architecture, security model, and setup: [`docs/codex-telegram-server.md`](docs/codex-telegram-server.md).

## Automations

```sh
vel automations run --all
```

Cron-scheduled recurring agent runs across a multi-repo project registry, with catch-up for missed schedules and a SQLite run history. See [`docs/automations-setup.md`](docs/automations-setup.md).

## Secrets vault

API keys and tokens live in an encrypted vault (XChaCha20-Poly1305, Argon2 key derivation) with optional OS keyring integration — never in config files. See [`docs/vault.md`](docs/vault.md).

## Workspace layout

Velor is a Cargo workspace with three libraries and two applications:

| Path | Contents |
|---|---|
| `crates/velor-core` | Agent runners, config loading, template rendering, retry logic, notifications, rules engine |
| `crates/automations` | Cron scheduler, SQLite run store, project registry |
| `crates/velor-vault` | Encrypted secrets vault with OS keyring backends |
| `apps/velor-cli` | The `vel` CLI binary (`vel once`, `vel auto`, `vel serve`, TUI, …) |
| `apps/velor` | Tauri desktop application for managing sessions and runs |

## Configuration

Config is TOML, loaded with this precedence (highest first):

1. CLI `--set` overrides
2. Project config: `{git_root}/.velor/velor.toml`
3. Home config: `~/.velor/velor.toml`
4. Built-in defaults

Secrets come from environment variables or the encrypted vault — never from config files.

## Development

```sh
just check         # license gate, fmt, clippy, svelte-check
just test          # cargo nextest
just install       # build + install vel to ~/bin
```

## Status

Velor was my primary autonomous-coding harness for about seven months of daily use — including using Velor to develop Velor. That era produced the features above and hardened the crash-recovery and supervision paths the hard way.

I've since moved most of my day-to-day work to [Oh My Pi](https://omp.sh/) with my own extensions covering the parts of Velor I relied on, which buys me a maintained UI and upstream improvements for free. In the professional space, [Xirp](https://xirp.spotify.com/) from Spotify is pointing at a similar problem — agentic development grounded in real organisational context. Both are, in different ways, successors to what Velor was for me.

Velor is published as a working record of that period: a real, self-hosted agent harness that ran production-personal workloads every day. It remains experimental and is not under active feature development.

## License

Velor is **source available** under the [PolyForm Noncommercial License 1.0.0](LICENSE).

- **Permitted** — personal, hobby, educational, research, and other noncommercial use, modification, and redistribution, under the license terms.
- **Not permitted** — commercial use of any kind, including use within a commercial organisation or as part of a commercial product or service.
- **Commercial licensing** — commercial use of Velor requires a separate commercial license from the copyright holder. To discuss one, [open an issue](https://github.com/liamwh/velor/issues) or get in touch via [veloxide.dev](https://veloxide.dev).

### AI and text-and-data-mining reservation

To the maximum extent permitted by applicable law and subject to applicable statutory rights and exceptions, machine-learning training, fine-tuning, and evaluation on Velor's source code, and the creation of datasets, corpora, or models from it, are reserved to the copyright holder. Where such use is commercial it requires a separate commercial license; where it genuinely qualifies as noncommercial research, the PolyForm Noncommercial terms apply. This reservation complements the license's restriction of commercial use — it does not override statutory rights such as fair use or fair dealing.
