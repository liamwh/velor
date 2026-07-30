//! Oh My Pi (`omp --mode rpc`) subprocess adapter.
//!
//! `omp` speaks a newline-delimited JSON RPC over stdio: the client sends a
//! `prompt` command on stdin and consumes a stream of events on stdout. Like
//! Claude Code's `--print` stream-json mode, `omp --mode rpc` does **not** exit
//! when a turn completes — it keeps stdin open waiting for more commands — so
//! Velor must close stdin (EOF) itself once the terminal `agent_end` event
//! arrives. This mirrors the Claude streaming path.
//!
//! Protocol (verified against `omp` v17.1.5): Velor writes
//! `{"id":"vel","type":"prompt","message":"<prompt>"}\n` as the streaming
//! initial frame, then reads events until `agent_end`, e.g.
//!
//! ```text
//! {"type":"ready","protocolVersion":1,...}          // handshake (ignored)
//! {"id":"vel","type":"response","command":"prompt","success":true}  // ack
//! {"type":"agent_start"}
//! {"type":"turn_start"}
//! {"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":"Hi"}}
//! {"type":"tool_execution_end","toolName":"bash","result":{...},"isError":false}
//! {"type":"turn_end","message":{"usage":{"input":..,"output":..,"cacheRead":..}}}
//! {"type":"agent_end","messages":[...]}             // terminal → close stdin
//! ```

use async_trait::async_trait;
use bytes::Bytes;
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;

use crate::agent::{AgentEvent, AgentRunResult};
use crate::config::OmpConfig;
use crate::execution_service::adapter::{AgentAdapter, AgentEventSink, LineDecoder};
use crate::execution_service::adapters::claude::map_outcome;
use crate::execution_service::classify::ProviderKind;
use crate::execution_service::error::AgentExecutionError;
use crate::execution_service::supervisor::{
    ProcessEvent, ProcessInput, ProcessInputCommand, ProcessSpec, ProcessTimeouts, RunningProcess,
};

/// Maximum length of a single omp JSONL frame (line) before rejection. omp
/// itself advertises a 1 MiB per-frame cap and a 64 MiB reassembly cap.
const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

/// Grace window after `agent_end` + EOF for `omp` to exit naturally before the
/// supervisor force-cancels the group. `omp` normally exits in well under a
/// second; this only guards a linger.
const STREAM_EXIT_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

/// Correlation id Velor uses for its single `prompt` command. omp echoes it on
/// the matching `response` ack.
const PROMPT_REQUEST_ID: &str = "vel";

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
            extra_args: Vec::new(),
            extra_env: Vec::new(),
            timeouts: ProcessTimeouts::default(),
            cancellation: CancellationToken::new(),
        }
    }

    /// Frames the rendered prompt as the RPC `prompt` command (the streaming
    /// initial frame). The trailing newline is required: omp is
    /// newline-delimited JSON and the supervisor writes the bytes verbatim.
    fn frame_prompt_command(&self) -> Result<Bytes, AgentExecutionError> {
        let message = std::str::from_utf8(&self.prompt).map_err(|_| {
            AgentExecutionError::LiveSteeringUnavailable {
                reason: crate::execution_service::error::LiveSteeringUnavailableReason::ProtocolRejected,
            }
        })?;
        let frame = serde_json::json!({
            "id": PROMPT_REQUEST_ID,
            "type": "prompt",
            "message": message,
        });
        let mut bytes = serde_json::to_vec(&frame)
            .map_err(|_| AgentExecutionError::LiveSteeringUnavailable {
                reason: crate::execution_service::error::LiveSteeringUnavailableReason::ProtocolRejected,
            })?;
        bytes.push(b'\n');
        Ok(Bytes::from(bytes))
    }
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
            // Headless/ephemeral: keep the run out of ~/.omp/agent/sessions/.
            // Velor drives each iteration independently and never resumes an omp
            // session, so persisting one would only accumulate clutter.
            .arg("--no-session")
            .cwd(self.params.working_directory.clone())
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
        _live_input: Option<tokio::sync::mpsc::Receiver<crate::agent::AgentInput>>,
    ) -> Result<AgentRunResult, AgentExecutionError> {
        let _ = _live_input; // omp does not support live steering.
        let initial = self.params.frame_prompt_command()?;
        let spec = self.build_spec(initial);
        let process: RunningProcess =
            crate::execution_service::supervisor::spawn(spec, self.params.cancellation.clone())
                .await?;
        run_omp_stream(process, sink).await
    }
}

/// Drives a running omp subprocess: decodes stdout frames, emits events, and
/// closes stdin (EOF) once `agent_end` arrives so the process exits cleanly.
async fn run_omp_stream(
    mut process: RunningProcess,
    sink: &mut dyn AgentEventSink,
) -> Result<AgentRunResult, AgentExecutionError> {
    let mut decoder = LineDecoder::new(MAX_FRAME_BYTES);
    let mut collected = String::new();
    let mut structured_error: Option<String> = None;
    let mut close_sent = false;

    while let Some(event) = process.next_event().await {
        match event {
            ProcessEvent::Stdout(chunk) => {
                let lines = decoder.push(&chunk.bytes)?;
                let mut saw_agent_end = false;
                for line in lines {
                    let text = String::from_utf8_lossy(&line);
                    if frame_is_agent_end(&text) {
                        saw_agent_end = true;
                    }
                    for agent_event in parse_omp_line(&text, &mut collected) {
                        if structured_error.is_none()
                            && let AgentEvent::Error { message } = &agent_event
                        {
                            structured_error = Some(message.clone());
                        }
                        sink.emit(agent_event)
                            .await
                            .map_err(|_| AgentExecutionError::Cancelled)?;
                    }
                }
                // The prompt is fully handled: send EOF so omp exits, then stop
                // reading and finalise. omp exits promptly on this EOF.
                if saw_agent_end
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
        for agent_event in parse_omp_line(&text, &mut collected) {
            sink.emit(agent_event)
                .await
                .map_err(|_| AgentExecutionError::Cancelled)?;
        }
    }

    if close_sent {
        return finalize_streaming(process, collected, structured_error).await;
    }
    // The process exited before `agent_end` (crash, --max-time, or error).
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

/// Cheap check: is this line the terminal `agent_end` frame?
fn frame_is_agent_end(line: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
        return false;
    };
    value.get("type").and_then(|v| v.as_str()) == Some("agent_end")
}

/// Parses one omp JSONL line into zero or more [`AgentEvent`]s, appending
/// streamed assistant text to `collected`.
fn parse_omp_line(line: &str, collected: &mut String) -> Vec<AgentEvent> {
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
                .map(|s| truncate(s, 240))
                .unwrap_or_default();
            events.push(AgentEvent::ToolResult {
                tool,
                detail,
                success: Some(!is_error),
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
    for key in ["command", "path", "file_path", "pattern", "query", "url"] {
        if let Some(s) = args.get(key).and_then(|v| v.as_str()) {
            return truncate(s, 200);
        }
    }
    truncate(&args.to_string(), 200)
}

/// Truncates a string to approximately `max` bytes on a char boundary.
fn truncate(s: &str, max: usize) -> String {
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
    fn parses_text_delta_and_collects() {
        let mut collected = String::new();
        let events = parse_omp_line(
            r#"{"type":"message_update","assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"BANANA"}}"#,
            &mut collected,
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
        );
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::ToolResult { tool, success, .. }
            if tool.as_str() == "bash" && *success == Some(true)
        )));
    }

    #[test]
    fn parses_usage_from_turn_end() {
        let mut collected = String::new();
        let events = parse_omp_line(
            r#"{"type":"turn_end","message":{"usage":{"input":10,"output":20,"cacheRead":5}}}"#,
            &mut collected,
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
        );
        assert_eq!(collected, "answer");
    }

    #[test]
    fn frame_is_agent_end_detects_terminal() {
        assert!(frame_is_agent_end(r#"{"type":"agent_end","messages":[]}"#));
        assert!(!frame_is_agent_end(r#"{"type":"turn_end"}"#));
        assert!(!frame_is_agent_end("not json"));
        assert!(!frame_is_agent_end(""));
    }

    #[test]
    fn ignores_noise_and_garbage() {
        let mut collected = String::new();
        assert!(
            parse_omp_line(
                r#"{"type":"available_commands_update","commands":[]}"#,
                &mut collected
            )
            .is_empty()
        );
        assert!(parse_omp_line("nope", &mut collected).is_empty());
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
}
