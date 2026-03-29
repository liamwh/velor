//! Agent runner interface and configuration.
//!
//! This module provides types and traits for running AI agents with
//! different providers and communication protocols.

use crate::acp;
use crate::config::{AcpConfig, AgentProvider, CodexConfig, Protocol};
use color_eyre::eyre::WrapErr;
use serde::Deserialize;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;

/// Maximum length for command display before truncating
const MAX_COMMAND_DISPLAY_LEN: usize = 60;

/// Truncates a string to approximately `max_bytes` bytes.
///
/// Uses `floor_char_boundary` to avoid cutting through multi-byte UTF-8 sequences.
/// The actual result may be slightly shorter than `max_bytes` to ensure valid UTF-8.
fn truncate_str(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let safe_idx = s.floor_char_boundary(max_bytes);
    &s[..safe_idx]
}

/// Stream event types from Claude's stream-json output.
#[derive(Deserialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
#[allow(dead_code)] // Fields are needed for deserialization but not all are used
enum StreamEvent {
    /// Assistant message containing content blocks (text, tool_use)
    Assistant { message: Message },
    /// User message containing tool results
    User { message: Message },
    /// System initialization events (session_id, tools, cwd, model)
    System(serde_json::Value),
    /// Final result events
    Result(serde_json::Value),
    /// Content block delta events (streaming text chunks)
    ContentBlockDelta { delta: ContentBlockDelta },
    /// Content block start events
    ContentBlockStart { content_block: ContentBlock },
    /// Other unhandled event types
    #[serde(other)]
    Unknown,
}

/// Message structure containing content blocks.
#[derive(Deserialize, Debug)]
struct Message {
    content: Vec<ContentBlock>,
}

/// Content block delta for streaming text.
#[derive(Deserialize, Debug)]
struct ContentBlockDelta {
    text: String,
}

/// Content blocks within messages.
#[derive(Deserialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
#[allow(dead_code)] // Some fields needed for deserialization but not used
enum ContentBlock {
    /// Plain text content
    Text { text: String },
    /// Tool invocation (Read, Bash, Edit, etc.)
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    /// Tool result output
    ToolResult {
        tool_use_id: String,
        content: serde_json::Value,
    },
    /// Other content types
    #[serde(other)]
    Unknown,
}

/// Represents a tool call that can be displayed to the user.
#[derive(Debug, Clone)]
struct ToolCall {
    name: String,
    args_display: String,
}

impl ToolCall {
    /// Formats the tool call for display.
    fn format_display(&self) -> String {
        format!("🔧 {}: {}", self.name, self.args_display)
    }
}

/// Attempts to extract a tool call from an assistant event.
fn extract_tool_call(event: &StreamEvent) -> Option<ToolCall> {
    match event {
        StreamEvent::Assistant { message } => {
            message.content.iter().find_map(|block| match block {
                ContentBlock::ToolUse { name, input, .. } => Some(format_tool_args(name, input)),
                _ => None,
            })
        }
        _ => None,
    }
}

/// Formats tool arguments for display based on the tool type.
fn format_tool_args(name: &str, input: &serde_json::Value) -> ToolCall {
    let args_display = match name {
        "Read" => input
            .get("file_path")
            .or(input.get("file_name"))
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string(),

        "Bash" => {
            let command = input.get("command").and_then(|v| v.as_str()).unwrap_or("");
            if command.len() > MAX_COMMAND_DISPLAY_LEN {
                format!("{}...", truncate_str(command, MAX_COMMAND_DISPLAY_LEN))
            } else {
                command.to_string()
            }
        }

        "Glob" => input
            .get("pattern")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string(),

        "Grep" => {
            let pattern = input.get("pattern").and_then(|v| v.as_str()).unwrap_or("?");
            if let Some(path) = input.get("path").and_then(|v| v.as_str()) {
                format!("{} path={}", pattern, path)
            } else {
                pattern.to_string()
            }
        }

        "Edit" => input
            .get("file_path")
            .and_then(|v| v.as_str())
            .map(|p| format!("{} (replace)", p))
            .unwrap_or_else(|| "? (replace)".to_string()),

        "Write" => input
            .get("file_path")
            .and_then(|v| v.as_str())
            .map(|p| format!("{} (new)", p))
            .unwrap_or_else(|| "? (new)".to_string()),

        _ => {
            // Generic handling for other tools - show the input
            let input_str = if input.is_string() {
                input.as_str().unwrap_or("").to_string()
            } else {
                input.to_string()
            };
            if input_str.len() > MAX_COMMAND_DISPLAY_LEN {
                format!("{}...", truncate_str(&input_str, MAX_COMMAND_DISPLAY_LEN))
            } else {
                input_str
            }
        }
    };

    ToolCall {
        name: name.to_string(),
        args_display,
    }
}

/// Result of running a Claude command.
#[derive(Debug)]
pub struct AgentRunResult {
    /// The standard output from the provider.
    pub stdout: String,
}

/// Backward-compatible alias for legacy callsites.
pub type ClaudeRunResult = AgentRunResult;

/// Structured streaming events emitted by provider runners.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentEvent {
    /// Lifecycle or status update from the provider.
    Status {
        /// Human-readable status text.
        message: String,
    },
    /// Incremental assistant text output.
    TextDelta {
        /// Text delta payload.
        text: String,
    },
    /// Tool/action execution started.
    ToolCall {
        /// Tool/action name.
        tool: String,
        /// Provider-formatted summary of the invocation.
        detail: String,
    },
    /// Tool/action execution completed.
    ToolResult {
        /// Tool/action name.
        tool: String,
        /// Provider-formatted result summary.
        detail: String,
        /// Whether the tool execution succeeded if known.
        success: Option<bool>,
    },
    /// Token usage update if available.
    Usage {
        /// Input token count.
        input_tokens: Option<u64>,
        /// Output token count.
        output_tokens: Option<u64>,
        /// Cached input token count.
        cached_input_tokens: Option<u64>,
    },
    /// Error event emitted by provider stream.
    Error {
        /// Error detail.
        message: String,
    },
}

/// Event type emitted by `codex exec --json`.
#[derive(Deserialize, Debug)]
#[serde(tag = "type")]
enum CodexEvent {
    /// Thread started.
    #[serde(rename = "thread.started")]
    ThreadStarted { thread_id: String },
    /// Turn started.
    #[serde(rename = "turn.started")]
    TurnStarted,
    /// Item started.
    #[serde(rename = "item.started")]
    ItemStarted { item: CodexItem },
    /// Item completed.
    #[serde(rename = "item.completed")]
    ItemCompleted { item: CodexItem },
    /// Turn completed with usage.
    #[serde(rename = "turn.completed")]
    TurnCompleted { usage: Option<CodexUsage> },
    /// Stream-level error event.
    #[serde(rename = "error")]
    Error {
        message: Option<String>,
        error: Option<String>,
    },
    /// Unknown/unhandled codex stream event.
    #[serde(other)]
    Unknown,
}

/// Item payload for Codex events.
#[derive(Deserialize, Debug)]
struct CodexItem {
    #[serde(rename = "type")]
    item_type: String,
    text: Option<String>,
    command: Option<String>,
    aggregated_output: Option<String>,
    exit_code: Option<i32>,
}

/// Usage payload for Codex events.
#[derive(Deserialize, Debug)]
struct CodexUsage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cached_input_tokens: Option<u64>,
}

/// Agent runner abstraction across supported providers.
#[derive(Debug, Clone)]
pub enum AgentRunner {
    /// Claude subprocess with stream-json.
    ClaudeSubprocess,
    /// Claude ACP (Agent Client Protocol) via stdio.
    ClaudeAcp(AcpConfig),
    /// Codex CLI via `codex exec --json`.
    Codex(CodexConfig),
}

impl AgentRunner {
    /// Creates a new runner from provider + protocol configuration.
    ///
    /// # Arguments
    ///
    /// * `provider` - Provider implementation selector
    /// * `protocol` - The communication protocol to use
    /// * `acp_config` - ACP configuration (only used for Claude ACP)
    /// * `codex_config` - Codex configuration (only used for Codex provider)
    #[must_use]
    pub fn from_config(
        provider: AgentProvider,
        protocol: Protocol,
        acp_config: AcpConfig,
        codex_config: CodexConfig,
    ) -> Self {
        match provider {
            AgentProvider::Codex => Self::Codex(codex_config),
            AgentProvider::Claude => match protocol {
                Protocol::Subprocess => Self::ClaudeSubprocess,
                Protocol::Acp => Self::ClaudeAcp(acp_config),
            },
        }
    }

    /// Returns `true` if this is an ACP runner.
    #[must_use]
    #[allow(dead_code)]
    pub const fn is_acp(&self) -> bool {
        matches!(self, Self::ClaudeAcp(_))
    }

    /// Returns `true` if this is a subprocess runner.
    #[must_use]
    #[allow(dead_code)]
    pub const fn is_subprocess(&self) -> bool {
        matches!(self, Self::ClaudeSubprocess)
    }

    /// Returns `true` if this is a Codex runner.
    #[must_use]
    #[allow(dead_code)]
    pub const fn is_codex(&self) -> bool {
        matches!(self, Self::Codex(_))
    }

    /// Runs the agent with the given parameters.
    ///
    /// This is an async method that dispatches to the appropriate implementation
    /// based on the runner variant. For subprocess mode, the synchronous call
    /// is wrapped in `spawn_blocking` to avoid blocking the async runtime.
    ///
    /// # Arguments
    ///
    /// * `binary` - Path to the agent binary (e.g., "claude-glm" or "claude-agent-acp")
    /// * `permission_mode` - Permission mode for subprocess mode (e.g., "acceptEdits")
    /// * `prompt` - The rendered prompt text to send
    /// * `prompt_name` - Name of the prompt for logging
    /// * `cwd` - Current working directory
    ///
    /// # Errors
    ///
    /// Returns an error if the agent cannot be executed, fails, or returns non-zero exit code.
    #[tracing::instrument(level = "debug", fields(binary = %binary, prompt_name = %prompt_name, runner = ?self), ret, err)]
    pub async fn run(
        &self,
        binary: &str,
        permission_mode: &str,
        prompt: &str,
        prompt_name: &str,
        cwd: &Path,
    ) -> color_eyre::eyre::Result<ClaudeRunResult> {
        match self {
            Self::ClaudeSubprocess => {
                // Wrap sync subprocess call in spawn_blocking to avoid blocking async runtime
                let binary = binary.to_string();
                let permission_mode = permission_mode.to_string();
                let prompt = prompt.to_string();
                let prompt_name = prompt_name.to_string();

                tokio::task::spawn_blocking(move || {
                    run_claude(&binary, &permission_mode, &prompt, &prompt_name)
                })
                .await
                .wrap_err("subprocess task failed")?
            }
            Self::ClaudeAcp(config) => {
                // ACP mode is natively async
                tracing::info!("AgentRunner::run: entering ACP mode with binary {}", binary);
                let acp_result = acp::run_acp(binary, prompt, prompt_name, config, cwd).await?;
                tracing::info!("AgentRunner::run: ACP run completed");

                // Convert AcpRunResult to ClaudeRunResult for compatibility
                Ok(ClaudeRunResult {
                    stdout: acp_result.stdout,
                })
            }
            Self::Codex(config) => {
                let binary = binary.to_string();
                let prompt = prompt.to_string();
                let prompt_name = prompt_name.to_string();
                let config = config.clone();
                let cwd = cwd.to_path_buf();
                tokio::task::spawn_blocking(move || {
                    run_codex(&binary, &prompt, &prompt_name, &cwd, &config, &[])
                })
                .await
                .wrap_err("codex task failed")?
            }
        }
    }

    /// Runs the agent and emits structured events as they arrive.
    ///
    /// This method is intended for integrations (GUI/server) that need rich
    /// streaming updates.
    ///
    /// # Errors
    ///
    /// Returns an error if provider execution fails.
    #[allow(clippy::too_many_arguments)]
    #[tracing::instrument(level = "debug", skip(on_event, images), fields(binary = %binary, prompt_name = %prompt_name, runner = ?self), ret, err)]
    pub async fn run_with_events<F>(
        &self,
        binary: &str,
        permission_mode: &str,
        prompt: &str,
        prompt_name: &str,
        cwd: &Path,
        images: &[PathBuf],
        mut on_event: F,
    ) -> color_eyre::eyre::Result<AgentRunResult>
    where
        F: FnMut(AgentEvent) + Send,
    {
        match self {
            Self::Codex(config) => {
                let binary = binary.to_string();
                let prompt = prompt.to_string();
                let prompt_name = prompt_name.to_string();
                let cwd = cwd.to_path_buf();
                let images = images.to_vec();
                let config = config.clone();
                let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();

                let mut task = Box::pin(tokio::task::spawn_blocking(move || {
                    run_codex_with_events(
                        &binary,
                        &prompt,
                        &prompt_name,
                        &cwd,
                        &config,
                        &images,
                        |event| {
                            let _ = tx.send(event);
                        },
                    )
                }));

                loop {
                    tokio::select! {
                        maybe_event = rx.recv() => {
                            if let Some(event) = maybe_event {
                                on_event(event);
                            }
                        }
                        result = &mut task => {
                            let result = result.wrap_err("codex task failed")??;
                            while let Some(event) = rx.recv().await {
                                on_event(event);
                            }
                            return Ok(result);
                        }
                    }
                }
            }
            Self::ClaudeSubprocess => {
                on_event(AgentEvent::Status {
                    message: "running claude subprocess".to_string(),
                });
                let result = self
                    .run(binary, permission_mode, prompt, prompt_name, cwd)
                    .await?;
                if !result.stdout.is_empty() {
                    on_event(AgentEvent::TextDelta {
                        text: result.stdout.clone(),
                    });
                }
                Ok(result)
            }
            Self::ClaudeAcp(config) => {
                on_event(AgentEvent::Status {
                    message: "running claude via acp".to_string(),
                });
                let acp_result = acp::run_acp(binary, prompt, prompt_name, config, cwd).await?;
                if !acp_result.stdout.is_empty() {
                    on_event(AgentEvent::TextDelta {
                        text: acp_result.stdout.clone(),
                    });
                }
                Ok(AgentRunResult {
                    stdout: acp_result.stdout,
                })
            }
        }
    }
}

/// Verifies that the configured agent binary is available on PATH.
///
/// # Errors
///
/// Returns an error if the binary is not found or cannot be executed.
#[tracing::instrument(level = "debug", ret)]
pub fn require_agent_on_path(binary: &str) -> color_eyre::eyre::Result<()> {
    let output = Command::new(binary).arg("--version").output();

    match &output {
        Ok(output) if output.status.success() => {
            let version = String::from_utf8_lossy(&output.stdout);
            tracing::info!("{binary} found: {version}");
            Ok(())
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(color_eyre::eyre::eyre!(
                "{binary} --version failed with status {}: {}",
                output.status,
                stderr.trim()
            ))
        }
        Err(e) => Err(color_eyre::eyre::eyre!(
            "{binary} not found on PATH (or not runnable): {e}\n\nHINT: Ensure {binary} is installed and accessible. Try:\n  1. Run 'which {binary}' to check if it's on PATH\n  2. Check your config file for the 'binary' setting\n  3. Set the correct binary via: --binary <name>"
        )),
    }
}

/// Legacy compatibility wrapper.
///
/// # Errors
///
/// Returns an error if the binary is not found or cannot be executed.
#[tracing::instrument(level = "debug", ret)]
pub fn require_claude_on_path(binary: &str) -> color_eyre::eyre::Result<()> {
    require_agent_on_path(binary)
}

/// Runs Claude with the given permission mode and prompt.
///
/// All stdio (stdin/stdout/stderr) are inherited directly for real-time visibility.
///
/// # Errors
///
/// Returns an error if Claude cannot be executed or returns a non-zero exit code.
#[tracing::instrument(level = "debug", fields(permission_mode = %permission_mode, prompt_name = %prompt_name), ret, err)]
pub fn run_claude(
    binary: &str,
    permission_mode: &str,
    prompt: &str,
    prompt_name: &str,
) -> color_eyre::eyre::Result<ClaudeRunResult> {
    eprintln!(
        "🤖 Invoking {binary} with permission-mode='{permission_mode}' (prompt: '{prompt_name}')..."
    );
    let mut child = Command::new(binary)
        .args([
            "--permission-mode",
            permission_mode,
            "-p",
            "--verbose",
            "--input-format",
            "text",
            "--output-format",
            "stream-json",
            "--include-partial-messages",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| color_eyre::eyre::eyre!("failed to execute {binary}: {e}"))?;

    // Write to stdin and explicitly close it to signal EOF to the child process
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| color_eyre::eyre::eyre!("failed to open {binary} stdin"))?;

    // Log prompt preview for debugging
    let prompt_preview = if prompt.len() > 200 {
        format!(
            "{}... ({} chars total)",
            truncate_str(prompt, 200),
            prompt.len()
        )
    } else {
        format!("{} ({} chars)", prompt, prompt.len())
    };
    tracing::debug!("sending prompt to {binary}: {prompt_preview}");

    stdin.write_all(prompt.as_bytes())?;
    if !prompt.ends_with('\n') {
        stdin.write_all(b"\n")?;
    }
    drop(stdin); // Explicitly close stdin so child knows we're done sending input

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| color_eyre::eyre::eyre!("failed to capture {binary} stdout"))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| color_eyre::eyre::eyre!("failed to capture {binary} stderr"))?;

    let stdout_handle = thread::spawn(move || -> color_eyre::eyre::Result<String> {
        let mut collected = String::new();
        let mut out = std::io::stdout();
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        loop {
            let n = reader.read_line(&mut line)?;
            if n == 0 {
                break;
            }
            let trimmed = line.trim_end();
            if trimmed.is_empty() {
                line.clear();
                continue;
            }

            // Process the line for text and tool calls
            let (text_opt, tool_call_opt) = process_stream_line(trimmed);

            // Display tool call first (on its own line)
            if let Some(tool_call) = tool_call_opt {
                let display = tool_call.format_display();
                writeln!(out, "{display}")?;
                out.flush()?;
                // Don't include tool calls in collected output (they're visual feedback only)
            }

            // Then display text content
            if let Some(chunk) = text_opt {
                out.write_all(chunk.as_bytes())?;
                // Add newline after colons to separate thoughts
                if chunk.ends_with(':') {
                    out.write_all(b"\n")?;
                    collected.push('\n');
                }
                out.flush()?;
                collected.push_str(&chunk);
            }

            line.clear();
        }
        Ok(collected)
    });

    let stderr_handle = thread::spawn(move || -> color_eyre::eyre::Result<String> {
        let mut err = std::io::stderr();
        let mut collected = String::new();
        let mut buf = [0u8; 8192];
        loop {
            let n = stderr.read(&mut buf)?;
            if n == 0 {
                break;
            }
            let chunk = std::str::from_utf8(&buf[..n])
                .unwrap_or("<binary data>")
                .to_string();
            err.write_all(&buf[..n])?;
            err.flush()?;
            collected.push_str(&chunk);
        }
        Ok(collected)
    });

    let status = child.wait()?;

    let stdout = stdout_handle
        .join()
        .map_err(|_| color_eyre::eyre::eyre!("stdout reader thread panicked"))??;
    let stderr = stderr_handle
        .join()
        .map_err(|_| color_eyre::eyre::eyre!("stderr reader thread panicked"))??;

    if !status.success() {
        // Trim stderr for cleaner error messages, but include up to 500 chars
        let stderr_summary = if stderr.len() > 500 {
            format!("{}...", truncate_str(&stderr, 500))
        } else {
            stderr.clone()
        };
        let stderr_summary = stderr_summary.trim().replace('\n', " | ");

        // Build helpful error message with diagnostics
        let stderr_display = if stderr_summary.is_empty() {
            "<empty (check if binary is installed and configured correctly)>"
        } else {
            &stderr_summary
        };

        let stdout_preview = if stdout.is_empty() {
            "<no output>".to_string()
        } else if stdout.len() > 200 {
            format!(
                "{}... ({} chars total)",
                truncate_str(&stdout, 200),
                stdout.len()
            )
        } else {
            stdout.clone()
        };

        // Check if stdout contains what looks like our prompt (echoed input)
        let prompt_start = prompt.chars().take(50).collect::<String>();
        let stdout_contains_prompt = stdout.starts_with(&prompt_start)
            || prompt_start.contains(&stdout.chars().take(50).collect::<String>());

        // Check for partial output that suggests mid-stream crash
        let ends_abruptly = !stdout.is_empty()
            && !stdout.ends_with('.')
            && !stdout.ends_with('!')
            && !stdout.ends_with('?')
            && !stdout.ends_with('"')
            && !stdout.ends_with('`')
            && !stdout.ends_with(')');

        // Check for tool use in output (might indicate tool crash)
        let has_tool_use = stdout.contains("tool_use")
            || stdout.contains("<antml")
            || stdout.contains("function_call");

        let hint = if !stderr_summary.is_empty() {
            // stderr has content - use it as-is
            ""
        } else if stdout_contains_prompt {
            "\n  HINT: stdout appears to contain the prompt text. This may indicate the subprocess\n        echoed stdin to stdout before crashing, or there's an I/O redirection issue."
        } else if has_tool_use {
            "\n  HINT: stdout contains tool use indicators. The crash may have occurred during\n        tool execution - check if the tool being called (Bash, Read, etc.) caused the failure."
        } else if ends_abruptly && stdout.len() > 1000 {
            "\n  HINT: Output was cut off mid-sentence after significant output. This suggests\n        claude-glm crashed during generation - possibly due to an API error, timeout,\n        or signal (SIGTERM/SIGKILL). Try increasing the timeout or check system logs."
        } else if stderr_summary.is_empty() {
            "\n  HINT: Empty stderr with exit status 1 often indicates an internal error.\n        Try running the command manually to diagnose."
        } else {
            ""
        };

        return Err(color_eyre::eyre::eyre!(
            "{binary} exited with non-zero status: {status}\n  stderr: {stderr_display}\n  stdout: {stdout_preview}\n  prompt length: {} chars{hint}",
            prompt.len(),
            hint = hint
        ));
    }

    Ok(ClaudeRunResult { stdout })
}

/// Runs Codex in non-interactive JSON streaming mode and prints rich updates.
///
/// # Errors
///
/// Returns an error if Codex cannot be executed or exits non-zero.
#[tracing::instrument(level = "debug", fields(prompt_name = %prompt_name, cwd = %cwd.display()), ret, err)]
pub fn run_codex(
    binary: &str,
    prompt: &str,
    prompt_name: &str,
    cwd: &Path,
    config: &CodexConfig,
    images: &[PathBuf],
) -> color_eyre::eyre::Result<AgentRunResult> {
    let mut out = std::io::stdout();
    run_codex_with_events(
        binary,
        prompt,
        prompt_name,
        cwd,
        config,
        images,
        |event| match event {
            AgentEvent::TextDelta { text } => {
                let _ = out.write_all(text.as_bytes());
                let _ = out.flush();
            }
            AgentEvent::ToolCall { detail, .. } => {
                let _ = writeln!(out, "🔧 {}", detail);
                let _ = out.flush();
            }
            AgentEvent::ToolResult {
                detail, success, ..
            } => {
                let prefix = if success == Some(false) {
                    "⚠️"
                } else {
                    "✅"
                };
                let _ = writeln!(out, "{prefix} {}", detail);
                let _ = out.flush();
            }
            AgentEvent::Status { message } => {
                let _ = writeln!(out, "ℹ️ {}", message);
                let _ = out.flush();
            }
            AgentEvent::Error { message } => {
                let _ = writeln!(out, "❌ {}", message);
                let _ = out.flush();
            }
            AgentEvent::Usage { .. } => {}
        },
    )
}

/// Runs Codex and emits structured stream events through a callback.
///
/// # Errors
///
/// Returns an error if Codex cannot be executed or exits non-zero.
#[allow(clippy::too_many_arguments)]
#[tracing::instrument(level = "debug", skip(on_event, images), fields(prompt_name = %prompt_name, cwd = %cwd.display()), ret, err)]
fn run_codex_with_events<F>(
    binary: &str,
    prompt: &str,
    prompt_name: &str,
    cwd: &Path,
    config: &CodexConfig,
    images: &[PathBuf],
    mut on_event: F,
) -> color_eyre::eyre::Result<AgentRunResult>
where
    F: FnMut(AgentEvent),
{
    on_event(AgentEvent::Status {
        message: format!("invoking {binary} (prompt: '{prompt_name}')"),
    });

    let mut cmd = Command::new(binary);
    cmd.arg("exec")
        .arg("--json")
        .arg("-C")
        .arg(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    if config.full_auto {
        cmd.arg("--full-auto");
    }
    if !config.sandbox.trim().is_empty() {
        cmd.arg("--sandbox").arg(config.sandbox.trim());
    }
    if config.skip_git_repo_check {
        cmd.arg("--skip-git-repo-check");
    }
    if config.progress_cursor {
        cmd.arg("--progress-cursor");
    }
    if let Some(model) = config.model.as_ref().filter(|m| !m.trim().is_empty()) {
        cmd.arg("--model").arg(model);
    }
    if let Some(effort) = config.model_reasoning_effort {
        cmd.arg("-c")
            .arg(format!("model_reasoning_effort=\"{}\"", effort.as_str()));
    }
    if let Some(profile) = config.profile.as_ref().filter(|p| !p.trim().is_empty()) {
        cmd.arg("--profile").arg(profile);
    }
    for image in images {
        cmd.arg("--image").arg(image);
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| color_eyre::eyre::eyre!("failed to execute {binary}: {e}"))?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| color_eyre::eyre::eyre!("failed to open {binary} stdin"))?;
    stdin.write_all(prompt.as_bytes())?;
    if !prompt.ends_with('\n') {
        stdin.write_all(b"\n")?;
    }
    drop(stdin);

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| color_eyre::eyre::eyre!("failed to capture {binary} stdout"))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| color_eyre::eyre::eyre!("failed to capture {binary} stderr"))?;

    let stdout_handle = thread::spawn(
        move || -> color_eyre::eyre::Result<(String, Vec<AgentEvent>)> {
            let mut collected = String::new();
            let mut events = Vec::new();
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();

            loop {
                let n = reader.read_line(&mut line)?;
                if n == 0 {
                    break;
                }
                let trimmed = line.trim_end();
                if !trimmed.is_empty() {
                    process_codex_stream_line(trimmed, &mut collected, &mut events);
                }
                line.clear();
            }
            Ok((collected, events))
        },
    );

    let stderr_handle = thread::spawn(move || -> color_eyre::eyre::Result<String> {
        let mut collected = String::new();
        let mut buf = [0u8; 8192];
        loop {
            let n = stderr.read(&mut buf)?;
            if n == 0 {
                break;
            }
            let chunk = std::str::from_utf8(&buf[..n])
                .unwrap_or("<binary data>")
                .to_string();
            collected.push_str(&chunk);
        }
        Ok(collected)
    });

    let status = child.wait()?;
    let (stdout, events) = stdout_handle
        .join()
        .map_err(|_| color_eyre::eyre::eyre!("stdout reader thread panicked"))??;
    let stderr = stderr_handle
        .join()
        .map_err(|_| color_eyre::eyre::eyre!("stderr reader thread panicked"))??;

    for event in events {
        on_event(event);
    }

    if !status.success() {
        let stderr_summary = if stderr.len() > 500 {
            format!("{}...", truncate_str(&stderr, 500))
        } else {
            stderr.clone()
        };
        return Err(color_eyre::eyre::eyre!(
            "{binary} exited with non-zero status: {status}\n  stderr: {}",
            stderr_summary.trim()
        ));
    }

    Ok(AgentRunResult { stdout })
}

/// Parses one Codex JSONL event line and updates collected output/events.
fn process_codex_stream_line(line: &str, collected: &mut String, events: &mut Vec<AgentEvent>) {
    let Ok(event) = serde_json::from_str::<CodexEvent>(line) else {
        return;
    };

    match event {
        CodexEvent::ThreadStarted { thread_id } => events.push(AgentEvent::Status {
            message: format!("thread started: {thread_id}"),
        }),
        CodexEvent::TurnStarted => events.push(AgentEvent::Status {
            message: "turn started".to_string(),
        }),
        CodexEvent::ItemStarted { item } => {
            if item.item_type == "command_execution" {
                let detail = item
                    .command
                    .as_deref()
                    .map(|cmd| {
                        if cmd.len() > MAX_COMMAND_DISPLAY_LEN {
                            format!("{}...", truncate_str(cmd, MAX_COMMAND_DISPLAY_LEN))
                        } else {
                            cmd.to_string()
                        }
                    })
                    .unwrap_or_else(|| "command execution".to_string());
                events.push(AgentEvent::ToolCall {
                    tool: "command_execution".to_string(),
                    detail,
                });
            }
        }
        CodexEvent::ItemCompleted { item } => {
            if item.item_type == "agent_message" {
                if let Some(text) = item.text {
                    collected.push_str(&text);
                    events.push(AgentEvent::TextDelta { text });
                }
            } else if item.item_type == "command_execution" {
                let command = item
                    .command
                    .unwrap_or_else(|| "command execution".to_string());
                let output_preview = item
                    .aggregated_output
                    .as_deref()
                    .map(|s| truncate_str(s, MAX_COMMAND_DISPLAY_LEN).to_string())
                    .unwrap_or_default();
                let detail = if output_preview.is_empty() {
                    command
                } else {
                    format!("{command} => {output_preview}")
                };
                let success = item.exit_code.map(|code| code == 0);
                events.push(AgentEvent::ToolResult {
                    tool: "command_execution".to_string(),
                    detail,
                    success,
                });
            }
        }
        CodexEvent::TurnCompleted { usage } => {
            if let Some(usage) = usage {
                events.push(AgentEvent::Usage {
                    input_tokens: usage.input_tokens,
                    output_tokens: usage.output_tokens,
                    cached_input_tokens: usage.cached_input_tokens,
                });
            }
            events.push(AgentEvent::Status {
                message: "turn completed".to_string(),
            });
        }
        CodexEvent::Error { message, error } => {
            let msg = message
                .or(error)
                .unwrap_or_else(|| "codex stream error".to_string());
            events.push(AgentEvent::Error { message: msg });
        }
        CodexEvent::Unknown => {}
    }
}

/// Processes a single line of stream-json output, extracting text and tool calls.
///
/// Returns a tuple of (optional text chunk, optional tool call display).
#[tracing::instrument(level = "debug", ret)]
fn process_stream_line(line: &str) -> (Option<String>, Option<ToolCall>) {
    // Try to parse as a typed StreamEvent first
    if let Ok(event) = serde_json::from_str::<StreamEvent>(line) {
        // Check for tool calls in assistant events
        let tool_call = extract_tool_call(&event);

        // Extract text from various event types
        let text = match &event {
            StreamEvent::ContentBlockDelta { delta } => Some(delta.text.clone()),
            StreamEvent::ContentBlockStart {
                content_block: ContentBlock::Text { text },
            } => Some(text.clone()),
            StreamEvent::ContentBlockStart { .. } => None,
            StreamEvent::Assistant { message } => {
                // Concatenate all text blocks in the message
                let texts: Vec<_> = message
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect();
                if !texts.is_empty() {
                    Some(texts.join(""))
                } else {
                    None
                }
            }
            _ => None,
        };

        return (text, tool_call);
    }

    // Fallback to legacy JSON parsing for backward compatibility
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(line) {
        let text = extract_text_chunk(&value);
        (text, None)
    } else {
        (None, None)
    }
}

/// Attempts to extract text content from Claude's stream-json output (legacy).
#[tracing::instrument(level = "debug", ret)]
fn extract_text_chunk(value: &serde_json::Value) -> Option<String> {
    if let Some(text) = value
        .get("delta")
        .and_then(|delta| delta.get("text"))
        .and_then(|text| text.as_str())
    {
        return Some(text.to_string());
    }

    if let Some(text) = value
        .get("content_block")
        .and_then(|block| block.get("text"))
        .and_then(|text| text.as_str())
    {
        return Some(text.to_string());
    }

    if let Some(text) = value
        .get("message")
        .and_then(|message| message.get("content"))
        .and_then(|content| content.as_array())
        .and_then(|content| concat_text_items(content))
    {
        return Some(text);
    }

    value
        .get("content")
        .and_then(|content| content.as_array())
        .and_then(|content| concat_text_items(content))
}

/// Concatenates any `text` fields found in a content array.
fn concat_text_items(items: &[serde_json::Value]) -> Option<String> {
    let mut out = String::new();
    for item in items {
        if let Some(text) = item.get("text").and_then(|text| text.as_str()) {
            out.push_str(text);
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

#[cfg(test)]
mod tests {
    use super::{
        AgentRunner, ToolCall, concat_text_items, extract_text_chunk, format_tool_args,
        process_stream_line,
    };
    use crate::config::{AcpConfig, AgentProvider, CodexConfig, PermissionMode, Protocol};

    #[test]
    fn extract_text_from_delta() {
        let value = serde_json::json!({
            "type": "content_block_delta",
            "delta": {"text": "Hello"}
        });

        let out = extract_text_chunk(&value);
        assert_eq!(out, Some("Hello".to_string()), "expected delta text");
    }

    #[test]
    fn extract_text_from_content_block() {
        let value = serde_json::json!({
            "type": "content_block_start",
            "content_block": {"text": "Hi"}
        });

        let out = extract_text_chunk(&value);
        assert_eq!(out, Some("Hi".to_string()), "expected content block text");
    }

    #[test]
    fn concat_text_items_joins_text_fields() {
        let items = vec![
            serde_json::json!({"text": "A"}),
            serde_json::json!({"text": "B"}),
        ];

        let out = concat_text_items(&items);
        assert_eq!(out, Some("AB".to_string()), "expected concatenated text");
    }

    #[test]
    fn test_agent_runner_from_config_subprocess() {
        let acp_config = AcpConfig::default();
        let runner = AgentRunner::from_config(
            AgentProvider::Claude,
            Protocol::Subprocess,
            acp_config,
            CodexConfig::default(),
        );

        assert!(runner.is_subprocess(), "expected subprocess runner");
        assert!(!runner.is_acp(), "expected non-acp runner");
    }

    #[test]
    fn test_agent_runner_from_config_acp() {
        let acp_config = AcpConfig {
            api_key_env: "CUSTOM_KEY".to_string(),
            permission_mode: PermissionMode::Deny,
            persist_adapter: false,
        };
        let runner = AgentRunner::from_config(
            AgentProvider::Claude,
            Protocol::Acp,
            acp_config,
            CodexConfig::default(),
        );

        assert!(runner.is_acp(), "expected acp runner");
        assert!(!runner.is_subprocess(), "expected non-subprocess runner");
    }

    #[test]
    fn test_agent_runner_is_acp() {
        let runner = AgentRunner::ClaudeAcp(AcpConfig::default());
        assert!(runner.is_acp(), "Acp variant should return true for is_acp");
        assert!(
            !runner.is_subprocess(),
            "Acp variant should return false for is_subprocess"
        );
    }

    #[test]
    fn test_agent_runner_is_subprocess() {
        let runner = AgentRunner::ClaudeSubprocess;
        assert!(
            runner.is_subprocess(),
            "Subprocess variant should return true for is_subprocess"
        );
        assert!(
            !runner.is_acp(),
            "Subprocess variant should return false for is_acp"
        );
    }

    #[test]
    fn test_agent_runner_clone() {
        let acp_config = AcpConfig::default();
        let runner = AgentRunner::from_config(
            AgentProvider::Claude,
            Protocol::Acp,
            acp_config,
            CodexConfig::default(),
        );
        let _cloned = runner.clone();
        // Just verifying that Clone is implemented
    }

    #[test]
    fn test_agent_runner_debug() {
        let runner = AgentRunner::ClaudeSubprocess;
        let debug_str = format!("{:?}", runner);
        assert!(
            debug_str.contains("ClaudeSubprocess"),
            "Debug output should contain ClaudeSubprocess"
        );
    }

    // ===== New tests for stream event parsing and tool call display =====

    #[test]
    fn format_tool_args_read_shows_file_path() {
        let input = serde_json::json!({"file_path": "src/main.rs"});
        let tool_call = format_tool_args("Read", &input);

        assert_eq!(tool_call.name, "Read");
        assert_eq!(tool_call.args_display, "src/main.rs");
    }

    #[test]
    fn format_tool_args_read_shows_file_name_fallback() {
        let input = serde_json::json!({"file_name": "Cargo.toml"});
        let tool_call = format_tool_args("Read", &input);

        assert_eq!(tool_call.name, "Read");
        assert_eq!(tool_call.args_display, "Cargo.toml");
    }

    #[test]
    fn format_tool_args_read_shows_question_mark_missing_path() {
        let input = serde_json::json!({});
        let tool_call = format_tool_args("Read", &input);

        assert_eq!(tool_call.name, "Read");
        assert_eq!(tool_call.args_display, "?");
    }

    #[test]
    fn format_tool_args_bash_shows_command() {
        let input = serde_json::json!({"command": "cargo test"});
        let tool_call = format_tool_args("Bash", &input);

        assert_eq!(tool_call.name, "Bash");
        assert_eq!(tool_call.args_display, "cargo test");
    }

    #[test]
    fn format_tool_args_bash_truncates_long_command() {
        let long_command = "cargo test -- --test-threads=1 --show-output some_long_argument_here";
        let input = serde_json::json!({"command": long_command});
        let tool_call = format_tool_args("Bash", &input);

        assert_eq!(tool_call.name, "Bash");
        assert!(tool_call.args_display.len() <= super::MAX_COMMAND_DISPLAY_LEN + 3); // +3 for "..."
        assert!(
            tool_call.args_display.ends_with("..."),
            "should truncate with ..."
        );
    }

    #[test]
    fn format_tool_args_glob_shows_pattern() {
        let input = serde_json::json!({"pattern": "**/*.rs"});
        let tool_call = format_tool_args("Glob", &input);

        assert_eq!(tool_call.name, "Glob");
        assert_eq!(tool_call.args_display, "**/*.rs");
    }

    #[test]
    fn format_tool_args_grep_shows_pattern_and_path() {
        let input = serde_json::json!({"pattern": "test", "path": "src/"});
        let tool_call = format_tool_args("Grep", &input);

        assert_eq!(tool_call.name, "Grep");
        assert_eq!(tool_call.args_display, "test path=src/");
    }

    #[test]
    fn format_tool_args_grep_shows_pattern_only() {
        let input = serde_json::json!({"pattern": "TODO"});
        let tool_call = format_tool_args("Grep", &input);

        assert_eq!(tool_call.name, "Grep");
        assert_eq!(tool_call.args_display, "TODO");
    }

    #[test]
    fn format_tool_args_edit_shows_replace_indicator() {
        let input = serde_json::json!({"file_path": "src/lib.rs"});
        let tool_call = format_tool_args("Edit", &input);

        assert_eq!(tool_call.name, "Edit");
        assert_eq!(tool_call.args_display, "src/lib.rs (replace)");
    }

    #[test]
    fn format_tool_args_write_shows_new_indicator() {
        let input = serde_json::json!({"file_path": "tests/new_test.rs"});
        let tool_call = format_tool_args("Write", &input);

        assert_eq!(tool_call.name, "Write");
        assert_eq!(tool_call.args_display, "tests/new_test.rs (new)");
    }

    #[test]
    fn format_tool_args_unknown_tool_shows_json() {
        let input = serde_json::json!({"foo": "bar"});
        let tool_call = format_tool_args("UnknownTool", &input);

        assert_eq!(tool_call.name, "UnknownTool");
        assert_eq!(tool_call.args_display, "{\"foo\":\"bar\"}");
    }

    #[test]
    fn tool_call_format_display_includes_emoji() {
        let tool_call = ToolCall {
            name: "Read".to_string(),
            args_display: "src/main.rs".to_string(),
        };

        let display = tool_call.format_display();
        assert_eq!(display, "🔧 Read: src/main.rs");
    }

    #[test]
    fn process_stream_line_content_block_delta_extracts_text() {
        let line = r#"{"type":"content_block_delta","delta":{"text":"Hello"}}"#;
        let (text, tool_call) = process_stream_line(line);

        assert_eq!(text, Some("Hello".to_string()));
        assert!(tool_call.is_none(), "should not have tool call");
    }

    #[test]
    fn process_stream_line_content_block_start_extracts_text() {
        let line = r#"{"type":"content_block_start","content_block":{"type":"text","text":"Hi"}}"#;
        let (text, tool_call) = process_stream_line(line);

        assert_eq!(text, Some("Hi".to_string()));
        assert!(tool_call.is_none(), "should not have tool call");
    }

    #[test]
    fn process_stream_line_assistant_with_tool_use_extracts_tool_call() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_1","name":"Read","input":{"file_path":"src/main.rs"}}]}}"#;
        let (text, tool_call) = process_stream_line(line);

        assert!(text.is_none(), "should not have text");
        assert!(tool_call.is_some(), "should have tool call");
        let tc = tool_call.expect("should have tool call");
        assert_eq!(tc.name, "Read");
        assert_eq!(tc.args_display, "src/main.rs");
    }

    #[test]
    fn process_stream_line_assistant_with_multiple_tool_uses_first_tool() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_1","name":"Glob","input":{"pattern":"**/*.rs"}},{"type":"tool_use","id":"toolu_2","name":"Read","input":{"file_path":"src/lib.rs"}}]}}"#;
        let (_text, tool_call) = process_stream_line(line);

        assert!(tool_call.is_some(), "should have tool call");
        let tc = tool_call.expect("should have tool call");
        assert_eq!(tc.name, "Glob", "should extract first tool call");
        assert_eq!(tc.args_display, "**/*.rs");
    }

    #[test]
    fn process_stream_line_assistant_with_text_extracts_text() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Thinking about this..."}]}}"#;
        let (text, tool_call) = process_stream_line(line);

        assert_eq!(text, Some("Thinking about this...".to_string()));
        assert!(tool_call.is_none(), "should not have tool call");
    }

    #[test]
    fn process_stream_line_system_event_returns_none() {
        let line = r#"{"type":"system","session_id":"abc123"}"#;
        let (text, tool_call) = process_stream_line(line);

        assert!(text.is_none(), "should not have text");
        assert!(tool_call.is_none(), "should not have tool call");
    }

    #[test]
    fn process_stream_line_user_event_returns_none() {
        let line = r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"output"}]}}"#;
        let (text, tool_call) = process_stream_line(line);

        assert!(text.is_none(), "should not have text");
        assert!(
            tool_call.is_none(),
            "should not have tool call from user event"
        );
    }

    #[test]
    fn process_stream_line_invalid_json_returns_none() {
        let line = "not valid json";
        let (text, tool_call) = process_stream_line(line);

        assert!(text.is_none(), "should not have text");
        assert!(tool_call.is_none(), "should not have tool call");
    }

    #[test]
    fn process_stream_line_legacy_delta_format_fallback() {
        let line = r#"{"type":"content_block_delta","delta":{"text":"Legacy text"}}"#;
        let (text, tool_call) = process_stream_line(line);

        assert_eq!(text, Some("Legacy text".to_string()));
        assert!(tool_call.is_none());
    }

    #[test]
    fn format_tool_args_bash_empty_command() {
        let input = serde_json::json!({"command": ""});
        let tool_call = format_tool_args("Bash", &input);

        assert_eq!(tool_call.name, "Bash");
        assert_eq!(tool_call.args_display, "");
    }

    #[test]
    fn format_tool_args_bash_no_command_field() {
        let input = serde_json::json!({});
        let tool_call = format_tool_args("Bash", &input);

        assert_eq!(tool_call.name, "Bash");
        assert_eq!(tool_call.args_display, "");
    }

    // ===== Integration tests for multi-tool conversation streams =====

    #[test]
    fn integration_multi_tool_conversation_sequence() {
        // Simulates a realistic conversation where Claude uses multiple tools
        let stream_lines = vec![
            // Assistant starts with text
            r#"{"type":"content_block_delta","delta":{"text":"I'll examine the codebase structure."}}"#,
            // Then uses Glob
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_1","name":"Glob","input":{"pattern":"**/*.rs"}}]}}"#,
            // Then some more text
            r#"{"type":"content_block_delta","delta":{"text":"Found the files."}}"#,
            // Then uses Read
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_2","name":"Read","input":{"file_path":"src/main.rs"}}]}}"#,
            // Then uses Grep
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_3","name":"Grep","input":{"pattern":"test","path":"src/"}}]}}"#,
            // Final response
            r#"{"type":"content_block_delta","delta":{"text":"Analysis complete."}}"#,
        ];

        let mut tool_calls = Vec::new();
        let mut text_parts = Vec::new();

        for line in stream_lines {
            let (text, tool_call) = process_stream_line(line);
            if let Some(tc) = tool_call {
                tool_calls.push(tc);
            }
            if let Some(t) = text {
                text_parts.push(t);
            }
        }

        // Verify we extracted all tool calls
        assert_eq!(tool_calls.len(), 3, "should extract 3 tool calls");

        // Verify tool call types and display formats
        assert_eq!(tool_calls[0].name, "Glob");
        assert_eq!(tool_calls[0].args_display, "**/*.rs");
        assert_eq!(tool_calls[0].format_display(), "🔧 Glob: **/*.rs");

        assert_eq!(tool_calls[1].name, "Read");
        assert_eq!(tool_calls[1].args_display, "src/main.rs");
        assert_eq!(tool_calls[1].format_display(), "🔧 Read: src/main.rs");

        assert_eq!(tool_calls[2].name, "Grep");
        assert_eq!(tool_calls[2].args_display, "test path=src/");
        assert_eq!(tool_calls[2].format_display(), "🔧 Grep: test path=src/");

        // Verify text was also extracted
        assert_eq!(text_parts.len(), 3, "should extract 3 text parts");
        assert_eq!(text_parts[0], "I'll examine the codebase structure.");
        assert_eq!(text_parts[1], "Found the files.");
        assert_eq!(text_parts[2], "Analysis complete.");
    }

    #[test]
    fn integration_all_common_tools_in_one_stream() {
        // Tests all common tool types in a single stream
        let stream_lines = vec![
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"1","name":"Read","input":{"file_path":"Cargo.toml"}}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"2","name":"Bash","input":{"command":"cargo test"}}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"3","name":"Glob","input":{"pattern":"src/**/*.rs"}}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"4","name":"Grep","input":{"pattern":"TODO"}}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"5","name":"Edit","input":{"file_path":"src/lib.rs"}}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"6","name":"Write","input":{"file_path":"tests/new_test.rs"}}]}}"#,
        ];

        let mut tool_calls = Vec::new();
        for line in stream_lines {
            let (_, tool_call) = process_stream_line(line);
            if let Some(tc) = tool_call {
                tool_calls.push(tc);
            }
        }

        assert_eq!(tool_calls.len(), 6, "should extract all 6 tool calls");

        // Verify each tool call has correct display format with emoji
        for tc in &tool_calls {
            let display = tc.format_display();
            assert!(
                display.starts_with("🔧 "),
                "tool call should start with 🔧 emoji"
            );
            assert!(
                display.contains(": "),
                "tool call should have colon separator"
            );
        }
    }

    #[test]
    fn integration_mixed_text_and_tools_stream() {
        // Tests realistic interleaving of text and tool calls
        let stream_lines = vec![
            r#"{"type":"content_block_delta","delta":{"text":"Let me check"}}"#,
            r#"{"type":"content_block_delta","delta":{"text":" the project structure."}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"1","name":"Read","input":{"file_path":"README.md"}}]}}"#,
            r#"{"type":"content_block_delta","delta":{"text":"Now I'll "}}"#,
            r#"{"type":"content_block_delta","delta":{"text":"search for tests."}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"2","name":"Grep","input":{"pattern":"test","path":"src/"}}]}}"#,
            r#"{"type":"content_block_delta","delta":{"text":"Done searching."}}"#,
        ];

        let mut tool_calls = Vec::new();
        let mut full_text = String::new();

        for line in stream_lines {
            let (text, tool_call) = process_stream_line(line);
            if let Some(tc) = tool_call {
                tool_calls.push(tc);
            }
            if let Some(t) = text {
                full_text.push_str(&t);
            }
        }

        assert_eq!(tool_calls.len(), 2);
        assert_eq!(tool_calls[0].name, "Read");
        assert_eq!(tool_calls[1].name, "Grep");
        assert_eq!(
            full_text,
            "Let me check the project structure.Now I'll search for tests.Done searching."
        );
    }

    #[test]
    fn integration_long_command_truncation() {
        // Verifies long commands are properly truncated for display
        let long_command = "cargo test -- --test-threads=1 --show-output --nocapture some_very_long_test_name_that_exceeds_limit";

        // Test format_tool_args directly (JSON escaping makes raw string test difficult)
        let input = serde_json::json!({"command": long_command});
        let tool_call = format_tool_args("Bash", &input);

        assert_eq!(tool_call.name, "Bash");
        assert!(
            tool_call.args_display.len() <= super::MAX_COMMAND_DISPLAY_LEN + 3,
            "display should be truncated to {} + 3 for ...",
            super::MAX_COMMAND_DISPLAY_LEN
        );
        assert!(
            tool_call.args_display.ends_with("..."),
            "truncated display should end with ..."
        );
        assert!(
            tool_call.format_display().starts_with("🔧 Bash: "),
            "display should have emoji prefix"
        );
    }

    #[test]
    fn integration_original_crash_scenario() {
        // Tests the exact command that caused the original panic:
        // "byte index 60 is not a char boundary; it is inside '✔' (bytes 58..61)"
        let grep_command = "just check 2>&1 | grep -E \"(✓|✅|Error|error|FAIL|fail|✔|warning.*error)\" | tail -30";

        // This should not panic when displaying the command
        let input = serde_json::json!({"command": grep_command});
        let tool_call = format_tool_args("Bash", &input);

        assert_eq!(tool_call.name, "Bash");
        // The command is longer than MAX_COMMAND_DISPLAY_LEN (60)
        assert!(tool_call.args_display.len() <= super::MAX_COMMAND_DISPLAY_LEN + 3);
        assert!(tool_call.args_display.ends_with("..."));

        // The displayed string should be valid UTF-8 (not cut through a multi-byte char)
        assert!(
            tool_call
                .args_display
                .is_char_boundary(tool_call.args_display.len())
        );

        // Verify the display format works
        let display = tool_call.format_display();
        assert!(display.starts_with("🔧 Bash: "));
    }

    // ===== Tests for UTF-8 safe truncation =====

    #[test]
    fn truncate_str_handles_multi_byte_chars() {
        // "✔" is 3 bytes, "🌍" is 4 bytes
        let s = "Hello ✔ World 🌍 Test";

        // Truncate to ~12 bytes (should include the ✔ fully)
        let truncated = super::truncate_str(s, 12);
        // Result should be "Hello ✔ Wor" (12 bytes, ending at char boundary)
        assert!(truncated.is_char_boundary(truncated.len()));
        assert!(truncated.len() <= 12);
        assert!(
            !truncated.contains("World"),
            "should truncate before 'World'"
        );

        // No truncation when under limit
        assert_eq!(super::truncate_str(s, 1000), s);

        // Edge case: truncate in middle of multi-byte char
        // Byte 7 is in the middle of "✔" (bytes 6-8)
        let result = super::truncate_str(s, 7);
        assert_eq!(result, "Hello ", "should stop at the boundary before ✔");

        // The original failing case: grep command with emoji
        let grep_cmd = "just check 2>&1 | grep -E \"(✓|✅|Error|error|FAIL|fail|✔|warning.*error)\" | tail -30";
        let result = super::truncate_str(grep_cmd, 60);
        // Should not panic and should be valid UTF-8
        assert!(result.is_char_boundary(result.len()));
        assert!(result.len() <= 60);
    }

    #[test]
    fn truncate_str_empty_string() {
        assert_eq!(super::truncate_str("", 100), "");
    }

    #[test]
    fn truncate_str_shorter_than_limit() {
        let s = "Short";
        assert_eq!(super::truncate_str(s, 100), s);
    }

    #[test]
    fn truncate_str_exactly_at_limit() {
        let s = "abc";
        assert_eq!(super::truncate_str(s, 3), s);
    }

    #[test]
    fn truncate_str_various_emoji() {
        let s = "Test 😀 🎉 🔥 ❤️";
        // Truncate somewhere in the middle
        let result = super::truncate_str(s, 10);
        assert!(result.is_char_boundary(result.len()));
        // Should be valid UTF-8
        assert!(result.chars().count() < s.chars().count());
    }
}
