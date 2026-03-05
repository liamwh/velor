//! Two-stage cancellation handler for graceful shutdown.
//!
//! This module provides a cancellation mechanism that:
//! 1. First Ctrl+C: Signals graceful shutdown (complete current iteration, then stop)
//! 2. Second Ctrl+C within 3 seconds: Force quit immediately

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use tokio_util::sync::CancellationToken;

/// Time window for second Ctrl+C to trigger force cancellation (3 seconds)
const FORCE_CANCEL_WINDOW: Duration = Duration::from_secs(3);

/// Two-stage cancellation handler.
///
/// # States
///
/// - **Normal**: No Ctrl+C pressed
/// - **Graceful Shutdown Requested**: First Ctrl+C pressed, will stop after current iteration
/// - **Force Cancel Requested**: Second Ctrl+C pressed within 3 seconds of first
#[derive(Debug, Clone)]
pub struct CancellationHandler {
    /// Inner state shared across all clones
    inner: Arc<CancellationInner>,
    /// Tokio cancellation token for immediate cancellation
    cancel_token: CancellationToken,
}

#[derive(Debug)]
struct CancellationInner {
    /// First Ctrl+C received (graceful shutdown requested)
    graceful_shutdown_requested: AtomicBool,
    /// Second Ctrl+C received (force cancel requested)
    force_cancel_requested: AtomicBool,
    /// Timestamp of first Ctrl+C press (milliseconds since epoch)
    first_press_time: AtomicU64,
}

impl CancellationHandler {
    /// Creates a new cancellation handler with an associated cancellation token.
    #[must_use]
    pub fn new() -> (Self, CancellationToken) {
        Self::new_with_handler(true)
    }

    /// Creates a new cancellation handler, optionally registering the Ctrl+C handler.
    ///
    /// For tests, pass `register_handler = false` to avoid the "MultipleHandlers" error.
    #[must_use]
    fn new_with_handler(register_handler: bool) -> (Self, CancellationToken) {
        let cancel_token = CancellationToken::new();

        let handler = Self {
            inner: Arc::new(CancellationInner {
                graceful_shutdown_requested: AtomicBool::new(false),
                force_cancel_requested: AtomicBool::new(false),
                first_press_time: AtomicU64::new(0),
            }),
            cancel_token: cancel_token.clone(),
        };

        if register_handler {
            // Register Ctrl+C handler
            let inner = handler.inner.clone();
            let token_for_handler = cancel_token.clone();
            ctrlc::set_handler(move || {
                let now = now_millis();

                // Check if this is the first or second press
                if !inner.graceful_shutdown_requested.load(Ordering::SeqCst) {
                    // First Ctrl+C - request graceful shutdown
                    inner.graceful_shutdown_requested.store(true, Ordering::SeqCst);
                    inner.first_press_time.store(now, Ordering::SeqCst);

                    println!("\n⚠️  Graceful shutdown requested. Will stop after current iteration completes.");
                    println!("💡 Press Ctrl+C again within 3 seconds to force quit immediately.");
                } else {
                    // Second Ctrl+C - check if within the time window
                    let first_press = inner.first_press_time.load(Ordering::SeqCst);
                    let elapsed = now.saturating_sub(first_press);

                    if elapsed <= FORCE_CANCEL_WINDOW.as_millis() as u64 {
                        // Within time window - force cancel
                        inner.force_cancel_requested.store(true, Ordering::SeqCst);
                        token_for_handler.cancel();
                        println!("\n🛑 Force quit requested! Shutting down immediately...");
                    } else {
                        // Outside time window - treat as new graceful shutdown request
                        inner.first_press_time.store(now, Ordering::SeqCst);
                        println!("\n⚠️  Graceful shutdown requested again. Will stop after current iteration.");
                        println!("💡 Press Ctrl+C again within 3 seconds to force quit immediately.");
                    }
                }
            })
            .expect("failed to register Ctrl+C handler");
        }

        (handler, cancel_token)
    }

    /// Returns `true` if graceful shutdown has been requested (first Ctrl+C).
    #[must_use]
    pub fn graceful_shutdown_requested(&self) -> bool {
        self.inner
            .graceful_shutdown_requested
            .load(Ordering::SeqCst)
    }

    /// Returns `true` if force cancellation has been requested (second Ctrl+C within window).
    #[must_use]
    #[allow(dead_code)]
    pub fn force_cancel_requested(&self) -> bool {
        self.inner.force_cancel_requested.load(Ordering::SeqCst)
    }

    /// Returns `true` if the associated cancellation token has been cancelled.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancel_token.is_cancelled()
    }

    /// Returns the cancellation token for passing to other functions.
    #[must_use]
    pub const fn token(&self) -> &CancellationToken {
        &self.cancel_token
    }

    /// Resets the graceful shutdown flag (useful for testing or continuing after interruption).
    #[allow(dead_code)]
    pub fn reset(&self) {
        self.inner
            .graceful_shutdown_requested
            .store(false, Ordering::SeqCst);
        self.inner
            .force_cancel_requested
            .store(false, Ordering::SeqCst);
        self.inner.first_press_time.store(0, Ordering::SeqCst);
    }
}

impl Default for CancellationHandler {
    fn default() -> Self {
        let (handler, _) = Self::new();
        handler
    }
}

/// Returns the current time in milliseconds since Unix epoch.
fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cancellation_handler_initial_state() {
        let (handler, _token) = CancellationHandler::new_with_handler(false);
        assert!(!handler.graceful_shutdown_requested());
        assert!(!handler.force_cancel_requested());
        assert!(!handler.is_cancelled());
    }

    #[test]
    fn test_cancellation_handler_reset() {
        let (handler, _token) = CancellationHandler::new_with_handler(false);
        handler
            .inner
            .graceful_shutdown_requested
            .store(true, Ordering::SeqCst);
        handler
            .inner
            .force_cancel_requested
            .store(true, Ordering::SeqCst);

        handler.reset();

        assert!(!handler.graceful_shutdown_requested());
        assert!(!handler.force_cancel_requested());
    }

    #[test]
    fn test_now_millis_returns_reasonable_value() {
        let now = now_millis();
        // Should be a timestamp sometime after 2020
        assert!(now > 1_577_836_800_000);
    }
}
