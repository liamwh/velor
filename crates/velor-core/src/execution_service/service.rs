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
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::agent::{AgentEvent, AgentRunResult};
use crate::execution_service::adapter::{AgentAdapter, AgentEventSink, AgentSinkError};
use crate::execution_service::adapters::acp::{AcpAdapter, AcpParams};
use crate::execution_service::adapters::claude::{ClaudeParams, ClaudeSubprocessAdapter};
use crate::execution_service::adapters::codex::{CodexParams, CodexSubprocessAdapter};
use crate::execution_service::error::AgentExecutionError;

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
}

impl AgentProfile {
    /// Returns the cancellation token for this profile.
    #[must_use]
    pub fn cancellation(&self) -> &CancellationToken {
        match self {
            Self::Claude(p) => &p.cancellation,
            Self::Codex(p) => &p.cancellation,
            Self::Acp(p) => &p.cancellation,
        }
    }

    /// Returns the prompt byte length (UTF-8), for sizing metrics.
    #[must_use]
    pub fn prompt_byte_len(&self) -> usize {
        match self {
            Self::Claude(p) => p.prompt.len(),
            Self::Codex(p) => p.prompt.len(),
            Self::Acp(p) => p.prompt.len(),
        }
    }
}

/// Builds the concrete adapter for a profile (on the worker's `LocalSet`).
fn build_adapter(profile: AgentProfile) -> Box<dyn AgentAdapter> {
    match profile {
        AgentProfile::Claude(params) => Box::new(ClaudeSubprocessAdapter::new(params)),
        AgentProfile::Codex(params) => Box::new(CodexSubprocessAdapter::new(params)),
        AgentProfile::Acp(params) => Box::new(AcpAdapter::new(params)),
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
}

impl AgentExecution {
    /// Receives the next event, or `None` when the run has finished.
    pub async fn next_event(&mut self) -> Option<AgentEvent> {
        self.events.as_mut()?.recv().await
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
}

/// The long-lived agent execution service. One instance per application context.
///
/// Spawns a dedicated worker thread running a current-thread Tokio runtime +
/// `LocalSet`, where every adapter runs (uniformly handling `!Send` ACP). The
/// public [`AgentExecution`] handle is `Send`-safe.
pub struct AgentExecutionService {
    job_tx: mpsc::Sender<WorkerJob>,
    _worker: Arc<WorkerHandle>,
}

struct WorkerHandle {
    _thread: std::thread::JoinHandle<()>,
}

impl AgentExecutionService {
    /// Creates a new service, starting its worker thread.
    #[must_use]
    pub fn new() -> Self {
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
        }
    }

    /// Starts an execution, returning a handle to its event stream + completion.
    ///
    /// # Errors
    /// Returns an error only if the worker has stopped accepting jobs.
    pub async fn execute(
        &self,
        profile: AgentProfile,
    ) -> Result<AgentExecution, AgentExecutionError> {
        let cancellation = profile.cancellation().clone();
        let (event_tx, event_rx) = mpsc::channel::<AgentEvent>(256);
        let (result_tx, result_rx) = oneshot::channel();
        let job = WorkerJob {
            profile,
            event_tx,
            result_tx,
        };
        if self.job_tx.send(job).await.is_err() {
            return Err(AgentExecutionError::Cancelled);
        }
        Ok(AgentExecution {
            events: Some(event_rx),
            completion: Some(result_rx),
            cancellation,
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
    } = job;
    let mut adapter = build_adapter(profile);
    let mut sink = ChannelSink { tx: event_tx };
    let result = adapter.execute(&mut sink).await;
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
        // A slow consumer must not stall adapter draining. The supervisor drains
        // unconditionally; here a full channel drops verbose events (terminal /
        // error events are emitted before verbose deltas in practice). This keeps
        // the adapter responsive.
        self.tx.try_send(event).map_err(|_| AgentSinkError)
    }
}
