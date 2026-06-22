//! ACP (Agent Client Protocol) adapter.
//!
//! Unlike the subprocess adapters, ACP does **not** run through the generic
//! [`crate::execution_service::supervisor`]: it drives a JSON-RPC session over a
//! long-lived stdio connection via the `agent-client-protocol` SDK, whose
//! connection object is `!Send`. This adapter wraps the one-shot
//! [`crate::acp::run_acp`] behind the [`AgentAdapter`] trait and runs on the
//! service's dedicated `LocalSet` worker.
//!
//! Cancellation races `run_acp` against the cancellation token; dropping the
//! `run_acp` future closes its `kill_on_drop` child.

use async_trait::async_trait;
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;

use crate::agent::{AgentEvent, AgentRunResult};
use crate::config::AcpConfig;
use crate::execution_service::adapter::{AgentAdapter, AgentEventSink};
use crate::execution_service::error::{AcpError, AgentExecutionError};

/// Parameters for one ACP invocation.
#[derive(Debug, Clone)]
pub struct AcpParams {
    /// Binary to invoke.
    pub binary: String,
    /// Rendered prompt.
    pub prompt: String,
    /// Prompt name (diagnostics/logging).
    pub prompt_name: String,
    /// ACP configuration.
    pub config: AcpConfig,
    /// Working directory.
    pub working_directory: PathBuf,
    /// Cancellation token.
    pub cancellation: CancellationToken,
}

/// ACP adapter. `!Send` — run on a `LocalSet`.
pub struct AcpAdapter {
    params: AcpParams,
}

impl AcpAdapter {
    /// Creates an adapter for the given parameters.
    #[must_use]
    pub fn new(params: AcpParams) -> Self {
        Self { params }
    }
}

#[async_trait(?Send)]
impl AgentAdapter for AcpAdapter {
    async fn execute(
        &mut self,
        sink: &mut dyn AgentEventSink,
    ) -> Result<AgentRunResult, AgentExecutionError> {
        let result = tokio::select! {
            biased;
            r = crate::acp::run_acp(
                &self.params.binary,
                &self.params.prompt,
                &self.params.prompt_name,
                &self.params.config,
                &self.params.working_directory,
            ) => r,
            _ = self.params.cancellation.cancelled() => return Err(AgentExecutionError::Cancelled),
        };
        match result {
            Ok(acp_result) => {
                if !acp_result.stdout.is_empty() {
                    let _ = sink
                        .emit(AgentEvent::TextDelta {
                            text: acp_result.stdout.clone(),
                        })
                        .await;
                }
                Ok(AgentRunResult {
                    stdout: acp_result.stdout,
                })
            }
            Err(err) => Err(AgentExecutionError::Acp(AcpError(err.to_string()))),
        }
    }
}
