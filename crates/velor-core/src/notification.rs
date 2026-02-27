//! Notification system for Velor Agent CLI.
//!
//! This module provides notification capabilities that send messages when runs complete,
//! reach max iterations, or fail. Supports Telegram and macOS notifications.

use color_eyre::eyre::{WrapErr, eyre};
use secrecy::{ExposeSecret, SecretString};
use std::time::Duration;
use url::Url;

use crate::config::{MacOSConfig, NotificationsConfig, TelegramConfig, TelegramParseMode};

/// Default Telegram API base URL.
const DEFAULT_TELEGRAM_API_URL: &str = "https://api.telegram.org";

/// Result of a completed run for notifications.
#[derive(Debug, Clone)]
pub struct NotificationPayload {
    /// Execution mode ("once" or "auto").
    pub mode: &'static str,
    /// Number of iterations completed.
    pub iterations_completed: u32,
    /// Maximum iterations allowed.
    pub max_iterations: u32,
    /// Total duration of the run.
    pub duration: Duration,
    /// Final status of the run.
    pub status: RunStatus,
    /// Preview of the output (truncated).
    pub output_preview: Option<String>,
    /// Name of the prompt used.
    pub prompt_name: String,
}

/// Status of a completed run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    /// Completion token detected - successful finish.
    Completed,
    /// Ran all iterations without complete token.
    MaxIterationsReached,
    /// Error occurred during execution.
    Failed,
    /// User cancelled via Ctrl+C.
    Cancelled,
}

impl RunStatus {
    /// Returns the emoji for this status.
    #[must_use]
    pub const fn emoji(&self) -> &'static str {
        match self {
            Self::Completed => "✅",
            Self::MaxIterationsReached => "⚠️",
            Self::Failed => "❌",
            Self::Cancelled => "🛑",
        }
    }

    /// Returns the human-readable label for this status.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Completed => "Completed",
            Self::MaxIterationsReached => "Max Iterations Reached",
            Self::Failed => "Failed",
            Self::Cancelled => "Cancelled",
        }
    }
}

/// Enum-based notifier (simpler than trait objects for small number of backends).
#[derive(Debug)]
pub enum Notifier {
    /// Telegram notifier.
    Telegram(TelegramNotifier),
    /// macOS notifier.
    MacOS(MacOSNotifier),
}

impl Notifier {
    /// Sends a notification with the given payload.
    ///
    /// # Errors
    ///
    /// Returns an error if the notification fails to send.
    #[tracing::instrument(level = "debug", ret, err, skip(self))]
    pub fn notify(&self, payload: &NotificationPayload) -> color_eyre::eyre::Result<()> {
        match self {
            Self::Telegram(n) => n.notify(payload),
            Self::MacOS(n) => n.notify(payload),
        }
    }

    /// Returns the name of this notifier type.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Telegram(_) => "Telegram",
            Self::MacOS(_) => "macOS",
        }
    }
}

/// Telegram notifier implementation.
#[derive(Debug)]
pub struct TelegramNotifier {
    /// Bot token (never logged).
    bot_token: SecretString,
    /// Chat ID to send messages to.
    chat_id: String,
    /// API base URL (for proxies).
    api_base_url: url::Url,
    /// Parse mode for messages.
    parse_mode: Option<TelegramParseMode>,
    /// HTTP client.
    http_client: reqwest::blocking::Client,
    /// Maximum characters for output preview.
    output_preview_chars: u32,
}

impl TelegramNotifier {
    /// Creates a new Telegram notifier from configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The bot token environment variable is not set
    /// - The HTTP client fails to build
    /// - The API base URL is invalid
    #[tracing::instrument(level = "debug", ret, err)]
    pub fn new(
        config: &TelegramConfig,
        output_preview_chars: u32,
    ) -> color_eyre::eyre::Result<Self> {
        let bot_token: SecretString = std::env::var(&config.bot_token_env)
            .map(SecretString::new)
            .wrap_err_with(|| {
                format!(
                    "Telegram bot token not found. Set the {} environment variable.",
                    config.bot_token_env
                )
            })?;

        let http_client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(10))
            .connect_timeout(Duration::from_secs(5))
            .user_agent("velor-agent-cli")
            .build()
            .wrap_err("failed to build HTTP client for Telegram")?;

        let api_base_url = config
            .api_base_url
            .as_ref()
            .map(|s| s.parse::<Url>())
            .transpose()
            .wrap_err("invalid Telegram API base URL")?
            .unwrap_or_else(|| {
                Url::parse(DEFAULT_TELEGRAM_API_URL)
                    .expect("default Telegram API URL should be valid")
            });

        Ok(Self {
            bot_token,
            chat_id: config.chat_id.clone(),
            api_base_url,
            parse_mode: config.parse_mode,
            http_client,
            output_preview_chars,
        })
    }

    /// Sends a notification to Telegram.
    ///
    /// # Errors
    ///
    /// Returns an error if the HTTP request fails or Telegram returns an error.
    #[tracing::instrument(level = "debug", ret, err, skip(self))]
    pub fn notify(&self, payload: &NotificationPayload) -> color_eyre::eyre::Result<()> {
        let url = self
            .api_base_url
            .join(&format!(
                "bot{}/sendMessage",
                self.bot_token.expose_secret()
            ))
            .wrap_err("failed to construct Telegram API URL")?;

        let text = format_telegram_message(payload, self.output_preview_chars, self.parse_mode);

        let mut body = serde_json::json!({
            "chat_id": self.chat_id,
            "text": text,
        });

        if let Some(mode) = self.parse_mode {
            body["parse_mode"] = serde_json::json!(match mode {
                TelegramParseMode::MarkdownV2 => "MarkdownV2",
                TelegramParseMode::Html => "HTML",
            });
        }

        let response = self
            .http_client
            .post(url.clone())
            .json(&body)
            .send()
            .wrap_err_with(|| {
                format!(
                    "failed to send Telegram notification request to chat {} (API URL: {})",
                    self.chat_id, url
                )
            })?;

        let status = response.status();
        if !status.is_success() {
            let body = response
                .text()
                .unwrap_or_else(|_| "<unable to read response body>".to_string());
            return Err(eyre!(
                "Telegram API returned error {status} for chat {}: {body}",
                self.chat_id
            ));
        }

        Ok(())
    }
}

/// macOS notifier using osascript (AppleScript).
#[derive(Debug)]
pub struct MacOSNotifier {
    /// Sound to play with notification.
    sound: Option<String>,
    /// Maximum characters for output preview.
    output_preview_chars: u32,
}

impl MacOSNotifier {
    /// Creates a new macOS notifier from configuration.
    #[must_use]
    pub fn new(config: &MacOSConfig, output_preview_chars: u32) -> Self {
        Self {
            sound: config.sound.clone(),
            output_preview_chars,
        }
    }

    /// Sends a notification via macOS Notification Center.
    ///
    /// # Errors
    ///
    /// Returns an error if osascript fails to execute.
    #[tracing::instrument(level = "debug", ret, err, skip(self))]
    pub fn notify(&self, payload: &NotificationPayload) -> color_eyre::eyre::Result<()> {
        let title = format!(
            "{} Velor Run {}",
            payload.status.emoji(),
            payload.status.label()
        );
        let message = format_macos_message(payload, self.output_preview_chars);

        // Build AppleScript command
        let sound_clause = self
            .sound
            .as_ref()
            .map(|s| format!(" sound name \"{}\"", escape_applescript_string(s)))
            .unwrap_or_default();

        let script = format!(
            "display notification \"{}\" with title \"{}\"{}",
            escape_applescript_string(&message),
            escape_applescript_string(&title),
            sound_clause
        );

        // Execute osascript
        let output = std::process::Command::new("osascript")
            .arg("-e")
            .arg(&script)
            .output()
            .wrap_err("failed to execute osascript for macOS notification")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            return Err(eyre!(
                "macOS notification via osascript failed (exit code: {}): stderr={}, stdout={}",
                output.status.code().unwrap_or(-1),
                stderr,
                stdout
            ));
        }

        Ok(())
    }
}

/// Formats a notification message for Telegram.
///
/// This function is testable in isolation from HTTP transport.
///
/// # Arguments
///
/// * `payload` - The notification payload containing run details
/// * `output_preview_chars` - Maximum characters for output preview
/// * `parse_mode` - Optional parse mode (affects escaping)
///
/// # Returns
///
/// A formatted message string, clamped to 4096 characters (Telegram's limit).
#[must_use]
pub fn format_telegram_message(
    payload: &NotificationPayload,
    output_preview_chars: u32,
    parse_mode: Option<TelegramParseMode>,
) -> String {
    let duration_str = humantime::format_duration(payload.duration);
    let status_emoji = payload.status.emoji();
    let status_label = payload.status.label();

    // Build the header
    let mut message = format!(
        "{status_emoji} Velor Run {status_label}\n\n\
         Mode: {}\n\
         Prompt: {}\n\
         Iterations: {}/{}\n\
         Duration: {duration_str}\n\
         Status: {status_label}",
        payload.mode, payload.prompt_name, payload.iterations_completed, payload.max_iterations
    );

    // Add output preview if available
    if let Some(preview) = &payload.output_preview {
        let preview = strip_ansi_escapes::strip_str(preview);
        let preview = truncate_str(&preview, output_preview_chars as usize);
        if !preview.is_empty() {
            message.push_str("\n\nPreview:\n");
            message.push_str(preview);
        }
    }

    // Escape for MarkdownV2 if needed
    let message = if parse_mode == Some(TelegramParseMode::MarkdownV2) {
        escape_markdown_v2(&message)
    } else {
        message
    };

    // Clamp to 4096 characters (Telegram's limit)
    truncate_str(&message, 4096).to_string()
}

/// Escapes special characters for Telegram MarkdownV2 format.
///
/// Characters that need escaping: _ * [ ] ( ) ~ ` > # + - = | { } . !
#[must_use]
pub fn escape_markdown_v2(text: &str) -> String {
    let mut result = String::with_capacity(text.len() * 2);
    for c in text.chars() {
        match c {
            '_' | '*' | '[' | ']' | '(' | ')' | '~' | '`' | '>' | '#' | '+' | '-' | '=' | '|'
            | '{' | '}' | '.' | '!' => {
                result.push('\\');
                result.push(c);
            }
            _ => result.push(c),
        }
    }
    result
}

/// Formats a notification message for macOS (plain text).
///
/// # Arguments
///
/// * `payload` - The notification payload containing run details
/// * `output_preview_chars` - Maximum characters for output preview
///
/// # Returns
///
/// A formatted message string, truncated to 200 characters (macOS notification limit).
#[must_use]
pub fn format_macos_message(payload: &NotificationPayload, output_preview_chars: u32) -> String {
    let duration_str = humantime::format_duration(payload.duration);

    let mut message = format!(
        "Mode: {}\nPrompt: {}\nIterations: {}/{}\nDuration: {}",
        payload.mode,
        payload.prompt_name,
        payload.iterations_completed,
        payload.max_iterations,
        duration_str
    );

    if let Some(preview) = &payload.output_preview {
        let preview = strip_ansi_escapes::strip_str(preview);
        let preview = truncate_str(&preview, output_preview_chars as usize);
        if !preview.is_empty() {
            message.push_str("\n\n");
            message.push_str(preview);
        }
    }

    // macOS notifications have a character limit around 256 chars for the body
    truncate_str(&message, 200).to_string()
}

/// Escapes special characters for AppleScript strings.
#[must_use]
pub fn escape_applescript_string(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Truncates a string to at most `max_len` characters, respecting character boundaries.
#[must_use]
fn truncate_str(s: &str, max_len: usize) -> &str {
    if s.len() <= max_len {
        return s;
    }

    // Find the largest valid char boundary <= max_len
    let mut end = max_len;
    while !s.is_char_boundary(end) && end > 0 {
        end -= 1;
    }
    &s[..end]
}

/// Builds notifiers from configuration.
///
/// # Errors
///
/// Returns an error if a notifier fails to initialize (e.g., missing env var).
#[tracing::instrument(level = "debug", ret, err)]
pub fn build_notifiers(config: &NotificationsConfig) -> color_eyre::eyre::Result<Vec<Notifier>> {
    let mut notifiers = Vec::new();

    if let Some(telegram_config) = &config.telegram
        && telegram_config.enabled
    {
        let notifier = TelegramNotifier::new(telegram_config, config.output_preview_chars)?;
        notifiers.push(Notifier::Telegram(notifier));
    }

    if let Some(macos_config) = &config.macos
        && macos_config.enabled
    {
        let notifier = MacOSNotifier::new(macos_config, config.output_preview_chars);
        notifiers.push(Notifier::MacOS(notifier));
    }

    Ok(notifiers)
}

/// Sends notifications to all configured notifiers.
///
/// This function is fire-and-forget: errors are logged but don't fail the run.
#[tracing::instrument(level = "debug", skip(notifiers))]
pub fn send_notifications(notifiers: &[Notifier], payload: &NotificationPayload) {
    for notifier in notifiers {
        if let Err(e) = notifier.notify(payload) {
            tracing::error!("Failed to send {} notification: {}", notifier.name(), e);
        }
    }
}

/// Checks if a notification should be sent based on the status and configuration.
#[must_use]
pub const fn should_notify(status: RunStatus, config: &NotificationsConfig) -> bool {
    if !config.enabled {
        return false;
    }

    match status {
        RunStatus::Completed => config.notify_on_success,
        RunStatus::MaxIterationsReached => config.notify_on_max_iterations,
        RunStatus::Failed => config.notify_on_failure,
        RunStatus::Cancelled => config.notify_on_failure,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_payload(status: RunStatus) -> NotificationPayload {
        NotificationPayload {
            mode: "auto",
            iterations_completed: 5,
            max_iterations: 50,
            duration: Duration::from_secs(754), // 12m 34s
            status,
            output_preview: Some("Test output with ANSI: \x1b[32mgreen\x1b[0m".to_string()),
            prompt_name: "implement-plan".to_string(),
        }
    }

    #[test]
    fn test_run_status_emoji() {
        assert_eq!(RunStatus::Completed.emoji(), "✅");
        assert_eq!(RunStatus::MaxIterationsReached.emoji(), "⚠️");
        assert_eq!(RunStatus::Failed.emoji(), "❌");
        assert_eq!(RunStatus::Cancelled.emoji(), "🛑");
    }

    #[test]
    fn test_run_status_label() {
        assert_eq!(RunStatus::Completed.label(), "Completed");
        assert_eq!(
            RunStatus::MaxIterationsReached.label(),
            "Max Iterations Reached"
        );
        assert_eq!(RunStatus::Failed.label(), "Failed");
        assert_eq!(RunStatus::Cancelled.label(), "Cancelled");
    }

    #[test]
    fn test_format_telegram_message_completed() {
        let payload = test_payload(RunStatus::Completed);
        let message = format_telegram_message(&payload, 500, None);

        assert!(message.contains("✅"));
        assert!(message.contains("Completed"));
        assert!(message.contains("Mode: auto"));
        assert!(message.contains("Prompt: implement-plan"));
        assert!(message.contains("Iterations: 5/50"));
        assert!(message.contains("12m 34s"));
    }

    #[test]
    fn test_format_telegram_message_strips_ansi() {
        let payload = test_payload(RunStatus::Completed);
        let message = format_telegram_message(&payload, 500, None);

        // ANSI codes should be stripped
        assert!(!message.contains("\x1b["));
        assert!(!message.contains("[32m"));
    }

    #[test]
    fn test_escape_markdown_v2() {
        // Test all special characters
        assert_eq!(escape_markdown_v2("_"), "\\_");
        assert_eq!(escape_markdown_v2("*"), "\\*");
        assert_eq!(escape_markdown_v2("["), "\\[");
        assert_eq!(escape_markdown_v2("]"), "\\]");
        assert_eq!(escape_markdown_v2("("), "\\(");
        assert_eq!(escape_markdown_v2(")"), "\\)");
        assert_eq!(escape_markdown_v2("~"), "\\~");
        assert_eq!(escape_markdown_v2("`"), "\\`");
        assert_eq!(escape_markdown_v2(">"), "\\>");
        assert_eq!(escape_markdown_v2("#"), "\\#");
        assert_eq!(escape_markdown_v2("+"), "\\+");
        assert_eq!(escape_markdown_v2("-"), "\\-");
        assert_eq!(escape_markdown_v2("="), "\\=");
        assert_eq!(escape_markdown_v2("|"), "\\|");
        assert_eq!(escape_markdown_v2("{"), "\\{");
        assert_eq!(escape_markdown_v2("}"), "\\}");
        assert_eq!(escape_markdown_v2("."), "\\.");
        assert_eq!(escape_markdown_v2("!"), "\\!");
    }

    #[test]
    fn test_escape_markdown_v2_preserves_alphanumeric() {
        let input = "Hello World 123";
        assert_eq!(escape_markdown_v2(input), input);
    }

    #[test]
    fn test_escape_markdown_v2_complex() {
        let input = "Status: Done! (success) - 100%";
        let expected = "Status: Done\\! \\(success\\) \\- 100%";
        assert_eq!(escape_markdown_v2(input), expected);
    }

    #[test]
    fn test_truncate_str_no_truncation_needed() {
        let s = "Hello, world!";
        assert_eq!(truncate_str(s, 100), s);
    }

    #[test]
    fn test_truncate_str_exact_length() {
        let s = "Hello, world!";
        assert_eq!(truncate_str(s, 13), s);
    }

    #[test]
    fn test_truncate_str_shorter() {
        let s = "Hello, world!";
        assert_eq!(truncate_str(s, 5), "Hello");
    }

    #[test]
    fn test_truncate_str_respects_char_boundary() {
        // "héllo" has a multi-byte character
        let s = "héllo";
        let truncated = truncate_str(s, 2);
        assert_eq!(truncated, "h"); // Should not cut in the middle of "é"
    }

    #[test]
    fn test_format_message_clamped_to_4096() {
        let mut payload = test_payload(RunStatus::Completed);
        // Create a very long output preview
        payload.output_preview = Some("x".repeat(10_000));
        let message = format_telegram_message(&payload, 500, None);

        assert!(
            message.len() <= 4096,
            "Message length {} exceeds 4096",
            message.len()
        );
    }

    #[test]
    fn test_format_message_with_markdownv2() {
        let payload = test_payload(RunStatus::Completed);
        let message = format_telegram_message(&payload, 500, Some(TelegramParseMode::MarkdownV2));

        // Special characters should be escaped - the prompt name contains a hyphen
        assert!(
            message.contains("\\-"),
            "Hyphens should be escaped in: {message}"
        );
        // Duration contains a colon which doesn't need escaping, but the prompt name has hyphen
    }

    #[test]
    fn test_should_notify_enabled() {
        let config = NotificationsConfig {
            enabled: true,
            notify_on_success: true,
            notify_on_max_iterations: true,
            notify_on_failure: true,
            ..Default::default()
        };

        assert!(should_notify(RunStatus::Completed, &config));
        assert!(should_notify(RunStatus::MaxIterationsReached, &config));
        assert!(should_notify(RunStatus::Failed, &config));
    }

    #[test]
    fn test_should_notify_disabled() {
        let config = NotificationsConfig {
            enabled: false,
            ..Default::default()
        };

        assert!(!should_notify(RunStatus::Completed, &config));
        assert!(!should_notify(RunStatus::Failed, &config));
    }

    #[test]
    fn test_should_notify_selective() {
        let config = NotificationsConfig {
            enabled: true,
            notify_on_success: false,
            notify_on_max_iterations: true,
            notify_on_failure: true,
            ..Default::default()
        };

        assert!(!should_notify(RunStatus::Completed, &config));
        assert!(should_notify(RunStatus::MaxIterationsReached, &config));
        assert!(should_notify(RunStatus::Failed, &config));
    }

    // macOS notifier tests
    #[test]
    fn test_format_macos_message_completed() {
        let payload = test_payload(RunStatus::Completed);
        let message = format_macos_message(&payload, 500);

        assert!(message.contains("Mode: auto"));
        assert!(message.contains("Prompt: implement-plan"));
        assert!(message.contains("Iterations: 5/50"));
        assert!(message.contains("12m 34s"));
    }

    #[test]
    fn test_format_macos_message_strips_ansi() {
        let payload = test_payload(RunStatus::Completed);
        let message = format_macos_message(&payload, 500);

        // ANSI codes should be stripped
        assert!(!message.contains("\x1b["));
        assert!(!message.contains("[32m"));
    }

    #[test]
    fn test_format_macos_message_truncated_to_200() {
        let mut payload = test_payload(RunStatus::Completed);
        // Create a very long output preview
        payload.output_preview = Some("x".repeat(10_000));
        let message = format_macos_message(&payload, 500);

        assert!(
            message.len() <= 200,
            "Message length {} exceeds 200",
            message.len()
        );
    }

    #[test]
    fn test_escape_applescript_string_backslash() {
        assert_eq!(escape_applescript_string("a\\b"), "a\\\\b");
    }

    #[test]
    fn test_escape_applescript_string_quote() {
        assert_eq!(
            escape_applescript_string("say \"hello\""),
            "say \\\"hello\\\""
        );
    }

    #[test]
    fn test_escape_applescript_string_both() {
        assert_eq!(
            escape_applescript_string("path\\to\"file\""),
            "path\\\\to\\\"file\\\""
        );
    }

    #[test]
    fn test_escape_applescript_string_preserves_normal() {
        let input = "Hello World 123! @#$%^&*()";
        assert_eq!(escape_applescript_string(input), input);
    }

    #[test]
    fn test_macos_notifier_new() {
        let config = MacOSConfig {
            enabled: true,
            sound: Some("Sosumi".to_string()),
        };
        let notifier = MacOSNotifier::new(&config, 500);

        assert_eq!(notifier.sound, Some("Sosumi".to_string()));
        assert_eq!(notifier.output_preview_chars, 500);
    }

    #[test]
    fn test_macos_notifier_new_no_sound() {
        let config = MacOSConfig {
            enabled: true,
            sound: None,
        };
        let notifier = MacOSNotifier::new(&config, 300);

        assert_eq!(notifier.sound, None);
        assert_eq!(notifier.output_preview_chars, 300);
    }
}

#[cfg(test)]
mod proptest_tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn test_escape_markdown_v2_preserves_ascii_letters(s in "[a-zA-Z0-9 ]*") {
            assert_eq!(escape_markdown_v2(&s), s);
        }

        #[test]
        fn test_escape_markdown_v2_result_length(s in ".*") {
            let escaped = escape_markdown_v2(&s);
            // Each char can at most double in length (if escaped)
            assert!(escaped.len() <= s.len() * 2);
        }

        #[test]
        fn test_truncate_str_always_valid_utf8(s in ".*", max_len in 0usize..1000) {
            let truncated = truncate_str(&s, max_len);
            // Result should always be valid UTF-8
            assert!(truncated.is_char_boundary(truncated.len()));
            assert!(truncated.len() <= max_len || max_len == 0);
        }

        #[test]
        fn test_format_message_never_exceeds_4096(
            iterations_completed in 0u32..1000u32,
            max_iterations in 1u32..1000u32,
            preview_len in 0usize..10000usize
        ) {
            let payload = NotificationPayload {
                mode: "auto",
                iterations_completed,
                max_iterations: max_iterations.max(iterations_completed),
                duration: Duration::from_secs(123),
                status: RunStatus::Completed,
                output_preview: Some("x".repeat(preview_len)),
                prompt_name: "test".to_string(),
            };

            let message = format_telegram_message(&payload, 500, Some(TelegramParseMode::MarkdownV2));
            prop_assert!(message.len() <= 4096);
        }
    }
}

#[cfg(test)]
mod wiremock_tests {
    use super::*;

    fn run_blocking_test<F, R>(f: F) -> R
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        std::thread::spawn(f).join().expect("test thread panicked")
    }

    #[test]
    fn test_telegram_notifier_sends_correct_request() {
        run_blocking_test(|| {
            use wiremock::matchers::{method, path};
            use wiremock::{Mock, MockServer, ResponseTemplate};

            let rt = tokio::runtime::Runtime::new().expect("should create tokio runtime");
            let mock_server = rt.block_on(MockServer::start());

            // Set up the mock to expect a POST to the Telegram API
            rt.block_on(async {
                Mock::given(method("POST"))
                    .and(path("/botTEST_TOKEN/sendMessage"))
                    .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                        "ok": true,
                        "result": {}
                    })))
                    .expect(1)
                    .mount(&mock_server)
                    .await
            });

            // Set the environment variable
            unsafe {
                std::env::set_var("TEST_TELEGRAM_TOKEN", "TEST_TOKEN");
            }

            let config = TelegramConfig {
                enabled: true,
                bot_token_env: "TEST_TELEGRAM_TOKEN".to_string(),
                chat_id: "-1001234567890".to_string(),
                api_base_url: Some(mock_server.uri()),
                parse_mode: Some(TelegramParseMode::MarkdownV2),
            };

            let notifier = TelegramNotifier::new(&config, 500).expect("should create notifier");
            let payload = NotificationPayload {
                mode: "auto",
                iterations_completed: 1,
                max_iterations: 1,
                duration: Duration::from_secs(60),
                status: RunStatus::Completed,
                output_preview: None,
                prompt_name: "test".to_string(),
            };

            notifier
                .notify(&payload)
                .expect("notification should succeed");

            unsafe {
                std::env::remove_var("TEST_TELEGRAM_TOKEN");
            }
        })
    }

    #[test]
    fn test_telegram_notifier_handles_error_response() {
        run_blocking_test(|| {
            use wiremock::matchers::{method, path};
            use wiremock::{Mock, MockServer, ResponseTemplate};

            let rt = tokio::runtime::Runtime::new().expect("should create tokio runtime");
            let mock_server = rt.block_on(MockServer::start());

            rt.block_on(async {
                Mock::given(method("POST"))
                    .and(path("/botTEST_TOKEN/sendMessage"))
                    .respond_with(ResponseTemplate::new(429).set_body_json(serde_json::json!({
                        "ok": false,
                        "error_code": 429,
                        "description": "Too Many Requests"
                    })))
                    .expect(1)
                    .mount(&mock_server)
                    .await
            });

            unsafe {
                std::env::set_var("TEST_TELEGRAM_ERROR_TOKEN", "TEST_TOKEN");
            }

            let config = TelegramConfig {
                enabled: true,
                bot_token_env: "TEST_TELEGRAM_ERROR_TOKEN".to_string(),
                chat_id: "-1001234567890".to_string(),
                api_base_url: Some(mock_server.uri()),
                parse_mode: Some(TelegramParseMode::MarkdownV2),
            };

            let notifier = TelegramNotifier::new(&config, 500).expect("should create notifier");
            let payload = NotificationPayload {
                mode: "auto",
                iterations_completed: 1,
                max_iterations: 1,
                duration: Duration::from_secs(60),
                status: RunStatus::Completed,
                output_preview: None,
                prompt_name: "test".to_string(),
            };

            let result = notifier.notify(&payload);
            assert!(result.is_err());
            let err = result.expect_err("result should be an error");
            assert!(err.to_string().contains("429"));

            unsafe {
                std::env::remove_var("TEST_TELEGRAM_ERROR_TOKEN");
            }
        })
    }

    #[test]
    fn test_telegram_notifier_missing_token() {
        // Ensure the env var is not set
        unsafe {
            std::env::remove_var("MISSING_TELEGRAM_TOKEN");
        }

        let config = TelegramConfig {
            enabled: true,
            bot_token_env: "MISSING_TELEGRAM_TOKEN".to_string(),
            chat_id: "-1001234567890".to_string(),
            api_base_url: None,
            parse_mode: Some(TelegramParseMode::MarkdownV2),
        };

        let result = TelegramNotifier::new(&config, 500);
        assert!(result.is_err());
        let err = result.expect_err("result should be an error");
        assert!(err.to_string().contains("MISSING_TELEGRAM_TOKEN"));
    }

    #[test]
    fn test_build_notifiers_creates_telegram() {
        unsafe {
            std::env::set_var("BUILD_TEST_TOKEN", "test_token");
        }

        let config = NotificationsConfig {
            enabled: true,
            telegram: Some(TelegramConfig {
                enabled: true,
                bot_token_env: "BUILD_TEST_TOKEN".to_string(),
                chat_id: "-1001234567890".to_string(),
                api_base_url: None,
                parse_mode: Some(TelegramParseMode::MarkdownV2),
            }),
            ..Default::default()
        };

        let notifiers = build_notifiers(&config).expect("should build notifiers");
        assert_eq!(notifiers.len(), 1);
        assert_eq!(notifiers[0].name(), "Telegram");

        unsafe {
            std::env::remove_var("BUILD_TEST_TOKEN");
        }
    }

    #[test]
    fn test_build_notifiers_skips_disabled() {
        unsafe {
            std::env::set_var("SKIP_TEST_TOKEN", "test_token");
        }

        let config = NotificationsConfig {
            enabled: true,
            telegram: Some(TelegramConfig {
                enabled: false, // Disabled
                bot_token_env: "SKIP_TEST_TOKEN".to_string(),
                chat_id: "-1001234567890".to_string(),
                api_base_url: None,
                parse_mode: Some(TelegramParseMode::MarkdownV2),
            }),
            ..Default::default()
        };

        let notifiers = build_notifiers(&config).expect("should build notifiers");
        assert!(notifiers.is_empty());

        unsafe {
            std::env::remove_var("SKIP_TEST_TOKEN");
        }
    }

    #[test]
    fn test_build_notifiers_creates_macos() {
        let config = NotificationsConfig {
            enabled: true,
            macos: Some(MacOSConfig {
                enabled: true,
                sound: Some("default".to_string()),
            }),
            ..Default::default()
        };

        let notifiers = build_notifiers(&config).expect("should build notifiers");
        assert_eq!(notifiers.len(), 1);
        assert_eq!(notifiers[0].name(), "macOS");
    }

    #[test]
    fn test_build_notifiers_creates_both() {
        unsafe {
            std::env::set_var("BOTH_TEST_TOKEN", "test_token");
        }

        let config = NotificationsConfig {
            enabled: true,
            telegram: Some(TelegramConfig {
                enabled: true,
                bot_token_env: "BOTH_TEST_TOKEN".to_string(),
                chat_id: "-1001234567890".to_string(),
                api_base_url: None,
                parse_mode: Some(TelegramParseMode::MarkdownV2),
            }),
            macos: Some(MacOSConfig {
                enabled: true,
                sound: Some("Sosumi".to_string()),
            }),
            ..Default::default()
        };

        let notifiers = build_notifiers(&config).expect("should build notifiers");
        assert_eq!(notifiers.len(), 2);

        let names: Vec<&str> = notifiers.iter().map(|n| n.name()).collect();
        assert!(
            names.contains(&"Telegram"),
            "Should contain Telegram notifier"
        );
        assert!(names.contains(&"macOS"), "Should contain macOS notifier");

        unsafe {
            std::env::remove_var("BOTH_TEST_TOKEN");
        }
    }

    #[test]
    fn test_build_notifiers_skips_disabled_macos() {
        let config = NotificationsConfig {
            enabled: true,
            macos: Some(MacOSConfig {
                enabled: false, // Disabled
                sound: Some("default".to_string()),
            }),
            ..Default::default()
        };

        let notifiers = build_notifiers(&config).expect("should build notifiers");
        assert!(notifiers.is_empty());
    }
}
