//! Codex subprocess adapter (`codex exec --json`).
//!
//! Drives the supervisor, owns JSONL framing, emits [`crate::agent::AgentEvent`]s,
//! and classifies via the shared [`super::claude::map_outcome`].

use async_trait::async_trait;
use bytes::Bytes;
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;

use crate::agent::{AgentEvent, AgentRunResult};
use crate::config::CodexConfig;
use crate::execution_service::adapter::{AgentAdapter, AgentEventSink, LineDecoder};
use crate::execution_service::adapters::claude::map_outcome;
use crate::execution_service::classify::ProviderKind;
use crate::execution_service::error::AgentExecutionError;
use crate::execution_service::supervisor::{
    ProcessInput, ProcessSpec, ProcessTimeouts, ProcessEvent, RunningProcess,
};

/// Maximum length of a single codex JSONL frame (line) before rejection.
const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

/// Parameters for one Codex invocation.
#[derive(Debug, Clone)]
pub struct CodexParams {
    /// Binary to invoke (e.g. `codex`).
    pub binary: String,
    /// The rendered prompt delivered over stdin.
    pub prompt: Bytes,
    /// Working directory.
    pub working_directory: PathBuf,
    /// Effective codex configuration (already had `with_max_permissions` applied).
    pub config: CodexConfig,
    /// Image attachment paths.
    pub images: Vec<PathBuf>,
    /// Deadlines for this attempt.
    pub timeouts: ProcessTimeouts,
    /// Cancellation token for this attempt.
    pub cancellation: CancellationToken,
}

impl CodexParams {
    /// Creates parameters with default config.
    #[must_use]
    pub fn new(binary: impl Into<String>, prompt: Bytes, working_directory: PathBuf) -> Self {
        Self {
            binary: binary.into(),
            prompt,
            working_directory,
            config: CodexConfig::default(),
            images: Vec::new(),
            timeouts: ProcessTimeouts::default(),
            cancellation: CancellationToken::new(),
        }
    }
}

/// Codex subprocess adapter.
pub struct CodexSubprocessAdapter {
    params: CodexParams,
}

impl CodexSubprocessAdapter {
    /// Creates an adapter for the given parameters.
    #[must_use]
    pub fn new(params: CodexParams) -> Self {
        Self { params }
    }

    fn build_spec(&self) -> ProcessSpec {
        let cfg = self.params.config.with_max_permissions();
        let mut builder = ProcessSpec::builder(&self.params.binary)
            .arg("exec")
            .arg("--json")
            .arg("-C")
            .arg(self.params.working_directory.clone())
            .cwd(self.params.working_directory.clone())
            .input(ProcessInput::Bytes(self.params.prompt.clone()))
            .timeouts(self.params.timeouts.clone())
            .capture_bytes(64 * 1024);

        if cfg.full_auto {
            builder = builder.arg("--full-auto");
        }
        if !cfg.sandbox.trim().is_empty() {
            builder = builder.arg("--sandbox").arg(cfg.sandbox.trim());
        }
        if cfg.skip_git_repo_check {
            builder = builder.arg("--skip-git-repo-check");
        }
        if cfg.progress_cursor {
            builder = builder.arg("--progress-cursor");
        }
        if let Some(model) = cfg.model.as_ref().filter(|m| !m.trim().is_empty()) {
            builder = builder.arg("--model").arg(model);
        }
        if let Some(effort) = cfg.model_reasoning_effort {
            builder = builder
                .arg("-c")
                .arg(format!("model_reasoning_effort=\"{}\"", effort.as_str()));
        }
        if let Some(profile) = cfg.profile.as_ref().filter(|p| !p.trim().is_empty()) {
            builder = builder.arg("--profile").arg(profile);
        }
        for image in &self.params.images {
            builder = builder.arg("--image").arg(image);
        }
        builder.build()
    }
}

#[async_trait(?Send)]
impl AgentAdapter for CodexSubprocessAdapter {
    async fn execute(
        &mut self,
        sink: &mut dyn AgentEventSink,
    ) -> Result<AgentRunResult, AgentExecutionError> {
        let spec = self.build_spec();
        let process: RunningProcess =
            crate::execution_service::supervisor::spawn(spec, self.params.cancellation.clone())
                .await?;
        run_codex_stream(process, sink).await
    }
}

async fn run_codex_stream(
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
                    for agent_event in parse_codex_line(&text, &mut collected) {
                        if structured_error.is_none() {
                            if let AgentEvent::Error { message } = &agent_event {
                                structured_error = Some(message.clone());
                            }
                        }
                        sink.emit(agent_event).await.map_err(|_| AgentExecutionError::Cancelled)?;
                    }
                }
            }
            ProcessEvent::Stderr(_) | ProcessEvent::StdinWritten | ProcessEvent::Exited => {}
        }
    }

    if let Some(remainder) = decoder.flush_remainder()? {
        let text = String::from_utf8_lossy(&remainder);
        for agent_event in parse_codex_line(&text, &mut collected) {
            sink.emit(agent_event).await.map_err(|_| AgentExecutionError::Cancelled)?;
        }
    }

    let output = process.complete().await?;
    map_outcome(output, collected, structured_error, ProviderKind::Codex)
}

/// Parses one Codex JSONL line into zero or more [`AgentEvent`]s.
fn parse_codex_line(line: &str, collected: &mut String) -> Vec<AgentEvent> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return Vec::new();
    };
    let mut events = Vec::new();
    let event_type = value.get("type").and_then(|v| v.as_str()).unwrap_or("");
    match event_type {
        "thread.started" => {
            if let Some(thread) = value.get("thread_id").and_then(|v| v.as_str()) {
                events.push(AgentEvent::Status {
                    message: format!("thread started: {thread}"),
                });
            }
        }
        "turn.started" => {
            events.push(AgentEvent::Status {
                message: "turn started".to_string(),
            });
        }
        "turn.completed" => {
            if let Some(usage) = value.get("usage") {
                events.push(AgentEvent::Usage {
                    input_tokens: usage.get("input_tokens").and_then(|v| v.as_u64()),
                    output_tokens: usage.get("output_tokens").and_then(|v| v.as_u64()),
                    cached_input_tokens: usage.get("cached_input_tokens").and_then(|v| v.as_u64()),
                });
            }
            events.push(AgentEvent::Status {
                message: "turn completed".to_string(),
            });
        }
        "item.started" => {
            if let Some(item) = value.get("item") {
                if item.get("type").and_then(|t| t.as_str()) == Some("command_execution") {
                    if let Some(cmd) = item.get("command").and_then(|c| c.as_str()) {
                        events.push(AgentEvent::ToolCall {
                            tool: "command_execution".to_string(),
                            detail: truncate(cmd, 120),
                        });
                    }
                }
            }
        }
        "item.completed" => {
            if let Some(item) = value.get("item") {
                let item_type = item.get("type").and_then(|t| t.as_str()).unwrap_or("");
                if item_type == "agent_message" {
                    if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                        collected.push_str(text);
                        events.push(AgentEvent::TextDelta { text: text.to_string() });
                    }
                } else if item_type == "command_execution" {
                    let command = item
                        .get("command")
                        .and_then(|c| c.as_str())
                        .unwrap_or("command execution");
                    let output_preview = item
                        .get("aggregated_output")
                        .and_then(|o| o.as_str())
                        .map(|s| truncate(s, 80))
                        .unwrap_or_default();
                    let detail = if output_preview.is_empty() {
                        command.to_string()
                    } else {
                        format!("{command} => {output_preview}")
                    };
                    let success = item.get("exit_code").and_then(|c| c.as_i64()).map(|code| code == 0);
                    events.push(AgentEvent::ToolResult {
                        tool: "command_execution".to_string(),
                        detail,
                        success,
                    });
                }
            }
        }
        "error" => {
            let msg = value
                .get("message")
                .and_then(|v| v.as_str())
                .or_else(|| value.get("error").and_then(|v| v.as_str()))
                .unwrap_or("codex stream error")
                .to_string();
            events.push(AgentEvent::Error { message: msg });
        }
        _ => {}
    }
    events
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
    fn parses_agent_message() {
        let mut collected = String::new();
        let events = parse_codex_line(
            r#"{"type":"item.completed","item":{"type":"agent_message","text":"hello"}}"#,
            &mut collected,
        );
        assert_eq!(collected, "hello");
        assert!(events.iter().any(|e| matches!(e, AgentEvent::TextDelta { .. })));
    }

    #[test]
    fn parses_error_event() {
        let mut collected = String::new();
        let events = parse_codex_line(
            r#"{"type":"error","message":"rate limited"}"#,
            &mut collected,
        );
        assert!(events.iter().any(|e| matches!(e, AgentEvent::Error { .. })));
    }

    #[test]
    fn parses_usage() {
        let mut collected = String::new();
        let events = parse_codex_line(
            r#"{"type":"turn.completed","usage":{"input_tokens":5,"output_tokens":7}}"#,
            &mut collected,
        );
        assert!(events.iter().any(|e| matches!(e, AgentEvent::Usage { .. })));
    }

    #[test]
    fn ignores_garbage() {
        let mut collected = String::new();
        assert!(parse_codex_line("nope", &mut collected).is_empty());
    }
}
