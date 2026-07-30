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
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::config::{AcpConfig, AgentProvider, CodexConfig, OmpConfig, Protocol};
use crate::execution_service::adapters::acp::AcpParams;
use crate::execution_service::adapters::claude::ClaudeParams;
use crate::execution_service::adapters::codex::CodexParams;
use crate::execution_service::adapters::omp::OmpParams;
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
    /// Incremental model thinking/reasoning output (kept separate from
    /// [`AgentEvent::TextDelta`] so consumers can show or hide it on demand).
    Thinking {
        /// Reasoning token delta payload.
        text: String,
    },
    /// Tool/action execution started.
    ToolCall {
        /// Tool/action name.
        tool: String,
        /// Provider-formatted summary of the invocation.
        detail: String,
        /// The raw tool input JSON (for rich rendering — e.g. Edit diffs).
        input: serde_json::Value,
    },
    /// Tool/action execution completed.
    ToolResult {
        /// Tool/action name.
        tool: String,
        /// Provider-formatted result summary.
        detail: String,
        /// Whether the tool execution succeeded if known.
        success: Option<bool>,
        /// The originating [`AgentEvent::ToolCall`]'s `detail` (the command,
        /// file path, etc.), when the adapter can correlate the two via the
        /// provider's own call id. `None` when the provider doesn't expose a
        /// correlating id or the matching call wasn't seen (e.g. truncated
        /// history) — callers must not assume this is always populated.
        command: Option<String>,
    },
    /// A captured file edit — the real before/after diff for a file the agent
    /// modified, computed from the filesystem (not the agent's claimed patch).
    /// Emitted alongside the corresponding [`AgentEvent::ToolCall`] /
    /// [`AgentEvent::ToolResult`] for first-class edit tools.
    FileEdit {
        /// The structured edit (path, syntax, hunks, kind).
        edit: crate::file_edit::FileEdit,
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

/// The delivery state of a one-shot live-steering message, tracked from
/// acceptance through acknowledgement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SteeringDelivery {
    /// Accepted by Vel's controller (not yet written).
    Queued,
    /// Written and flushed to the agent's stdin pipe.
    Written,
    /// A matching replayed-user-message frame was observed from the agent.
    Acknowledged,
    /// No writable active live-input session exists.
    Unavailable,
    /// The pipe write succeeded, but the process failed before acknowledgement.
    DeliveryUnknown,
}

/// Error returned when a live-steering [`AgentInput`] could not be delivered.
#[derive(Debug, Clone, thiserror::Error)]
pub enum AgentInputError {
    /// Live steering was unavailable for the given [`crate::execution_service::error::LiveSteeringUnavailableReason`].
    #[error("live steering unavailable: {reason}")]
    Unavailable {
        /// Why steering was unavailable.
        #[from]
        reason: crate::execution_service::error::LiveSteeringUnavailableReason,
    },
}

/// Typed runtime input delivered to an active agent session. Lives on the
/// agent-runner side; adapters forward these to the supervisor's streaming stdin.
#[derive(Debug)]
pub enum AgentInput {
    /// A one-shot user message steering the active session. The acknowledgement
    /// resolves with the message's [`SteeringDelivery`] (or an
    /// [`AgentInputError`] if it could not be delivered).
    UserMessage {
        /// The steering text.
        text: crate::execution_service::adapters::claude_stream::SteeringText,
        /// Resolves once the message's delivery state is known.
        acknowledgement: oneshot::Sender<Result<SteeringDelivery, AgentInputError>>,
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
    /// Oh My Pi CLI via `omp --mode rpc`.
    Omp(OmpConfig),
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
    /// * `omp_config` - Oh My Pi configuration (only used for the omp provider)
    #[must_use]
    pub fn from_config(
        provider: AgentProvider,
        protocol: Protocol,
        acp_config: AcpConfig,
        codex_config: CodexConfig,
        omp_config: OmpConfig,
    ) -> Self {
        match provider {
            AgentProvider::Codex => Self::Codex(codex_config),
            AgentProvider::Omp => Self::Omp(omp_config),
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

    /// Returns `true` if this is an Oh My Pi runner.
    #[must_use]
    #[allow(dead_code)]
    pub const fn is_omp(&self) -> bool {
        matches!(self, Self::Omp(_))
    }

    /// Builds the execution profile for this runner variant.
    #[allow(clippy::too_many_arguments)]
    fn build_profile(
        &self,
        binary: &str,
        permission_mode: &str,
        prompt: &str,
        prompt_name: &str,
        cwd: &Path,
        images: &[PathBuf],
        timeouts: ProcessTimeouts,
        cancellation: CancellationToken,
        enable_live_steering: bool,
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
                cancellation,
                enable_live_steering,
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
                cancellation,
            }),
            Self::ClaudeAcp(config) => AgentProfile::Acp(AcpParams {
                binary: binary.to_string(),
                prompt: prompt.to_string(),
                prompt_name: prompt_name.to_string(),
                config: config.clone(),
                working_directory: cwd.to_path_buf(),
                cancellation,
            }),
            Self::Omp(config) => AgentProfile::Omp(OmpParams {
                binary: binary.to_string(),
                prompt: Bytes::copy_from_slice(prompt.as_bytes()),
                working_directory: cwd.to_path_buf(),
                config: config.clone(),
                extra_args: Vec::new(),
                extra_env: Vec::new(),
                timeouts,
                cancellation,
            }),
        }
    }

    /// Runs the agent, returning the collected output.
    ///
    /// Routes through the shared [`crate::execution_service::service`]. Events
    /// are not streamed in this mode (use [`Self::run_with_events`]). `timeouts`
    /// bounds this attempt; `cancellation` lets the caller (e.g. Ctrl+C) abort +
    /// reap the agent process mid-run.
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
        cancellation: CancellationToken,
    ) -> Result<ClaudeRunResult, AgentExecutionError> {
        let profile = self.build_profile(
            binary,
            permission_mode,
            prompt,
            prompt_name,
            cwd,
            &[],
            timeouts,
            cancellation,
            false,
        );
        let execution = shared_service().execute(profile).await?;
        let report = execution.complete().await?;
        Ok(ClaudeRunResult {
            stdout: report.result.stdout,
        })
    }

    /// Runs the agent and forwards structured events to `on_event` as they arrive.
    /// `timeouts` bounds this attempt; `cancellation` lets the caller abort +
    /// reap the agent mid-run.
    ///
    /// # Errors
    ///
    /// Returns [`AgentExecutionError`] if provider execution fails.
    #[allow(clippy::too_many_arguments)]
    pub async fn run_with_events<F>(
        &self,
        binary: &str,
        permission_mode: &str,
        prompt: &str,
        prompt_name: &str,
        cwd: &Path,
        images: &[PathBuf],
        timeouts: ProcessTimeouts,
        cancellation: CancellationToken,
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
            cancellation,
            false,
        );
        let mut execution = shared_service().execute(profile).await?;
        while let Some(event) = execution.next_event().await {
            on_event(event);
        }
        let report = execution.complete().await?;
        Ok(report.result)
    }

    /// Runs the agent with streaming events *and* live steering.
    ///
    /// Like [`Self::run_with_events`], but also drains `steering` for
    /// [`AgentInput`] commands and forwards each to the active execution's input
    /// sender while the execution is live (status `Ready`). Steering is forwarded
    /// only while the run is active; once it finishes, the loop drains remaining
    /// events to completion and returns. The existing [`Self::run_with_events`]
    /// is left intact for non-interactive callers.
    ///
    /// # Errors
    ///
    /// Returns [`AgentExecutionError`] if provider execution fails.
    #[allow(clippy::too_many_arguments)]
    pub async fn run_with_events_and_steering<F>(
        &self,
        binary: &str,
        permission_mode: &str,
        prompt: &str,
        prompt_name: &str,
        cwd: &Path,
        images: &[PathBuf],
        timeouts: ProcessTimeouts,
        cancellation: CancellationToken,
        steering: &mut Option<tokio::sync::mpsc::Receiver<AgentInput>>,
        mut on_event: F,
    ) -> Result<AgentRunResult, AgentExecutionError>
    where
        F: FnMut(AgentEvent) + Send,
    {
        // Enable the streaming path only when a steering receiver is wired in;
        // otherwise this behaves like the plain event path. The receiver is
        // borrowed (not consumed) so a retry driver can reuse it across attempts.
        let enable_live_steering = steering.is_some();
        let profile = self.build_profile(
            binary,
            permission_mode,
            prompt,
            prompt_name,
            cwd,
            images,
            timeouts,
            cancellation.clone(),
            enable_live_steering,
        );
        let mut execution = shared_service().execute(profile).await?;
        let capabilities = execution.capabilities();
        let input_tx = execution.input_sender();
        let mut steering_open = steering.is_some();

        loop {
            tokio::select! {
                biased;

                _ = cancellation.cancelled() => {
                    let report = execution.cancel().await?;
                    return Ok(report.result);
                }

                // Forward a steering command only while the run is live and the
                // adapter advertises live steering. When the execution finishes
                // (next_event returns None) we stop accepting new steering.
                input = async {
                    match steering.as_mut() {
                        Some(rx) => rx.recv().await,
                        None => None,
                    }
                }, if steering_open && capabilities.live_steering => {
                    match input {
                        Some(agent_input) => {
                            // Relay on a separate task so a slow/full input
                            // channel can never stall the event-drain loop.
                            let tx = input_tx.clone();
                            tokio::spawn(async move {
                                relay_steering(agent_input, tx).await;
                            });
                        }
                        // Steering sender dropped or never provided: stop polling.
                        None => steering_open = false,
                    }
                }

                event = execution.next_event() => match event {
                    Some(e) => on_event(e),
                    None => {
                        let report = execution.complete().await?;
                        return Ok(report.result);
                    }
                },
            }
        }
    }
}

/// Relays one steering [`AgentInput`] to the active execution's input sender.
/// On success the adapter resolves the input's acknowledgement with its delivery
/// state; if delivery is impossible (no sender, or the writer has gone away) the
/// acknowledgement is resolved here as unavailable, so the caller always observes
/// an explicit outcome rather than a dropped command.
async fn relay_steering(input: AgentInput, sender: Option<tokio::sync::mpsc::Sender<AgentInput>>) {
    use crate::execution_service::error::LiveSteeringUnavailableReason;
    match sender {
        Some(tx) => match tx.send(input).await {
            Ok(()) => {} // the adapter resolves the acknowledgement.
            Err(err) => {
                // The writer is gone (process closing/terminated). Recover the
                // input and resolve its acknowledgement ourselves.
                let AgentInput::UserMessage {
                    acknowledgement, ..
                } = err.0;
                let _ = acknowledgement.send(Err(AgentInputError::Unavailable {
                    reason: LiveSteeringUnavailableReason::StdinClosed,
                }));
            }
        },
        None => {
            let AgentInput::UserMessage {
                acknowledgement, ..
            } = input;
            let _ = acknowledgement.send(Err(AgentInputError::Unavailable {
                reason: LiveSteeringUnavailableReason::Inactive,
            }));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        AcpConfig, AgentProvider, CodexConfig, OmpConfig, PermissionMode, Protocol,
    };

    #[test]
    fn test_agent_runner_from_config_subprocess() {
        let runner = AgentRunner::from_config(
            AgentProvider::Claude,
            Protocol::Subprocess,
            AcpConfig::default(),
            CodexConfig::default(),
            OmpConfig::default(),
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
            OmpConfig::default(),
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
            OmpConfig::default(),
        );
        assert!(runner.is_codex());
    }

    #[test]
    fn test_agent_runner_from_config_omp() {
        let runner = AgentRunner::from_config(
            AgentProvider::Omp,
            Protocol::Subprocess,
            AcpConfig::default(),
            CodexConfig::default(),
            OmpConfig::default(),
        );
        assert!(runner.is_omp());
    }

    #[test]
    fn test_agent_runner_clone() {
        let runner = AgentRunner::from_config(
            AgentProvider::Claude,
            Protocol::Subprocess,
            AcpConfig::default(),
            CodexConfig::default(),
            OmpConfig::default(),
        );
        let _cloned = runner.clone();
    }

    #[test]
    fn test_agent_runner_debug() {
        let runner = AgentRunner::ClaudeSubprocess;
        let debug_str = format!("{runner:?}");
        assert!(debug_str.contains("ClaudeSubprocess"));
    }

    #[tokio::test]
    async fn relay_resolves_inactive_when_no_sender() {
        let (ack_tx, ack_rx) = oneshot::channel();
        let input = AgentInput::UserMessage {
            text: crate::execution_service::adapters::claude_stream::SteeringText::new("hi")
                .unwrap(),
            acknowledgement: ack_tx,
        };
        relay_steering(input, None).await;
        let result = ack_rx.await.unwrap();
        assert!(matches!(
            result,
            Err(AgentInputError::Unavailable {
                reason: crate::execution_service::error::LiveSteeringUnavailableReason::Inactive
            })
        ));
    }

    #[tokio::test]
    async fn relay_resolves_stdin_closed_when_writer_dropped() {
        let (tx, rx) = tokio::sync::mpsc::channel::<AgentInput>(1);
        drop(rx); // the adapter's receiver is gone → send fails
        let (ack_tx, ack_rx) = oneshot::channel();
        let input = AgentInput::UserMessage {
            text: crate::execution_service::adapters::claude_stream::SteeringText::new("hi")
                .unwrap(),
            acknowledgement: ack_tx,
        };
        relay_steering(input, Some(tx)).await;
        let result = ack_rx.await.unwrap();
        assert!(matches!(
            result,
            Err(AgentInputError::Unavailable {
                reason: crate::execution_service::error::LiveSteeringUnavailableReason::StdinClosed
            })
        ));
    }

    #[tokio::test]
    async fn relay_forwards_when_channel_open_without_resolving_ack() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<AgentInput>(1);
        let (ack_tx, mut ack_rx) = oneshot::channel();
        let input = AgentInput::UserMessage {
            text: crate::execution_service::adapters::claude_stream::SteeringText::new("hi")
                .unwrap(),
            acknowledgement: ack_tx,
        };
        relay_steering(input, Some(tx)).await;
        // The input reaches the receiver; the relay does not resolve the ack on
        // success (the adapter owns that).
        assert!(matches!(
            rx.recv().await.unwrap(),
            AgentInput::UserMessage { .. }
        ));
        assert!(ack_rx.try_recv().is_err());
    }
}
