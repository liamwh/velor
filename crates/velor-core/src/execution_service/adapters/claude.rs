//! Claude Code subprocess adapter.
//!
//! Drives [`crate::execution_service::supervisor`] with the Claude
//! `stream-json` invocation, owns the UTF-8 → newline → stream-json framing,
//! emits [`crate::agent::AgentEvent`]s, and classifies the final output via
//! [`crate::execution_service::classify`].

use async_trait::async_trait;
use bytes::Bytes;
use std::collections::HashMap;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use tokio_util::sync::CancellationToken;

use crate::agent::{AgentEvent, AgentRunResult};
use crate::execution_service::adapter::{AgentAdapter, AgentEventSink, LineDecoder};
use crate::execution_service::classify::{ClassifiedProvider, ProviderKind, classify_output};
use crate::execution_service::error::{
    AgentExecutionError, LiveSteeringUnavailableReason, ProcessError, UnsuccessfulExit,
};
use crate::execution_service::output::Termination;
use crate::execution_service::supervisor::{
    ProcessEvent, ProcessInput, ProcessInputCommand, ProcessSpec, ProcessTimeouts, RunningProcess,
};
use crate::file_edit::{
    DEFAULT_MAX_DIFF_LINES, FileEdit, FileEditKind, compute_file_edit, infer_syntax,
};

/// Maximum length of a single stream-json frame (line) before it is rejected.
const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

/// Maximum bytes of a `tool_result`'s content carried on
/// [`crate::agent::AgentEvent::ToolResult::detail`]. Generous — this is the
/// real output a user can expand to read in full, not a preview — the TUI's
/// own transcript byte budget (`TuiLimits::max_bytes`) is the actual memory
/// bound, not this constant.
const TOOL_RESULT_DETAIL_MAX: usize = 20_000;

/// Backstop grace for the streaming path: how long to wait for the subprocess to
/// exit on its own after we close stdin (EOF) following the `result` frame.
/// Claude Code normally exits promptly on that EOF, but if it ever lingers we
/// finalise from the already-observed turn output and tear the process down, so a
/// completed turn can never wedge the iteration indefinitely.
const STREAM_EXIT_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

/// Parameters for one Claude subprocess invocation.
#[derive(Debug, Clone)]
pub struct ClaudeParams {
    /// Binary to invoke (e.g. `claude-glm` or `glm5`).
    pub binary: String,
    /// Permission mode passed via `--permission-mode`.
    pub permission_mode: String,
    /// The rendered prompt delivered over stdin.
    pub prompt: Bytes,
    /// Working directory.
    pub working_directory: PathBuf,
    /// Optional model override (`--model`).
    pub model: Option<String>,
    /// Optional session to resume (`--resume <id>`).
    pub resume_session: Option<String>,
    /// Extra CLI arguments appended after the standard flags.
    pub extra_args: Vec<String>,
    /// Extra environment variables (key, value).
    pub extra_env: Vec<(String, String)>,
    /// Deadlines for this attempt.
    pub timeouts: ProcessTimeouts,
    /// Cancellation token for this attempt.
    pub cancellation: CancellationToken,
    /// Whether to enable the live-steering streaming path. Configuration only —
    /// runtime input channels are passed to the adapter separately, never stored
    /// here. When `false`, the adapter uses the one-shot `--input-format text`
    /// path exactly as before.
    pub enable_live_steering: bool,
}

impl ClaudeParams {
    /// Creates parameters with sensible empty/none defaults.
    #[must_use]
    pub fn new(binary: impl Into<String>, prompt: Bytes, working_directory: PathBuf) -> Self {
        Self {
            binary: binary.into(),
            permission_mode: "acceptEdits".to_string(),
            prompt,
            working_directory,
            model: None,
            resume_session: None,
            extra_args: Vec::new(),
            extra_env: Vec::new(),
            timeouts: ProcessTimeouts::default(),
            cancellation: CancellationToken::new(),
            enable_live_steering: false,
        }
    }
}

/// Claude Code subprocess adapter.
pub struct ClaudeSubprocessAdapter {
    params: ClaudeParams,
}

impl ClaudeSubprocessAdapter {
    /// Creates an adapter for the given parameters.
    #[must_use]
    pub fn new(params: ClaudeParams) -> Self {
        Self { params }
    }

    /// Builds the process specification for the supervisor. `input` selects the
    /// delivery mode: [`ProcessInput::Streaming`] for the live-steering path,
    /// [`ProcessInput::Bytes`] for the one-shot path.
    fn build_spec(&self, input: ProcessInput) -> ProcessSpec {
        let mut builder = ProcessSpec::builder(&self.params.binary)
            .arg("--permission-mode")
            .arg(self.params.permission_mode.clone())
            .arg("--dangerously-skip-permissions")
            .arg("--verbose")
            .cwd(self.params.working_directory.clone())
            .input(input)
            .timeouts(self.params.timeouts.clone())
            .capture_bytes(64 * 1024);

        if self.params.enable_live_steering {
            // Streaming path: no positional prompt; the initial frame (and all
            // later steering) travel over stream-json stdin.
            builder = builder
                .arg("--print")
                .arg("--input-format")
                .arg("stream-json")
                .arg("--output-format")
                .arg("stream-json")
                .arg("--replay-user-messages");
        } else {
            // One-shot path: text input, then EOF. Unchanged behaviour.
            builder = builder
                .arg("-p")
                .arg("--input-format")
                .arg("text")
                .arg("--output-format")
                .arg("stream-json")
                .arg("--include-partial-messages");
        }

        if let Some(model) = &self.params.model {
            builder = builder.arg("--model").arg(model.clone());
        }
        if let Some(session) = &self.params.resume_session {
            builder = builder.arg("--resume").arg(session.clone());
        }
        for arg in &self.params.extra_args {
            builder = builder.arg(arg.clone());
        }
        for (key, value) in &self.params.extra_env {
            builder = builder.env(key.clone(), value.clone());
        }
        builder.build()
    }

    /// Frames the prompt as the streaming initial user message. Only fails if the
    /// prompt is empty/whitespace or serde cannot encode it (effectively never).
    fn frame_initial(&self) -> Result<Bytes, AgentExecutionError> {
        let prompt_str = std::str::from_utf8(&self.params.prompt).map_err(|_| {
            AgentExecutionError::LiveSteeringUnavailable {
                reason: LiveSteeringUnavailableReason::ProtocolRejected,
            }
        })?;
        let text = crate::execution_service::adapters::claude_stream::SteeringText::new(prompt_str)
            .map_err(|_| AgentExecutionError::LiveSteeringUnavailable {
                reason: LiveSteeringUnavailableReason::ProtocolRejected,
            })?;
        crate::execution_service::adapters::claude_stream::frame_user_message(&text).map_err(|_| {
            AgentExecutionError::LiveSteeringUnavailable {
                reason: LiveSteeringUnavailableReason::ProtocolRejected,
            }
        })
    }
}

#[async_trait(?Send)]
impl AgentAdapter for ClaudeSubprocessAdapter {
    async fn execute(
        &mut self,
        sink: &mut dyn AgentEventSink,
        live_input: Option<tokio::sync::mpsc::Receiver<crate::agent::AgentInput>>,
    ) -> Result<AgentRunResult, AgentExecutionError> {
        // Select the delivery mode and build the spec. The streaming path frames
        // the prompt as the initial user message and carries the steering
        // receiver through; the one-shot path passes raw bytes and closes stdin
        // (no steering), dropping any receiver it was handed.
        let (spec, steering, live_input) = if self.params.enable_live_steering {
            let initial = self.frame_initial()?;
            (
                self.build_spec(ProcessInput::Streaming { initial }),
                true,
                live_input,
            )
        } else {
            (
                self.build_spec(ProcessInput::Bytes(self.params.prompt.clone())),
                false,
                None,
            )
        };

        let process: RunningProcess =
            crate::execution_service::supervisor::spawn(spec, self.params.cancellation.clone())
                .await?;

        // The supervisor's streaming-stdin command sender. Present only for the
        // streaming path; reused both to forward steering and to close stdin when
        // the turn completes.
        let command_sender = process.input_sender();

        // Spawn the steering-forwarding task only for the streaming path, and only
        // when both a writable command sender and a steering receiver exist.
        let forward_handle = if steering {
            match (command_sender.clone(), live_input) {
                (Some(command_tx), Some(live_rx)) => Some(tokio::spawn(forward_steering(
                    live_rx,
                    command_tx,
                    self.params.cancellation.clone(),
                ))),
                // Nothing to forward: drop whichever half is present.
                _ => None,
            }
        } else {
            None
        };

        let result = run_claude_stream(
            process,
            sink,
            command_sender.as_ref(),
            &self.params.working_directory,
        )
        .await;
        // The process is finished; stop the forwarding task promptly so it cannot
        // outlive the execution.
        if let Some(handle) = forward_handle {
            handle.abort();
        }
        result
    }
}

/// Forwards typed live-steering [`AgentInput`]s to the supervisor's streaming
/// stdin as framed Claude user messages, acknowledging each with its delivery
/// state. Stops when the steering receiver closes, the supervisor's writer goes
/// away, or the attempt is cancelled. Never closes stdin itself — only the
/// deliberate execution shutdown does.
async fn forward_steering(
    mut live_input: tokio::sync::mpsc::Receiver<crate::agent::AgentInput>,
    command_tx: tokio::sync::mpsc::Sender<ProcessInputCommand>,
    cancel: CancellationToken,
) {
    use crate::agent::{AgentInput, AgentInputError, SteeringDelivery};
    use crate::execution_service::adapters::claude_stream::{SteeringText, frame_user_message};
    use crate::execution_service::error::LiveSteeringUnavailableReason;

    loop {
        let input = tokio::select! {
            biased;
            _ = cancel.cancelled() => return,
            input = live_input.recv() => match input {
                Some(i) => i,
                None => return, // steering sender dropped
            },
        };
        let AgentInput::UserMessage {
            text,
            acknowledgement,
        } = input;
        let ack = deliver(&command_tx, &text, &cancel).await;
        // If the consumer dropped their acknowledgement receiver, the send fails
        // harmlessly.
        let _ = acknowledgement.send(ack);
    }

    async fn deliver(
        command_tx: &tokio::sync::mpsc::Sender<ProcessInputCommand>,
        text: &SteeringText,
        cancel: &CancellationToken,
    ) -> Result<SteeringDelivery, AgentInputError> {
        let frame = frame_user_message(text).map_err(|_| AgentInputError::Unavailable {
            reason: LiveSteeringUnavailableReason::ProtocolRejected,
        })?;
        let (write_ack, write_rx) = tokio::sync::oneshot::channel();
        let send = command_tx.send(ProcessInputCommand::Write {
            bytes: frame,
            acknowledgement: write_ack,
        });
        tokio::pin!(send);
        let sent = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(AgentInputError::Unavailable {
                reason: LiveSteeringUnavailableReason::WriteFailed,
            }),
            s = &mut send => s,
        };
        if sent.is_err() {
            // The supervisor's writer has gone away (process closing/terminated).
            return Err(AgentInputError::Unavailable {
                reason: LiveSteeringUnavailableReason::StdinClosed,
            });
        }
        let result = match write_rx.await {
            Ok(r) => r,
            Err(_) => {
                // Writer dropped without acknowledging: the bytes may or may not
                // have landed before it went away.
                return Ok(SteeringDelivery::DeliveryUnknown);
            }
        };
        match result {
            Ok(()) => Ok(SteeringDelivery::Written),
            Err(super::super::supervisor::ProcessInputWriteError::Closed) => {
                Err(AgentInputError::Unavailable {
                    reason: LiveSteeringUnavailableReason::StdinClosed,
                })
            }
            Err(_) => Err(AgentInputError::Unavailable {
                reason: LiveSteeringUnavailableReason::WriteFailed,
            }),
        }
    }
}

/// Drives a running Claude subprocess: decodes stdout frames, emits events,
/// then classifies the final output.
///
/// `streaming_close` is `Some` for the live-steering path. In Claude Code's
/// stream-json `--print` mode the process does **not** exit when a turn reaches
/// `end_turn` — it keeps stdin open waiting for more messages. So once the
/// `result` (end_turn) frame is observed we deliberately close stdin (EOF) so
/// the process exits cleanly and the iteration completes. Any mid-turn steering
/// is incorporated *before* that result, so this never cuts off an in-flight
/// steer.
async fn run_claude_stream(
    mut process: RunningProcess,
    sink: &mut dyn AgentEventSink,
    streaming_close: Option<&tokio::sync::mpsc::Sender<ProcessInputCommand>>,
    cwd: &Path,
) -> Result<AgentRunResult, AgentExecutionError> {
    let mut decoder = LineDecoder::new(MAX_FRAME_BYTES);
    let mut collected = String::new();
    let mut structured_error: Option<String> = None;
    let mut close_sent = false;
    // Pre-edit snapshots for first-class edit tools, captured when the tool_use
    // is observed (before the edit lands) and diffed against the post-edit file
    // when the tool result arrives. See [`process_event`].
    let mut pending_edits: Vec<(String, ReadState)> = Vec::new();
    // Tool name + summary keyed by tool_use id, so the matching `tool_result`
    // (which only carries the id) can recover both. See `parse_claude_line`.
    let mut pending_tool_calls: HashMap<String, (String, String)> = HashMap::new();

    while let Some(event) = process.next_event().await {
        match event {
            ProcessEvent::Stdout(chunk) => {
                let lines = decoder.push(&chunk.bytes)?;
                let mut saw_result = false;
                for line in lines {
                    let text = String::from_utf8_lossy(&line);
                    if streaming_close.is_some() && frame_is_result(&text) {
                        saw_result = true;
                    }
                    for agent_event in
                        parse_claude_line(&text, &mut collected, &mut pending_tool_calls)
                    {
                        if let Some(msg) =
                            process_event(sink, cwd, &mut pending_edits, agent_event).await?
                            && structured_error.is_none()
                        {
                            structured_error = Some(msg);
                        }
                    }
                }
                // The turn is complete: send EOF so the subprocess exits, then
                // stop reading and finalise. Claude Code exits promptly on this
                // EOF; finalising from the observed result frame (rather than
                // waiting for exit inside the loop) also bounds any lingering.
                if saw_result
                    && !close_sent
                    && let Some(tx) = streaming_close
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
                // Captured by the supervisor for classification; not streamed as
                // AgentEvents (stderr is diagnostic, not protocol output).
            }
            ProcessEvent::StdinWritten
            | ProcessEvent::StdinInitialised
            | ProcessEvent::StdinWriteFailed(_)
            | ProcessEvent::Exited => {}
        }
    }

    // Flush any trailing frame without a newline.
    if let Some(remainder) = decoder.flush_remainder()? {
        let text = String::from_utf8_lossy(&remainder);
        for agent_event in parse_claude_line(&text, &mut collected, &mut pending_tool_calls) {
            if let Some(msg) = process_event(sink, cwd, &mut pending_edits, agent_event).await?
                && structured_error.is_none()
            {
                structured_error = Some(msg);
            }
        }
    }

    // Best-effort: emit diffs for any edits whose result frame we never observed
    // (e.g. the process ended mid-turn). Reads the final on-disk state.
    drain_pending_edits(cwd, &mut pending_edits, sink).await?;

    // One-shot path (`close_sent` stays false): the loop ended because the
    // process exited naturally, so collect the real exit status. Streaming path
    // (`close_sent`): we broke out after the result frame + EOF; finalise
    // without requiring the subprocess to exit.
    if close_sent {
        return finalize_streaming(process, collected, structured_error, STREAM_EXIT_GRACE).await;
    }
    let output = process.complete().await?;
    map_outcome(output, collected, structured_error, ProviderKind::Claude)
}

/// Finalises a streaming run whose turn already completed (the `result` frame
/// was observed and EOF was sent).
///
/// The subprocess normally exits promptly on that EOF; we race that natural exit
/// against a short grace so a completed turn can never wedge the iteration. If it
/// exits within the grace its real status is classified as usual; if it ever
/// lingers, the process group is cancelled (reaped by the detached supervisor)
/// and the run succeeds from the already-collected turn output — the `result`
/// frame proved the turn done.
async fn finalize_streaming(
    process: RunningProcess,
    collected: String,
    structured_error: Option<String>,
    grace: std::time::Duration,
) -> Result<AgentRunResult, AgentExecutionError> {
    let cancellation = process.cancellation().clone();
    match tokio::time::timeout(grace, process.complete()).await {
        Ok(Ok(output)) => map_outcome(output, collected, structured_error, ProviderKind::Claude),
        Ok(Err(e)) => Err(e.into()),
        Err(_) => {
            // Lingered past grace: tear the group down. The supervisor task is
            // detached (its completion future was dropped) but still reaps the
            // child because we cancel its token.
            cancellation.cancel();
            Ok(AgentRunResult { stdout: collected })
        }
    }
}

/// Emits an event to the sink, mapping a closed sink to a cancellation.
async fn emit_event(
    sink: &mut dyn AgentEventSink,
    event: AgentEvent,
) -> Result<(), AgentExecutionError> {
    sink.emit(event)
        .await
        .map_err(|_| AgentExecutionError::Cancelled)
}

// ── File-edit capture ───────────────────────────────────────────────────────
//
// The Claude subprocess executes its own tools; Velor only sees the stream-json
// event stream. To show the *real* resulting edit (not the agent's claimed
// patch) we snapshot a file's contents when its `tool_use` is observed — before
// the edit lands — and diff it against the post-edit contents once the matching
// `tool_result` arrives (by which point the edit is on disk). Only first-class
// edit tools (Edit/Write/MultiEdit/NotebookEdit) are observed; shell-driven file
// mutations via Bash are not reliably observable and are intentionally excluded.

/// On-disk state of a file at a capture point.
enum ReadState {
    /// The file did not exist.
    Missing,
    /// The file existed with these bytes.
    Bytes(Vec<u8>),
    /// The file existed but could not be read (e.g. a permission error).
    Unreadable(String),
}

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
    emit_event(sink, event).await?;
    Ok(err_msg)
}

/// Records a pre-edit snapshot, preserving insertion order and ignoring
/// duplicate paths within a single in-flight batch (one diff per file).
fn note_pending(pending: &mut Vec<(String, ReadState)>, path: String, state: ReadState) {
    if !pending.iter().any(|(p, _)| p == &path) {
        pending.push((path, state));
    }
}

/// Emits a [`AgentEvent::FileEdit`] for each pending edit, reading the post-edit
/// file state and computing the diff. No-op (and cheap) when nothing is pending.
async fn drain_pending_edits(
    cwd: &Path,
    pending: &mut Vec<(String, ReadState)>,
    sink: &mut dyn AgentEventSink,
) -> Result<(), AgentExecutionError> {
    if pending.is_empty() {
        return Ok(());
    }
    let drained = std::mem::take(pending);
    for (path, pre) in drained {
        if let Some(edit) = build_edit(cwd, &path, pre).await {
            emit_event(sink, AgentEvent::FileEdit { edit }).await?;
        }
    }
    Ok(())
}

/// Builds the [`FileEdit`] for one path from its pre-edit state and the current
/// post-edit state. Returns `None` when the edit made no effective change.
async fn build_edit(cwd: &Path, path: &str, pre: ReadState) -> Option<FileEdit> {
    let pre_bytes: Option<Vec<u8>> = match pre {
        ReadState::Bytes(bytes) => Some(bytes),
        ReadState::Missing => None,
        ReadState::Unreadable(ref reason) => {
            return Some(capture_failed(path, reason.clone()));
        }
    };
    match read_file_state(cwd, path).await {
        ReadState::Unreadable(reason) => Some(capture_failed(path, reason)),
        ReadState::Missing => {
            compute_file_edit(path, pre_bytes.as_deref(), None, DEFAULT_MAX_DIFF_LINES)
        }
        ReadState::Bytes(post) => compute_file_edit(
            path,
            pre_bytes.as_deref(),
            Some(&post),
            DEFAULT_MAX_DIFF_LINES,
        ),
    }
}

/// Reads the on-disk state of `path` resolved against `cwd`. Absolute paths
/// override `cwd` (as `PathBuf::join` does).
async fn read_file_state(cwd: &Path, path: &str) -> ReadState {
    let resolved = cwd.join(path);
    match tokio::fs::read(&resolved).await {
        Ok(bytes) => ReadState::Bytes(bytes),
        Err(err) if err.kind() == ErrorKind::NotFound => ReadState::Missing,
        Err(err) => ReadState::Unreadable(err.to_string()),
    }
}

/// A [`FileEdit`] recording that the real before/after state could not be
/// captured, so the transcript surfaces the failure rather than silently
/// presenting the agent's claimed patch as the result.
fn capture_failed(path: &str, reason: String) -> FileEdit {
    FileEdit {
        path: path.to_string(),
        syntax: infer_syntax(path),
        kind: FileEditKind::CaptureFailed { reason },
        hunks: Vec::new(),
        omitted_lines: 0,
        // No source to highlight for a capture failure.
        full_new_source: None,
    }
}

/// The target file path for a first-class edit tool's input, if any.
fn edit_target_path(tool: &str, input: &serde_json::Value) -> Option<String> {
    if !is_file_edit_tool(tool) {
        return None;
    }
    input
        .get("file_path")
        .or_else(|| input.get("file_name"))
        .or_else(|| input.get("notebook_path"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .filter(|s| !s.is_empty())
}

/// Whether `tool` is a first-class file-edit tool whose filesystem result Velor
/// can observe by reading the file before and after the result arrives.
fn is_file_edit_tool(tool: &str) -> bool {
    matches!(tool, "Edit" | "Write" | "MultiEdit" | "NotebookEdit")
}

/// Maps a finished [`crate::execution_service::output::ProcessOutput`] to a
/// result, classifying non-zero exits. Shared across subprocess adapters.
pub(super) fn map_outcome(
    output: crate::execution_service::output::ProcessOutput,
    collected: String,
    structured_error: Option<String>,
    kind: ProviderKind,
) -> Result<AgentRunResult, AgentExecutionError> {
    match output.termination {
        Termination::Exited(status) if status.success() => Ok(AgentRunResult { stdout: collected }),
        Termination::Exited(status) => {
            if let Some(ClassifiedProvider { error, evidence }) =
                classify_output(&output, kind, structured_error.as_deref())
            {
                return Err(AgentExecutionError::Provider { error, evidence });
            }
            Err(AgentExecutionError::UnsuccessfulExit(Box::new(
                UnsuccessfulExit {
                    code: status.code(),
                    stdout: output.stdout,
                    stderr: output.stderr,
                },
            )))
        }
        Termination::TimedOut { which } => {
            Err(AgentExecutionError::Process(ProcessError::TimedOut {
                which,
            }))
        }
        Termination::Cancelled => Err(AgentExecutionError::Cancelled),
    }
}

/// Cheap check: is this stream-json line the terminal `result` (end_turn) frame?
/// Used by the streaming path to know when to close stdin.
fn frame_is_result(line: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
        return false;
    };
    value.get("type").and_then(|v| v.as_str()) == Some("result")
}

/// Parses one Claude stream-json line into zero or more [`AgentEvent`]s and
/// appends assistant text to `collected`.
///
/// This is the canonical home for Claude stream parsing (the legacy copy in
/// `agent::process_stream_line` is removed once consumers migrate).
fn parse_claude_line(
    line: &str,
    collected: &mut String,
    pending_tool_calls: &mut HashMap<String, (String, String)>,
) -> Vec<AgentEvent> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return Vec::new();
    };
    let mut events = Vec::new();
    let event_type = value.get("type").and_then(|v| v.as_str()).unwrap_or("");
    match event_type {
        "content_block_delta" => {
            // Check for thinking delta first (delta.type == "thinking_delta").
            if let Some(thinking) = value
                .get("delta")
                .and_then(|d| d.get("thinking"))
                .and_then(|t| t.as_str())
                && !thinking.is_empty()
            {
                events.push(AgentEvent::Thinking {
                    text: thinking.to_string(),
                });
            } else if let Some(text) = value
                .get("delta")
                .and_then(|d| d.get("text"))
                .and_then(|t| t.as_str())
                && !text.is_empty()
            {
                collected.push_str(text);
                events.push(AgentEvent::TextDelta {
                    text: text.to_string(),
                });
            }
        }
        "content_block_start" => {
            if let Some(text) = value
                .get("content_block")
                .and_then(|b| b.get("text"))
                .and_then(|t| t.as_str())
                && !text.is_empty()
            {
                collected.push_str(text);
                events.push(AgentEvent::TextDelta {
                    text: text.to_string(),
                });
            }
        }
        "assistant" | "user" => {
            // Only assistant text is agent output: `collected` feeds
            // completion-token detection and the notification preview. User
            // frames — the prompt replayed back by `--replay-user-messages`
            // and steering echoes — are surfaced for display but must never be
            // collected. Every prompt quotes the completion token as an
            // instruction to the agent, so replaying it into `collected` would
            // make `contains(complete_token)` true on every iteration and trip a
            // false completion after two iterations, regardless of what the
            // agent actually produced.
            let is_assistant = event_type == "assistant";
            if let Some(content) = value
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_array())
            {
                for item in content {
                    let item_type = item.get("type").and_then(|t| t.as_str()).unwrap_or("");
                    match item_type {
                        "text" => {
                            if let Some(text) = item.get("text").and_then(|t| t.as_str())
                                && !text.is_empty()
                            {
                                if is_assistant {
                                    collected.push_str(text);
                                }
                                events.push(AgentEvent::TextDelta {
                                    text: text.to_string(),
                                });
                            }
                        }
                        "tool_use" => {
                            let name = item
                                .get("name")
                                .and_then(|n| n.as_str())
                                .unwrap_or("tool")
                                .to_string();
                            let detail = summarize_tool_input(&name, item.get("input"));
                            // Remembered so the matching `tool_result` (which
                            // only carries `tool_use_id`, not the tool name or
                            // input) can recover both — see `tool_result` below.
                            if let Some(id) = item.get("id").and_then(|v| v.as_str()) {
                                pending_tool_calls
                                    .insert(id.to_string(), (name.clone(), detail.clone()));
                            }
                            let input = item
                                .get("input")
                                .cloned()
                                .unwrap_or(serde_json::Value::Null);
                            events.push(AgentEvent::ToolCall {
                                tool: name,
                                detail,
                                input,
                            });
                        }
                        "tool_result" => {
                            // Generous: this is the actual output a user
                            // reads in full via the TUI's expand toggle, not
                            // a one-line call summary (`summarize_tool_input`
                            // stays at 200 for that). The TUI's own transcript
                            // byte budget is the real memory bound.
                            let detail = item
                                .get("content")
                                .map(|c| truncate_value(c, TOOL_RESULT_DETAIL_MAX))
                                .unwrap_or_default();
                            let matched = item
                                .get("tool_use_id")
                                .and_then(|v| v.as_str())
                                .and_then(|id| pending_tool_calls.remove(id));
                            let (tool, command) = match matched {
                                Some((name, call_detail)) => (name, Some(call_detail)),
                                None => ("tool".to_string(), None),
                            };
                            events.push(AgentEvent::ToolResult {
                                tool,
                                detail,
                                success: None,
                                command,
                            });
                        }
                        _ => {}
                    }
                }
            }
        }
        "system" => {
            if let Some(session) = value.get("session_id").and_then(|s| s.as_str()) {
                events.push(AgentEvent::Status {
                    message: format!("session: {session}"),
                });
            }
        }
        "result" => {
            if let Some(usage) = value.get("usage") {
                events.push(AgentEvent::Usage {
                    input_tokens: usage.get("input_tokens").and_then(|v| v.as_u64()),
                    output_tokens: usage.get("output_tokens").and_then(|v| v.as_u64()),
                    cached_input_tokens: usage
                        .get("cache_read_input_tokens")
                        .or_else(|| usage.get("cached_input_tokens"))
                        .and_then(|v| v.as_u64()),
                });
            }
        }
        _ => {}
    }
    events
}

/// Summarises a tool invocation's input for display.
fn summarize_tool_input(name: &str, input: Option<&serde_json::Value>) -> String {
    let Some(input) = input else {
        return String::new();
    };
    let max = 200usize;
    match name {
        "Read" => input
            .get("file_path")
            .or_else(|| input.get("file_name"))
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_default(),
        "Bash" => input
            .get("command")
            .and_then(|v| v.as_str())
            .map(|c| truncate_str(c, max))
            .unwrap_or_default(),
        "Glob" => input
            .get("pattern")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_default(),
        "Grep" => {
            let pattern = input.get("pattern").and_then(|v| v.as_str()).unwrap_or("?");
            if let Some(path) = input.get("path").and_then(|v| v.as_str()) {
                format!("{pattern} path={path}")
            } else {
                pattern.to_string()
            }
        }
        "Edit" | "Write" => input
            .get("file_path")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_default(),
        _ => truncate_value(input, max),
    }
}

/// Truncates a JSON value's string form to `max` bytes on a UTF-8 boundary.
fn truncate_value(value: &serde_json::Value, max: usize) -> String {
    let s = if value.is_string() {
        value.as_str().unwrap_or("").to_string()
    } else {
        value.to_string()
    };
    truncate_str(&s, max)
}

/// Truncates a string to approximately `max` bytes on a char boundary.
fn truncate_str(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let idx = s.floor_char_boundary(max);
    let mut out = s[..idx].to_string();
    out.push_str("...");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_text_delta() {
        let mut collected = String::new();
        let events = parse_claude_line(
            r#"{"type":"content_block_delta","delta":{"text":"hello"}}"#,
            &mut collected,
            &mut HashMap::new(),
        );
        assert_eq!(collected, "hello");
        assert!(matches!(events[0], AgentEvent::TextDelta { .. }));
    }

    #[test]
    fn parses_thinking_delta_as_thinking_event() {
        let mut collected = String::new();
        let events = parse_claude_line(
            r#"{"type":"content_block_delta","delta":{"thinking":"reasoning here"}}"#,
            &mut collected,
            &mut HashMap::new(),
        );
        // Thinking must NOT be collected into assistant output.
        assert!(collected.is_empty());
        assert!(matches!(events[0], AgentEvent::Thinking { .. }));
        if let AgentEvent::Thinking { text } = &events[0] {
            assert_eq!(text, "reasoning here");
        }
    }

    #[test]
    fn parses_tool_use() {
        let mut collected = String::new();
        let events = parse_claude_line(
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read","input":{"file_path":"src/lib.rs"}}]}}"#,
            &mut collected,
            &mut HashMap::new(),
        );
        assert!(matches!(events[0], AgentEvent::ToolCall { .. }));
        if let AgentEvent::ToolCall { tool, detail, .. } = &events[0] {
            assert_eq!(tool, "Read");
            assert_eq!(detail, "src/lib.rs");
        }
    }

    #[test]
    fn tool_result_recovers_the_tool_name_and_command_via_id_correlation() {
        // Regression: `tool_result` blocks only carry `tool_use_id`, not the
        // tool name or input — before correlation the tool name fell back to
        // the literal string "tool" and there was no way to show the command
        // that produced the output.
        let mut collected = String::new();
        let mut pending = HashMap::new();
        let _ = parse_claude_line(
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"echo hi"}}]}}"#,
            &mut collected,
            &mut pending,
        );
        let events = parse_claude_line(
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"hi\n"}]}}"#,
            &mut collected,
            &mut pending,
        );
        let result = events
            .iter()
            .find(|e| matches!(e, AgentEvent::ToolResult { .. }))
            .expect("a ToolResult event");
        let AgentEvent::ToolResult { tool, command, .. } = result else {
            unreachable!()
        };
        assert_eq!(tool, "Bash");
        assert_eq!(command.as_deref(), Some("echo hi"));
        assert!(
            pending.is_empty(),
            "the pending entry is consumed, not leaked"
        );
    }

    #[test]
    fn tool_result_without_a_matching_id_falls_back_gracefully() {
        let mut collected = String::new();
        let events = parse_claude_line(
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"unknown","content":"x"}]}}"#,
            &mut collected,
            &mut HashMap::new(),
        );
        let AgentEvent::ToolResult { tool, command, .. } = events
            .iter()
            .find(|e| matches!(e, AgentEvent::ToolResult { .. }))
            .expect("a ToolResult event")
        else {
            unreachable!()
        };
        assert_eq!(tool, "tool");
        assert!(command.is_none());
    }

    #[test]
    fn parses_usage_from_result() {
        let mut collected = String::new();
        let events = parse_claude_line(
            r#"{"type":"result","usage":{"input_tokens":10,"output_tokens":20}}"#,
            &mut collected,
            &mut HashMap::new(),
        );
        assert!(events.iter().any(|e| matches!(e, AgentEvent::Usage { .. })));
    }

    #[test]
    fn ignores_garbage_line() {
        let mut collected = String::new();
        let events = parse_claude_line("not json", &mut collected, &mut HashMap::new());
        assert!(events.is_empty());
        assert!(collected.is_empty());
    }

    #[test]
    fn user_frame_text_is_not_collected() {
        // A replayed user message — the prompt, which quotes the completion
        // token as an instruction — must NOT be collected into assistant
        // output. Otherwise `contains(complete_token)` matches the prompt on
        // every iteration and fires a false completion after two iterations.
        let mut collected = String::new();
        let events = parse_claude_line(
            r#"{"type":"user","message":{"content":[{"type":"text","text":"If done, output exactly: <promise>COMPLETE</promise>"}]}}"#,
            &mut collected,
            &mut HashMap::new(),
        );
        assert!(
            collected.is_empty(),
            "user text must not be collected, got: {collected}"
        );
        // It is still surfaced for display (the transcript shows steering/prompt
        // echoes); only the collection buffer is protected.
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::TextDelta { .. })),
            "user text should still emit a TextDelta for display"
        );
    }

    #[test]
    fn assistant_frame_text_is_collected() {
        // Genuine assistant output — including the completion token when the
        // agent actually emits it — IS collected so completion detection works.
        let mut collected = String::new();
        let _ = parse_claude_line(
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"<promise>COMPLETE</promise>"}]}}"#,
            &mut collected,
            &mut HashMap::new(),
        );
        assert_eq!(collected, "<promise>COMPLETE</promise>");
    }

    #[test]
    fn truncate_str_respects_char_boundary() {
        let s = "héllo world 😀👏"; // mixed multibyte
        let t = truncate_str(s, 5);
        assert!(t.is_char_boundary(t.len()));
    }

    #[test]
    fn frame_is_result_detects_end_turn() {
        // The terminal `result` frame — what the streaming path closes stdin on.
        assert!(frame_is_result(
            r#"{"type":"result","subtype":"success","is_error":false}"#
        ));
        // Other frame types must NOT trigger the close.
        assert!(!frame_is_result(r#"{"type":"assistant","message":{}}"#));
        assert!(!frame_is_result(r#"{"type":"user","message":{}}"#));
        assert!(!frame_is_result(
            r#"{"type":"content_block_delta","delta":{}}"#
        ));
        // Non-JSON / partial lines are not results (and must not panic).
        assert!(!frame_is_result("not json"));
        assert!(!frame_is_result(""));
    }
}

#[cfg(test)]
mod adapter_tests {
    use super::*;
    use std::time::Duration;

    #[cfg(unix)]
    #[test]
    fn map_outcome_classifies_overload() {
        use crate::execution_service::output::{CaptureBuilder, ProcessOutput};
        use std::os::unix::process::ExitStatusExt;
        let mut so = CaptureBuilder::new(4096);
        so.push(b"API Error: 529 [1305][overloaded]\n");
        let status = ExitStatusExt::from_raw(1 << 8);
        let output = ProcessOutput {
            stdout: so.finish(),
            stderr: CaptureBuilder::new(4096).finish(),
            termination: Termination::Exited(status),
            duration: Duration::ZERO,
            pid: Some(1),
        };
        match map_outcome(output, String::new(), None, ProviderKind::Claude) {
            Err(AgentExecutionError::Provider { error, .. }) => {
                assert_eq!(
                    error.kind(),
                    crate::execution_service::error::ProviderErrorKind::Overloaded
                );
            }
            other => panic!("expected Provider overload, got: {other:?}"),
        }
    }

    /// A streaming subprocess that loiters past the grace (the shape of Claude
    /// Code lingering after `end_turn`, ignoring stdin EOF) is force-finalised:
    /// success from the collected output, promptly, with no hang.
    #[cfg(unix)]
    #[tokio::test]
    async fn finalize_streaming_force_finalises_a_loitering_process() {
        use crate::execution_service::supervisor::spawn;

        let spec = ProcessSpec::builder("sh")
            .arg("-c")
            .arg("sleep 30")
            .input(ProcessInput::Streaming {
                initial: Bytes::from_static(b""),
            })
            .timeouts(ProcessTimeouts {
                total: Some(Duration::from_secs(30)),
                termination_grace: Duration::from_secs(2),
                ..Default::default()
            })
            .build();
        let cancel = CancellationToken::new();
        let proc = spawn(spec, cancel).await.expect("spawn");

        let started = std::time::Instant::now();
        let result = finalize_streaming(
            proc,
            "captured output".to_string(),
            None,
            Duration::from_millis(50),
        )
        .await;
        let elapsed = started.elapsed();

        assert!(
            elapsed < Duration::from_secs(2),
            "force-finalise should be prompt, took {elapsed:?}"
        );
        match result {
            Ok(run) => assert_eq!(run.stdout, "captured output"),
            other => panic!("expected Ok from loitering process, got {other:?}"),
        }
    }

    /// A streaming subprocess that exits within the grace yields its real
    /// (successful) exit status — the common path when the provider does exit.
    #[cfg(unix)]
    #[tokio::test]
    async fn finalize_streaming_uses_real_exit_when_process_exits() {
        use crate::execution_service::supervisor::spawn;

        let spec = ProcessSpec::builder("sh")
            .arg("-c")
            .arg("true")
            .input(ProcessInput::Streaming {
                initial: Bytes::from_static(b""),
            })
            .timeouts(ProcessTimeouts {
                total: Some(Duration::from_secs(10)),
                termination_grace: Duration::from_secs(2),
                ..Default::default()
            })
            .build();
        let cancel = CancellationToken::new();
        let proc = spawn(spec, cancel).await.expect("spawn");

        let result = finalize_streaming(
            proc,
            "captured output".to_string(),
            None,
            Duration::from_secs(5),
        )
        .await;

        match result {
            Ok(run) => assert_eq!(run.stdout, "captured output"),
            other => panic!("expected Ok from cleanly-exiting process, got {other:?}"),
        }
    }

    // ── File-edit capture ───────────────────────────────────────────────────

    #[test]
    fn detects_first_class_edit_tools() {
        assert!(is_file_edit_tool("Edit"));
        assert!(is_file_edit_tool("Write"));
        assert!(is_file_edit_tool("MultiEdit"));
        assert!(is_file_edit_tool("NotebookEdit"));
        assert!(!is_file_edit_tool("Read"));
        assert!(!is_file_edit_tool("Bash"));
        assert!(!is_file_edit_tool("Grep"));
        assert!(!is_file_edit_tool("todo"));
    }

    #[test]
    fn edit_target_path_reads_file_path_or_notebook_path() {
        let file_input = serde_json::json!({"file_path": "src/lib.rs"});
        assert_eq!(
            edit_target_path("Edit", &file_input).as_deref(),
            Some("src/lib.rs")
        );
        assert_eq!(
            edit_target_path("Write", &file_input).as_deref(),
            Some("src/lib.rs")
        );

        // NotebookEdit carries notebook_path instead.
        let nb_input = serde_json::json!({"notebook_path": "nb.ipynb"});
        assert_eq!(
            edit_target_path("NotebookEdit", &nb_input).as_deref(),
            Some("nb.ipynb")
        );

        // Non-edit tools never report a target even with a file_path.
        let read_input = serde_json::json!({"file_path": "src/lib.rs"});
        assert!(edit_target_path("Read", &read_input).is_none());

        // Empty path is ignored.
        let empty = serde_json::json!({"file_path": ""});
        assert!(edit_target_path("Edit", &empty).is_none());
    }

    #[tokio::test]
    async fn build_edit_reports_real_modified_diff() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cwd = dir.path();
        let path = "src/lib.rs";
        let file = cwd.join(path);
        std::fs::create_dir_all(file.parent().expect("parent")).expect("mkdir");
        std::fs::write(&file, "fn main() {}\n").expect("write pre");

        // Pre-edit snapshot captured the original contents.
        let pre = read_file_state(cwd, path).await;
        // The agent then edits the file on disk.
        std::fs::write(&file, "fn main() { todo!() }\n").expect("write post");

        let edit = build_edit(cwd, path, pre)
            .await
            .expect("a real change produces an edit");
        assert_eq!(edit.path, path);
        assert!(matches!(
            edit.kind,
            crate::file_edit::FileEditKind::Modified
        ));
        assert!(!edit.hunks.is_empty());
    }

    #[tokio::test]
    async fn build_edit_reports_creation_and_deletion() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cwd = dir.path();
        let path = "new.txt";

        // File did not exist before, exists after → created.
        let pre = ReadState::Missing;
        std::fs::write(cwd.join(path), "hello\n").expect("write");
        let created = build_edit(cwd, path, pre).await.expect("created edit");
        assert!(matches!(
            created.kind,
            crate::file_edit::FileEditKind::Created
        ));

        // File existed before, gone after → deleted.
        std::fs::remove_file(cwd.join(path)).expect("remove");
        let deleted = build_edit(cwd, path, ReadState::Bytes(b"hello\n".to_vec()))
            .await
            .expect("deleted edit");
        assert!(matches!(
            deleted.kind,
            crate::file_edit::FileEditKind::Deleted
        ));
    }

    #[tokio::test]
    async fn build_edit_emits_nothing_for_no_effective_change() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cwd = dir.path();
        let path = "same.txt";
        std::fs::write(cwd.join(path), "unchanged\n").expect("write");
        let pre = ReadState::Bytes(b"unchanged\n".to_vec());
        // File unchanged → no edit.
        assert!(build_edit(cwd, path, pre).await.is_none());
    }

    #[tokio::test]
    async fn build_edit_reports_capture_failure_when_post_read_fails() {
        // Pre existed but post read fails (unreadable): surface a capture-failed
        // entry rather than silently presenting a successful-looking edit.
        let dir = tempfile::tempdir().expect("tempdir");
        let cwd = dir.path();
        let path = "guarded.txt";
        let edit = build_edit(cwd, path, ReadState::Unreadable("denied".to_string()))
            .await
            .expect("capture failure is reported");
        assert!(matches!(
            edit.kind,
            crate::file_edit::FileEditKind::CaptureFailed { .. }
        ));
        assert!(edit.hunks.is_empty());
    }
}
