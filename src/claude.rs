//! Interface to the Claude CLI and ACP agent.
//!
//! This module provides functionality to interact with AI agents via:
//! - Subprocess spawning (original method with Claude CLI)
//! - ACP (Agent Client Protocol) for structured communication

use crate::acp::{self, ChunkCallback};
use crate::config::{AcpConfig, Protocol};
use color_eyre::eyre::WrapErr;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;

/// Result of running a Claude command.
#[derive(Debug)]
pub struct ClaudeRunResult {
    /// The standard output from Claude.
    pub stdout: String,
}

/// Agent runner that supports both subprocess and ACP protocols.
///
/// This enum provides a unified interface for running AI agents using either
/// the traditional subprocess spawning method or the ACP protocol.
#[derive(Debug, Clone)]
pub enum AgentRunner {
    /// Spawn subprocess with stdin/stdout (original behavior).
    Subprocess,
    /// ACP (Agent Client Protocol) via stdio.
    Acp(AcpConfig),
}

impl AgentRunner {
    /// Creates a new `AgentRunner` from the protocol configuration.
    ///
    /// # Arguments
    ///
    /// * `protocol` - The communication protocol to use
    /// * `acp_config` - ACP configuration (only used when protocol is Acp)
    #[must_use]
    pub fn from_config(protocol: Protocol, acp_config: AcpConfig) -> Self {
        match protocol {
            Protocol::Subprocess => Self::Subprocess,
            Protocol::Acp => Self::Acp(acp_config),
        }
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
    /// * `on_chunk` - Optional callback for streaming output chunks
    ///
    /// # Errors
    ///
    /// Returns an error if the agent cannot be executed, fails, or returns non-zero exit code.
    #[tracing::instrument(level = "debug", fields(binary = %binary, prompt_name = %prompt_name, runner = ?self), skip(on_chunk), ret, err)]
    pub async fn run(
        &self,
        binary: &str,
        permission_mode: &str,
        prompt: &str,
        prompt_name: &str,
        cwd: &Path,
        on_chunk: Option<ChunkCallback>,
    ) -> color_eyre::eyre::Result<ClaudeRunResult> {
        match self {
            Self::Subprocess => {
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
            Self::Acp(config) => {
                // ACP mode is natively async
                let acp_result =
                    acp::run_acp(binary, prompt, prompt_name, config, cwd, on_chunk).await?;

                // Convert AcpRunResult to ClaudeRunResult for compatibility
                Ok(ClaudeRunResult {
                    stdout: acp_result.stdout,
                })
            }
        }
    }

    /// Returns `true` if this is an ACP runner.
    #[must_use]
    pub const fn is_acp(&self) -> bool {
        matches!(self, Self::Acp(_))
    }

    /// Returns `true` if this is a subprocess runner.
    #[must_use]
    pub const fn is_subprocess(&self) -> bool {
        matches!(self, Self::Subprocess)
    }
}

/// Verifies that the Claude CLI is available on PATH.
///
/// # Errors
///
/// Returns an error if Claude is not found or cannot be executed.
#[tracing::instrument(level = "debug", ret)]
pub fn require_claude_on_path(binary: &str) -> color_eyre::eyre::Result<()> {
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
        format!("{}... ({} chars total)", &prompt[..200], prompt.len())
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
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
                if let Some(chunk) = extract_text_chunk(&value) {
                    out.write_all(chunk.as_bytes())?;
                    // Add newline after colons to separate thoughts
                    if chunk.ends_with(':') {
                        out.write_all(b"\n")?;
                        collected.push('\n');
                    }
                    out.flush()?;
                    collected.push_str(&chunk);
                }
            } else {
                out.write_all(line.as_bytes())?;
                out.flush()?;
                collected.push_str(&line);
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
            format!("{}...", &stderr[..500])
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
            format!("{}... ({} chars total)", &stdout[..200], stdout.len())
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

/// Attempts to extract text content from Claude's stream-json output.
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
    use super::{concat_text_items, extract_text_chunk};

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
}

#[cfg(test)]
mod agent_runner_tests {
    use super::*;
    use crate::config::{AcpConfig, PermissionMode, Protocol};

    #[test]
    fn test_agent_runner_from_config_subprocess() {
        let acp_config = AcpConfig::default();
        let runner = AgentRunner::from_config(Protocol::Subprocess, acp_config);

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
        let runner = AgentRunner::from_config(Protocol::Acp, acp_config);

        assert!(runner.is_acp(), "expected acp runner");
        assert!(!runner.is_subprocess(), "expected non-subprocess runner");
    }

    #[test]
    fn test_agent_runner_is_acp() {
        let runner = AgentRunner::Acp(AcpConfig::default());
        assert!(runner.is_acp(), "Acp variant should return true for is_acp");
        assert!(
            !runner.is_subprocess(),
            "Acp variant should return false for is_subprocess"
        );
    }

    #[test]
    fn test_agent_runner_is_subprocess() {
        let runner = AgentRunner::Subprocess;
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
        let runner = AgentRunner::from_config(Protocol::Acp, acp_config);
        let _cloned = runner.clone();
        // Just verifying that Clone is implemented
    }

    #[test]
    fn test_agent_runner_debug() {
        let runner = AgentRunner::Subprocess;
        let debug_str = format!("{:?}", runner);
        assert!(
            debug_str.contains("Subprocess"),
            "Debug output should contain Subprocess"
        );
    }
}
