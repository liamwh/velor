//! Crash resilience and retry logic for the auto loop.
//!
//! This module provides:
//! - Conversation history management for crash recovery
//! - Exponential backoff retry logic
//! - Error classification (permanent vs retryable)
//! - Configuration for retry behaviour

use color_eyre::eyre::Error;
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

		self
			.entries
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
}

impl std::fmt::Display for RetryError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			RetryError::Retryable(msg) => write!(f, "retryable error: {}", msg),
			RetryError::Permanent(msg) => write!(f, "permanent error: {}", msg),
			RetryError::TimeoutExceeded(msg) => write!(f, "timeout exceeded: {}", msg),
		}
	}
}

impl std::error::Error for RetryError {}

/// Calculates exponential backoff delay for a given attempt number.
///
/// # Arguments
///
/// * `attempt` - The attempt number (1-based)
/// * `base_ms` - Base backoff in milliseconds
/// * `max_ms` - Maximum backoff in milliseconds
///
/// # Returns
///
/// A `Duration` for the backoff delay.
///
/// # Examples
///
/// ```rust
/// use std::time::Duration;
/// // With base=100ms, max=1600ms:
/// // attempt 1: 100ms
/// // attempt 2: 200ms
/// // attempt 3: 400ms
/// // attempt 4: 800ms
/// // attempt 5: 1600ms
/// // attempt 6+: 1600ms (capped at max)
/// ```
pub fn calculate_backoff(attempt: u32, base_ms: u64, max_ms: u64) -> Duration {
	let delay_ms = (base_ms * 2_u64.pow(attempt.saturating_sub(1))).min(max_ms);
	Duration::from_millis(delay_ms)
}

/// Determines if an error is permanent (not retryable).
///
/// Permanent errors include:
/// - Binary not found on PATH
/// - Permission denied
/// - Invalid configuration
/// - Template parsing errors
pub fn is_permanent_error(error: &Error) -> bool {
	let error_msg = error.to_string().to_lowercase();

	error_msg.contains("not found on path")
		|| error_msg.contains("no such file or directory")
		|| error_msg.contains("permission denied")
		|| error_msg.contains("invalid config")
		|| error_msg.contains("failed to parse template")
		|| error_msg.contains("prompt.*not found")
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
	fn test_calculate_backoff() {
		// Test exponential backoff sequence: 100, 200, 400, 800, 1600
		assert_eq!(calculate_backoff(1, 100, 1600), Duration::from_millis(100));
		assert_eq!(calculate_backoff(2, 100, 1600), Duration::from_millis(200));
		assert_eq!(calculate_backoff(3, 100, 1600), Duration::from_millis(400));
		assert_eq!(calculate_backoff(4, 100, 1600), Duration::from_millis(800));
		assert_eq!(calculate_backoff(5, 100, 1600), Duration::from_millis(1600));
		assert_eq!(calculate_backoff(6, 100, 1600), Duration::from_millis(1600)); // capped at max
	}

	#[test]
	fn test_calculate_backoff_different_base() {
		assert_eq!(calculate_backoff(1, 50, 400), Duration::from_millis(50));
		assert_eq!(calculate_backoff(2, 50, 400), Duration::from_millis(100));
		assert_eq!(calculate_backoff(3, 50, 400), Duration::from_millis(200));
		assert_eq!(calculate_backoff(4, 50, 400), Duration::from_millis(400)); // capped
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
		assert!(err.to_string().contains("retryable"));
		assert!(err.to_string().contains("temporary failure"));

		let err = RetryError::Permanent("config error".to_string());
		assert!(err.to_string().contains("permanent"));
		assert!(err.to_string().contains("config error"));
	}
}
