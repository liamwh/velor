//! Claude Code subprocess adapter.
//!
//! Drives [`crate::execution_service::supervisor`] with the Claude
//! `stream-json` invocation, owns the UTF-8 → newline → stream-json framing,
//! emits [`crate::agent::AgentEvent`]s, and classifies the final output via
//! [`crate::execution_service::classify`].

use async_trait::async_trait;
use bytes::Bytes;
use std::path::PathBuf;
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

/// Maximum length of a single stream-json frame (line) before it is rejected.
const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

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

        // Spawn the steering-forwarding task only for the streaming path, and only
        // when both a writable command sender and a steering receiver exist.
        let forward_handle = if steering {
            match (process.input_sender(), live_input) {
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

        let result = run_claude_stream(process, sink).await;
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
async fn run_claude_stream(
    mut process: RunningProcess,
    sink: &mut dyn AgentEventSink,
) -> Result<AgentRunResult, AgentExecutionError> {
    let mut decoder = LineDecoder::new(MAX_FRAME_BYTES);
    let mut collected = String::new();
    let mut structured_error: Option<String> = None;

    while let Some(event) = process.next_event().await {
        match event {
            ProcessEvent::Stdout(chunk) => {
                let lines = decoder.push(&chunk.bytes)?;
                for line in lines {
                    let text = String::from_utf8_lossy(&line);
                    for agent_event in parse_claude_line(&text, &mut collected) {
                        if structured_error.is_none()
                            && let AgentEvent::Error { message } = &agent_event
                        {
                            structured_error = Some(message.clone());
                        }
                        emit_event(sink, agent_event).await?;
                    }
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
        for agent_event in parse_claude_line(&text, &mut collected) {
            if structured_error.is_none()
                && let AgentEvent::Error { message } = &agent_event
            {
                structured_error = Some(message.clone());
            }
            emit_event(sink, agent_event).await?;
        }
    }

    let output = process.complete().await?;
    map_outcome(output, collected, structured_error, ProviderKind::Claude)
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

/// Parses one Claude stream-json line into zero or more [`AgentEvent`]s and
/// appends assistant text to `collected`.
///
/// This is the canonical home for Claude stream parsing (the legacy copy in
/// `agent::process_stream_line` is removed once consumers migrate).
fn parse_claude_line(line: &str, collected: &mut String) -> Vec<AgentEvent> {
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
                                collected.push_str(text);
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
                            let detail = item
                                .get("content")
                                .map(|c| truncate_value(c, 200))
                                .unwrap_or_default();
                            events.push(AgentEvent::ToolResult {
                                tool: "tool".to_string(),
                                detail,
                                success: None,
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
        );
        assert!(matches!(events[0], AgentEvent::ToolCall { .. }));
        if let AgentEvent::ToolCall { tool, detail, .. } = &events[0] {
            assert_eq!(tool, "Read");
            assert_eq!(detail, "src/lib.rs");
        }
    }

    #[test]
    fn parses_usage_from_result() {
        let mut collected = String::new();
        let events = parse_claude_line(
            r#"{"type":"result","usage":{"input_tokens":10,"output_tokens":20}}"#,
            &mut collected,
        );
        assert!(events.iter().any(|e| matches!(e, AgentEvent::Usage { .. })));
    }

    #[test]
    fn ignores_garbage_line() {
        let mut collected = String::new();
        let events = parse_claude_line("not json", &mut collected);
        assert!(events.is_empty());
        assert!(collected.is_empty());
    }

    #[test]
    fn truncate_str_respects_char_boundary() {
        let s = "héllo world 😀👏"; // mixed multibyte
        let t = truncate_str(s, 5);
        assert!(t.is_char_boundary(t.len()));
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
}
