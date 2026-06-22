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
use crate::execution_service::error::{AgentExecutionError, ProcessError, UnsuccessfulExit};
use crate::execution_service::output::Termination;
use crate::execution_service::supervisor::{
    ProcessEvent, ProcessInput, ProcessSpec, ProcessTimeouts, RunningProcess,
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

    /// Builds the process specification for the supervisor.
    fn build_spec(&self) -> ProcessSpec {
        let mut builder = ProcessSpec::builder(&self.params.binary)
            .arg("--permission-mode")
            .arg(self.params.permission_mode.clone())
            .arg("--dangerously-skip-permissions")
            .arg("-p")
            .arg("--verbose")
            .arg("--input-format")
            .arg("text")
            .arg("--output-format")
            .arg("stream-json")
            .arg("--include-partial-messages")
            .cwd(self.params.working_directory.clone())
            .input(ProcessInput::Bytes(self.params.prompt.clone()))
            .timeouts(self.params.timeouts.clone())
            .capture_bytes(64 * 1024);

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
}

#[async_trait(?Send)]
impl AgentAdapter for ClaudeSubprocessAdapter {
    async fn execute(
        &mut self,
        sink: &mut dyn AgentEventSink,
    ) -> Result<AgentRunResult, AgentExecutionError> {
        let spec = self.build_spec();
        let process: RunningProcess =
            crate::execution_service::supervisor::spawn(spec, self.params.cancellation.clone())
                .await?;
        run_claude_stream(process, sink).await
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
                        if matches!(agent_event, AgentEvent::Error { .. })
                            && structured_error.is_none()
                        {
                            if let AgentEvent::Error { message } = &agent_event {
                                structured_error = Some(message.clone());
                            }
                        }
                        emit_event(sink, agent_event).await?;
                    }
                }
            }
            ProcessEvent::Stderr(_) => {
                // Captured by the supervisor for classification; not streamed as
                // AgentEvents (stderr is diagnostic, not protocol output).
            }
            ProcessEvent::StdinWritten | ProcessEvent::Exited => {}
        }
    }

    // Flush any trailing frame without a newline.
    if let Some(remainder) = decoder.flush_remainder()? {
        let text = String::from_utf8_lossy(&remainder);
        for agent_event in parse_claude_line(&text, &mut collected) {
            if matches!(agent_event, AgentEvent::Error { .. }) && structured_error.is_none() {
                if let AgentEvent::Error { message } = &agent_event {
                    structured_error = Some(message.clone());
                }
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
            Err(AgentExecutionError::UnsuccessfulExit(UnsuccessfulExit {
                code: status.code(),
                stdout: output.stdout,
                stderr: output.stderr,
            }))
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
            if let Some(text) = value
                .get("delta")
                .and_then(|d| d.get("text"))
                .and_then(|t| t.as_str())
            {
                if !text.is_empty() {
                    collected.push_str(text);
                    events.push(AgentEvent::TextDelta {
                        text: text.to_string(),
                    });
                }
            }
        }
        "content_block_start" => {
            if let Some(text) = value
                .get("content_block")
                .and_then(|b| b.get("text"))
                .and_then(|t| t.as_str())
            {
                if !text.is_empty() {
                    collected.push_str(text);
                    events.push(AgentEvent::TextDelta {
                        text: text.to_string(),
                    });
                }
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
                            if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                                if !text.is_empty() {
                                    collected.push_str(text);
                                    events.push(AgentEvent::TextDelta {
                                        text: text.to_string(),
                                    });
                                }
                            }
                        }
                        "tool_use" => {
                            let name = item
                                .get("name")
                                .and_then(|n| n.as_str())
                                .unwrap_or("tool")
                                .to_string();
                            let detail = summarize_tool_input(&name, item.get("input"));
                            events.push(AgentEvent::ToolCall { tool: name, detail });
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
    fn parses_tool_use() {
        let mut collected = String::new();
        let events = parse_claude_line(
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read","input":{"file_path":"src/lib.rs"}}]}}"#,
            &mut collected,
        );
        assert!(matches!(events[0], AgentEvent::ToolCall { .. }));
        if let AgentEvent::ToolCall { tool, detail } = &events[0] {
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
