//! The process supervisor: the one owner of a spawned child for its entire life.
//!
//! Design rules (see the overhaul plan):
//!
//! - **One owner.** A single supervisor task owns the [`AsyncGroupChild`] from
//!   spawn to reap. Consumers only receive [`ProcessEvent`]s and a completion
//!   handle; there is no asynchronous work in `Drop`.
//! - **Byte chunks, not lines.** The supervisor emits raw [`bytes::Bytes`]; UTF-8
//!   decoding and protocol framing are the adapter's responsibility.
//! - **Deadlock-free.** stdin write, stdout drain, and stderr drain run as
//!   independent tasks, so a child that writes 10 MB while we write a large prompt
//!   cannot wedge a pipe.
//! - **Whole-group lifecycle.** The child runs in its own process group (Unix) or
//!   job object (Windows, via `command-group`). On timeout or cancellation:
//!   - **Unix:** `SIGTERM` the group → wait up to `termination_grace` → `SIGKILL`
//!     the group → reap the direct child. `killpg` reaches the whole group, so
//!     grandchildren (the `node`/`claude` provider client beneath the wrapper) are
//!     signalled too.
//!   - **Non-Unix:** the group is killed immediately (no graceful phase — there is
//!     no `SIGTERM` equivalent in the job-object path) and the direct child reaped.
//!   Descendant termination is best-effort on the configured mechanism: on Unix it
//!   depends on grandchildren staying in the spawned process group (a child that
//!   itself calls `setsid`/`setpgid` can escape); on Windows it depends on the job
//!   object. It is not a universal kernel guarantee.

use bytes::Bytes;
use command_group::{AsyncCommandGroup, AsyncGroupChild};
use std::ffi::OsString;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::process::Stdio;
use std::sync::Arc;
use std::task::Poll;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{ChildStdin, ChildStdout};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::execution_service::error::{ProcessError, TimeoutKind};
use crate::execution_service::output::{CaptureBuilder, OutputStream, ProcessOutput, Termination};

/// How the prompt is delivered to the child's standard input.
#[derive(Debug, Clone)]
pub enum ProcessInput {
    /// Inherit Velor's stdin (rarely used; adapters pass bytes).
    Inherit,
    /// Connect `/dev/null` to the child's stdin.
    Null,
    /// Write these bytes, then close stdin to signal end-of-file.
    Bytes(Bytes),
    /// Keep stdin open for the life of the process. The `initial` frame is
    /// written and flushed up front (and acknowledged via
    /// [`ProcessEvent::StdinInitialised`]); further frames are sent on demand
    /// through the supervisor's command channel (see [`ProcessInputCommand`]).
    ///
    /// Only an explicit [`ProcessInputCommand::Close`] — sent by the central
    /// input-controller shutdown path — closes stdin. Dropping command senders
    /// does not by itself close it.
    Streaming {
        /// The first frame written to the child's stdin.
        initial: Bytes,
    },
}

impl ProcessInput {
    /// Returns the byte payload if this is [`ProcessInput::Bytes`].
    #[must_use]
    pub fn as_bytes(&self) -> Option<&Bytes> {
        match self {
            Self::Bytes(b) => Some(b),
            Self::Inherit | Self::Null | Self::Streaming { .. } => None,
        }
    }
}

/// The set of deadlines that bound one process attempt.
///
/// All fields are optional except `termination_grace`. A coding agent can
/// legitimately run for a long time while producing regular output, so an
/// [`ProcessTimeouts::idle`] deadline is usually a safer bound than a tight total.
#[derive(Debug, Clone)]
pub struct ProcessTimeouts {
    /// Maximum time to wait for the first output (otherwise [`TimeoutKind::Startup`]).
    pub startup: Option<Duration>,
    /// Maximum time to spend writing the prompt to stdin (otherwise [`TimeoutKind::StdinWrite`]).
    pub stdin_write: Option<Duration>,
    /// Maximum gap between output chunks (otherwise [`TimeoutKind::Idle`]).
    pub idle: Option<Duration>,
    /// Hard deadline measured from spawn (otherwise [`TimeoutKind::Total`]).
    pub total: Option<Duration>,
    /// Grace period between graceful (SIGTERM) and forced (SIGKILL) termination.
    pub termination_grace: Duration,
}

impl Default for ProcessTimeouts {
    fn default() -> Self {
        Self {
            startup: None,
            stdin_write: None,
            idle: None,
            total: None,
            termination_grace: Duration::from_secs(5),
        }
    }
}

/// A provider-agnostic process invocation specification.
#[derive(Debug, Clone)]
pub struct ProcessSpec {
    /// Program to execute (resolved through the same `PATH` semantics as
    /// `std::process::Command`).
    pub program: OsString,
    /// Arguments passed directly (no shell interpolation).
    pub args: Vec<OsString>,
    /// Working directory (`None` inherits Velor's).
    pub cwd: Option<PathBuf>,
    /// Environment overrides. `None` unsets the variable; `Some` sets it.
    pub env: Vec<(OsString, Option<OsString>)>,
    /// What to write to the child's stdin.
    pub input: ProcessInput,
    /// Deadlines for this attempt.
    pub timeouts: ProcessTimeouts,
    /// Per-stream capture cap (bytes retained for head and for tail).
    pub capture_bytes: usize,
}

impl ProcessSpec {
    /// Creates a builder for a program.
    #[must_use]
    pub fn builder(program: impl Into<OsString>) -> ProcessSpecBuilder {
        ProcessSpecBuilder {
            spec: ProcessSpec {
                program: program.into(),
                args: Vec::new(),
                cwd: None,
                env: Vec::new(),
                input: ProcessInput::Null,
                timeouts: ProcessTimeouts::default(),
                capture_bytes: 64 * 1024,
            },
        }
    }
}

/// Builder for [`ProcessSpec`].
#[derive(Debug, Clone)]
pub struct ProcessSpecBuilder {
    spec: ProcessSpec,
}

impl ProcessSpecBuilder {
    /// Appends an argument.
    #[must_use]
    pub fn arg(mut self, arg: impl Into<OsString>) -> Self {
        self.spec.args.push(arg.into());
        self
    }
    /// Appends several arguments.
    #[must_use]
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.spec.args.extend(args.into_iter().map(Into::into));
        self
    }
    /// Sets the working directory.
    #[must_use]
    pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.spec.cwd = Some(cwd.into());
        self
    }
    /// Sets an environment variable.
    #[must_use]
    pub fn env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.spec.env.push((key.into(), Some(value.into())));
        self
    }
    /// Marks an environment variable for removal in the child.
    #[must_use]
    pub fn env_remove(mut self, key: impl Into<OsString>) -> Self {
        self.spec.env.push((key.into(), None));
        self
    }
    /// Sets the stdin input.
    #[must_use]
    pub fn input(mut self, input: ProcessInput) -> Self {
        self.spec.input = input;
        self
    }
    /// Sets the deadlines.
    #[must_use]
    pub fn timeouts(mut self, timeouts: ProcessTimeouts) -> Self {
        self.spec.timeouts = timeouts;
        self
    }
    /// Sets the per-stream capture cap.
    #[must_use]
    pub fn capture_bytes(mut self, cap: usize) -> Self {
        self.spec.capture_bytes = cap;
        self
    }
    /// Builds the specification.
    #[must_use]
    pub fn build(self) -> ProcessSpec {
        self.spec
    }
}

/// Commands sent to the supervisor's long-lived stdin writer when a process is
/// spawned with [`ProcessInput::Streaming`]. Each carries an acknowledgement
/// that resolves only once the bytes have been written to *and flushed through*
/// the child-process pipe — never merely queued.
#[derive(Debug)]
pub enum ProcessInputCommand {
    /// Write and flush these bytes, then report the outcome.
    Write {
        /// The frame to deliver.
        bytes: Bytes,
        /// Resolves with `Ok` once the bytes are written and flushed.
        acknowledgement: oneshot::Sender<Result<(), ProcessInputWriteError>>,
    },
    /// Shut the child's stdin down (send EOF). Only the central input-controller
    /// shutdown path sends this; dropping command senders does not.
    Close {
        /// Resolves once stdin has been shut down (or was already closed).
        acknowledgement: oneshot::Sender<Result<(), ProcessInputWriteError>>,
    },
}

/// Why writing to (or shutting down) a streaming child stdin failed.
///
/// Carries a real [`std::io::Error`] source (e.g. a broken pipe when the child
/// dies) but is still [`Clone`] so a failure can be acknowledged to the command
/// sender *and* surfaced as a [`ProcessEvent::StdinWriteFailed`] event. The
/// clone reconstructs the error from its OS code where possible.
#[derive(Debug, thiserror::Error)]
pub enum ProcessInputWriteError {
    /// The child process stdin has already closed.
    #[error("the child process stdin has already closed")]
    Closed,
    /// Failed to write to the child process stdin.
    #[error("failed to write to child process stdin")]
    Write {
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// Failed to flush the child process stdin.
    #[error("failed to flush child process stdin")]
    Flush {
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// Failed to shut down the child process stdin.
    #[error("failed to shut down child process stdin")]
    Shutdown {
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
}

impl Clone for ProcessInputWriteError {
    fn clone(&self) -> Self {
        match self {
            Self::Closed => Self::Closed,
            Self::Write { source } => Self::Write {
                source: clone_io_error(source),
            },
            Self::Flush { source } => Self::Flush {
                source: clone_io_error(source),
            },
            Self::Shutdown { source } => Self::Shutdown {
                source: clone_io_error(source),
            },
        }
    }
}

/// Reconstructs an [`std::io::Error`] from its OS code where possible, else from
/// its display string — the closest available [`Clone`] for a non-`Clone` error.
fn clone_io_error(e: &std::io::Error) -> std::io::Error {
    match e.raw_os_error() {
        Some(code) => std::io::Error::from_raw_os_error(code),
        None => std::io::Error::other(e.to_string()),
    }
}

/// A timestamped chunk of process output.
#[derive(Debug, Clone)]
pub struct ProcessChunk {
    /// Monotonic sequence number across both streams for this process.
    pub sequence: u64,
    /// When the chunk was observed.
    pub observed_at: Instant,
    /// The raw bytes (may be non-UTF-8).
    pub bytes: Bytes,
}

/// Events emitted by a running process. Carries raw bytes; framing is the
/// consumer's responsibility.
#[derive(Debug, Clone)]
pub enum ProcessEvent {
    /// A chunk read from stdout.
    Stdout(ProcessChunk),
    /// A chunk read from stderr.
    Stderr(ProcessChunk),
    /// The prompt was fully written to stdin and stdin was closed (EOF) — the
    /// one-shot [`ProcessInput::Bytes`] path.
    StdinWritten,
    /// The initial frame of a [`ProcessInput::Streaming`] process was written
    /// and flushed; stdin remains open for further commands.
    StdinInitialised,
    /// A write to a streaming process's stdin failed. Delivery of further
    /// commands will report [`ProcessInputWriteError::Closed`].
    StdinWriteFailed(ProcessInputWriteError),
    /// The child exited and output draining is complete.
    Exited,
}

impl ProcessEvent {
    /// Returns the chunk if this is a stdout/stderr chunk.
    #[must_use]
    pub fn chunk(&self) -> Option<&ProcessChunk> {
        match self {
            Self::Stdout(c) | Self::Stderr(c) => Some(c),
            Self::StdinWritten
            | Self::StdinInitialised
            | Self::StdinWriteFailed(_)
            | Self::Exited => None,
        }
    }
}

/// A handle to a running process. The supervisor task owns the child; this handle
/// only exposes events and completion.
///
/// Deterministic cleanup requires awaiting [`RunningProcess::complete`] or
/// [`RunningProcess::cancel`]. Dropping the handle cancels the process token and
/// detaches the supervisor so it still terminates and reaps the child, but the
/// final [`ProcessOutput`] is then unavailable.
pub struct RunningProcess {
    events: Option<mpsc::Receiver<ProcessEvent>>,
    completion: Option<JoinHandle<Result<ProcessOutput, ProcessError>>>,
    cancellation: CancellationToken,
    /// Sender for streaming-stdin commands. `Some` only when the process was
    /// spawned with [`ProcessInput::Streaming`]; otherwise `None`.
    input_sender: Option<mpsc::Sender<ProcessInputCommand>>,
}

impl RunningProcess {
    /// Receives the next event, or `None` when the supervisor has finished.
    pub async fn next_event(&mut self) -> Option<ProcessEvent> {
        let events = self.events.as_mut()?;
        events.recv().await
    }

    /// Returns a cloned sender for streaming-stdin commands, or `None` when the
    /// process was not spawned with [`ProcessInput::Streaming`] (or the writer has
    /// gone away). Prefer this protocol-neutral accessor over a steering-specific
    /// one: the supervisor owns no steering semantics, only framed input.
    #[must_use]
    pub fn input_sender(&self) -> Option<mpsc::Sender<ProcessInputCommand>> {
        self.input_sender.clone()
    }

    /// Polls for the next event without blocking.
    pub fn poll_next_event(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<ProcessEvent>> {
        match self.events.as_mut() {
            Some(events) => events.poll_recv(cx),
            None => Poll::Ready(None),
        }
    }

    /// Waits for the process to finish naturally and returns its output.
    ///
    /// # Errors
    /// Returns [`ProcessError`] if the process failed, timed out, was cancelled,
    /// or the supervisor task panicked.
    pub async fn complete(mut self) -> Result<ProcessOutput, ProcessError> {
        if let Some(events) = self.events.take() {
            drop(events);
        }
        match self.completion.take() {
            Some(handle) => join_completion(handle).await,
            None => Err(ProcessError::Spawn {
                executable: PathBuf::from("<supervisor>"),
                source: std::io::Error::other("supervisor already consumed"),
            }),
        }
    }

    /// Requests cancellation (terminating the process group) and waits for the
    /// reaped output. The outcome's [`Termination`] will be [`Termination::Cancelled`].
    ///
    /// # Errors
    /// See [`RunningProcess::complete`].
    pub async fn cancel(self) -> Result<ProcessOutput, ProcessError> {
        self.cancellation.cancel();
        self.complete().await
    }

    /// Returns the cancellation token for this process.
    #[must_use]
    pub fn cancellation(&self) -> &CancellationToken {
        &self.cancellation
    }
}

impl Drop for RunningProcess {
    fn drop(&mut self) {
        // If the consumer never awaited complete()/cancel(), request cancellation
        // so the detached supervisor still terminates and reaps the child. We
        // cannot await here (Drop is synchronous).
        self.cancellation.cancel();
    }
}

/// Spawns a process under supervision and returns a [`RunningProcess`] handle.
///
/// The child is placed in its own process group. The returned handle is the only
/// way to observe events and collect the final output.
///
/// # Errors
/// Returns [`ProcessError::ExecutableNotFound`] / [`ProcessError::PermissionDenied`]
/// / [`ProcessError::Spawn`] if the process cannot be started.
pub async fn spawn(
    spec: ProcessSpec,
    cancellation: CancellationToken,
) -> Result<RunningProcess, ProcessError> {
    let mut cmd = tokio::process::Command::new(&spec.program);
    cmd.args(&spec.args);
    if let Some(cwd) = &spec.cwd {
        cmd.current_dir(cwd);
    }
    for (key, value) in &spec.env {
        match value {
            Some(value) => {
                cmd.env(key, value);
            }
            None => {
                cmd.env_remove(key);
            }
        }
    }
    let stdin = match &spec.input {
        ProcessInput::Inherit => Stdio::inherit(),
        ProcessInput::Null | ProcessInput::Bytes(_) | ProcessInput::Streaming { .. } => {
            Stdio::piped()
        }
    };
    cmd.stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let program = PathBuf::from(&spec.program);
    let mut group_child = cmd
        .group_spawn()
        .map_err(|e| ProcessError::from_spawn_error(program.clone(), e))?;

    let child_stdin = group_child.inner().stdin.take();
    let child_stdout = group_child.inner().stdout.take();
    let child_stderr = group_child.inner().stderr.take();

    let (events_tx, events_rx) = mpsc::channel::<ProcessEvent>(256);
    let process_token = cancellation.child_token();

    // Only the streaming variant owns a long-lived stdin and a command channel.
    let (input_tx, input_rx) = if matches!(spec.input, ProcessInput::Streaming { .. }) {
        let (tx, rx) = mpsc::channel::<ProcessInputCommand>(16);
        (Some(tx), Some(rx))
    } else {
        (None, None)
    };

    let cap = spec.capture_bytes;
    let completion = tokio::spawn(supervise(
        group_child,
        child_stdin,
        child_stdout,
        child_stderr,
        spec.input.clone(),
        spec.timeouts.clone(),
        process_token,
        events_tx,
        input_rx,
        cap,
    ));

    Ok(RunningProcess {
        events: Some(events_rx),
        completion: Some(completion),
        cancellation,
        input_sender: input_tx,
    })
}

/// Convenience: spawn, collect all events (discarding them), and return the
/// final output. Use when the caller does not need streaming events.
///
/// # Errors
/// See [`spawn`] and [`RunningProcess::complete`].
pub async fn run(
    spec: ProcessSpec,
    cancellation: CancellationToken,
) -> Result<ProcessOutput, ProcessError> {
    let process = spawn(spec, cancellation).await?;
    process.complete().await
}

/// Internal chunk sent from a drain task to the supervisor.
#[derive(Debug)]
struct DrainChunk {
    stream: OutputStream,
    bytes: Bytes,
}

/// The supervisor task. Owns the child and all I/O for its entire life.
#[allow(clippy::too_many_arguments)]
async fn supervise(
    mut group_child: AsyncGroupChild,
    stdin: Option<ChildStdin>,
    stdout: Option<ChildStdout>,
    stderr: Option<tokio::process::ChildStderr>,
    input: ProcessInput,
    timeouts: ProcessTimeouts,
    cancel: CancellationToken,
    events_tx: mpsc::Sender<ProcessEvent>,
    input_rx: Option<mpsc::Receiver<ProcessInputCommand>>,
    cap: usize,
) -> Result<ProcessOutput, ProcessError> {
    let pid = group_child.id();
    let started = Instant::now();

    let (drain_tx, mut drain_rx) = mpsc::channel::<DrainChunk>(64);
    let dropped = Arc::new(std::sync::atomic::AtomicU64::new(0));

    let stdout_handle =
        stdout.map(|s| tokio::spawn(drain_stream(s, OutputStream::Stdout, drain_tx.clone())));
    let stderr_handle =
        stderr.map(|s| tokio::spawn(drain_stream(s, OutputStream::Stderr, drain_tx.clone())));
    // Drop our copy so `drain_rx` returning None means both drains are done.
    drop(drain_tx);

    // The stdin writer. For the one-shot path it writes the bytes and closes; for
    // the streaming path it owns the long-lived stdin and serves acknowledged
    // commands until the process terminates or the supervisor shuts down.
    // `stdin_close` is an internal token the supervisor cancels once its main loop
    // ends, guaranteeing the streaming writer always exits (never hangs on recv).
    let stdin_close = CancellationToken::new();
    let stdin_handle = match (stdin, input, input_rx) {
        (Some(s), ProcessInput::Streaming { initial }, Some(rx)) => {
            Some(tokio::spawn(streaming_stdin_writer(
                s,
                initial,
                rx,
                events_tx.clone(),
                stdin_close.clone(),
                timeouts.stdin_write,
            )))
        }
        (Some(s), one_shot_input, _) => Some(tokio::spawn(write_stdin(
            s,
            one_shot_input,
            timeouts.stdin_write,
            events_tx.clone(),
        ))),
        (None, _, _) => None,
    };

    let mut stdout_cap = CaptureBuilder::new(cap);
    let mut stderr_cap = CaptureBuilder::new(cap);
    let mut last_activity: Option<Instant> = None;
    let mut startup_satisfied = false;
    let mut seq: u64 = 0;
    let mut drains_closed = false;
    let mut termination: Option<Termination> = None;

    // Forward a drained chunk to capture + the consumer events channel. Activity
    // bookkeeping (last_activity / startup_satisfied) is handled by the caller so
    // the post-loop drain can reuse this without dead stores.
    macro_rules! ingest {
        ($chunk:expr) => {{
            let chunk: DrainChunk = $chunk;
            seq = seq.saturating_add(1);
            let observed_at = Instant::now();
            match chunk.stream {
                OutputStream::Stdout => stdout_cap.push(&chunk.bytes),
                OutputStream::Stderr => stderr_cap.push(&chunk.bytes),
            }
            let pc = ProcessChunk {
                sequence: seq,
                observed_at,
                bytes: chunk.bytes,
            };
            let event = match chunk.stream {
                OutputStream::Stdout => ProcessEvent::Stdout(pc),
                OutputStream::Stderr => ProcessEvent::Stderr(pc),
            };
            if events_tx.try_send(event).is_err() {
                dropped.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        }};
    }

    let result = async {
        loop {
            let total_deadline = timeouts.total.and_then(|d| started.checked_add(d));
            let startup_deadline = (!startup_satisfied)
                .then(|| timeouts.startup.and_then(|d| started.checked_add(d)))
                .flatten();
            let idle_deadline = match (timeouts.idle, last_activity) {
                (Some(d), Some(t)) => t.checked_add(d),
                _ => None,
            };
            let next = earliest(total_deadline, earliest(startup_deadline, idle_deadline));

            tokio::select! {
                biased;

                _ = cancel.cancelled() => {
                    termination = Some(Termination::Cancelled);
                    break;
                }

                _ = sleep_until_option(next), if next.is_some() => {
                    let fired_at = Instant::now();
                    let which = if total_deadline.is_some_and(|d| fired_at >= d) {
                        TimeoutKind::Total
                    } else if startup_deadline.is_some_and(|d| fired_at >= d) {
                        TimeoutKind::Startup
                    } else {
                        TimeoutKind::Idle
                    };
                    termination = Some(Termination::TimedOut { which });
                    break;
                }

                status = group_child.inner().wait() => {
                    match status {
                        Ok(s) => {
                            termination = Some(Termination::Exited(s));
                            break;
                        }
                        Err(e) => return Err(ProcessError::Reap { source: e }),
                    }
                }

                chunk = drain_rx.recv(), if !drains_closed => {
                    match chunk {
                        Some(c) => {
                            ingest!(c);
                            last_activity = Some(Instant::now());
                            startup_satisfied = true;
                        }
                        None => drains_closed = true,
                    }
                }
            }
        }
        Ok::<(), ProcessError>(())
    }
    .await;

    if let Err(e) = result {
        // Best-effort cleanup before propagating.
        terminate_and_reap(&mut group_child, timeouts.termination_grace).await;
        let _ = stdout_handle.map(|h| h.abort());
        let _ = stderr_handle.map(|h| h.abort());
        let _ = stdin_handle.map(|h| h.abort());
        return Err(e);
    }

    let Some(term) = termination else {
        // Unreachable: every loop path sets `termination` before breaking.
        return Err(ProcessError::Spawn {
            executable: PathBuf::from("<supervisor>"),
            source: std::io::Error::other("supervisor exited without termination"),
        });
    };

    // If we broke out due to a timeout or cancellation, terminate + reap the group.
    if matches!(term, Termination::TimedOut { .. } | Termination::Cancelled) {
        terminate_and_reap(&mut group_child, timeouts.termination_grace).await;
    }

    // Drain any output that arrived between the break and the kill.
    while let Some(c) = drain_rx.recv().await {
        ingest!(c);
    }

    // Signal the streaming stdin writer to stop (the process is terminating),
    // then join the I/O tasks so we do not leak them.
    stdin_close.cancel();
    if let Some(h) = stdout_handle {
        let _ = h.await;
    }
    if let Some(h) = stderr_handle {
        let _ = h.await;
    }
    if let Some(h) = stdin_handle {
        let _ = h.await;
    }

    let _ = events_tx.send(ProcessEvent::Exited).await;

    Ok(ProcessOutput {
        stdout: stdout_cap.finish(),
        stderr: stderr_cap.finish(),
        termination: term,
        duration: started.elapsed(),
        pid,
    })
}

/// Reads a stream to EOF, forwarding chunks to the supervisor.
async fn drain_stream<R: tokio::io::AsyncRead + Unpin + Send + 'static>(
    mut reader: R,
    stream: OutputStream,
    tx: mpsc::Sender<DrainChunk>,
) {
    let mut buf = [0u8; 16 * 1024];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => {
                let chunk = DrainChunk {
                    stream,
                    bytes: Bytes::copy_from_slice(&buf[..n]),
                };
                if tx.send(chunk).await.is_err() {
                    // Supervisor gone; stop draining.
                    break;
                }
            }
            Err(_) => break,
        }
    }
}

/// Writes the prompt to stdin and closes it, bounded by an optional deadline.
/// Emits [`ProcessEvent::StdinWritten`] once the bytes are written and stdin is
/// shut down (EOF).
async fn write_stdin(
    mut stdin: ChildStdin,
    input: ProcessInput,
    deadline: Option<Duration>,
    events_tx: mpsc::Sender<ProcessEvent>,
) {
    if let ProcessInput::Bytes(bytes) = input {
        let write_fut = async {
            if stdin.write_all(&bytes).await.is_err() {
                return false;
            }
            // Closing stdin signals EOF to the child; only report "written" when
            // the full payload was accepted and stdin shut down cleanly.
            stdin.shutdown().await.is_ok()
        };
        let fully_written = match deadline {
            Some(d) => tokio::time::timeout(d, write_fut).await.unwrap_or(false),
            None => write_fut.await,
        };
        if fully_written {
            let _ = events_tx.try_send(ProcessEvent::StdinWritten);
        }
    }
    // stdin is dropped here → EOF.
}

/// The long-lived streaming stdin writer. Owns the child's stdin for the life of
/// the process: writes and flushes the initial frame, emits
/// [`ProcessEvent::StdinInitialised`], then serves acknowledged
/// [`ProcessInputCommand`]s until the process terminates (`done` is cancelled),
/// every command sender is dropped, or an explicit [`ProcessInputCommand::Close`]
/// arrives.
///
/// Only `Close` deliberately shuts stdin down. A broken-pipe write failure flips
/// the writer into a "closed" state (subsequent commands report
/// [`ProcessInputWriteError::Closed`]) rather than exiting immediately, so late
/// steering attempts get a typed result instead of a dropped channel; the
/// supervisor's `done` token still reaps the writer promptly once the child dies.
async fn streaming_stdin_writer(
    mut stdin: ChildStdin,
    initial: Bytes,
    mut cmd_rx: mpsc::Receiver<ProcessInputCommand>,
    events_tx: mpsc::Sender<ProcessEvent>,
    done: CancellationToken,
    initial_deadline: Option<Duration>,
) {
    // 1. Write + flush the initial frame.
    match write_flush(&mut stdin, &initial, initial_deadline).await {
        Ok(()) => {
            let _ = events_tx.try_send(ProcessEvent::StdinInitialised);
        }
        Err(e) => {
            let _ = events_tx.try_send(ProcessEvent::StdinWriteFailed(e));
            return;
        }
    }

    // 2. Serve acknowledged commands until shutdown.
    let mut closed = false;
    loop {
        tokio::select! {
            biased;
            _ = done.cancelled() => return,
            cmd = cmd_rx.recv() => match cmd {
                None => return,
                Some(ProcessInputCommand::Write { bytes, acknowledgement }) => {
                    let result = if closed {
                        Err(ProcessInputWriteError::Closed)
                    } else {
                        match write_flush(&mut stdin, &bytes, None).await {
                            Ok(()) => Ok(()),
                            Err(e) => {
                                closed = true;
                                let _ = events_tx
                                    .try_send(ProcessEvent::StdinWriteFailed(e.clone()));
                                Err(e)
                            }
                        }
                    };
                    let _ = acknowledgement.send(result);
                }
                Some(ProcessInputCommand::Close { acknowledgement }) => {
                    // Close is the deliberate end-of-input signal: shut stdin down
                    // (unless it already broke) and exit the writer.
                    let result = if closed {
                        Err(ProcessInputWriteError::Closed)
                    } else {
                        match stdin.shutdown().await {
                            Ok(()) => Ok(()),
                            Err(e) => Err(ProcessInputWriteError::Shutdown { source: e }),
                        }
                    };
                    let _ = acknowledgement.send(result);
                    return;
                }
            }
        }
    }
}

/// Writes `bytes` to `stdin` and flushes, bounded by an optional deadline. Maps
/// I/O failures to the corresponding [`ProcessInputWriteError`] variant.
async fn write_flush(
    stdin: &mut ChildStdin,
    bytes: &[u8],
    deadline: Option<Duration>,
) -> Result<(), ProcessInputWriteError> {
    let fut = async {
        stdin
            .write_all(bytes)
            .await
            .map_err(|e| ProcessInputWriteError::Write { source: e })?;
        stdin
            .flush()
            .await
            .map_err(|e| ProcessInputWriteError::Flush { source: e })?;
        Ok(())
    };
    match deadline {
        Some(d) => match tokio::time::timeout(d, fut).await {
            Ok(r) => r,
            Err(_) => Err(ProcessInputWriteError::Write {
                source: std::io::Error::other("stdin write timed out"),
            }),
        },
        None => fut.await,
    }
}

/// Gracefully terminates the whole process group and reaps the direct child.
///
/// Sequence (Unix): `SIGTERM` the group → wait up to `grace` → `SIGKILL` the
/// group → reap. Exactly one `wait()` ever resolves a reap. If `grace` is zero,
/// skip straight to `SIGKILL`. Reaping uses tokio's own `Child::wait` (via
/// [`AsyncGroupChild::inner`]) rather than the process-group crate's waiter, which
/// is more robust under tokio's `SIGCHLD` handling.
///
/// [`AsyncGroupChild::inner`]: command_group::AsyncGroupChild::inner
async fn terminate_and_reap(child: &mut AsyncGroupChild, grace: Duration) {
    #[cfg(unix)]
    {
        let Some(pid) = child.id() else {
            return;
        };
        if !grace.is_zero() {
            // Safe: killpg signals the whole process group. The direct child is
            // the group leader (PGID == its pid), so this reaches grandchildren
            // too (the `node`/`claude` provider clients beneath the wrapper).
            unsafe {
                let _ = libc::killpg(pid as libc::pid_t, libc::SIGTERM);
            }
            if tokio::time::timeout(grace, child.inner().wait())
                .await
                .is_ok()
            {
                // Reaped during the grace window.
                return;
            }
        }
        unsafe {
            let _ = libc::killpg(pid as libc::pid_t, libc::SIGKILL);
        }
        let _ = child.inner().wait().await;
    }
    #[cfg(not(unix))]
    {
        let _ = grace;
        let _ = child.kill();
        let _ = child.inner().wait().await;
    }
}

/// Joins a supervisor completion handle, mapping a task panic to an error.
async fn join_completion(
    completion: JoinHandle<Result<ProcessOutput, ProcessError>>,
) -> Result<ProcessOutput, ProcessError> {
    match completion.await {
        Ok(result) => result,
        Err(join_err) => Err(ProcessError::Spawn {
            executable: PathBuf::from("<supervisor>"),
            source: std::io::Error::other(format!("supervisor task failed: {join_err}")),
        }),
    }
}

/// Returns the earlier of two `Option<Instant>` deadlines.
fn earliest(a: Option<Instant>, b: Option<Instant>) -> Option<Instant> {
    match (a, b) {
        (Some(a), Some(b)) => Some(if a <= b { a } else { b }),
        (Some(a), None) | (None, Some(a)) => Some(a),
        (None, None) => None,
    }
}

/// Sleeps until a deadline, or never if `None`. Pinned for use in `select!`.
fn sleep_until_option(deadline: Option<Instant>) -> Pin<Box<dyn Future<Output = ()> + Send>> {
    match deadline {
        Some(t) => Box::pin(async move {
            tokio::time::sleep_until(tokio::time::Instant::from_std(t)).await;
        }),
        None => Box::pin(std::future::pending()),
    }
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Deadlines generous enough for a real subprocess but short enough to fail
    /// fast if something wedges.
    fn test_timeouts() -> ProcessTimeouts {
        ProcessTimeouts {
            startup: Some(Duration::from_secs(5)),
            idle: Some(Duration::from_secs(5)),
            total: Some(Duration::from_secs(15)),
            termination_grace: Duration::from_secs(2),
            ..Default::default()
        }
    }

    /// Drains a process's event stream to completion, collecting stdout bytes and
    /// the set of non-output event kinds observed.
    async fn drain(mut proc: RunningProcess) -> (Vec<u8>, Vec<&'static str>) {
        let mut out = Vec::new();
        let mut kinds = Vec::new();
        while let Some(ev) = proc.next_event().await {
            match ev {
                ProcessEvent::Stdout(c) => out.extend_from_slice(&c.bytes),
                ProcessEvent::Stderr(_) => {}
                ProcessEvent::StdinWritten => kinds.push("StdinWritten"),
                ProcessEvent::StdinInitialised => kinds.push("StdinInitialised"),
                ProcessEvent::StdinWriteFailed(_) => kinds.push("StdinWriteFailed"),
                ProcessEvent::Exited => kinds.push("Exited"),
            }
        }
        (out, kinds)
    }

    #[tokio::test]
    async fn streaming_stdin_writes_initial_and_acknowledged_commands() {
        // `cat` echoes stdin to stdout and exits on EOF.
        let spec = ProcessSpec::builder("sh")
            .arg("-c")
            .arg("cat")
            .input(ProcessInput::Streaming {
                initial: Bytes::from_static(b"first\n"),
            })
            .timeouts(test_timeouts())
            .capture_bytes(64 * 1024)
            .build();
        let cancel = CancellationToken::new();
        let proc = spawn(spec, cancel).await.expect("spawn");
        let sender = proc.input_sender().expect("streaming exposes a sender");

        // Two acknowledged writes after the initial frame.
        let (a1, r1) = oneshot::channel();
        sender
            .send(ProcessInputCommand::Write {
                bytes: Bytes::from_static(b"second\n"),
                acknowledgement: a1,
            })
            .await
            .expect("send write 1");
        assert!(matches!(r1.await, Ok(Ok(()))), "write 1 acknowledged Ok");

        let (a2, r2) = oneshot::channel();
        sender
            .send(ProcessInputCommand::Write {
                bytes: Bytes::from_static(b"third\n"),
                acknowledgement: a2,
            })
            .await
            .expect("send write 2");
        assert!(matches!(r2.await, Ok(Ok(()))), "write 2 acknowledged Ok");

        // Deliberately close stdin so `cat` sees EOF and exits.
        let (ac, rc) = oneshot::channel();
        sender
            .send(ProcessInputCommand::Close {
                acknowledgement: ac,
            })
            .await
            .expect("send close");
        assert!(matches!(rc.await, Ok(Ok(()))), "close acknowledged Ok");

        let (out, kinds) = drain(proc).await;
        let text = String::from_utf8_lossy(&out);
        assert_eq!(
            text, "first\nsecond\nthird\n",
            "all frames delivered in order"
        );
        assert!(
            kinds.contains(&"StdinInitialised"),
            "initial frame acknowledged via StdinInitialised: {kinds:?}"
        );
        assert!(kinds.contains(&"Exited"), "process exited cleanly");
    }

    #[tokio::test]
    async fn streaming_stdin_close_then_late_write_reports_closed() {
        let spec = ProcessSpec::builder("sh")
            .arg("-c")
            .arg("cat")
            .input(ProcessInput::Streaming {
                initial: Bytes::from_static(b""),
            })
            .timeouts(test_timeouts())
            .capture_bytes(64 * 1024)
            .build();
        let cancel = CancellationToken::new();
        let proc = spawn(spec, cancel).await.expect("spawn");
        let sender = proc.input_sender().expect("sender");

        // Close deliberately.
        let (ac, rc) = oneshot::channel();
        sender
            .send(ProcessInputCommand::Close {
                acknowledgement: ac,
            })
            .await
            .expect("send close");
        assert!(matches!(rc.await, Ok(Ok(()))));

        // After Close the writer has exited: a further send fails (no receiver).
        let (a2, _r2) = oneshot::channel();
        let late = sender.try_send(ProcessInputCommand::Write {
            bytes: Bytes::from_static(b"late\n"),
            acknowledgement: a2,
        });
        // The writer is gone: the command cannot be delivered.
        assert!(late.is_err(), "late write after close is not delivered");

        drain(proc).await;
    }

    #[tokio::test]
    async fn streaming_stdin_broken_pipe_reports_failure() {
        // Close the child's stdin read end but keep the process alive briefly, so
        // a subsequent write hits a broken pipe while the supervisor is still
        // running (not yet torn down).
        let spec = ProcessSpec::builder("sh")
            .arg("-c")
            .arg("exec 0<&-; sleep 1")
            .input(ProcessInput::Streaming {
                initial: Bytes::from_static(b""),
            })
            .timeouts(test_timeouts())
            .capture_bytes(64 * 1024)
            .build();
        let cancel = CancellationToken::new();
        let proc = spawn(spec, cancel).await.expect("spawn");
        let sender = proc.input_sender().expect("sender");

        // Give the shell a moment to close its stdin, then write repeatedly until
        // the broken pipe surfaces.
        let mut saw_failure = false;
        for _ in 0..50 {
            let (a, r) = oneshot::channel();
            if sender
                .send(ProcessInputCommand::Write {
                    bytes: Bytes::from_static(b"x\n"),
                    acknowledgement: a,
                })
                .await
                .is_err()
            {
                break; // writer gone (process exited)
            }
            if matches!(r.await, Ok(Err(_))) {
                saw_failure = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(saw_failure, "a broken-pipe write reports Err to the ack");
        let (_, kinds) = drain(proc).await;
        assert!(
            kinds.contains(&"StdinWriteFailed"),
            "broken pipe surfaced as StdinWriteFailed: {kinds:?}"
        );
    }

    #[tokio::test]
    async fn dropping_one_sender_clone_does_not_close_stdin() {
        // `cat` keeps stdin open until EOF, so a dropped sender clone must not end
        // input.
        let spec = ProcessSpec::builder("sh")
            .arg("-c")
            .arg("cat")
            .input(ProcessInput::Streaming {
                initial: Bytes::from_static(b""),
            })
            .timeouts(test_timeouts())
            .capture_bytes(64 * 1024)
            .build();
        let cancel = CancellationToken::new();
        let proc = spawn(spec, cancel).await.expect("spawn");

        // Two independent clones; drop one immediately.
        let primary = proc.input_sender().expect("sender");
        let dropper = proc.input_sender().expect("second clone");
        drop(dropper);

        // The remaining clone must still be able to write (stdin stayed open).
        let (a, r) = oneshot::channel();
        primary
            .send(ProcessInputCommand::Write {
                bytes: Bytes::from_static(b"survived-drop\n"),
                acknowledgement: a,
            })
            .await
            .expect("send after dropping a clone");
        assert!(
            matches!(r.await, Ok(Ok(()))),
            "write after a clone dropped is still delivered"
        );

        // Close to finish.
        let (ac, rc) = oneshot::channel();
        primary
            .send(ProcessInputCommand::Close {
                acknowledgement: ac,
            })
            .await
            .expect("send close");
        assert!(matches!(rc.await, Ok(Ok(()))));

        let (out, _) = drain(proc).await;
        assert!(
            String::from_utf8_lossy(&out).contains("survived-drop"),
            "the dropped clone did not close stdin"
        );
    }
}
