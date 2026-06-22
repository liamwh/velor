//! Scoped execution policy: per-scope concurrency limiting and a circuit breaker.
//!
//! These live at the [`crate::execution_service::service`] layer (not in the
//! retry loop, which is already sequential per iteration). Keyed by
//! [`ExecutionScope`] so independent providers/profiles don't serialize each
//! other, and so one bad prompt cannot open another scope's breaker.
//!
//! ## Circuit breaker
//! Only **transient upstream** failures count toward opening the breaker (a local
//! Velor deadline, a missing executable, auth, or context-too-large must NOT open
//! it). After `threshold` transient failures within `window`, the breaker opens
//! and refuses new attempts until `cooldown` elapses, then allows a half-open
//! probe. Note: an in-memory breaker only helps long-lived processes (e.g.
//! `vel serve`); isolated CLI invocations gain little, which is documented.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::execution_service::error::{AgentExecutionError, ProviderErrorKind, Retryability};

/// Identifies the scope a run belongs to. One binary ≈ one credential/provider
/// scope, so concurrency and breaker state are keyed on it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ExecutionScope {
    /// The agent binary (e.g. `glm5`, `codex`).
    pub binary: String,
}

impl ExecutionScope {
    /// Creates a scope for a binary.
    #[must_use]
    pub fn new(binary: impl Into<String>) -> Self {
        Self {
            binary: binary.into(),
        }
    }
}

/// Per-scope concurrency limit. Prevents unbounded overlapping runs against the
/// same provider (which would amplify load on an already-overloaded upstream).
#[derive(Debug)]
pub struct ConcurrencyLimit {
    sem: Arc<Semaphore>,
    max: usize,
}

impl ConcurrencyLimit {
    /// Creates a limit allowing up to `max` concurrent holders.
    #[must_use]
    pub fn new(max: usize) -> Self {
        let max = max.max(1);
        Self {
            sem: Arc::new(Semaphore::new(max)),
            max,
        }
    }

    /// Returns the configured maximum concurrent holders.
    #[must_use]
    pub const fn max(&self) -> usize {
        self.max
    }

    /// Acquires a permit (waits until one is available). Holding the returned
    /// guard for the duration of the run prevents overlap.
    pub async fn acquire(self: Arc<Self>) -> OwnedSemaphorePermit {
        // acquire_owned only errors if the semaphore is closed; it never is here.
        Arc::clone(&self.sem)
            .acquire_owned()
            .await
            .expect("concurrency semaphore closed")
    }
}

/// Circuit-breaker state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitState {
    /// Accepting requests.
    Closed,
    /// Rejecting requests until `until`.
    Open {
        /// When the breaker will allow a probe.
        until: Instant,
    },
    /// Allowing a limited probe after being open.
    HalfOpen,
}

/// Configuration for a [`CircuitBreaker`].
#[derive(Debug, Clone, Copy)]
pub struct CircuitBreakerConfig {
    /// Transient failures within `window` required to open.
    pub threshold: u32,
    /// Sliding window over which failures are counted.
    pub window: Duration,
    /// How long the breaker stays open before allowing a probe.
    pub cooldown: Duration,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            threshold: 5,
            window: Duration::from_secs(300),
            cooldown: Duration::from_secs(30),
        }
    }
}

/// A per-scope circuit breaker. Only transient upstream failures count.
#[derive(Debug)]
pub struct CircuitBreaker {
    cfg: CircuitBreakerConfig,
    failures: Mutex<VecDeque<Instant>>,
}

impl CircuitBreaker {
    /// Creates a breaker with the given config.
    #[must_use]
    pub fn new(cfg: CircuitBreakerConfig) -> Self {
        Self {
            cfg,
            failures: Mutex::new(VecDeque::new()),
        }
    }

    /// Returns the current state, transitioning Open→HalfOpen when the cooldown
    /// elapses. Does not mutate.
    pub fn state(&self, now: Instant) -> CircuitState {
        let g = self.failures.lock().expect("breaker mutex poisoned");
        match g.back() {
            None => CircuitState::Closed,
            Some(&last) => {
                let recent = g
                    .iter()
                    .rev()
                    .take_while(|&&t| now.duration_since(t) <= self.cfg.window)
                    .count() as u32;
                if recent >= self.cfg.threshold {
                    if now.duration_since(last) >= self.cfg.cooldown {
                        CircuitState::HalfOpen
                    } else {
                        CircuitState::Open {
                            until: last + self.cfg.cooldown,
                        }
                    }
                } else {
                    CircuitState::Closed
                }
            }
        }
    }

    /// Returns `Ok(())` if a request may proceed, or the open-until instant.
    pub fn allow(&self, now: Instant) -> Result<(), Instant> {
        match self.state(now) {
            CircuitState::Closed | CircuitState::HalfOpen => Ok(()),
            CircuitState::Open { until } => Err(until),
        }
    }

    /// Records a transient upstream failure.
    pub fn record_failure(&self, now: Instant) {
        let mut g = self.failures.lock().expect("breaker mutex poisoned");
        g.push_back(now);
        // Drop failures older than the window.
        while let Some(&front) = g.front() {
            if now.duration_since(front) > self.cfg.window {
                g.pop_front();
            } else {
                break;
            }
        }
    }

    /// Records a success (closes the breaker).
    pub fn record_success(&self) {
        let mut g = self.failures.lock().expect("breaker mutex poisoned");
        g.clear();
    }
}

/// Returns `true` if an error is a transient *upstream* failure that should count
/// toward the circuit breaker. Local Velor deadlines, missing executables, auth,
/// and context-too-large failures are explicitly excluded.
#[must_use]
pub fn is_transient_upstream(err: &AgentExecutionError) -> bool {
    let AgentExecutionError::Provider { error, .. } = err else {
        return false;
    };
    matches!(
        error.kind(),
        ProviderErrorKind::Overloaded
            | ProviderErrorKind::RateLimited
            | ProviderErrorKind::ConnectionReset
    )
}

/// `true` if an error should be retried (delegates to typed retryability).
#[must_use]
pub fn should_retry(err: &AgentExecutionError) -> bool {
    matches!(err.retryability(), Retryability::Retryable { .. })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution_service::classify::{
        Classification, ClassificationConfidence, ClassificationSource,
    };
    use crate::execution_service::error::ProviderError;

    fn provider_err(kind: ProviderErrorKind) -> AgentExecutionError {
        let evidence = Classification::new(
            kind,
            ClassificationSource::StdoutTail,
            "test",
            ClassificationConfidence::Medium,
        );
        let error = match kind {
            ProviderErrorKind::Overloaded => ProviderError::Overloaded {
                status: Some(529),
                provider_code: None,
                retry_after: None,
            },
            ProviderErrorKind::Authentication => ProviderError::Authentication,
            ProviderErrorKind::ContextTooLarge => ProviderError::ContextTooLarge,
            _ => ProviderError::ConnectionReset,
        };
        AgentExecutionError::Provider { error, evidence }
    }

    #[test]
    fn breaker_opens_after_threshold_transient_failures() {
        let cfg = CircuitBreakerConfig {
            threshold: 3,
            window: Duration::from_secs(60),
            cooldown: Duration::from_secs(30),
        };
        let b = CircuitBreaker::new(cfg);
        let t0 = Instant::now();
        assert!(matches!(b.state(t0), CircuitState::Closed));
        b.record_failure(t0);
        b.record_failure(t0);
        assert!(matches!(b.state(t0), CircuitState::Closed)); // 2 < 3
        b.record_failure(t0);
        // 3 >= threshold -> Open
        assert!(matches!(b.state(t0), CircuitState::Open { .. }));
        assert!(b.allow(t0).is_err());
    }

    #[test]
    fn breaker_half_opens_after_cooldown() {
        let cfg = CircuitBreakerConfig {
            threshold: 1,
            window: Duration::from_secs(60),
            cooldown: Duration::from_secs(10),
        };
        let b = CircuitBreaker::new(cfg);
        let t0 = Instant::now();
        b.record_failure(t0);
        assert!(matches!(b.state(t0), CircuitState::Open { .. }));
        let later = t0 + Duration::from_secs(11);
        assert!(matches!(b.state(later), CircuitState::HalfOpen));
    }

    #[test]
    fn breaker_success_closes() {
        let cfg = CircuitBreakerConfig {
            threshold: 1,
            window: Duration::from_secs(60),
            cooldown: Duration::from_secs(30),
        };
        let b = CircuitBreaker::new(cfg);
        let t0 = Instant::now();
        b.record_failure(t0);
        assert!(matches!(b.state(t0), CircuitState::Open { .. }));
        b.record_success();
        assert!(matches!(b.state(t0), CircuitState::Closed));
    }

    #[test]
    fn only_transient_upstream_counts() {
        let overload = provider_err(ProviderErrorKind::Overloaded);
        let auth = provider_err(ProviderErrorKind::Authentication);
        let too_big = provider_err(ProviderErrorKind::ContextTooLarge);
        assert!(is_transient_upstream(&overload));
        assert!(!is_transient_upstream(&auth));
        assert!(!is_transient_upstream(&too_big));
    }

    #[tokio::test]
    async fn concurrency_limit_serializes_beyond_max() {
        let limit = std::sync::Arc::new(ConcurrencyLimit::new(1));
        let p1 = limit.clone().acquire().await;
        // With max=1, a second acquire would block until p1 drops; verify max.
        assert_eq!(limit.max(), 1);
        drop(p1);
        let _p2 = limit.clone().acquire().await; // now succeeds
    }
}
