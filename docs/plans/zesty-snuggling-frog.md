# Notification System Implementation Plan

## Context

Add a notifications capability to Velor Agent CLI that sends notifications when runs complete (not on Ctrl+C). The first implementation will be Telegram notifications.

## Requirements

1. Generic abstraction for notifiers (enum-based, not trait objects)
2. First implementation: Telegram notifications via Bot API
3. Send notification when run completes, reaches max iterations, or fails (not on Ctrl+C)
4. Include summary: iterations completed, duration, status, truncated output

## Design Decisions

- **Enum over trait objects**: Use `enum Notifier { Telegram(TelegramNotifier), ... }` for simpler testing and no object-safety issues
- **Blocking HTTP**: Use `reqwest::blocking` (already a dependency) for simplicity
- **Failure handling**: Log errors but don't fail the run (fire-and-forget with logging)
- **Secrets**: Bot token from environment variable only, wrapped in `secrecy::SecretString`
- **Timeout**: 10-second HTTP timeout with connect timeout for faster DNS/TCP failure
- **Notification triggers**: Send on completion, max iterations reached, AND failures
- **CLI flag**: Add `--no-notify` to disable notifications per-run
- **RunStatus enum**: `Completed | MaxIterationsReached | Failed | Interrupted` (Interrupted = Ctrl+C, never notifies)

## Telegram-Specific Constraints

- **4096 character limit**: Clamp messages after escaping (escaping can expand text)
- **MarkdownV2 escaping**: Fussy rules, one missed character fails the whole message
- **Separate header + preview**: Consider splitting into two messages if combined exceeds limit
- **Strip ANSI codes**: Remove terminal color codes before sending (breaks Markdown parsing)

## Implementation

### 1. New Module: `src/notification.rs`

**Core types:**
```rust
/// Result of a completed run for notifications.
pub struct NotificationPayload {
    pub mode: RunMode,              // "once" or "auto"
    pub iterations_completed: u32,
    pub max_iterations: u32,
    pub duration: Duration,
    pub status: RunStatus,
    pub output_preview: Option<String>,
    pub prompt_name: String,
    pub started_at: DateTime<Utc>,
    pub ended_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    /// Completion token detected - successful finish
    Completed,
    /// Ran all iterations without complete token
    MaxIterationsReached,
    /// Error occurred during execution
    Failed,
    /// User interrupted with Ctrl+C (never notifies)
    Interrupted,
}

/// Enum-based notifier (simpler than trait objects for small number of backends)
#[derive(Debug)]
pub enum Notifier {
    Telegram(TelegramNotifier),
    // Future: Slack(SlackNotifier), Discord(DiscordNotifier), etc.
}

impl Notifier {
    pub fn notify(&self, payload: &NotificationPayload) -> Result<()> {
        match self {
            Self::Telegram(n) => n.notify(payload),
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Telegram(_) => "Telegram",
        }
    }
}
```

**Telegram implementation:**
```rust
use secrecy::{ExposeSecret, SecretString};

pub struct TelegramNotifier {
    bot_token: SecretString,  // Never logged/debug-printed
    chat_id: String,
    api_base_url: url::Url,
    parse_mode: Option<TelegramParseMode>,
    http_client: reqwest::blocking::Client,
}

impl TelegramNotifier {
    pub fn new(config: TelegramConfig) -> Result<Self> {
        let bot_token: SecretString = std::env::var(&config.bot_token_env)
            .map(SecretString::new)
            .map_err(|_| Error::MissingBotToken(config.bot_token_env.clone()))?;

        let http_client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(10))
            .connect_timeout(Duration::from_secs(5))
            .user_agent("velor-agent-cli")
            .build()?;

        let api_base_url = config.api_base_url
            .unwrap_or_else(|| "https://api.telegram.org".parse().unwrap());

        Ok(Self { bot_token, chat_id: config.chat_id, api_base_url, parse_mode: config.parse_mode, http_client })
    }

    fn format_message(&self, payload: &NotificationPayload) -> String {
        // 1. Build message parts (header, body, preview)
        // 2. Strip ANSI codes from preview
        // 3. Escape for MarkdownV2 if needed
        // 4. Clamp to 4096 chars AFTER escaping
    }

    fn escape_markdown_v2(&self, text: &str) -> String {
        // Escape: _ * [ ] ( ) ~ ` > # + - = | { } . !
    }
}

impl TelegramNotifier {
    pub fn notify(&self, payload: &NotificationPayload) -> Result<()> {
        let url = self.api_base_url
            .join(&format!("bot{}/sendMessage", self.bot_token.expose_secret()))?;

        let text = self.format_message(payload);

        // Clamp after escaping
        let text = if text.len() > 4096 { &text[..4096] } else { &text };

        // POST JSON request...
    }
}
```

**Message formatting (separate from transport):**
```rust
/// Formats a notification message for Telegram.
/// Testable in isolation without HTTP.
pub fn format_telegram_message(payload: &NotificationPayload, config: &TelegramConfig) -> String {
    let status_emoji = match payload.status {
        RunStatus::Completed => "✅",
        RunStatus::MaxIterationsReached => "⚠️",
        RunStatus::Failed => "❌",
        RunStatus::Interrupted => "🛑",
    };

    let duration = humantime::format_duration(payload.duration);

    // Build message, strip ANSI from preview, escape, clamp
}
```

**Factory function:**
```rust
pub fn build_notifiers(config: &NotificationsConfig) -> Result<Vec<Notifier>>;
pub fn send_notifications(notifiers: &[Notifier], payload: &NotificationPayload, config: &NotificationsConfig);
```

### 2. Configuration: `src/config.rs`

Add to `FileConfig`:
```rust
#[serde(default)]
pub notifications: NotificationsConfig,
```

New config structs:
```rust
pub struct NotificationsConfig {
    pub enabled: bool,                    // default: false
    pub notify_on_success: bool,          // default: true (Completed status)
    pub notify_on_max_iterations: bool,   // default: true (explicit control)
    pub notify_on_failure: bool,          // default: true (Failed status)
    pub output_preview_chars: u32,        // default: 500
    pub telegram: Option<TelegramConfig>,
}

pub struct TelegramConfig {
    pub enabled: bool,
    pub bot_token_env: String,            // env var name, default: "TELEGRAM_BOT_TOKEN"
    pub chat_id: String,                  // required
    pub api_base_url: Option<String>,     // optional proxy URL (validated with `url` crate)
    pub parse_mode: Option<TelegramParseMode>, // MarkdownV2, HTML, or None
}
```

Example `velor.toml`:
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

### 3. Integration: `src/main.rs`

**Modify `run_auto_loop`:**
- Track start time and final output
- Return `AutoLoopResult` instead of `()`
- Include status (Completed vs MaxIterationsReached)
- **Handle Ctrl+C**: Use `ctrlc` crate or check for signal, set status to `Interrupted`

**Modify `run_auto`:**
- Build notifiers from config
- After `run_auto_loop` completes, build `NotificationPayload`
- Call `send_notifications` (short-circuits if status is `Interrupted`)
- Handle both success and error cases

**Add CLI flag:**
```rust
/// Disable notifications for this run.
#[arg(long)]
no_notify: bool,
```

**Ctrl+C handling (optional but recommended):**
```rust
// In run_auto, before starting loop:
let interrupted = Arc::new(AtomicBool::new(false));
let r = interrupted.clone();
ctrlc::set_handler(move || {
    r.store(true, Ordering::SeqCst);
}).expect("Error setting Ctrl-C handler");

// Check in loop and set status accordingly
```

### 4. Files to Modify

| File | Changes |
|------|---------|
| `src/notification.rs` | **NEW** - Notifier enum, TelegramNotifier, format functions, error types |
| `src/config.rs` | Add `NotificationsConfig`, `TelegramConfig`, update `FileConfig` |
| `src/main.rs` | Add `mod notification`, modify `run_auto_loop` to return result, add notification calls |
| `Cargo.toml` | Add: humantime, strip-ansi-escapes, secrecy, url, wiremock (dev) |
| `.velor/velor.toml` | Add example `[notifications]` section |

### 5. Telegram Message Format

Example notification:
```
✅ Velor Run Completed

Mode: auto
Prompt: implement-plan
Iterations: 5/25
Duration: 12m 34s
Status: Completed

Preview:
...final output truncated to 500 chars...
```

**Note**: Message is built in `format_telegram_message()`, tested separately from HTTP transport. Clamp to 4096 chars after escaping.

## New Dependencies

Add to `Cargo.toml`:
```toml
[dependencies]
# ... existing ...
humantime = "2"              # Duration formatting (12m 34s)
strip-ansi-escapes = "0.2"   # Remove terminal color codes from preview
secrecy = "0.8"              # SecretString for bot token
url = "2"                    # URL validation/joining for api_base_url
ctrlc = "3"                  # Ctrl+C signal handling

[dev-dependencies]
# ... existing ...
wiremock = "0.6"             # HTTP mocking for Telegram API tests
```

**Note**: `thiserror` not added since project uses `color-eyre`. We'll use `color_eyre::eyre::eyre!` for errors.

## Testing

### Unit Tests
- `Notifier` enum dispatch
- `TelegramNotifier::new` validation (missing env var)
- `escape_markdown_v2` with all special characters
- `format_telegram_message` output structure
- ANSI stripping from preview
- 4096 char clamping (after escaping)
- `RunStatus::Interrupted` never triggers notification

### Property Tests (proptest)
- MarkdownV2 escaping preserves alphanumeric
- Escaping + clamping never exceeds 4096 chars
- Duration formatting with humantime

### HTTP Mocking (wiremock)
```rust
#[test]
fn test_telegram_sends_correct_request() {
    let server = MockServer::start();

    // Set api_base_url to server.uri()
    // Call notify()
    // Assert request to /bot{token}/sendMessage
    // Assert JSON body has expected fields
}

#[test]
fn test_telegram_handles_error_response() {
    // Mock returns 429 Too Many Requests
    // Assert error is returned but doesn't panic
}
```

### Integration Tests
- Full flow: config → build_notifiers → send_notifications with mock
- Interrupted status short-circuits notification logic

### Manual Test
1. Configure Telegram in `velor.toml`
2. Set `TELEGRAM_BOT_TOKEN` env var
3. Run `velor auto`
4. Verify notification received on phone/client

## Verification

1. `cargo check -q` - Compiles without errors
2. `cargo nextest run` - All tests pass (unit + property + wiremock)
3. `cargo clippy` - No warnings
4. Manual test: Configure Telegram, run `velor auto`, verify notification received
5. Test `--no-notify` flag works
6. Test Ctrl+C doesn't send notification (status = Interrupted)

## Implementation Order

1. **Add dependencies** to `Cargo.toml`
2. **Config structs** in `src/config.rs` with tests
3. **Core types** in `src/notification.rs` (RunStatus, NotificationPayload, Notifier enum)
4. **format_telegram_message()** with comprehensive tests (escaping, clamping, ANSI stripping)
5. **TelegramNotifier** with wiremock tests
6. **Integration** in `run_auto_loop` and `run_auto`
7. **Ctrl+C handling** using `ctrlc` crate, set status to `Interrupted`
