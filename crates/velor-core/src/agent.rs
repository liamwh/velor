//! Agent runner interface and configuration.
//!
//! [`AgentRunner`] is a thin facade over the unified
//! [`crate::execution_service`] substrate. All execution routes through the
//! shared [`crate::execution_service::service`] (one worker thread per app
//! context), which drives the appropriate adapter (Claude/Codex subprocess or
//! ACP) and returns typed [`crate::execution_service::error::AgentExecutionError`].
//! The legacy per-protocol `run_claude`/`run_codex` implementations and their
//! stream parsers have been removed (the adapters own framing + classification).

use bytes::Bytes;
use std::path::{Path, PathBuf};
use std::process::Command;
use tokio_util::sync::CancellationToken;

use crate::config::{AcpConfig, AgentProvider, CodexConfig, Protocol};
use crate::execution_service::adapters::acp::AcpParams;
use crate::execution_service::adapters::claude::ClaudeParams;
use crate::execution_service::adapters::codex::CodexParams;
use crate::execution_service::error::AgentExecutionError;
use crate::execution_service::service::{AgentProfile, shared_service};
use crate::execution_service::supervisor::ProcessTimeouts;

/// Result of running an agent.
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

    /// Builds the execution profile for this runner variant.
    fn build_profile(
        &self,
        binary: &str,
        permission_mode: &str,
        prompt: &str,
        prompt_name: &str,
        cwd: &Path,
        images: &[PathBuf],
        timeouts: ProcessTimeouts,
    ) -> AgentProfile {
        match self {
            Self::ClaudeSubprocess => AgentProfile::Claude(ClaudeParams {
                binary: binary.to_string(),
                permission_mode: permission_mode.to_string(),
                prompt: Bytes::copy_from_slice(prompt.as_bytes()),
                working_directory: cwd.to_path_buf(),
                model: None,
                resume_session: None,
                extra_args: Vec::new(),
                extra_env: Vec::new(),
                timeouts,
                cancellation: CancellationToken::new(),
            }),
            Self::Codex(config) => AgentProfile::Codex(CodexParams {
                binary: binary.to_string(),
                prompt: Bytes::copy_from_slice(prompt.as_bytes()),
                working_directory: cwd.to_path_buf(),
                config: config.clone(),
                images: images.to_vec(),
                resume_session: None,
                extra_args: Vec::new(),
                extra_env: Vec::new(),
                timeouts,
                cancellation: CancellationToken::new(),
            }),
            Self::ClaudeAcp(config) => AgentProfile::Acp(AcpParams {
                binary: binary.to_string(),
                prompt: prompt.to_string(),
                prompt_name: prompt_name.to_string(),
                config: config.clone(),
                working_directory: cwd.to_path_buf(),
                cancellation: CancellationToken::new(),
            }),
        }
    }

    /// Runs the agent, returning the collected output.
    ///
    /// Routes through the shared [`crate::execution_service::service`]. Events
    /// are not streamed in this mode (use [`Self::run_with_events`]). `timeouts`
    /// bounds this attempt (startup/idle/total + termination grace).
    ///
    /// # Errors
    ///
    /// Returns [`AgentExecutionError`] on any failure (process, protocol,
    /// provider, cancellation, deadline).
    pub async fn run(
        &self,
        binary: &str,
        permission_mode: &str,
        prompt: &str,
        prompt_name: &str,
        cwd: &Path,
        timeouts: ProcessTimeouts,
    ) -> Result<ClaudeRunResult, AgentExecutionError> {
        let profile = self.build_profile(
            binary,
            permission_mode,
            prompt,
            prompt_name,
            cwd,
            &[],
            timeouts,
        );
        let execution = shared_service().execute(profile).await?;
        let report = execution.complete().await?;
        Ok(ClaudeRunResult {
            stdout: report.result.stdout,
        })
    }

    /// Runs the agent and forwards structured events to `on_event` as they arrive.
    /// `timeouts` bounds this attempt.
    ///
    /// # Errors
    ///
    /// Returns [`AgentExecutionError`] if provider execution fails.
    pub async fn run_with_events<F>(
        &self,
        binary: &str,
        permission_mode: &str,
        prompt: &str,
        prompt_name: &str,
        cwd: &Path,
        images: &[PathBuf],
        timeouts: ProcessTimeouts,
        mut on_event: F,
    ) -> Result<AgentRunResult, AgentExecutionError>
    where
        F: FnMut(AgentEvent) + Send,
    {
        let profile = self.build_profile(
            binary,
            permission_mode,
            prompt,
            prompt_name,
            cwd,
            images,
            timeouts,
        );
        let mut execution = shared_service().execute(profile).await?;
        while let Some(event) = execution.next_event().await {
            on_event(event);
        }
        let report = execution.complete().await?;
        Ok(report.result)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AcpConfig, AgentProvider, CodexConfig, PermissionMode, Protocol};

    #[test]
    fn test_agent_runner_from_config_subprocess() {
        let runner = AgentRunner::from_config(
            AgentProvider::Claude,
            Protocol::Subprocess,
            AcpConfig::default(),
            CodexConfig::default(),
        );
        assert!(runner.is_subprocess());
        assert!(!runner.is_acp());
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
        assert!(runner.is_acp());
        assert!(!runner.is_subprocess());
    }

    #[test]
    fn test_agent_runner_from_config_codex() {
        let runner = AgentRunner::from_config(
            AgentProvider::Codex,
            Protocol::Subprocess,
            AcpConfig::default(),
            CodexConfig::default(),
        );
        assert!(runner.is_codex());
    }

    #[test]
    fn test_agent_runner_clone() {
        let runner = AgentRunner::from_config(
            AgentProvider::Claude,
            Protocol::Subprocess,
            AcpConfig::default(),
            CodexConfig::default(),
        );
        let _cloned = runner.clone();
    }

    #[test]
    fn test_agent_runner_debug() {
        let runner = AgentRunner::ClaudeSubprocess;
        let debug_str = format!("{runner:?}");
        assert!(debug_str.contains("ClaudeSubprocess"));
    }
}
