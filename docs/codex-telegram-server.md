# Codex + Telegram via `vel serve`

## Overview

Vel now supports a single-binary server mode for Telegram-triggered Codex execution:

- CLI entrypoint: `vel serve`
- Telegram transport: `teloxide` long-polling
- Execution provider: `velor-core::AgentRunner` (`AgentProvider::Codex` by default for Telegram requests)

There is no separate `velor-server` runtime in this architecture.

## Architecture

### Provider abstraction

`velor-core` exposes provider-aware execution through:

- `AgentProvider` (`claude`, `codex`)
- `AgentRunner::from_config(...)`
- `AgentRunner::run_with_events(...)` for structured streaming

This keeps Telegram transport logic isolated from provider implementation details.

### `vel serve` runtime layers

`apps/velor-cli/src/serve.rs` is split by responsibility:

- **Ingress/transport**: Telegram polling + update fetch via `teloxide`
- **Normalization**: convert inbound updates into internal `NormalizedRequest`
- **Security controls**: allowlist, replay cache, rate limit, concurrency guard
- **Media pipeline**: best-photo selection, download, MIME/size validation, temp storage
- **Execution orchestration**: run Codex with event callbacks, timeout, lifecycle replies

### Streaming model

Server-side progress is driven by `AgentEvent` emitted from `run_with_events`:

- status
- tool call / tool result
- usage
- error

Progress replies are throttled before posting back to Telegram to avoid message spam.

## Telegram request flow

1. `vel serve` polls Telegram updates (`message`, `edited_message`, `channel_post`).
2. Update is deduplicated by `update_id` (in-memory replay window).
3. Request is authorized against configured chat/user allowlists.
4. Text/caption is parsed using trigger prefix (`codex:` by default, `/codex` also accepted).
5. Optional photo is normalized to highest resolution candidate.
6. Photo is fetched from Telegram, validated, and written to temporary media storage.
7. Codex executes with text prompt + image path context.
8. Progress and final result are sent back to Telegram.
9. Temporary media is cleaned up.

## Security controls implemented

- Chat allowlist from `[notifications.telegram].chat_id` (+ optional env extension)
- Optional user allowlist via `VELOR_TELEGRAM_ALLOWED_USER_IDS`
- Replay/duplicate protection (`update_id` cache with TTL)
- Sliding-window rate limiting per requester
- Concurrency cap via semaphore
- Execution timeout
- Attachment size/type validation for photos
- Structured audit logging (task/update/chat/user IDs)
- Secret sourcing from environment variables only

## Configuration

### Required Vel config

Use your existing `.velor/velor.toml` Telegram config:

```toml
[notifications.telegram]
enabled = true
bot_token_env = "TELEGRAM_BOT_TOKEN"
chat_id = "-1003873464939"
```

### Required environment

- `TELEGRAM_BOT_TOKEN` (or whichever env var name `bot_token_env` points to)

### Optional environment overrides

- `VELOR_TELEGRAM_ALLOWED_CHAT_IDS` (comma-separated extra chat IDs)
- `VELOR_TELEGRAM_ALLOWED_USER_IDS` (comma-separated user IDs)
- `VELOR_TELEGRAM_RATE_LIMIT_PER_MINUTE` (default `20`)
- `VELOR_TELEGRAM_MAX_CONCURRENT` (default `2`)
- `VELOR_TELEGRAM_TASK_TIMEOUT_SECS` (default `1800`)
- `VELOR_TELEGRAM_MAX_PHOTO_BYTES` (default `20971520`)
- `VELOR_MEDIA_DIR` (default temp dir)
- `VELOR_PROVIDER_BINARY` (explicit provider binary override)

## Running

```bash
source .env
vel serve --cwd /path/to/repo
```

Trigger examples from Telegram:

- `codex: create a file called telegram-test.txt with contents hello`
- `codex: inspect this repo and summarize the CLI architecture`
- photo + caption: `codex: explain what this screenshot implies for the bug`

## Notes

- Telegram does not deliver a bot's own outbound messages back as inbound updates in typical setups. Real execution tests should use a message sent by an authorized user/chat participant.
- `vel serve` uses polling, so webhook secret validation is not applicable in this mode.
