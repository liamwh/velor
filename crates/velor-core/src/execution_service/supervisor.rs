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
use tokio::sync::mpsc;
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
}

impl ProcessInput {
    /// Returns the byte payload if this is [`ProcessInput::Bytes`].
    #[must_use]
    pub fn as_bytes(&self) -> Option<&Bytes> {
        match self {
            Self::Bytes(b) => Some(b),
            Self::Inherit | Self::Null => None,
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
    /// The prompt was fully written to stdin and stdin was closed (EOF).
    StdinWritten,
    /// The child exited and output draining is complete.
    Exited,
}

impl ProcessEvent {
    /// Returns the chunk if this is a stdout/stderr chunk.
    #[must_use]
    pub fn chunk(&self) -> Option<&ProcessChunk> {
        match self {
            Self::Stdout(c) | Self::Stderr(c) => Some(c),
            Self::StdinWritten | Self::Exited => None,
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
}

impl RunningProcess {
    /// Receives the next event, or `None` when the supervisor has finished.
    pub async fn next_event(&mut self) -> Option<ProcessEvent> {
        let events = self.events.as_mut()?;
        events.recv().await
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
        ProcessInput::Null | ProcessInput::Bytes(_) => Stdio::piped(),
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
        cap,
    ));

    Ok(RunningProcess {
        events: Some(events_rx),
        completion: Some(completion),
        cancellation,
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

    let stdin_handle = stdin.map(|s| tokio::spawn(write_stdin(s, input, timeouts.stdin_write)));

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

    // Join the I/O tasks so we do not leak them.
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
async fn write_stdin(mut stdin: ChildStdin, input: ProcessInput, deadline: Option<Duration>) {
    if let ProcessInput::Bytes(bytes) = input {
        let write_fut = async {
            if stdin.write_all(&bytes).await.is_err() {
                return;
            }
            // Closing stdin signals EOF to the child.
            let _ = stdin.shutdown().await;
        };
        match deadline {
            Some(d) => {
                let _ = tokio::time::timeout(d, write_fut).await;
            }
            None => write_fut.await,
        }
    }
    // stdin is dropped here → EOF.
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
