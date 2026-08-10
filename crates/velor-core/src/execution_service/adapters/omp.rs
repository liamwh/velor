//! Oh My Pi (`omp --mode rpc`) subprocess adapter.
//!
//! `omp` speaks a newline-delimited JSON RPC over stdio: the client sends a
//! `prompt` command on stdin and consumes a stream of events on stdout. Like
//! Claude Code's `--print` stream-json mode, `omp --mode rpc` does **not** exit
//! when a turn completes — it keeps stdin open waiting for more commands — so
//! Velor must close stdin (EOF) itself once a *terminal* `agent_end` event
//! arrives. This mirrors the Claude streaming path.
//!
//! Protocol (verified against `omp` v17.1.5, cross-checked against
//! `omp://rpc.md`): Velor writes `{"id":"vel-interrupt-mode","type":"set_interrupt_mode","mode":"immediate"}\n`
//! followed by `{"id":"vel","type":"prompt","message":"<prompt>"}\n` as the
//! streaming initial frame, then reads events until a terminal `agent_end`,
//! e.g.
//!
//! ```text
//! {"type":"ready","protocolVersion":1,...}          // handshake (ignored)
//! {"id":"vel-interrupt-mode","type":"response",...} // mode ack (ignored)
//! {"id":"vel","type":"response","command":"prompt","success":true}  // ack
//! {"type":"agent_start"}
//! {"type":"turn_start"}
//! {"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":"Hi"}}
//! {"type":"tool_execution_end","toolName":"bash","result":{...},"isError":false}
//! {"type":"turn_end","message":{"usage":{"input":..,"output":..,"cacheRead":..}}}
//! {"type":"agent_end","messages":[...]}             // terminal → close stdin
//! ```
//!
//! ## Live steer / follow-up / native abort
//!
//! When the caller wires a live-input channel ([`OmpParams::enable_live_steering`]),
//! [`forward_live_input`] translates each [`crate::agent::AgentInput`] into
//! omp's dedicated `steer`/`follow_up`/`abort` RPC commands and writes it to
//! the *same* running process's stdin — this is a redirect of the existing
//! session, never a kill-and-restart. Unlike Claude's blind message injection,
//! omp's RPC is request/response correlated by `id`: each forwarded command is
//! tracked in [`PendingAcks`] until [`run_omp_stream`]'s read loop observes the
//! matching `response` frame (or the stream ends first), so a caller learns
//! whether the provider actually accepted the command — not just that the
//! bytes reached the pipe.
//!
//! A `follow_up` command queues work for *after* the active turn — the session
//! resumes with a fresh `agent_start`/`agent_end` pair before its true final
//! settle, signalled by `agent_end.isTerminal: false`. Stdin therefore only
//! closes on a *terminal* `agent_end` ([`frame_is_terminal_agent_end`]), so a
//! queued follow-up is never cut off.

use async_trait::async_trait;
use bytes::Bytes;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::agent::{AgentEvent, AgentInput, AgentInputError, AgentRunResult, SteeringDelivery};
use crate::config::OmpConfig;
use crate::execution_service::adapter::{AgentAdapter, AgentEventSink, LineDecoder};
use crate::execution_service::adapters::claude::map_outcome;
use crate::execution_service::adapters::edit_capture::{
    ReadState, drain_pending_edits, note_pending, read_file_state,
};
use crate::execution_service::classify::ProviderKind;
use crate::execution_service::error::{AgentExecutionError, LiveSteeringUnavailableReason};
use crate::execution_service::steering::SteeringBehaviour;
use crate::execution_service::supervisor::{
    ProcessEvent, ProcessInput, ProcessInputCommand, ProcessInputWriteError, ProcessSpec,
    ProcessTimeouts, RunningProcess,
};

/// Maximum length of a single omp JSONL frame (line) before rejection. omp
/// itself advertises a 1 MiB per-frame cap and a 64 MiB reassembly cap.
pub(crate) const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

/// Maximum bytes of a `tool_execution_end` result carried on
/// [`crate::agent::AgentEvent::ToolResult::detail`]. Generous — this is the
/// real output a user can expand to read in full, not a preview — the TUI's
/// own transcript byte budget (`TuiLimits::max_bytes`) is the actual memory
/// bound, not this constant.
const TOOL_RESULT_DETAIL_MAX: usize = 20_000;

/// Grace window after a terminal `agent_end` + EOF for `omp` to exit naturally
/// before the supervisor force-cancels the group. `omp` normally exits in well
/// under a second; this only guards a linger.
pub(crate) const STREAM_EXIT_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

/// Correlation id Velor uses for its initial `prompt` command. omp echoes it on
/// the matching `response` ack.
pub(crate) const PROMPT_REQUEST_ID: &str = "vel";

/// Correlation id for the one-time `set_interrupt_mode` command sent as part of
/// the streaming initial frame.
pub(crate) const SET_INTERRUPT_MODE_REQUEST_ID: &str = "vel-interrupt-mode";

/// How long to wait for the RPC-level response to a forwarded steer/follow-up/
/// abort command before treating it as undelivered. Generous — per
/// `omp://rpc.md` these commands are processed promptly, not blocked on turn
/// completion — but bounded so a wedged provider can never hang a submission.
const RPC_ACK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Parameters for one Oh My Pi invocation.
#[derive(Debug, Clone)]
pub struct OmpParams {
    /// Binary to invoke (e.g. `omp`).
    pub binary: String,
    /// The rendered prompt, delivered as the `message` of the RPC `prompt` command.
    pub prompt: Bytes,
    /// Working directory.
    pub working_directory: PathBuf,
    /// Effective omp configuration.
    pub config: OmpConfig,
    /// Extra CLI arguments appended after the standard flags.
    pub extra_args: Vec<String>,
    /// Extra environment variables (key, value).
    pub extra_env: Vec<(String, String)>,
    /// Deadlines for this attempt.
    pub timeouts: ProcessTimeouts,
    /// Cancellation token for this attempt.
    pub cancellation: CancellationToken,
    /// Whether to enable live steer/follow-up/native-abort for this attempt.
    /// Configuration only — the runtime input channel is passed to the adapter
    /// separately. omp's RPC transport is always streaming regardless of this
    /// flag; the only difference it makes is whether a forwarding task is
    /// spawned to translate [`AgentInput`] into RPC commands.
    pub enable_live_steering: bool,
    /// Optional native session id to resume (`omp --resume <id>`). When `None`,
    /// `--no-session` is passed (ephemeral, nothing persisted) — the default
    /// for one-shot callers. When `Some`, `--resume <id>` is passed instead,
    /// and `--no-session` is omitted so omp persists the resumed session.
    pub resume_session: Option<String>,
}

impl OmpParams {
    /// Creates parameters with default config.
    #[must_use]
    pub fn new(binary: impl Into<String>, prompt: Bytes, working_directory: PathBuf) -> Self {
        Self {
            binary: binary.into(),
            prompt,
            working_directory,
            config: OmpConfig::default(),
            resume_session: None,
            extra_args: Vec::new(),
            extra_env: Vec::new(),
            timeouts: ProcessTimeouts::default(),
            cancellation: CancellationToken::new(),
            enable_live_steering: false,
        }
    }

    /// Frames the rendered prompt as the RPC `prompt` command. The trailing
    /// newline is required: omp is newline-delimited JSON and the supervisor
    /// writes the bytes verbatim.
    pub(crate) fn frame_prompt_command(&self) -> Result<Bytes, AgentExecutionError> {
        let message = std::str::from_utf8(&self.prompt).map_err(|_| {
            AgentExecutionError::LiveSteeringUnavailable {
                reason: LiveSteeringUnavailableReason::ProtocolRejected,
            }
        })?;
        let frame = serde_json::json!({
            "id": PROMPT_REQUEST_ID,
            "type": "prompt",
            "message": message,
        });
        let mut bytes = serde_json::to_vec(&frame).map_err(|_| {
            AgentExecutionError::LiveSteeringUnavailable {
                reason: LiveSteeringUnavailableReason::ProtocolRejected,
            }
        })?;
        bytes.push(b'\n');
        Ok(Bytes::from(bytes))
    }

    /// Frames the streaming initial input: `set_interrupt_mode: immediate` —
    /// so a steer/abort takes effect at the next safe boundary instead of
    /// waiting for the whole turn to finish (see `omp://rpc.md`'s queue/
    /// concurrency section; `immediate` is also the documented default, but
    /// Velor sets it explicitly rather than relying on that) — followed by the
    /// `prompt` command. Both travel as one write; omp processes stdin one
    /// JSONL line at a time, in order.
    fn frame_initial(&self) -> Result<Bytes, AgentExecutionError> {
        let prompt_frame = self.frame_prompt_command()?;
        let mode_frame = frame_set_interrupt_mode_immediate();
        let mut bytes = Vec::with_capacity(mode_frame.len() + prompt_frame.len());
        bytes.extend_from_slice(&mode_frame);
        bytes.extend_from_slice(&prompt_frame);
        Ok(Bytes::from(bytes))
    }
}

/// Frames the one-time `set_interrupt_mode: immediate` command. Infallible:
/// the shape is fixed and always serialises.
pub(crate) fn frame_set_interrupt_mode_immediate() -> Bytes {
    let frame = serde_json::json!({
        "id": SET_INTERRUPT_MODE_REQUEST_ID,
        "type": "set_interrupt_mode",
        "mode": "immediate",
    });
    let mut bytes = serde_json::to_vec(&frame).expect("static shape always serialises");
    bytes.push(b'\n');
    Bytes::from(bytes)
}

/// Frames a live steer or follow-up as a `prompt` command with the matching
/// `streamingBehavior`. Per `omp://rpc.md`, during active streaming the
/// dedicated `steer`/`follow_up` command types do not receive a response —
/// only `prompt` with `streamingBehavior` is acknowledged. Without this, a
/// mid-turn steer hangs waiting for a response that never arrives.
pub(crate) fn frame_live_message_command(
    id: &str,
    behaviour: SteeringBehaviour,
    message: &str,
) -> Bytes {
    let streaming_behavior = match behaviour {
        SteeringBehaviour::Steer => "steer",
        SteeringBehaviour::FollowUp => "followUp",
    };
    let frame = serde_json::json!({
        "id": id,
        "type": "prompt",
        "message": message,
        "streamingBehavior": streaming_behavior,
    });
    let mut bytes = serde_json::to_vec(&frame).expect("static shape always serialises");
    bytes.push(b'\n');
    Bytes::from(bytes)
}

/// Frames one `abort` RPC command. Infallible: the shape is fixed and always
/// serialises.
pub(crate) fn frame_abort_command(id: &str) -> Bytes {
    let frame = serde_json::json!({ "id": id, "type": "abort" });
    let mut bytes = serde_json::to_vec(&frame).expect("static shape always serialises");
    bytes.push(b'\n');
    Bytes::from(bytes)
}

/// Pending steer/follow-up/abort acknowledgements, keyed by the command's
/// correlation id. Shared between [`forward_live_input`] (which registers a
/// waiter before writing each command) and [`run_omp_stream`]'s read loop
/// (which resolves — and, once the stream ends, drains — them as `response`
/// frames with matching ids arrive).
pub(crate) type PendingAcks = Arc<Mutex<HashMap<String, oneshot::Sender<Result<(), String>>>>>;

/// Locks `acks`, recovering from poisoning: a panic elsewhere while holding
/// the lock must not permanently wedge every future steer/follow-up/abort.
pub(crate) fn lock_acks(
    acks: &PendingAcks,
) -> MutexGuard<'_, HashMap<String, oneshot::Sender<Result<(), String>>>> {
    acks.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Oh My Pi subprocess adapter.
pub struct OmpSubprocessAdapter {
    params: OmpParams,
}

impl OmpSubprocessAdapter {
    /// Creates an adapter for the given parameters.
    #[must_use]
    pub fn new(params: OmpParams) -> Self {
        Self { params }
    }

    fn build_spec(&self, initial: Bytes) -> ProcessSpec {
        let cfg = &self.params.config;
        let mut builder = ProcessSpec::builder(&self.params.binary)
            .arg("--mode")
            .arg("rpc")
            // When resuming a native session, pass --resume and omit
            // --no-session so omp persists the continued session. Otherwise,
            // keep the run ephemeral (--no-session) for one-shot callers.
            .cwd(self.params.working_directory.clone());
        if let Some(id) = self.params.resume_session.as_ref() {
            builder = builder.arg("--resume").arg(id);
        } else {
            builder = builder.arg("--no-session");
        }
        let mut builder = builder
            .input(ProcessInput::Streaming { initial })
            .timeouts(self.params.timeouts.clone())
            .capture_bytes(64 * 1024);

        if cfg.auto_approve {
            builder = builder.arg("--auto-approve");
        }
        if let Some(model) = cfg.model.as_ref().filter(|m| !m.trim().is_empty()) {
            builder = builder.arg("--model").arg(model);
        }
        if let Some(thinking) = cfg.thinking.as_ref().filter(|t| !t.trim().is_empty()) {
            builder = builder.arg("--thinking").arg(thinking);
        }
        if let Some(max_time) = cfg.max_time.as_ref().filter(|t| !t.trim().is_empty()) {
            builder = builder.arg("--max-time").arg(max_time);
        }
        if let Some(profile) = cfg.profile.as_ref().filter(|p| !p.trim().is_empty()) {
            builder = builder.arg("--profile").arg(profile);
        }
        for arg in &self.params.extra_args {
            builder = builder.arg(arg);
        }
        for (key, value) in &self.params.extra_env {
            builder = builder.env(key, value);
        }
        builder.build()
    }
}

#[async_trait(?Send)]
impl AgentAdapter for OmpSubprocessAdapter {
    async fn execute(
        &mut self,
        sink: &mut dyn AgentEventSink,
        live_input: Option<mpsc::Receiver<AgentInput>>,
    ) -> Result<AgentRunResult, AgentExecutionError> {
        let initial = self.params.frame_initial()?;
        let spec = self.build_spec(initial);
        let process: RunningProcess =
            crate::execution_service::supervisor::spawn(spec, self.params.cancellation.clone())
                .await?;

        let pending_acks: PendingAcks = Arc::new(Mutex::new(HashMap::new()));

        // Spawn the forwarding task only when steering was actually wired in:
        // both a writable command sender and a live-input receiver must exist.
        let forward_handle = if self.params.enable_live_steering {
            match (process.input_sender(), live_input) {
                (Some(command_tx), Some(live_rx)) => Some(tokio::spawn(forward_live_input(
                    live_rx,
                    command_tx,
                    self.params.cancellation.clone(),
                    pending_acks.clone(),
                ))),
                // Nothing to forward: drop whichever half is present.
                _ => None,
            }
        } else {
            None
        };

        let result =
            run_omp_stream(process, sink, &self.params.working_directory, pending_acks).await;
        // The process is finished; stop the forwarding task promptly so it
        // cannot outlive the execution (any in-flight acknowledgement wait is
        // dropped, resolving the caller's ack as unavailable).
        if let Some(handle) = forward_handle {
            handle.abort();
        }
        result
    }
}

/// Forwards typed live-session [`AgentInput`]s to the supervisor's streaming
/// stdin as framed omp RPC commands (`steer`, `follow_up`, `abort`) on the
/// *same* running process — this redirects the existing session, it never
/// spawns a replacement. Each command is acknowledged once the RPC response
/// frame with its correlation id is observed by [`run_omp_stream`] (or the
/// response window elapses, or the stream ends first — see [`deliver`]).
/// Stops when the input receiver closes, the supervisor's writer goes away, or
/// the attempt is cancelled. Never closes stdin itself — only the deliberate
/// execution shutdown does.
async fn forward_live_input(
    mut live_input: mpsc::Receiver<AgentInput>,
    command_tx: mpsc::Sender<ProcessInputCommand>,
    cancel: CancellationToken,
    pending_acks: PendingAcks,
) {
    let mut next_id: u64 = 0;
    loop {
        let input = tokio::select! {
            biased;
            _ = cancel.cancelled() => return,
            input = live_input.recv() => match input {
                Some(i) => i,
                None => return, // sender dropped
            },
        };
        next_id += 1;
        let id = format!("vel-live-{next_id}");
        match input {
            AgentInput::UserMessage {
                text,
                behaviour,
                acknowledgement,
            } => {
                let frame = frame_live_message_command(&id, behaviour, text.as_str());
                let ack = deliver(&command_tx, frame, &id, &pending_acks, &cancel).await;
                let _ = acknowledgement.send(ack);
            }
            AgentInput::Abort { acknowledgement } => {
                let frame = frame_abort_command(&id);
                let ack = deliver(&command_tx, frame, &id, &pending_acks, &cancel).await;
                let _ = acknowledgement.send(ack);
            }
        }
    }
}

/// Writes one framed RPC command to the process's streaming stdin, registers
/// its correlation id in `pending_acks` *before* writing (so a same-tick reply
/// can never race the registration), and awaits either the RPC-level response
/// resolved by [`resolve_pending_ack`], the response window elapsing, or the
/// stream ending first (which drains `pending_acks`, resolving this as
/// undelivered — see [`drain_pending_acks`]).
async fn deliver(
    command_tx: &mpsc::Sender<ProcessInputCommand>,
    frame: Bytes,
    id: &str,
    pending_acks: &PendingAcks,
    cancel: &CancellationToken,
) -> Result<SteeringDelivery, AgentInputError> {
    let (resp_tx, resp_rx) = oneshot::channel::<Result<(), String>>();
    lock_acks(pending_acks).insert(id.to_string(), resp_tx);

    let (write_ack, write_rx) = oneshot::channel();
    let send = command_tx.send(ProcessInputCommand::Write {
        bytes: frame,
        acknowledgement: write_ack,
    });
    tokio::pin!(send);
    let sent = tokio::select! {
        biased;
        _ = cancel.cancelled() => {
            lock_acks(pending_acks).remove(id);
            return Err(unavailable(LiveSteeringUnavailableReason::WriteFailed));
        }
        s = &mut send => s,
    };
    if sent.is_err() {
        // The supervisor's writer has gone away (process closing/terminated).
        lock_acks(pending_acks).remove(id);
        return Err(unavailable(LiveSteeringUnavailableReason::StdinClosed));
    }
    match write_rx.await {
        Ok(Ok(())) => {} // pipe write landed; now await the RPC-level ack.
        Ok(Err(ProcessInputWriteError::Closed)) => {
            lock_acks(pending_acks).remove(id);
            return Err(unavailable(LiveSteeringUnavailableReason::StdinClosed));
        }
        Ok(Err(_)) => {
            lock_acks(pending_acks).remove(id);
            return Err(unavailable(LiveSteeringUnavailableReason::WriteFailed));
        }
        Err(_) => {
            // Writer dropped without acknowledging: the bytes may or may not
            // have landed before it went away.
            lock_acks(pending_acks).remove(id);
            return Ok(SteeringDelivery::DeliveryUnknown);
        }
    }
    match tokio::time::timeout(RPC_ACK_TIMEOUT, resp_rx).await {
        Ok(Ok(Ok(()))) => Ok(SteeringDelivery::Acknowledged),
        Ok(Ok(Err(_message))) => Err(unavailable(LiveSteeringUnavailableReason::Rejected)),
        Ok(Err(_recv_error)) => {
            // The waiter was dropped without resolving: the stream ended
            // (process exited/crashed, or stdin closed) before a reply arrived.
            Err(unavailable(LiveSteeringUnavailableReason::StdinClosed))
        }
        Err(_elapsed) => {
            lock_acks(pending_acks).remove(id);
            Err(unavailable(LiveSteeringUnavailableReason::NoResponse))
        }
    }
}

fn unavailable(reason: LiveSteeringUnavailableReason) -> AgentInputError {
    AgentInputError::Unavailable { reason }
}

/// Resolves a pending steer/follow-up/abort acknowledgement from a `response`
/// frame, if `line` is one and its `id` has a registered waiter. Independent
/// of [`parse_omp_line`]'s handling of the same frame (which surfaces a failed
/// response as a generic [`AgentEvent::Error`] for transcript visibility) —
/// this additionally completes the specific command's delivery outcome.
fn resolve_pending_ack(line: &str, pending_acks: &PendingAcks) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return;
    };
    if value.get("type").and_then(|v| v.as_str()) != Some("response") {
        return;
    }
    let Some(id) = value.get("id").and_then(|v| v.as_str()) else {
        return;
    };
    let tx = lock_acks(pending_acks).remove(id);
    let Some(tx) = tx else {
        return;
    };
    let success = value
        .get("success")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let result = if success {
        Ok(())
    } else {
        let message = value
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("omp command failed")
            .to_string();
        Err(message)
    };
    let _ = tx.send(result);
}

/// Drains any still-pending acknowledgements, dropping each waiter so a
/// forwarding task's response wait resolves (as [`LiveSteeringUnavailableReason::StdinClosed`])
/// instead of hanging forever when the stream ends without a reply — the
/// process exited, crashed, or the loop closed stdin before a response arrived.
fn drain_pending_acks(pending_acks: &PendingAcks) {
    lock_acks(pending_acks).clear();
}

/// Drives a running omp subprocess: decodes stdout frames, emits events,
/// resolves any pending steer/follow-up/abort acknowledgements, and closes
/// stdin (EOF) once a *terminal* `agent_end` event arrives so the process
/// exits cleanly.
async fn run_omp_stream(
    mut process: RunningProcess,
    sink: &mut dyn AgentEventSink,
    cwd: &Path,
    pending_acks: PendingAcks,
) -> Result<AgentRunResult, AgentExecutionError> {
    let mut decoder = LineDecoder::new(MAX_FRAME_BYTES);
    let mut collected = String::new();
    let mut structured_error: Option<String> = None;
    let mut close_sent = false;
    // Tool-call summary keyed by call id, so the matching `tool_execution_end`
    // (which only carries `toolCallId`, not the original command/path) can
    // recover it. See `parse_omp_line`.
    let mut pending_tool_calls: HashMap<String, String> = HashMap::new();
    // Pre-edit snapshots for first-class edit tools. See [`process_event`].
    let mut pending_edits: Vec<(String, ReadState)> = Vec::new();

    while let Some(event) = process.next_event().await {
        match event {
            ProcessEvent::Stdout(chunk) => {
                let lines = decoder.push(&chunk.bytes)?;
                let mut saw_terminal_agent_end = false;
                for line in lines {
                    let text = String::from_utf8_lossy(&line);
                    resolve_pending_ack(&text, &pending_acks);
                    if frame_is_terminal_agent_end(&text) {
                        saw_terminal_agent_end = true;
                    }
                    for agent_event in
                        parse_omp_line(&text, &mut collected, &mut pending_tool_calls)
                    {
                        if let Some(msg) =
                            process_event(sink, cwd, &mut pending_edits, agent_event).await?
                            && structured_error.is_none()
                        {
                            structured_error = Some(msg);
                        }
                    }
                }
                // A follow-up may still be queued: only a *terminal* agent_end
                // (isTerminal != false) means the turn — and every queued
                // continuation — is fully done. Sending EOF here closes the
                // process; a non-terminal agent_end leaves stdin open so a
                // queued follow-up's next agent_start/agent_end cycle runs in
                // this same process.
                if saw_terminal_agent_end
                    && !close_sent
                    && let Some(tx) = process.input_sender().as_ref()
                {
                    let (ack, _r) = tokio::sync::oneshot::channel();
                    let _ = tx
                        .send(ProcessInputCommand::Close {
                            acknowledgement: ack,
                        })
                        .await;
                    close_sent = true;
                    break;
                }
            }
            ProcessEvent::Stderr(_) => {
                // Captured by the supervisor for classification; not protocol output.
            }
            ProcessEvent::StdinWritten
            | ProcessEvent::StdinInitialised
            | ProcessEvent::StdinWriteFailed(_)
            | ProcessEvent::Exited => {}
        }
    }

    if let Some(remainder) = decoder.flush_remainder()? {
        let text = String::from_utf8_lossy(&remainder);
        resolve_pending_ack(&text, &pending_acks);
        for agent_event in parse_omp_line(&text, &mut collected, &mut pending_tool_calls) {
            if let Some(msg) = process_event(sink, cwd, &mut pending_edits, agent_event).await?
                && structured_error.is_none()
            {
                structured_error = Some(msg);
            }
        }
    }

    // Best-effort: emit diffs for any edits whose result frame we never
    // observed (e.g. the process ended mid-turn). Reads the final on-disk state.
    drain_pending_edits(cwd, &mut pending_edits, sink).await?;
    // Any steer/follow-up/abort still awaiting a response now never will (the
    // stream just ended) — resolve waiters instead of leaving them hanging.
    drain_pending_acks(&pending_acks);

    if close_sent {
        return finalize_streaming(process, collected, structured_error).await;
    }
    // The process exited before a terminal `agent_end` (crash, --max-time, or
    // error).
    let output = process.complete().await?;
    map_outcome(output, collected, structured_error, ProviderKind::Omp)
}

/// Finalises a streaming run whose turn already completed (`agent_end` observed
/// and EOF sent). Races omp's natural exit against a short grace so a completed
/// turn can never wedge the iteration.
async fn finalize_streaming(
    process: RunningProcess,
    collected: String,
    structured_error: Option<String>,
) -> Result<AgentRunResult, AgentExecutionError> {
    let cancellation = process.cancellation().clone();
    match tokio::time::timeout(STREAM_EXIT_GRACE, process.complete()).await {
        Ok(Ok(output)) => map_outcome(output, collected, structured_error, ProviderKind::Omp),
        Ok(Err(e)) => Err(e.into()),
        Err(_) => {
            // Lingered past grace: tear the group down. Succeed from the
            // already-collected output — `agent_end` proved the turn done.
            cancellation.cancel();
            Ok(AgentRunResult { stdout: collected })
        }
    }
}

// ── File-edit capture ───────────────────────────────────────────────────────
//
// omp executes its own tools; Velor only sees the RPC event stream. To show
// the *real* resulting edit (not the agent's claimed patch) we snapshot a
// file's contents when its edit `toolcall_end` is observed — before the edit
// lands — and diff it against the post-edit contents once the matching
// `tool_execution_end` arrives (by which point the edit is on disk). Only
// first-class edit tools (`edit`/`write`) are observed; shell-driven file
// mutations via `bash` are not reliably observable and are intentionally
// excluded.

/// Processes one parsed agent event: snapshots pre-edit contents for edit-tool
/// calls, emits the real `FileEdit` diff when a tool result arrives, then emits
/// the event itself. Returns the message if `event` is an [`AgentEvent::Error`]
/// so the caller can track the structured error.
async fn process_event(
    sink: &mut dyn AgentEventSink,
    cwd: &Path,
    pending: &mut Vec<(String, ReadState)>,
    event: AgentEvent,
) -> Result<Option<String>, AgentExecutionError> {
    let err_msg = match &event {
        AgentEvent::Error { message } => Some(message.clone()),
        _ => None,
    };
    // Snapshot the pre-edit state when a first-class edit tool is invoked. The
    // tool has not executed yet (its result has not been emitted).
    if let AgentEvent::ToolCall { tool, input, .. } = &event
        && let Some(path) = edit_target_path(tool, input)
    {
        let state = read_file_state(cwd, &path).await;
        note_pending(pending, path, state);
    }
    // A tool result means the preceding edit(s) have landed on disk: read the
    // resulting state, compute the real diff, and emit it before the result so
    // the diff appears between the tool call and its result.
    if matches!(event, AgentEvent::ToolResult { .. }) {
        drain_pending_edits(cwd, pending, sink).await?;
    }
    sink.emit(event)
        .await
        .map_err(|_| AgentExecutionError::Cancelled)?;
    Ok(err_msg)
}

/// Whether `tool` is a first-class file-edit tool whose filesystem result Velor
/// can observe by reading the file before and after the result arrives.
fn is_file_edit_tool(tool: &str) -> bool {
    matches!(tool, "edit" | "write")
}

/// The target file path for a first-class edit tool's input, if any.
///
/// `write`'s args carry an explicit `path`. `edit`'s schema isn't a stable
/// Velor contract — its `input` is a compact patch DSL of `SWAP` hunks that
/// this crate does not parse — but every observed call opens with a
/// `[<path>#<hash>]` marker line, so the path is read off that marker rather
/// than attempting to understand the patch itself. Reading the real file
/// before and after (see [`process_event`]) means the patch DSL never needs
/// parsing at all: the diff shown is always what's actually on disk.
fn edit_target_path(tool: &str, input: &serde_json::Value) -> Option<String> {
    if !is_file_edit_tool(tool) {
        return None;
    }
    match tool {
        "write" => input
            .get("path")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .filter(|s| !s.is_empty()),
        "edit" => input
            .get("input")
            .and_then(|v| v.as_str())
            .and_then(leading_bracketed_path),
        _ => None,
    }
}

/// Extracts `path` from a leading `[path#hash]` marker line, if present.
fn leading_bracketed_path(patch: &str) -> Option<String> {
    let first_line = patch.lines().next()?;
    let inner = first_line.strip_prefix('[')?.strip_suffix(']')?;
    let path = inner.rsplit_once('#').map_or(inner, |(p, _)| p);
    (!path.is_empty()).then(|| path.to_string())
}

/// Cheap check: is this line a *terminal* `agent_end` frame — i.e. `type ==
/// "agent_end"` and `isTerminal` is absent or `true`. `isTerminal: false`
/// means a queued follow-up (or other maintenance work) has been scheduled and
/// the session will resume before its true final settle (see `omp://rpc.md`'s
/// `agent_end` shape), so stdin must stay open for it.
pub(crate) fn frame_is_terminal_agent_end(line: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
        return false;
    };
    if value.get("type").and_then(|v| v.as_str()) != Some("agent_end") {
        return false;
    }
    !matches!(
        value.get("isTerminal"),
        Some(serde_json::Value::Bool(false))
    )
}

/// Parses one omp JSONL line into zero or more [`AgentEvent`]s, appending
/// streamed assistant text to `collected`.
pub(crate) fn parse_omp_line(
    line: &str,
    collected: &mut String,
    pending_tool_calls: &mut HashMap<String, String>,
) -> Vec<AgentEvent> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return Vec::new();
    };
    let mut events = Vec::new();
    let event_type = value.get("type").and_then(|v| v.as_str()).unwrap_or("");
    match event_type {
        "response" => {
            // Command ack. A failed response carries the structured error.
            if value.get("success").and_then(|v| v.as_bool()) == Some(false) {
                let msg = value
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("omp command failed")
                    .to_string();
                events.push(AgentEvent::Error { message: msg });
            }
        }
        "agent_start" => {
            events.push(AgentEvent::Status {
                message: "agent started".to_string(),
            });
        }
        "turn_end" => {
            if let Some(usage) = value.get("message").and_then(|m| m.get("usage")) {
                events.push(AgentEvent::Usage {
                    input_tokens: usage.get("input").and_then(|v| v.as_u64()),
                    output_tokens: usage.get("output").and_then(|v| v.as_u64()),
                    cached_input_tokens: usage.get("cacheRead").and_then(|v| v.as_u64()),
                });
            }
        }
        "message_update" => {
            if let Some(ame) = value.get("assistantMessageEvent") {
                let kind = ame.get("type").and_then(|v| v.as_str()).unwrap_or("");
                match kind {
                    "text_delta" => {
                        if let Some(delta) = ame.get("delta").and_then(|v| v.as_str())
                            && !delta.is_empty()
                        {
                            collected.push_str(delta);
                            events.push(AgentEvent::TextDelta {
                                text: delta.to_string(),
                            });
                        }
                    }
                    "thinking_delta" => {
                        if let Some(delta) = ame.get("delta").and_then(|v| v.as_str())
                            && !delta.is_empty()
                        {
                            events.push(AgentEvent::Thinking {
                                text: delta.to_string(),
                            });
                        }
                    }
                    "toolcall_end" => {
                        if let Some(tool_call) = ame.get("toolCall") {
                            let name = tool_call
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("tool")
                                .to_string();
                            let args = tool_call.get("arguments").cloned().unwrap_or_default();
                            let detail = summarize_tool_input(&name, &args);
                            // Remembered so the matching `tool_execution_end`
                            // (which only carries `toolCallId`, not the
                            // original command/path) can recover it.
                            if let Some(id) = tool_call.get("id").and_then(|v| v.as_str()) {
                                pending_tool_calls.insert(id.to_string(), detail.clone());
                            }
                            events.push(AgentEvent::ToolCall {
                                tool: name,
                                detail,
                                input: args,
                            });
                        }
                    }
                    _ => {}
                }
            }
        }
        "tool_execution_end" => {
            let tool = value
                .get("toolName")
                .and_then(|v| v.as_str())
                .unwrap_or("tool")
                .to_string();
            let is_error = value
                .get("isError")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let detail = value
                .get("result")
                .and_then(|r| r.get("content"))
                .and_then(|c| c.as_array())
                .and_then(|parts| {
                    parts.iter().find_map(|p| {
                        if p.get("type").and_then(|t| t.as_str()) == Some("text") {
                            p.get("text").and_then(|t| t.as_str())
                        } else {
                            None
                        }
                    })
                })
                .map(|s| truncate(s, TOOL_RESULT_DETAIL_MAX))
                .unwrap_or_default();
            let command = value
                .get("toolCallId")
                .and_then(|v| v.as_str())
                .and_then(|id| pending_tool_calls.remove(id));
            events.push(AgentEvent::ToolResult {
                tool,
                detail,
                success: Some(!is_error),
                command,
            });
        }
        "agent_end" => {
            // Backstop: if no text streamed (no text_delta seen), pull the final
            // assistant text message out of the consolidated message history.
            if collected.is_empty()
                && let Some(messages) = value.get("messages").and_then(|m| m.as_array())
                && let Some(text) = messages.iter().rev().find_map(|m| {
                    if m.get("role").and_then(|r| r.as_str()) == Some("assistant") {
                        m.get("content")
                            .and_then(|c| c.as_array())
                            .and_then(|parts| {
                                parts.iter().find_map(|p| {
                                    if p.get("type").and_then(|t| t.as_str()) == Some("text") {
                                        p.get("text").and_then(|t| t.as_str())
                                    } else {
                                        None
                                    }
                                })
                            })
                    } else {
                        None
                    }
                })
            {
                collected.push_str(text);
            }
        }
        _ => {}
    }
    events
}

/// Builds a one-line summary of a tool-call's input. omp's tool schema isn't
/// fully known ahead of time, so this tries the field names real tools carry
/// — a shell command, a file/read path, a search pattern — in priority order
/// before falling back to a raw JSON dump. Without this, unrecognised tools
/// (e.g. a `read` call whose args are `{"i": "<intent>", "path": "..."}`)
/// would show their whole input JSON instead of the one field a human
/// actually wants to see.
fn summarize_tool_input(name: &str, args: &serde_json::Value) -> String {
    let _ = name;
    for key in ["command", "path", "pattern", "query", "url", "content"] {
        if let Some(s) = args.get(key).and_then(|v| v.as_str()) {
            return s.to_string();
        }
    }
    args.to_string()
}

/// Truncates a string to approximately `max` bytes on a char boundary.
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… ({} bytes truncated)", &s[..end], s.len() - end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_text_delta_and_collects() {
        let mut collected = String::new();
        let events = parse_omp_line(
            r#"{"type":"message_update","assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"BANANA"}}"#,
            &mut collected,
            &mut HashMap::new(),
        );
        assert_eq!(collected, "BANANA");
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::TextDelta { text } if text.as_str() == "BANANA"))
        );
    }

    #[test]
    fn parses_thinking_delta() {
        let mut collected = String::new();
        let events = parse_omp_line(
            r#"{"type":"message_update","assistantMessageEvent":{"type":"thinking_delta","delta":"hm"}}"#,
            &mut collected,
            &mut HashMap::new(),
        );
        assert!(collected.is_empty());
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::Thinking { .. }))
        );
    }

    #[test]
    fn parses_toolcall_end_as_tool_call() {
        let mut collected = String::new();
        let events = parse_omp_line(
            r#"{"type":"message_update","assistantMessageEvent":{"type":"toolcall_end","contentIndex":1,"toolCall":{"type":"toolCall","id":"c1","name":"bash","arguments":{"command":"echo hi"}}}}"#,
            &mut collected,
            &mut HashMap::new(),
        );
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::ToolCall { tool, detail, .. }
            if tool.as_str() == "bash" && detail.contains("echo hi")
        )));
    }

    #[test]
    fn summarize_tool_input_prefers_path_over_a_raw_json_dump() {
        // Regression: a `read`-style tool whose args are `{"i": "<intent>",
        // "path": "..."}` must summarise to the path, not the whole JSON blob
        // (the `"i"` field has no special meaning to a human reading the
        // transcript — it was leaking through verbatim).
        let args = serde_json::json!({"i": "Read last handoff", "path": "SPEC.md.progress.md"});
        assert_eq!(summarize_tool_input("read", &args), "SPEC.md.progress.md");
    }

    #[test]
    fn summarize_tool_input_falls_back_to_json_for_unknown_shapes() {
        let args = serde_json::json!({"foo": "bar"});
        assert_eq!(summarize_tool_input("mystery", &args), args.to_string());
    }

    #[test]
    fn parses_tool_execution_end_as_tool_result() {
        let mut collected = String::new();
        let events = parse_omp_line(
            r#"{"type":"tool_execution_end","toolCallId":"c1","toolName":"bash","result":{"content":[{"type":"text","text":"hello-omp\n"}]},"isError":false}"#,
            &mut collected,
            &mut HashMap::new(),
        );
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::ToolResult { tool, success, .. }
            if tool.as_str() == "bash" && *success == Some(true)
        )));
    }

    #[test]
    fn tool_execution_end_recovers_the_command_via_id_correlation() {
        let mut collected = String::new();
        let mut pending = HashMap::new();
        let _ = parse_omp_line(
            r#"{"type":"message_update","assistantMessageEvent":{"type":"toolcall_end","toolCall":{"id":"c1","name":"bash","arguments":{"command":"echo hi"}}}}"#,
            &mut collected,
            &mut pending,
        );
        let events = parse_omp_line(
            r#"{"type":"tool_execution_end","toolCallId":"c1","toolName":"bash","result":{"content":[{"type":"text","text":"hi\n"}]},"isError":false}"#,
            &mut collected,
            &mut pending,
        );
        let AgentEvent::ToolResult { command, .. } = events
            .iter()
            .find(|e| matches!(e, AgentEvent::ToolResult { .. }))
            .expect("a ToolResult event")
        else {
            unreachable!()
        };
        assert_eq!(command.as_deref(), Some("echo hi"));
        assert!(
            pending.is_empty(),
            "the pending entry is consumed, not leaked"
        );
    }

    #[test]
    fn tool_execution_end_without_a_matching_id_has_no_command() {
        let mut collected = String::new();
        let events = parse_omp_line(
            r#"{"type":"tool_execution_end","toolCallId":"unknown","toolName":"bash","result":{"content":[]},"isError":false}"#,
            &mut collected,
            &mut HashMap::new(),
        );
        let AgentEvent::ToolResult { command, .. } = events
            .iter()
            .find(|e| matches!(e, AgentEvent::ToolResult { .. }))
            .expect("a ToolResult event")
        else {
            unreachable!()
        };
        assert!(command.is_none());
    }

    #[test]
    fn parses_usage_from_turn_end() {
        let mut collected = String::new();
        let events = parse_omp_line(
            r#"{"type":"turn_end","message":{"usage":{"input":10,"output":20,"cacheRead":5}}}"#,
            &mut collected,
            &mut HashMap::new(),
        );
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::Usage {
                input_tokens: Some(10),
                output_tokens: Some(20),
                cached_input_tokens: Some(5)
            }
        )));
    }

    #[test]
    fn failed_response_is_structured_error() {
        let mut collected = String::new();
        let events = parse_omp_line(
            r#"{"id":"vel","type":"response","command":"prompt","success":false,"error":"invalid api key"}"#,
            &mut collected,
            &mut HashMap::new(),
        );
        assert!(events.iter().any(
            |e| matches!(e, AgentEvent::Error { message } if message.contains("invalid api key"))
        ));
    }

    #[test]
    fn agent_end_backstops_uncollected_text() {
        let mut collected = String::new();
        let _ = parse_omp_line(
            r#"{"type":"agent_end","messages":[{"role":"user","content":[{"type":"text","text":"q"}]},{"role":"assistant","content":[{"type":"text","text":"answer"}]}]}"#,
            &mut collected,
            &mut HashMap::new(),
        );
        assert_eq!(collected, "answer");
    }

    #[test]
    fn frame_is_terminal_agent_end_detects_terminal_vs_queued_follow_up() {
        assert!(frame_is_terminal_agent_end(
            r#"{"type":"agent_end","messages":[]}"#
        ));
        assert!(frame_is_terminal_agent_end(
            r#"{"type":"agent_end","messages":[],"isTerminal":true}"#
        ));
        // A queued follow-up (or other maintenance work) keeps the session
        // alive — this must NOT be treated as the run's true completion.
        assert!(!frame_is_terminal_agent_end(
            r#"{"type":"agent_end","messages":[],"isTerminal":false}"#
        ));
        assert!(!frame_is_terminal_agent_end(r#"{"type":"turn_end"}"#));
        assert!(!frame_is_terminal_agent_end("not json"));
        assert!(!frame_is_terminal_agent_end(""));
    }

    #[test]
    fn ignores_noise_and_garbage() {
        let mut collected = String::new();
        assert!(
            parse_omp_line(
                r#"{"type":"available_commands_update","commands":[]}"#,
                &mut collected,
                &mut HashMap::new()
            )
            .is_empty()
        );
        assert!(parse_omp_line("nope", &mut collected, &mut HashMap::new()).is_empty());
    }

    #[test]
    fn frames_prompt_command_with_newline() {
        let params = OmpParams::new(
            "omp",
            Bytes::from_static(b"hello\nworld"),
            PathBuf::from("/tmp"),
        );
        let frame = String::from_utf8(params.frame_prompt_command().unwrap().to_vec()).unwrap();
        assert!(frame.ends_with('\n'));
        let value: serde_json::Value = serde_json::from_str(frame.trim()).unwrap();
        assert_eq!(value["type"], "prompt");
        assert_eq!(value["message"], "hello\nworld");
        assert_eq!(value["id"], "vel");
    }

    /// The initial frame must set `interrupt_mode: immediate` *before* the
    /// prompt, so a later steer/abort can take effect at the next safe
    /// boundary rather than waiting for the whole turn — required regardless
    /// of whether this particular attempt ends up steered.
    #[test]
    fn frame_initial_sets_immediate_interrupt_mode_before_the_prompt() {
        let params = OmpParams::new("omp", Bytes::from_static(b"hi"), PathBuf::from("/tmp"));
        let initial = params.frame_initial().unwrap();
        let text = std::str::from_utf8(&initial).unwrap();
        let lines: Vec<&str> = text.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(lines.len(), 2, "exactly two commands in the initial frame");

        let mode: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(mode["type"], "set_interrupt_mode");
        assert_eq!(mode["mode"], "immediate");

        let prompt: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(prompt["type"], "prompt");
        assert_eq!(prompt["message"], "hi");
    }

    #[test]
    fn frame_live_message_command_uses_steer_for_steer_behaviour() {
        let bytes = frame_live_message_command("id-1", SteeringBehaviour::Steer, "don't do that");
        let value: serde_json::Value =
            serde_json::from_str(std::str::from_utf8(&bytes).unwrap().trim()).unwrap();
        assert_eq!(value["type"], "steer");
        assert_eq!(value["id"], "id-1");
        assert_eq!(value["message"], "don't do that");
    }

    #[test]
    fn frame_live_message_command_uses_follow_up_for_follow_up_behaviour() {
        let bytes =
            frame_live_message_command("id-2", SteeringBehaviour::FollowUp, "do this after");
        let value: serde_json::Value =
            serde_json::from_str(std::str::from_utf8(&bytes).unwrap().trim()).unwrap();
        assert_eq!(value["type"], "follow_up");
        assert_eq!(value["id"], "id-2");
        assert_eq!(value["message"], "do this after");
    }

    #[test]
    fn frame_abort_command_carries_no_message() {
        let bytes = frame_abort_command("id-3");
        let value: serde_json::Value =
            serde_json::from_str(std::str::from_utf8(&bytes).unwrap().trim()).unwrap();
        assert_eq!(value["type"], "abort");
        assert_eq!(value["id"], "id-3");
        assert!(value.get("message").is_none());
    }

    #[test]
    fn resolve_pending_ack_resolves_success_and_ignores_unrelated_ids() {
        let pending_acks: PendingAcks = Arc::new(Mutex::new(HashMap::new()));
        let (tx, mut rx) = oneshot::channel();
        lock_acks(&pending_acks).insert("mine".to_string(), tx);

        // A response for a different id must not resolve (or remove) ours.
        resolve_pending_ack(
            r#"{"id":"someone-else","type":"response","command":"steer","success":true}"#,
            &pending_acks,
        );
        assert!(rx.try_recv().is_err());
        assert!(lock_acks(&pending_acks).contains_key("mine"));

        resolve_pending_ack(
            r#"{"id":"mine","type":"response","command":"steer","success":true}"#,
            &pending_acks,
        );
        assert_eq!(rx.try_recv().unwrap(), Ok(()));
        assert!(!lock_acks(&pending_acks).contains_key("mine"));
    }

    #[test]
    fn resolve_pending_ack_resolves_rejection_with_the_error_text() {
        let pending_acks: PendingAcks = Arc::new(Mutex::new(HashMap::new()));
        let (tx, mut rx) = oneshot::channel();
        lock_acks(&pending_acks).insert("mine".to_string(), tx);

        resolve_pending_ack(
            r#"{"id":"mine","type":"response","command":"steer","success":false,"error":"no active turn"}"#,
            &pending_acks,
        );
        assert_eq!(rx.try_recv().unwrap(), Err("no active turn".to_string()));
    }

    #[test]
    fn drain_pending_acks_drops_waiters_so_they_do_not_hang() {
        let pending_acks: PendingAcks = Arc::new(Mutex::new(HashMap::new()));
        let (tx, mut rx) = oneshot::channel();
        lock_acks(&pending_acks).insert("mine".to_string(), tx);

        drain_pending_acks(&pending_acks);

        assert!(lock_acks(&pending_acks).is_empty());
        assert!(rx.try_recv().is_err(), "sender dropped, not resolved");
        // The receiver observes closure (not merely "still pending"): a
        // forwarding task's `.await` on this resolves immediately.
        assert!(matches!(
            rx.try_recv(),
            Err(oneshot::error::TryRecvError::Closed)
        ));
    }

    // ── File-edit capture ───────────────────────────────────────────────────

    #[derive(Default)]
    struct CollectingSink {
        events: Vec<AgentEvent>,
    }

    #[async_trait::async_trait(?Send)]
    impl AgentEventSink for CollectingSink {
        async fn emit(
            &mut self,
            event: AgentEvent,
        ) -> Result<(), crate::execution_service::adapter::AgentSinkError> {
            self.events.push(event);
            Ok(())
        }
    }

    #[test]
    fn detects_first_class_edit_tools() {
        assert!(is_file_edit_tool("edit"));
        assert!(is_file_edit_tool("write"));
        assert!(!is_file_edit_tool("read"));
        assert!(!is_file_edit_tool("bash"));
        assert!(!is_file_edit_tool("grep"));
        assert!(!is_file_edit_tool("todo"));
    }

    #[test]
    fn edit_target_path_reads_write_s_explicit_path() {
        let input = serde_json::json!({"i": "intent", "path": "src/lib.rs"});
        assert_eq!(
            edit_target_path("write", &input).as_deref(),
            Some("src/lib.rs")
        );

        // Non-edit tools never report a target even with a path.
        assert!(edit_target_path("read", &input).is_none());

        // Empty path is ignored.
        let empty = serde_json::json!({"path": ""});
        assert!(edit_target_path("write", &empty).is_none());
    }

    #[test]
    fn edit_target_path_reads_edit_s_leading_bracketed_marker() {
        let input = serde_json::json!({
            "i": "Replace iter().copied().collect() with to_vec()",
            "input": "[crates/domain/aq-feature-domain/src/quality_gathering.rs#72DB]\nSWAP 36.=36:\nuse crate::quality_features::{QualityEvaluation, QualityFeatureError, evaluate_quality};",
        });
        assert_eq!(
            edit_target_path("edit", &input).as_deref(),
            Some("crates/domain/aq-feature-domain/src/quality_gathering.rs")
        );
    }

    #[test]
    fn edit_target_path_ignores_edit_input_missing_a_marker() {
        let input = serde_json::json!({"i": "intent", "input": "no marker here\nSWAP 1.=1:\nx"});
        assert!(edit_target_path("edit", &input).is_none());
    }

    #[tokio::test]
    async fn omp_edit_reports_the_real_modified_diff_ignoring_the_patch_dsl() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cwd = dir.path();
        let path = "src/lib.rs";
        std::fs::create_dir_all(cwd.join("src")).expect("mkdir");
        std::fs::write(cwd.join(path), "fn main() {}\n").expect("write pre");

        let mut collected = String::new();
        let mut pending_tool_calls = HashMap::new();
        let mut pending_edits = Vec::new();
        let sink = &mut CollectingSink::default();

        let call_events = parse_omp_line(
            &serde_json::json!({
                "type": "message_update",
                "assistantMessageEvent": {
                    "type": "toolcall_end",
                    "toolCall": {
                        "id": "c1",
                        "name": "edit",
                        "arguments": {
                            "i": "swap body",
                            "input": "[src/lib.rs#AAAA]\nSWAP 1.=1:\nfn main() { todo!() }",
                        },
                    },
                },
            })
            .to_string(),
            &mut collected,
            &mut pending_tool_calls,
        );
        for event in call_events {
            process_event(sink, cwd, &mut pending_edits, event)
                .await
                .expect("process call event");
        }

        // The agent's tool lands the real edit on disk before its result frame.
        std::fs::write(cwd.join(path), "fn main() { todo!() }\n").expect("write post");

        let result_events = parse_omp_line(
            &serde_json::json!({
                "type": "tool_execution_end",
                "toolCallId": "c1",
                "toolName": "edit",
                "isError": false,
                "result": {"content": [{"type": "text", "text": "ok"}]},
            })
            .to_string(),
            &mut collected,
            &mut pending_tool_calls,
        );
        for event in result_events {
            process_event(sink, cwd, &mut pending_edits, event)
                .await
                .expect("process result event");
        }

        let edit = sink
            .events
            .iter()
            .find_map(|e| match e {
                AgentEvent::FileEdit { edit } => Some(edit),
                _ => None,
            })
            .expect("a FileEdit event derived from the real on-disk change");
        assert_eq!(edit.path, path);
        assert!(matches!(
            edit.kind,
            crate::file_edit::FileEditKind::Modified
        ));
    }

    // ── End-to-end steer/follow-up/failure behaviour against a fake omp ─────
    //
    // These drive `run_omp_stream` + `forward_live_input` against a real `sh`
    // subprocess simulating the RPC protocol (matching the existing
    // `ProcessSpec::builder("sh")` convention used elsewhere for supervisor-
    // level tests), rather than a real `omp` binary. This proves the adapter's
    // own correlation/continuation logic end-to-end without requiring `omp`,
    // auth, or network access.

    use crate::execution_service::steering::SteeringText;
    use std::time::Duration;

    fn fake_omp_spec(script: &str, initial: Bytes) -> ProcessSpec {
        ProcessSpec::builder("sh")
            .arg("-c")
            .arg(script)
            .input(ProcessInput::Streaming { initial })
            .timeouts(ProcessTimeouts {
                total: Some(Duration::from_secs(15)),
                termination_grace: Duration::from_secs(2),
                ..Default::default()
            })
            .capture_bytes(64 * 1024)
            .build()
    }

    /// Reads each JSONL input line and reacts like a minimal omp RPC server:
    /// on `prompt` it starts a turn; on `steer`/`follow_up`/`abort` it acks by
    /// echoing a `response` with the same id (extracted with `sed`) and prints
    /// whatever the test's `$1`/`$2`/`$3` placeholders specify.
    const FAKE_OMP_SHELL_HELPERS: &str = r#"
ack() {
  id=$(printf '%s' "$1" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
  printf '{"id":"%s","type":"response","command":"%s","success":%s%s}\n' "$id" "$2" "$3" "$4"
}
"#;

    #[tokio::test]
    async fn steer_targets_the_existing_session_not_a_replacement() {
        // The same process both streams text before the steer AND after the
        // steer's RPC ack — proving the steer redirected the running session
        // rather than Velor killing/respawning a new one.
        let script = format!(
            r#"{FAKE_OMP_SHELL_HELPERS}
while IFS= read -r line; do
  case "$line" in
    *'"type":"prompt"'*)
      echo '{{"type":"agent_start"}}'
      echo '{{"type":"message_update","assistantMessageEvent":{{"type":"text_delta","delta":"before-steer "}}}}'
      ;;
    *'"type":"steer"'*)
      ack "$line" steer true ""
      echo '{{"type":"message_update","assistantMessageEvent":{{"type":"text_delta","delta":"after-steer"}}}}'
      echo '{{"type":"agent_end","messages":[],"isTerminal":true}}'
      ;;
  esac
done
"#
        );

        let params = OmpParams::new("unused", Bytes::from_static(b"go"), PathBuf::from("/tmp"));
        let initial = params.frame_initial().unwrap();
        let spec = fake_omp_spec(&script, initial);
        let process = crate::execution_service::supervisor::spawn(spec, CancellationToken::new())
            .await
            .expect("spawn fake omp");

        let pending_acks: PendingAcks = Arc::new(Mutex::new(HashMap::new()));
        let command_tx = process.input_sender().expect("streaming input sender");
        let cancel = process.cancellation().clone();
        let (live_tx, live_rx) = mpsc::channel(4);
        let forward = tokio::spawn(forward_live_input(
            live_rx,
            command_tx,
            cancel,
            pending_acks.clone(),
        ));

        let (ack_tx, ack_rx) = oneshot::channel();
        live_tx
            .send(AgentInput::UserMessage {
                text: SteeringText::new("don't replace that, extend it").unwrap(),
                behaviour: SteeringBehaviour::Steer,
                acknowledgement: ack_tx,
            })
            .await
            .expect("send steer input");

        let mut sink = CollectingSink::default();
        let result = run_omp_stream(
            process,
            &mut sink,
            std::path::Path::new("/tmp"),
            pending_acks,
        )
        .await
        .expect("run completes");

        drop(live_tx);
        let _ = forward.await;

        assert!(matches!(
            ack_rx.await.unwrap(),
            Ok(SteeringDelivery::Acknowledged)
        ));
        assert!(result.stdout.contains("before-steer"));
        assert!(result.stdout.contains("after-steer"));
    }

    #[tokio::test]
    async fn follow_up_runs_after_the_current_turn_in_the_same_process() {
        // The first turn ends with `isTerminal:false` (a follow-up is queued);
        // the adapter must not close stdin there, and the follow-up's own
        // turn must complete in the SAME process before the final close.
        let script = format!(
            r#"{FAKE_OMP_SHELL_HELPERS}
while IFS= read -r line; do
  case "$line" in
    *'"type":"prompt"'*)
      echo '{{"type":"agent_start"}}'
      echo '{{"type":"message_update","assistantMessageEvent":{{"type":"text_delta","delta":"turn-one "}}}}'
      echo '{{"type":"agent_end","messages":[],"isTerminal":false}}'
      ;;
    *'"type":"follow_up"'*)
      ack "$line" follow_up true ""
      echo '{{"type":"agent_start"}}'
      echo '{{"type":"message_update","assistantMessageEvent":{{"type":"text_delta","delta":"turn-two"}}}}'
      echo '{{"type":"agent_end","messages":[],"isTerminal":true}}'
      ;;
  esac
done
"#
        );

        let params = OmpParams::new("unused", Bytes::from_static(b"go"), PathBuf::from("/tmp"));
        let initial = params.frame_initial().unwrap();
        let spec = fake_omp_spec(&script, initial);
        let process = crate::execution_service::supervisor::spawn(spec, CancellationToken::new())
            .await
            .expect("spawn fake omp");

        let pending_acks: PendingAcks = Arc::new(Mutex::new(HashMap::new()));
        let command_tx = process.input_sender().expect("streaming input sender");
        let cancel = process.cancellation().clone();
        let (live_tx, live_rx) = mpsc::channel(4);
        let forward = tokio::spawn(forward_live_input(
            live_rx,
            command_tx,
            cancel,
            pending_acks.clone(),
        ));

        let (ack_tx, ack_rx) = oneshot::channel();
        live_tx
            .send(AgentInput::UserMessage {
                text: SteeringText::new("also check the tests").unwrap(),
                behaviour: SteeringBehaviour::FollowUp,
                acknowledgement: ack_tx,
            })
            .await
            .expect("send follow-up input");

        let mut sink = CollectingSink::default();
        let result = run_omp_stream(
            process,
            &mut sink,
            std::path::Path::new("/tmp"),
            pending_acks,
        )
        .await
        .expect("run completes");

        drop(live_tx);
        let _ = forward.await;

        assert!(matches!(
            ack_rx.await.unwrap(),
            Ok(SteeringDelivery::Acknowledged)
        ));
        assert!(result.stdout.contains("turn-one"));
        assert!(result.stdout.contains("turn-two"));
    }

    #[tokio::test]
    async fn rejected_steer_surfaces_as_a_distinct_failure_not_a_hang() {
        let script = format!(
            r#"{FAKE_OMP_SHELL_HELPERS}
while IFS= read -r line; do
  case "$line" in
    *'"type":"prompt"'*) echo '{{"type":"agent_start"}}' ;;
    *'"type":"steer"'*)
      ack "$line" steer false ',"error":"no active turn"'
      echo '{{"type":"agent_end","messages":[],"isTerminal":true}}'
      ;;
  esac
done
"#
        );

        let params = OmpParams::new("unused", Bytes::from_static(b"go"), PathBuf::from("/tmp"));
        let initial = params.frame_initial().unwrap();
        let spec = fake_omp_spec(&script, initial);
        let process = crate::execution_service::supervisor::spawn(spec, CancellationToken::new())
            .await
            .expect("spawn fake omp");

        let pending_acks: PendingAcks = Arc::new(Mutex::new(HashMap::new()));
        let command_tx = process.input_sender().expect("streaming input sender");
        let cancel = process.cancellation().clone();
        let (live_tx, live_rx) = mpsc::channel(4);
        let forward = tokio::spawn(forward_live_input(
            live_rx,
            command_tx,
            cancel,
            pending_acks.clone(),
        ));

        let (ack_tx, ack_rx) = oneshot::channel();
        live_tx
            .send(AgentInput::UserMessage {
                text: SteeringText::new("steer this").unwrap(),
                behaviour: SteeringBehaviour::Steer,
                acknowledgement: ack_tx,
            })
            .await
            .expect("send steer input");

        let mut sink = CollectingSink::default();
        let _ = run_omp_stream(
            process,
            &mut sink,
            std::path::Path::new("/tmp"),
            pending_acks,
        )
        .await
        .expect("run completes");

        drop(live_tx);
        let _ = forward.await;

        assert!(matches!(
            ack_rx.await.unwrap(),
            Err(AgentInputError::Unavailable {
                reason: LiveSteeringUnavailableReason::Rejected
            })
        ));
    }

    #[tokio::test]
    async fn steer_fails_cleanly_instead_of_hanging_when_the_process_exits_unanswered() {
        // The fake server exits the instant it sees the steer command, never
        // sending a response. The forwarding task's wait must resolve (not
        // hang) once the stream ends.
        let script = r#"
while IFS= read -r line; do
  case "$line" in
    *'"type":"prompt"'*) echo '{"type":"agent_start"}' ;;
    *'"type":"steer"'*) exit 1 ;;
  esac
done
"#;

        let params = OmpParams::new("unused", Bytes::from_static(b"go"), PathBuf::from("/tmp"));
        let initial = params.frame_initial().unwrap();
        let spec = fake_omp_spec(script, initial);
        let process = crate::execution_service::supervisor::spawn(spec, CancellationToken::new())
            .await
            .expect("spawn fake omp");

        let pending_acks: PendingAcks = Arc::new(Mutex::new(HashMap::new()));
        let command_tx = process.input_sender().expect("streaming input sender");
        let cancel = process.cancellation().clone();
        let (live_tx, live_rx) = mpsc::channel(4);
        let forward = tokio::spawn(forward_live_input(
            live_rx,
            command_tx,
            cancel,
            pending_acks.clone(),
        ));

        let (ack_tx, ack_rx) = oneshot::channel();
        live_tx
            .send(AgentInput::UserMessage {
                text: SteeringText::new("steer this").unwrap(),
                behaviour: SteeringBehaviour::Steer,
                acknowledgement: ack_tx,
            })
            .await
            .expect("send steer input");

        let mut sink = CollectingSink::default();
        // The process exit is not itself a `agent_end`, so this attempt's
        // outcome is classified from the raw exit rather than `map_outcome`'s
        // success path — either way, the point under test is the ack below.
        let _ = tokio::time::timeout(
            Duration::from_secs(10),
            run_omp_stream(
                process,
                &mut sink,
                std::path::Path::new("/tmp"),
                pending_acks,
            ),
        )
        .await
        .expect("run_omp_stream must not hang once the process exits");

        drop(live_tx);
        let _ = forward.await;

        let ack = tokio::time::timeout(Duration::from_secs(5), ack_rx)
            .await
            .expect("the steer ack must resolve, not hang, once the stream ends")
            .unwrap();
        assert!(matches!(
            ack,
            Err(AgentInputError::Unavailable {
                reason: LiveSteeringUnavailableReason::StdinClosed
            })
        ));
    }
}
