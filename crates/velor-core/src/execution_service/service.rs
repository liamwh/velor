//! The [`AgentExecutionService`]: the single entry point for agent execution.
//!
//! All consumers (CLI, `vel serve`, Tauri) call this service instead of building
//! subprocesses directly. It wraps any [`AgentAdapter`] into a `Send`-safe
//! [`AgentExecution`] handle (an event stream + a completion future) and runs the
//! `!Send` ACP adapter on a dedicated worker thread + `LocalSet`.
//!
//! Concurrency limiting, circuit breaking, and retry orchestration are layered
//! onto this in later phases; this module provides the routing + ownership core.

use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::agent::{AgentEvent, AgentRunResult};
use crate::execution_service::adapter::{AgentAdapter, AgentEventSink, AgentSinkError};
use crate::execution_service::adapters::acp::{AcpAdapter, AcpParams};
use crate::execution_service::adapters::claude::{ClaudeParams, ClaudeSubprocessAdapter};
use crate::execution_service::adapters::codex::{CodexParams, CodexSubprocessAdapter};
use crate::execution_service::adapters::omp::{OmpParams, OmpSubprocessAdapter};
use crate::execution_service::error::AgentExecutionError;
use crate::execution_service::policy::{
    CircuitBreaker, CircuitBreakerConfig, ConcurrencyLimit, is_transient_upstream,
};

/// One agent invocation, selecting the adapter and carrying its parameters.
///
/// All variants' parameter structs are `Send` (they cross the worker-thread
/// boundary); the adapters built from them on the worker side may be `!Send`
/// (ACP), which is why adapter construction happens on the worker's `LocalSet`.
#[derive(Debug, Clone)]
pub enum AgentProfile {
    /// Claude Code subprocess (and GLM/Z.ai Claude-compatible wrappers).
    Claude(ClaudeParams),
    /// Codex `codex exec --json`.
    Codex(CodexParams),
    /// ACP over stdio (`!Send`).
    Acp(AcpParams),
    /// Oh My Pi `omp --mode rpc`.
    Omp(OmpParams),
}

impl AgentProfile {
    /// Returns the cancellation token for this profile.
    #[must_use]
    pub fn cancellation(&self) -> &CancellationToken {
        match self {
            Self::Claude(p) => &p.cancellation,
            Self::Codex(p) => &p.cancellation,
            Self::Acp(p) => &p.cancellation,
            Self::Omp(p) => &p.cancellation,
        }
    }

    /// Returns the prompt byte length (UTF-8), for sizing metrics.
    #[must_use]
    pub fn prompt_byte_len(&self) -> usize {
        match self {
            Self::Claude(p) => p.prompt.len(),
            Self::Codex(p) => p.prompt.len(),
            Self::Acp(p) => p.prompt.len(),
            Self::Omp(p) => p.prompt.len(),
        }
    }

    /// Returns the agent binary, used to key concurrency + circuit-breaker scope.
    #[must_use]
    pub fn binary(&self) -> &str {
        match self {
            Self::Claude(p) => &p.binary,
            Self::Codex(p) => &p.binary,
            Self::Acp(p) => &p.binary,
            Self::Omp(p) => &p.binary,
        }
    }

    /// Returns the static capabilities advertised by this profile. Claude reports
    /// live steering only when its streaming path is enabled; Codex, ACP, and omp
    /// do not support live steering.
    #[must_use]
    pub fn capabilities(&self) -> crate::execution_service::capabilities::AgentCapabilities {
        use crate::execution_service::capabilities::AgentCapabilities;
        match self {
            Self::Claude(p) => {
                if p.enable_live_steering {
                    AgentCapabilities::with_live_steering()
                } else {
                    AgentCapabilities::none()
                }
            }
            Self::Codex(_) | Self::Acp(_) | Self::Omp(_) => AgentCapabilities::none(),
        }
    }

    /// Returns a new profile with the cancellation token replaced.
    ///
    /// Used by [`AgentExecutionService::execute`] to swap in a child token so
    /// that a single execution's cleanup cannot cancel the caller's parent
    /// token.
    #[must_use]
    pub fn with_cancellation(self, cancellation: CancellationToken) -> Self {
        match self {
            Self::Claude(mut p) => {
                p.cancellation = cancellation;
                Self::Claude(p)
            }
            Self::Codex(mut p) => {
                p.cancellation = cancellation;
                Self::Codex(p)
            }
            Self::Acp(mut p) => {
                p.cancellation = cancellation;
                Self::Acp(p)
            }
            Self::Omp(mut p) => {
                p.cancellation = cancellation;
                Self::Omp(p)
            }
        }
    }
}

/// Builds the concrete adapter for a profile (on the worker's `LocalSet`).
fn build_adapter(profile: AgentProfile) -> Box<dyn AgentAdapter> {
    match profile {
        AgentProfile::Claude(params) => Box::new(ClaudeSubprocessAdapter::new(params)),
        AgentProfile::Codex(params) => Box::new(CodexSubprocessAdapter::new(params)),
        AgentProfile::Acp(params) => Box::new(AcpAdapter::new(params)),
        AgentProfile::Omp(params) => Box::new(OmpSubprocessAdapter::new(params)),
    }
}

/// A finished agent run, with per-attempt records (enriched in the retry phase).
#[derive(Debug)]
pub struct AgentRunReport {
    /// The agent's result.
    pub result: AgentRunResult,
    /// Per-attempt records (single attempt until retry orchestration is added).
    pub attempts: Vec<AttemptRecord>,
}

/// Record of one execution attempt (minimal now; extended in the retry phase).
#[derive(Debug, Clone)]
pub struct AttemptRecord {
    /// Whether this attempt succeeded.
    pub succeeded: bool,
}

/// A handle to a running execution: an event stream plus a completion future.
/// No raw `JoinHandle` is exposed.
pub struct AgentExecution {
    events: Option<mpsc::Receiver<AgentEvent>>,
    completion: Option<oneshot::Receiver<Result<AgentRunReport, AgentExecutionError>>>,
    cancellation: CancellationToken,
    capabilities: crate::execution_service::capabilities::AgentCapabilities,
    /// Sender for live-steering input, `Some` only for live-steering-capable
    /// executions. The paired receiver is delivered to the adapter on the worker.
    input_sender: Option<mpsc::Sender<crate::agent::AgentInput>>,
}

impl AgentExecution {
    /// Receives the next event, or `None` when the run has finished.
    pub async fn next_event(&mut self) -> Option<AgentEvent> {
        self.events.as_mut()?.recv().await
    }

    /// Returns the static capabilities advertised by this execution's profile.
    #[must_use]
    pub fn capabilities(&self) -> crate::execution_service::capabilities::AgentCapabilities {
        self.capabilities
    }

    /// Returns a cloned sender for live-steering input, or `None` when the
    /// execution does not support live steering. A successful channel send here
    /// only means Vel accepted the command — not that the agent received it; the
    /// command's acknowledgement carries the authoritative delivery state.
    #[must_use]
    pub fn input_sender(&self) -> Option<mpsc::Sender<crate::agent::AgentInput>> {
        self.input_sender.clone()
    }

    /// Returns the current live-steering availability for this execution.
    ///
    /// `Unsupported` when the profile has no live steering; `Ready` while a
    /// writable input sender exists and the run has not yet completed;
    /// `Closing` once the completion has been consumed (the run is finishing);
    /// `Inactive` otherwise.
    #[must_use]
    pub fn live_steering_status(
        &self,
    ) -> crate::execution_service::capabilities::LiveSteeringStatus {
        use crate::execution_service::capabilities::LiveSteeringStatus;
        if !self.capabilities.live_steering {
            return LiveSteeringStatus::Unsupported;
        }
        if self.completion.is_none() {
            return LiveSteeringStatus::Closing;
        }
        if self.input_sender.is_some() {
            LiveSteeringStatus::Ready
        } else {
            LiveSteeringStatus::Inactive
        }
    }

    /// Waits for the run to finish and returns its report.
    ///
    /// # Errors
    /// Returns [`AgentExecutionError`] if the run failed.
    pub async fn complete(mut self) -> Result<AgentRunReport, AgentExecutionError> {
        self.events.take(); // drop the receiver so the worker's sink can close
        match self.completion.take() {
            Some(rx) => match rx.await {
                Ok(result) => result,
                Err(_) => Err(AgentExecutionError::Cancelled),
            },
            None => Err(AgentExecutionError::Cancelled),
        }
    }

    /// Requests cancellation and waits for the (reaped) report.
    ///
    /// # Errors
    /// Returns [`AgentExecutionError::Cancelled`] (or the failure that preceded
    /// cancellation).
    pub async fn cancel(self) -> Result<AgentRunReport, AgentExecutionError> {
        self.cancellation.cancel();
        self.complete().await
    }

    /// Returns the cancellation token.
    #[must_use]
    pub fn cancellation(&self) -> &CancellationToken {
        &self.cancellation
    }
}

impl Drop for AgentExecution {
    fn drop(&mut self) {
        // If the consumer never awaited complete()/cancel(), cancel so the
        // worker-side adapter stops and reaps its child.
        self.cancellation.cancel();
    }
}

/// A job sent to the worker thread.
struct WorkerJob {
    profile: AgentProfile,
    event_tx: mpsc::Sender<AgentEvent>,
    result_tx: oneshot::Sender<Result<AgentRunReport, AgentExecutionError>>,
    scope: Option<Arc<ScopeState>>,
    /// Runtime live-steering receiver, delivered to the adapter. `None` for
    /// non-steering-capable executions.
    live_input: Option<mpsc::Receiver<crate::agent::AgentInput>>,
}

/// Per-scope policy state: a concurrency limit and a circuit breaker, keyed by
/// the agent binary so independent providers/profiles don't interfere.
struct ScopeState {
    concurrency: Arc<ConcurrencyLimit>,
    breaker: CircuitBreaker,
}

/// Configuration for the service's scoped policy.
#[derive(Debug, Clone)]
pub struct ServicePolicy {
    /// Maximum concurrent runs per scope (per agent binary).
    pub concurrency_max: usize,
    /// Circuit-breaker configuration per scope.
    pub breaker: CircuitBreakerConfig,
}

impl Default for ServicePolicy {
    fn default() -> Self {
        Self {
            concurrency_max: 4,
            breaker: CircuitBreakerConfig::default(),
        }
    }
}

/// The long-lived agent execution service. One instance per application context.
///
/// Spawns a dedicated worker thread running a current-thread Tokio runtime +
/// `LocalSet`, where every adapter runs (uniformly handling `!Send` ACP). The
/// public [`AgentExecution`] handle is `Send`-safe. Per-scope concurrency
/// limiting and a circuit breaker bound overlapping/overloaded runs.
pub struct AgentExecutionService {
    job_tx: mpsc::Sender<WorkerJob>,
    _worker: Arc<WorkerHandle>,
    scopes: Mutex<HashMap<String, Arc<ScopeState>>>,
    policy: ServicePolicy,
}

struct WorkerHandle {
    _thread: std::thread::JoinHandle<()>,
}

impl AgentExecutionService {
    /// Creates a new service with the default scoped policy.
    #[must_use]
    pub fn new() -> Self {
        Self::with_policy(ServicePolicy::default())
    }

    /// Creates a new service with a custom scoped policy.
    #[must_use]
    pub fn with_policy(policy: ServicePolicy) -> Self {
        let (job_tx, mut job_rx) = mpsc::channel::<WorkerJob>(64);
        let thread = std::thread::Builder::new()
            .name("velor-agent-exec".into())
            .spawn(move || {
                worker_main(&mut job_rx);
            })
            .expect("spawn agent-exec worker");
        Self {
            job_tx,
            _worker: Arc::new(WorkerHandle { _thread: thread }),
            scopes: Mutex::new(HashMap::new()),
            policy,
        }
    }

    /// Returns (creating if needed) the policy state for a scope.
    fn scope_state(&self, key: &str) -> Arc<ScopeState> {
        let mut g = self.scopes.lock().expect("scopes mutex poisoned");
        g.entry(key.to_string())
            .or_insert_with(|| {
                Arc::new(ScopeState {
                    concurrency: Arc::new(ConcurrencyLimit::new(self.policy.concurrency_max)),
                    breaker: CircuitBreaker::new(self.policy.breaker),
                })
            })
            .clone()
    }

    /// Starts an execution, returning a handle to its event stream + completion.
    ///
    /// # Errors
    /// Returns [`AgentExecutionError::CircuitOpen`] if the scope's breaker is
    /// open, or [`AgentExecutionError::Cancelled`] if the worker has stopped.
    pub async fn execute(
        &self,
        profile: AgentProfile,
    ) -> Result<AgentExecution, AgentExecutionError> {
        // Derive a child token so that `AgentExecution`'s `Drop` (which cancels
        // to reap an abandoned subprocess) only cancels this single execution,
        // NOT the caller's parent token. The child is still cancelled when the
        // parent fires (Ctrl+C×2), so cancellation still propagates downward.
        let parent = profile.cancellation().clone();
        let cancellation = parent.child_token();
        // Replace the profile's token with the child so the adapter/supervisor
        // observes the child (not the parent).
        let profile = profile.with_cancellation(cancellation.clone());
        let scope_key = profile.binary().to_string();
        let scope = self.scope_state(&scope_key);
        // Circuit-breaker gate: refuse fast if the upstream is saturated.
        if let Err(until) = scope.breaker.allow(Instant::now()) {
            return Err(AgentExecutionError::CircuitOpen {
                scope: scope_key,
                until,
            });
        }
        let (event_tx, event_rx) = mpsc::channel::<AgentEvent>(256);
        let (result_tx, result_rx) = oneshot::channel();
        // For live-steering-capable profiles, create the steering channel: the
        // receiver travels to the adapter on the worker, the sender stays on the
        // handle so callers can submit [`crate::agent::AgentInput`] commands.
        let capabilities = profile.capabilities();
        let (input_tx, input_rx) = if capabilities.live_steering {
            let (tx, rx) = mpsc::channel::<crate::agent::AgentInput>(16);
            (Some(tx), Some(rx))
        } else {
            (None, None)
        };
        let job = WorkerJob {
            profile,
            event_tx,
            result_tx,
            scope: Some(scope),
            live_input: input_rx,
        };
        if self.job_tx.send(job).await.is_err() {
            return Err(AgentExecutionError::Cancelled);
        }
        Ok(AgentExecution {
            events: Some(event_rx),
            completion: Some(result_rx),
            cancellation,
            capabilities,
            input_sender: input_tx,
        })
    }
}

impl Default for AgentExecutionService {
    fn default() -> Self {
        Self::new()
    }
}

/// Returns the process-global shared [`AgentExecutionService`] (one worker
/// thread per app context), creating it on first use.
///
/// This is the canonical entry point for consumers that build an
/// [`AgentProfile`] and want streaming events + a completion future. Legacy
/// callers that still go through [`crate::agent::AgentRunner`] also route here.
#[must_use]
pub fn shared_service() -> &'static AgentExecutionService {
    use std::sync::OnceLock;
    static SERVICE: OnceLock<AgentExecutionService> = OnceLock::new();
    SERVICE.get_or_init(AgentExecutionService::new)
}

/// The worker loop: builds each adapter on the LocalSet and runs it.
fn worker_main(job_rx: &mut mpsc::Receiver<WorkerJob>) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(_) => return,
    };
    let local = tokio::task::LocalSet::new();
    local.block_on(&runtime, async move {
        while let Some(job) = job_rx.recv().await {
            tokio::task::spawn_local(run_one_job(job));
        }
    });
}

/// Runs one job: builds the adapter, drives it with a channel sink, sends the report.
async fn run_one_job(job: WorkerJob) {
    let WorkerJob {
        profile,
        event_tx,
        result_tx,
        scope,
        live_input,
    } = job;
    // Acquire the per-scope concurrency permit (held for the run; released on drop)
    // so overlapping runs against the same provider are bounded.
    let _permit = match &scope {
        Some(s) => Some(s.concurrency.clone().acquire().await),
        None => None,
    };
    let mut adapter = build_adapter(profile);
    let mut sink = ChannelSink { tx: event_tx };
    let result = adapter.execute(&mut sink, live_input).await;
    // Update the circuit breaker: success closes it; only transient upstream
    // failures count toward opening it.
    if let Some(s) = &scope {
        match &result {
            Ok(_) => s.breaker.record_success(),
            Err(e) if is_transient_upstream(e) => s.breaker.record_failure(Instant::now()),
            Err(_) => {}
        }
    }
    let report = result.map(|r| AgentRunReport {
        result: r,
        attempts: vec![AttemptRecord { succeeded: true }],
    });
    // result_tx is dropped here; if the consumer already dropped the handle, the
    // send fails harmlessly.
    let _ = result_tx.send(report);
}

/// An [`AgentEventSink`] that forwards events over an mpsc channel.
struct ChannelSink {
    tx: mpsc::Sender<AgentEvent>,
}

#[async_trait(?Send)]
impl AgentEventSink for ChannelSink {
    async fn emit(&mut self, event: AgentEvent) -> Result<(), AgentSinkError> {
        // Best-effort: a full channel drops verbose events, and a closed channel
        // (the consumer dropped the receiver, e.g. the non-streaming run() path)
        // silently drops events too. The sink must NEVER abort the run — a dropped
        // consumer is not a cancellation request; cancellation flows through the
        // CancellationToken. This keeps the adapter running to completion.
        let _ = self.tx.try_send(event);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution_service::adapters::claude::ClaudeParams;

    #[test]
    fn with_cancellation_replaces_token() {
        let parent = CancellationToken::new();
        let child = parent.child_token();
        let profile = AgentProfile::Claude(ClaudeParams::new(
            "claude",
            bytes::Bytes::new(),
            std::path::PathBuf::from("/tmp"),
        ));
        let profile = profile.with_cancellation(child);
        // The profile's token should now be the child, not the default token.
        assert!(profile.cancellation().is_cancelled() == parent.is_cancelled());
        // Cancelling the child must NOT cancel the parent.
        profile.cancellation().cancel();
        assert!(!parent.is_cancelled());
    }

    #[test]
    fn child_token_propagates_parent_cancellation() {
        let parent = CancellationToken::new();
        let child = parent.child_token();
        let profile = AgentProfile::Claude(ClaudeParams::new(
            "claude",
            bytes::Bytes::new(),
            std::path::PathBuf::from("/tmp"),
        ));
        let profile = profile.with_cancellation(child);
        // Cancelling the parent MUST cancel the child (downward propagation).
        parent.cancel();
        assert!(profile.cancellation().is_cancelled());
    }
}
