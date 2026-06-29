//! Cancellation handler for user-initiated stops.
//!
//! This module provides a cancellation mechanism with two distinct stop
//! semantics:
//!
//! 1. **Force stop** — triggered by pressing Ctrl+C **twice** within a short
//!    window. Immediately cancels the in-flight agent subprocess via the
//!    [`CancellationToken`]. A single Ctrl+C press is a no-op (the user may
//!    have meant to interrupt a child tool, not the whole run); two presses
//!    are required to avoid accidental aborts.
//! 2. **Stop after iteration** — triggered by the `s` key in the TUI. Sets a
//!    flag that the auto loop checks between iterations; the current iteration
//!    is allowed to finish, then the run exits cleanly.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use tokio_util::sync::CancellationToken;

/// Time window for the second Ctrl+C to count as a force-stop (3 seconds).
const FORCE_CANCEL_WINDOW: Duration = Duration::from_secs(3);

/// Cancellation handler.
///
/// # States
///
/// - **Normal**: running, no stop requested.
/// - **Stop after iteration requested**: the `s` key (or an explicit API call)
///   asked to stop once the current iteration completes.
/// - **Force cancelled**: Ctrl+C was pressed twice within
///   [`FORCE_CANCEL_WINDOW`]; the [`CancellationToken`] is cancelled and the
///   in-flight subprocess should abort immediately.
#[derive(Debug, Clone)]
pub struct CancellationHandler {
    /// Inner state shared across all clones.
    inner: Arc<CancellationInner>,
    /// Tokio cancellation token for immediate (force) cancellation.
    cancel_token: CancellationToken,
}

#[derive(Debug)]
struct CancellationInner {
    /// `s` key (or explicit call): stop after the current iteration completes.
    stop_after_iteration_requested: AtomicBool,
    /// Ctrl+C pressed twice within the window: cancel immediately.
    force_cancel_requested: AtomicBool,
    /// Timestamp of the most recent Ctrl+C press (milliseconds since epoch).
    last_press_time: AtomicU64,
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
                stop_after_iteration_requested: AtomicBool::new(false),
                force_cancel_requested: AtomicBool::new(false),
                last_press_time: AtomicU64::new(0),
            }),
            cancel_token: cancel_token.clone(),
        };

        if register_handler {
            // Register Ctrl+C handler (if not already registered, e.g., in tests or scripts).
            let inner = handler.inner.clone();
            let token_for_handler = cancel_token.clone();
            if ctrlc::set_handler(move || {
                let now = now_millis();
                let last = inner.last_press_time.load(Ordering::SeqCst);
                let elapsed = now.saturating_sub(last);

                if last > 0 && elapsed <= FORCE_CANCEL_WINDOW.as_millis() as u64 {
                    // Second press within the window — force cancel now.
                    inner.force_cancel_requested.store(true, Ordering::SeqCst);
                    token_for_handler.cancel();
                    println!("\n🛑 Force stop requested! Shutting down immediately...");
                } else {
                    // First press (or stale) — just record the time. A single
                    // press does NOT stop the run; press Ctrl+C again within
                    // 3 seconds to force quit.
                    inner.last_press_time.store(now, Ordering::SeqCst);
                    println!(
                        "\n⏸  Ctrl+C received. Press again within 3s to force stop, or use the `s` key to stop after this iteration."
                    );
                }
            })
            .is_err() {
                // Handler already registered (e.g., in tests or non-interactive shell).
                // This is fine — the automation will still work, just without Ctrl+C handling.
            }
        }

        (handler, cancel_token)
    }

    /// Requests that the run stop after the current iteration completes.
    ///
    /// Toggled by the `s` key in the TUI. Idempotent: calling twice keeps it
    /// set; use [`clear_stop_after_iteration`](Self::clear_stop_after_iteration)
    /// to clear it.
    #[allow(dead_code)]
    pub fn request_stop_after_iteration(&self) {
        self.inner
            .stop_after_iteration_requested
            .store(true, Ordering::SeqCst);
    }

    /// Toggles the "stop after this iteration" request.
    ///
    /// Returns the new state (`true` = will stop after the current iteration).
    pub fn toggle_stop_after_iteration(&self) -> bool {
        let prev = self
            .inner
            .stop_after_iteration_requested
            .load(Ordering::SeqCst);
        let new = !prev;
        self.inner
            .stop_after_iteration_requested
            .store(new, Ordering::SeqCst);
        new
    }

    /// Clears a previous "stop after this iteration" request.
    pub fn clear_stop_after_iteration(&self) {
        self.inner
            .stop_after_iteration_requested
            .store(false, Ordering::SeqCst);
    }

    /// Returns `true` if stopping after the current iteration has been requested
    /// (the `s` key). The auto loop checks this between iterations.
    ///
    /// Replaces the old `graceful_shutdown_requested` name.
    #[must_use]
    pub fn stop_after_iteration_requested(&self) -> bool {
        self.inner
            .stop_after_iteration_requested
            .load(Ordering::SeqCst)
    }

    /// Returns `true` if force cancellation was requested (Ctrl+C twice).
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

    /// Resets all flags (useful for testing or continuing after interruption).
    #[allow(dead_code)]
    pub fn reset(&self) {
        self.inner
            .stop_after_iteration_requested
            .store(false, Ordering::SeqCst);
        self.inner
            .force_cancel_requested
            .store(false, Ordering::SeqCst);
        self.inner.last_press_time.store(0, Ordering::SeqCst);
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
        assert!(!handler.stop_after_iteration_requested());
        assert!(!handler.force_cancel_requested());
        assert!(!handler.is_cancelled());
    }

    #[test]
    fn test_stop_after_iteration_toggle() {
        let (handler, _token) = CancellationHandler::new_with_handler(false);
        assert!(!handler.stop_after_iteration_requested());
        assert!(handler.toggle_stop_after_iteration()); // on
        assert!(handler.stop_after_iteration_requested());
        assert!(!handler.toggle_stop_after_iteration()); // off
        assert!(!handler.stop_after_iteration_requested());
    }

    #[test]
    fn test_clear_stop_after_iteration() {
        let (handler, _token) = CancellationHandler::new_with_handler(false);
        handler.request_stop_after_iteration();
        assert!(handler.stop_after_iteration_requested());
        handler.clear_stop_after_iteration();
        assert!(!handler.stop_after_iteration_requested());
    }

    #[test]
    fn test_cancellation_handler_reset() {
        let (handler, _token) = CancellationHandler::new_with_handler(false);
        handler
            .inner
            .stop_after_iteration_requested
            .store(true, Ordering::SeqCst);
        handler
            .inner
            .force_cancel_requested
            .store(true, Ordering::SeqCst);

        handler.reset();

        assert!(!handler.stop_after_iteration_requested());
        assert!(!handler.force_cancel_requested());
    }

    #[test]
    fn test_now_millis_returns_reasonable_value() {
        let now = now_millis();
        // Should be a timestamp sometime after 2020.
        assert!(now > 1_577_836_800_000);
    }
}
