# Telegram Runner Control Plane via `vel serve`

## Overview

`vel serve` is a single-binary Telegram control plane built into `vel`.

- CLI entrypoint: `vel serve`
- Telegram transport: `teloxide` long-polling
- Router: prefix/command-based dispatch to named runner profiles
- Execution model: provider-agnostic runner profile registry (`claude` + `codex` today)

There is no separate server binary.

## Architecture

`apps/velor-cli/src/serve.rs` is organized as layered runtime logic:

1. Transport adapter: poll Telegram updates and normalize message payloads.
2. Security gate: chat/user allowlists, replay cache, rate limit, concurrency limits.
3. Command router: `/help`, `/models`, `/status` and prefix-to-runner routing.
4. Request model: transport metadata + runner selection + prompt + attachment metadata.
5. Execution runner: process-backed `claude` and `codex` profile execution.
6. Telegram notifier: ack/progress/final lifecycle responses.

Telegram-specific types stay in the transport layer; execution uses internal request/profile models.

## Prefix and command UX

### Prefix dispatch

Messages with configured prefixes dispatch to mapped runner profiles.

Default mappings:

- `opus:` -> `claude-opus-4-6`
- `sonnet:` -> `claude-sonnet-4-6`
- `5.3-codex:` -> `codex-gpt-5-3-codex`
- `5.3:` -> `codex-gpt-5-3-codex`
- `5.4:` -> `codex-gpt-5-4`
- `glm5.1:` -> `glm-5-1`
- `5.1:` -> `glm-5-1`
- `codex:` -> `codex-gpt-5-4`

### Reply-based continuation

If a user replies directly to a bot run message, `vel serve` resumes the exact underlying
runner session (Claude `--resume` or Codex `exec resume`) and sends the reply as the next turn.
No prefix is required for these reply continuations.

### Slash commands

- `/help` usage + configured prefixes
- `/models` configured profiles + model + prefix mapping
- `/status` uptime, allowlist summary, runner availability probes

## Attachments

`vel serve` supports photo and document inputs:

- photo: highest-resolution variant selected
- document: file metadata captured from Telegram message
- caption text is used as request text (same path as text messages)
- files are downloaded, MIME/size validated, persisted to media dir
- attachment metadata + local paths are appended to runner prompt context
- codex profiles additionally receive image paths via `--image` when MIME is `image/*`

## Security controls

Implemented controls:

- explicit chat allowlist (from `[notifications.telegram].chat_id` + optional `[serve.telegram].allowed_chat_ids`)
- optional user allowlist (`[serve.telegram].allowed_user_ids`)
- required prefix routing by default (`[serve.routing].require_prefix = true`)
- unknown prefix rejection with clean usage responses
- in-memory replay protection by Telegram `update_id`
- per-actor sliding-window rate limiting
- concurrency limits via semaphore
- runner timeout per profile
- attachment size and MIME validation
- structured audit logs (request/chat/user/runner IDs)

## Configuration

`vel serve` uses existing `[notifications.telegram]` for token + base chat allowlist, plus a dedicated `[serve]` runtime section.

```toml
[notifications.telegram]
enabled = true
bot_token_env = "TELEGRAM_BOT_TOKEN"
chat_id = "-1003873464939"

[serve]
enabled = true
poll_timeout_secs = 10
poll_limit = 50
include_backlog = false
max_requests_per_minute = 20
max_concurrent_tasks = 2
default_timeout_secs = 1800
media_dir = ".velor/telegram-media"

[serve.streaming]
enabled = true
edit_throttle_secs = 1
max_message_chars = 3600
flush_on_milestones = true

[serve.telegram]
allowed_chat_ids = []
allowed_user_ids = []
allow_channel_posts = true

[serve.attachments]
enabled = true
allow_photos = true
allow_documents = true
max_download_bytes = 20971520
keep_files = false
allowed_document_mime_prefixes = ["image/", "text/", "application/pdf", "application/json", "application/xml"]

[serve.session_resume]
enabled = true
store_path = ".velor/telegram-session-bindings.json"
max_bindings = 10000

[serve.routing]
require_prefix = true
default_runner = "codex-gpt-5-4"

[serve.routing.prefixes]
"sonnet:" = "claude-sonnet-4-6"
"opus:" = "claude-opus-4-6"
"5.3-codex:" = "codex-gpt-5-3-codex"
"5.3:" = "codex-gpt-5-3-codex"
"5.4:" = "codex-gpt-5-4"
"glm5.1:" = "glm-5-1"
"5.1:" = "glm-5-1"

[serve.runners."claude-sonnet-4-6"]
kind = "claude"
binary = "claude"
model = "claude-sonnet-4-6"

[serve.runners."codex-gpt-5-4"]
kind = "codex"
binary = "codex"
model = "gpt-5.4"
[serve.runners."codex-gpt-5-4".codex]
full_auto = true
sandbox = "workspace-write"

[serve.runners."codex-gpt-5-3-codex".codex]
reasoning_effort = "xhigh"

[serve.runners."glm-5-1"]
kind = "claude"
binary = "/Users/liam/bin/glm5.1"
model = "glm-5.1"
```

## Running

```bash
source .env
vel serve
```

`vel serve` defaults runner execution cwd to `~/git`. Override with `--cwd` when needed.

## Notes

- Telegram long-polling mode does not support webhook-signature validation.
- Telegram does not normally redeliver bot-originated outbound messages as inbound updates; real execution tests must come from an authorized human sender in the configured chat.
- `progress_update_interval_secs` is still accepted as a legacy fallback for edit throttle when `[serve.streaming].edit_throttle_secs` is not provided.
- `vel serve` loads runner/streaming config at process startup; after changing `.velor/velor.toml`, restart the service (`just serve-ensure-running`) to apply updates.
- Runner profile model settings support Codex reasoning control with `reasoning_effort = "low|medium|high|xhigh"` under `[serve.runners."<name>".codex]`.
- `codex-gpt-5-3-codex` is enforced to `xhigh` reasoning at runtime as a policy guardrail.
