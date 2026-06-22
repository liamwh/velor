//! Crash resilience and retry logic for the auto loop.
//!
//! This module provides:
//! - Conversation history management for crash recovery
//! - Exponential backoff retry logic
//! - Error classification (permanent vs retryable)
//! - Configuration for retry behaviour

use std::time::Duration;

/// Configuration for retry behaviour.
#[derive(Debug, Clone)]
pub struct RetryConfig {
    /// Maximum number of retry attempts per iteration.
    pub max_retries: u32,
    /// Base backoff duration in milliseconds.
    pub base_backoff_ms: u64,
    /// Maximum backoff duration in milliseconds.
    pub max_backoff_ms: u64,
    /// Absolute timeout for all retries combined in milliseconds (default: 5 hours).
    pub absolute_timeout_ms: u64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 5,
            base_backoff_ms: 100,
            max_backoff_ms: 1600,
            absolute_timeout_ms: 5 * 60 * 60 * 1000, // 5 hours in milliseconds
        }
    }
}

/// Manages conversation history for crash recovery.
///
/// Only preserves context when crashes occur - successful iterations clear the history.
#[derive(Debug, Default)]
pub struct ConversationHistory {
    entries: Vec<HistoryEntry>,
}

#[derive(Debug, Clone)]
struct HistoryEntry {
    iteration: u32,
    timestamp: String,
    prompt: String,
    output: String,
}

impl ConversationHistory {
    /// Creates a new empty conversation history.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds an entry to the conversation history.
    pub fn add(&mut self, iteration: u32, prompt: &str, output: &str) {
        self.entries.push(HistoryEntry {
            iteration,
            timestamp: chrono::Utc::now()
                .format("%Y-%m-%d %H:%M:%S UTC")
                .to_string(),
            prompt: prompt.to_string(),
            output: output.to_string(),
        });
    }

    /// Returns the previous conversation context formatted for prepending to a new prompt.
    pub fn get_previous_context(&self) -> String {
        if self.entries.is_empty() {
            return String::new();
        }

        self.entries
            .iter()
            .map(|entry| {
                format!(
                    "=== Iteration {} ({}) ===\nPROMPT:\n{}\n\nOUTPUT:\n{}\n",
                    entry.iteration, entry.timestamp, entry.prompt, entry.output
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// Clears all conversation history (called after successful iteration).
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Returns true if there are no entries in the history.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns the iteration number of the last entry, if any.
    #[allow(dead_code)]
    pub fn last_iteration(&self) -> Option<u32> {
        self.entries.last().map(|e| e.iteration)
    }

    /// Returns the failure reasons from all entries, if any.
    /// Extracts the error message from entries with `<FAILED: ...>` output format.
    pub fn get_failure_reasons(&self) -> Vec<String> {
        self.entries
            .iter()
            .filter_map(|entry| {
                let output = &entry.output;
                if let Some(start) = output.find("<FAILED: ") {
                    let rest = &output[start + 9..];
                    if let Some(end) = rest.find('>') {
                        return Some(rest[..end].to_string());
                    }
                }
                None
            })
            .collect()
    }
}

/// Error type distinguishing between retryable and permanent failures.
#[derive(Debug, Clone)]
pub enum RetryError {
    /// A retryable error - should trigger exponential backoff and retry.
    Retryable(String),
    /// A permanent error - should fail immediately without retry.
    Permanent(String),
    /// The absolute timeout for all retries combined was exceeded.
    TimeoutExceeded(String),
    /// User cancelled via Ctrl+C.
    Cancelled,
}

impl std::fmt::Display for RetryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RetryError::Retryable(msg) => write!(f, "retryable error: {}", msg),
            RetryError::Permanent(msg) => write!(f, "permanent error: {}", msg),
            RetryError::TimeoutExceeded(msg) => write!(f, "timeout exceeded: {}", msg),
            RetryError::Cancelled => write!(f, "cancelled by user"),
        }
    }
}

impl std::error::Error for RetryError {}

/// A source of jitter for [`BackoffPolicy`], injectable so retry tests are
/// deterministic. Production uses [`SystemJitter`]; tests use [`FixedJitter`].
pub trait JitterSource {
    /// Returns a duration uniformly in `[lo, hi]` (clamped to `lo` if `hi < lo`).
    fn in_range(&mut self, lo: Duration, hi: Duration) -> Duration;
}

/// Production jitter backed by the system RNG.
pub struct SystemJitter;

impl JitterSource for SystemJitter {
    fn in_range(&mut self, lo: Duration, hi: Duration) -> Duration {
        if hi <= lo {
            return lo;
        }
        let range = (hi.as_nanos() - lo.as_nanos()) as u64;
        let n = rand::random::<u64>() % range.saturating_add(1);
        lo + Duration::from_nanos(n)
    }
}

/// Deterministic jitter for tests: always returns `value` clamped to `[lo, hi]`.
#[cfg(test)]
#[derive(Debug, Clone, Copy)]
pub struct FixedJitter(pub Duration);

#[cfg(test)]
impl JitterSource for FixedJitter {
    fn in_range(&mut self, lo: Duration, hi: Duration) -> Duration {
        if self.0 < lo {
            lo
        } else if self.0 > hi {
            hi
        } else {
            self.0
        }
    }
}

/// Stateless exponential backoff with a non-zero floor and full-jitter-between.
///
/// For inference-provider failures, retrying immediately after an overload (the
/// old 100/200/400 ms policy) hammers an already-loaded upstream. This policy
/// starts at a multi-second floor, grows exponentially toward a cap, and applies
/// jitter within `[floor, cap]` so retries decorrelate across concurrent callers.
/// A provider-supplied `Retry-After` (or the per-class floor from
/// [`crate::execution_service::error::Retryability`]) takes precedence as a
/// minimum.
#[derive(Debug, Clone)]
pub struct BackoffPolicy {
    /// Initial delay cap (attempt 2). Default 2 s.
    pub initial: Duration,
    /// Hard maximum delay. Default 60 s.
    pub max: Duration,
    /// Exponential multiplier applied to the cap per attempt. Default 2.0.
    pub multiplier: f64,
    /// Non-zero minimum delay floor. Default 1 s.
    pub floor: Duration,
    /// Total executions including the initial attempt. Default 5.
    pub max_attempts: u32,
}

impl Default for BackoffPolicy {
    fn default() -> Self {
        Self {
            initial: Duration::from_secs(2),
            max: Duration::from_secs(60),
            multiplier: 2.0,
            floor: Duration::from_secs(1),
            max_attempts: 5,
        }
    }
}

impl BackoffPolicy {
    /// Computes the delay before `attempt` (1-based; attempt 1 is the initial run
    /// and is never delayed). The cap is `min(max, initial * multiplier^(attempt-2))`;
    /// the delay is uniform in `[floor, cap]`, then raised to `retry_after` if
    /// larger.
    #[must_use]
    pub fn delay(
        &self,
        attempt: u32,
        jitter: &mut dyn JitterSource,
        retry_after: Option<Duration>,
    ) -> Duration {
        let exp = self.multiplier.powi(attempt.saturating_sub(2) as i32);
        let capped = (self.initial.as_secs_f64() * exp).min(self.max.as_secs_f64());
        let cap = Duration::from_secs_f64(capped);
        let hi = cap.max(self.floor);
        let mut delay = jitter.in_range(self.floor, hi);
        if let Some(retry_after) = retry_after {
            delay = delay.max(retry_after);
        }
        delay.min(self.max).max(self.floor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_conversation_history_empty() {
        let history = ConversationHistory::new();
        assert!(history.is_empty());
        assert!(history.last_iteration().is_none());
        assert!(history.get_previous_context().is_empty());
    }

    #[test]
    fn test_conversation_history_add() {
        let mut history = ConversationHistory::new();
        history.add(1, "test prompt", "test output");
        assert!(!history.is_empty());
        assert_eq!(history.last_iteration(), Some(1));

        let context = history.get_previous_context();
        assert!(context.contains("Iteration 1"));
        assert!(context.contains("test prompt"));
        assert!(context.contains("test output"));
    }

    #[test]
    fn test_conversation_history_multiple_entries() {
        let mut history = ConversationHistory::new();
        history.add(1, "prompt 1", "output 1");
        history.add(2, "prompt 2", "output 2");

        assert!(!history.is_empty());
        assert_eq!(history.last_iteration(), Some(2));

        let context = history.get_previous_context();
        assert!(context.contains("Iteration 1"));
        assert!(context.contains("Iteration 2"));
        assert!(context.contains("output 1"));
        assert!(context.contains("output 2"));
    }

    #[test]
    fn test_conversation_history_clear() {
        let mut history = ConversationHistory::new();
        history.add(1, "test prompt", "test output");
        assert!(!history.is_empty());

        history.clear();
        assert!(history.is_empty());
        assert!(history.last_iteration().is_none());
    }

    #[test]
    fn test_get_failure_reasons_empty() {
        let history = ConversationHistory::new();
        assert!(history.get_failure_reasons().is_empty());
    }

    #[test]
    fn test_get_failure_reasons_no_failures() {
        let mut history = ConversationHistory::new();
        history.add(1, "prompt", "normal output");
        assert!(history.get_failure_reasons().is_empty());
    }

    #[test]
    fn test_get_failure_reasons_single() {
        let mut history = ConversationHistory::new();
        history.add(1, "prompt", "<FAILED: network timeout>");
        let reasons = history.get_failure_reasons();
        assert_eq!(reasons.len(), 1);
        assert_eq!(reasons[0], "network timeout");
    }

    #[test]
    fn test_get_failure_reasons_multiple() {
        let mut history = ConversationHistory::new();
        history.add(1, "prompt", "<FAILED: error one>");
        history.add(2, "prompt", "<FAILED: error two>");
        let reasons = history.get_failure_reasons();
        assert_eq!(reasons.len(), 2);
        assert_eq!(reasons[0], "error one");
        assert_eq!(reasons[1], "error two");
    }

    #[test]
    fn backoff_policy_default_starts_at_seconds_not_millis() {
        // The old policy was 100/200/400 ms; the new default floor is 1 s and the
        // attempt-2 cap is 2 s — never sub-second after a provider failure.
        let policy = BackoffPolicy::default();
        let mut jitter = FixedJitter(Duration::from_secs(1)); // min of range
        let d2 = policy.delay(2, &mut jitter, None);
        assert!(d2 >= policy.floor, "delay must respect the floor");
        assert!(
            d2 <= policy.initial,
            "attempt-2 delay must be <= initial cap"
        );
        assert!(
            d2.as_millis() >= 1000,
            "delay must be multi-second, not sub-second like the old policy (got {d2:?})"
        );
    }

    #[test]
    fn backoff_policy_grows_and_caps() {
        let policy = BackoffPolicy::default();
        // Max-jitter (FixedJitter returns hi clamped): delays should grow toward max.
        let caps = [2u64, 4, 8, 16, 32, 60];
        for (i, &cap) in caps.iter().enumerate() {
            let attempt = (i + 2) as u32;
            let mut jitter = FixedJitter(Duration::from_secs(cap + 10)); // clamps to hi
            let d = policy.delay(attempt, &mut jitter, None);
            let expected_cap = Duration::from_secs(cap).min(policy.max);
            assert!(
                d <= expected_cap,
                "attempt {attempt}: {d:?} > cap {expected_cap:?}"
            );
        }
    }

    #[test]
    fn backoff_policy_retry_after_takes_precedence() {
        let policy = BackoffPolicy::default();
        let mut jitter = FixedJitter(Duration::ZERO); // would normally clamp to floor
        let d = policy.delay(2, &mut jitter, Some(Duration::from_secs(7)));
        assert_eq!(
            d,
            Duration::from_secs(7),
            "Retry-After should override the jittered floor"
        );
    }

    #[test]
    fn backoff_policy_never_below_floor() {
        let policy = BackoffPolicy::default();
        let mut jitter = FixedJitter(Duration::ZERO);
        for attempt in 2..=6 {
            let d = policy.delay(attempt, &mut jitter, None);
            assert!(d >= policy.floor, "attempt {attempt}: {d:?} below floor");
        }
    }

    #[test]
    fn test_retry_config_default() {
        let config = RetryConfig::default();
        assert_eq!(config.max_retries, 5);
        assert_eq!(config.base_backoff_ms, 100);
        assert_eq!(config.max_backoff_ms, 1600);
        assert_eq!(config.absolute_timeout_ms, 5 * 60 * 60 * 1000); // 5 hours
    }

    #[test]
    fn test_retry_config_custom() {
        let config = RetryConfig {
            max_retries: 3,
            base_backoff_ms: 200,
            max_backoff_ms: 800,
            absolute_timeout_ms: 3600000, // 1 hour
        };
        assert_eq!(config.max_retries, 3);
        assert_eq!(config.base_backoff_ms, 200);
        assert_eq!(config.max_backoff_ms, 800);
        assert_eq!(config.absolute_timeout_ms, 3600000);
    }

    #[test]
    fn test_retry_error_display() {
        let err = RetryError::Retryable("temporary failure".to_string());
        assert!(
            err.to_string().contains("retryable"),
            "error should contain 'retryable'"
        );
        assert!(
            err.to_string().contains("temporary failure"),
            "error should contain message"
        );

        let err = RetryError::Permanent("config error".to_string());
        assert!(
            err.to_string().contains("permanent"),
            "error should contain 'permanent'"
        );
        assert!(
            err.to_string().contains("config error"),
            "error should contain message"
        );

        let err = RetryError::Cancelled;
        assert!(
            err.to_string().contains("cancelled"),
            "error should contain 'cancelled'"
        );
    }
}
