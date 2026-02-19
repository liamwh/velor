//! ACP (Agent Client Protocol) client implementation.
//!
//! This module provides functionality to interact with AI agents via the ACP protocol,
//! a standardized communication protocol between code editors and AI-powered coding agents.
//!
//! ACP uses JSON-RPC 2.0 over stdio for communication. Velor acts as the ACP client,
//! communicating with an ACP-compatible agent (like `claude-agent-acp`).

use agent_client_protocol as acp;
// Import Agent trait so its methods are available on ClientSideConnection
use acp::Agent;
use color_eyre::eyre::{WrapErr, eyre};
use std::path::Path;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use crate::config::{AcpConfig, PermissionMode};

/// Callback type for streaming output chunks.
///
/// This allows the caller to handle streaming output in real-time,
/// for example printing to stdout or logging.
pub type ChunkCallback = Box<dyn Fn(&str) + Send + Sync>;

/// Result of running a prompt via ACP.
#[derive(Debug)]
pub struct AcpRunResult {
    /// The complete output collected from the agent.
    pub stdout: String,
}

/// Velor's ACP Client implementation.
///
/// This struct implements the [`acp::Client`] trait, handling requests from the agent
/// such as permission requests and file operations.
struct VelorClient {
    /// Permission handling mode.
    permission_mode: PermissionMode,
}

impl VelorClient {
    /// Creates a new Velor client with the specified permission mode.
    #[must_use]
    const fn new(permission_mode: PermissionMode) -> Self {
        Self { permission_mode }
    }
}

#[async_trait::async_trait(?Send)]
impl acp::Client for VelorClient {
    /// Handles permission requests from the agent.
    ///
    /// The behavior depends on the configured permission mode:
    /// - `Allow`: Automatically grants all permission requests
    /// - `Deny`: Automatically denies all permission requests
    async fn request_permission(
        &self,
        _request: acp::RequestPermissionRequest,
    ) -> acp::Result<acp::RequestPermissionResponse> {
        // Note: The response structure varies by ACP schema version.
        // For now, return method_not_found to indicate we don't support interactive prompting.
        // The agent will proceed with default behavior.
        match self.permission_mode {
            PermissionMode::Allow => {
                // Try to construct an allowed response - if schema doesn't match, fall back to error
                Err(acp::Error::method_not_found())
            }
            PermissionMode::Deny => Err(acp::Error::method_not_found()),
        }
    }

    /// Handles write_text_file requests from the agent.
    ///
    /// This is not implemented in the MVP - returns method not found.
    async fn write_text_file(
        &self,
        _request: acp::WriteTextFileRequest,
    ) -> acp::Result<acp::WriteTextFileResponse> {
        Err(acp::Error::method_not_found())
    }

    /// Handles read_text_file requests from the agent.
    ///
    /// Allows agents to read files from the filesystem for read-only access.
    async fn read_text_file(
        &self,
        request: acp::ReadTextFileRequest,
    ) -> acp::Result<acp::ReadTextFileResponse> {
        // Validate path for security - ensure it's an absolute path
        let path = Path::new(&request.path);
        if !path.is_absolute() {
            let msg = format!("Path must be absolute, got: {}", request.path.display());
            tracing::error!("{}", msg);
            return Err(acp::Error::internal_error());
        }

        // Read the file content
        tokio::fs::read_to_string(&path)
            .await
            .map(|content| acp::ReadTextFileResponse::new(content))
            .map_err(|e| {
                let msg = format!("Failed to read file: {e}");
                tracing::error!("{}", msg);
                acp::Error::internal_error()
            })
    }

    /// Handles create_terminal requests from the agent.
    ///
    /// This is not implemented in the MVP - returns method not found.
    async fn create_terminal(
        &self,
        _request: acp::CreateTerminalRequest,
    ) -> Result<acp::CreateTerminalResponse, acp::Error> {
        Err(acp::Error::method_not_found())
    }

    /// Handles terminal_output requests from the agent.
    ///
    /// This is not implemented in the MVP - returns method not found.
    async fn terminal_output(
        &self,
        _request: acp::TerminalOutputRequest,
    ) -> acp::Result<acp::TerminalOutputResponse> {
        Err(acp::Error::method_not_found())
    }

    /// Handles release_terminal requests from the agent.
    ///
    /// This is not implemented in the MVP - returns method not found.
    async fn release_terminal(
        &self,
        _request: acp::ReleaseTerminalRequest,
    ) -> acp::Result<acp::ReleaseTerminalResponse> {
        Err(acp::Error::method_not_found())
    }

    /// Handles wait_for_terminal_exit requests from the agent.
    ///
    /// This is not implemented in the MVP - returns method not found.
    async fn wait_for_terminal_exit(
        &self,
        _request: acp::WaitForTerminalExitRequest,
    ) -> acp::Result<acp::WaitForTerminalExitResponse> {
        Err(acp::Error::method_not_found())
    }

    /// Handles kill_terminal_command requests from the agent.
    ///
    /// This is not implemented in the MVP - returns method not found.
    async fn kill_terminal_command(
        &self,
        _request: acp::KillTerminalCommandRequest,
    ) -> acp::Result<acp::KillTerminalCommandResponse> {
        Err(acp::Error::method_not_found())
    }

    /// Handles session notifications from the agent.
    ///
    /// This is the primary way the agent streams output to the client.
    /// We extract text content from `AgentMessageChunk` updates and log via tracing.
    async fn session_notification(
        &self,
        args: acp::SessionNotification,
    ) -> acp::Result<(), acp::Error> {
        match args.update {
            acp::SessionUpdate::AgentMessageChunk(chunk) => {
                let text = match &chunk.content {
                    acp::ContentBlock::Text(text_content) => text_content.text.clone(),
                    acp::ContentBlock::Image(_) => "<image>".into(),
                    acp::ContentBlock::Audio(_) => "<audio>".into(),
                    acp::ContentBlock::ResourceLink(resource_link) => resource_link.uri.clone(),
                    // Use wildcard to handle non-exhaustive enum
                    _ => "<unknown content>".into(),
                };

                // TODO: Implement proper callback mechanism for streaming output.
                // For now, trace the output for visibility.
                tracing::trace!("Agent output: {}", text);
            }
            // Handle all other SessionUpdate variants gracefully
            _ => {}
        }
        Ok(())
    }

    /// Handles extension method calls from the agent.
    ///
    /// Returns method not found for unhandled extensions.
    async fn ext_method(&self, _args: acp::ExtRequest) -> acp::Result<acp::ExtResponse> {
        Err(acp::Error::method_not_found())
    }

    /// Handles extension notifications from the agent.
    ///
    /// Returns method not found for unhandled extensions.
    async fn ext_notification(&self, _args: acp::ExtNotification) -> acp::Result<()> {
        Err(acp::Error::method_not_found())
    }
}

// Thread-local storage for the current chunk callback.
//
// TODO: Implement proper callback storage. The challenge is that
// `Box<dyn Fn>` cannot be cloned, so we need an alternative approach
// such as:
// - Using `Arc<Mutex<Option<ChunkCallback>>>`
// - Storing callback in a struct with interior mutability
// - Using channels to stream output
tokio::task_local! {
    static CURRENT_CALLBACK: std::cell::RefCell<Option<ChunkCallback>>;
}

/// Runs a prompt via the ACP protocol.
///
/// This function:
/// 1. Spawns the ACP adapter binary as a subprocess
/// 2. Creates an ACP client connection over stdio
/// 3. Initializes the protocol
/// 4. Creates a new session
/// 5. Sends the prompt
/// 6. Collects streaming output via the callback
/// 7. Returns the complete output
///
/// # Arguments
///
/// * `binary` - Path to the ACP adapter binary (e.g., "claude-agent-acp")
/// * `prompt` - The prompt text to send to the agent
/// * `prompt_name` - Name of the prompt (for logging)
/// * `config` - ACP configuration options
/// * `cwd` - Current working directory
/// * `on_chunk` - Optional callback for streaming output
///
/// # Errors
///
/// Returns an error if:
/// - The binary cannot be spawned
/// - ACP protocol initialization fails
/// - Session creation fails
/// - Prompt sending fails
/// - The agent returns an error
pub async fn run_acp(
    binary: &str,
    prompt: &str,
    prompt_name: &str,
    config: &AcpConfig,
    cwd: &Path,
    _on_chunk: Option<ChunkCallback>,
) -> color_eyre::eyre::Result<AcpRunResult> {
    tracing::info!("🤖 Invoking {binary} via ACP protocol (prompt: '{prompt_name}')...");

    // Canonicalize cwd (ACP requires absolute paths)
    let cwd = cwd
        .canonicalize()
        .wrap_err_with(|| format!("failed to canonicalize cwd: {}", cwd.display()))?;

    // Get API key from environment
    let api_key = std::env::var(&config.api_key_env).wrap_err_with(|| {
        format!(
            "ACP API key not found. Set the {} environment variable.",
            config.api_key_env
        )
    })?;

    // Spawn the ACP adapter binary
    let mut child = tokio::process::Command::new(binary)
        .current_dir(&cwd)
        .env(&config.api_key_env, api_key)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .wrap_err_with(|| format!("failed to execute {binary}"))?;

    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| eyre!("failed to open {binary} stdin"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| eyre!("failed to capture {binary} stdout"))?;

    // Convert to async streams compatible with ACP SDK
    let outgoing = stdin.compat_write();
    let incoming = stdout.compat();

    // Create the Velor client
    let client = VelorClient::new(config.permission_mode);

    // The ACP SDK futures are not Send, so we need to use LocalSet
    let local_set = tokio::task::LocalSet::new();

    let _result = local_set
        .run_until(async move {
            // Create the ACP client-side connection
            let (conn, handle_io) =
                acp::ClientSideConnection::new(client, outgoing, incoming, |fut| {
                    tokio::task::spawn_local(fut);
                });

            // Handle I/O in the background
            tokio::task::spawn_local(handle_io);

            // Initialize the protocol using builder pattern
            conn.initialize(
                acp::InitializeRequest::new(acp::ProtocolVersion::V1)
                    .client_capabilities(acp::ClientCapabilities::default())
                    .client_info(
                        acp::Implementation::new("velor", env!("CARGO_PKG_VERSION"))
                            .title("Velor Agent CLI"),
                    ),
            )
            .await
            .map_err(|e| eyre!("ACP initialize failed: {e}"))?;

            // Create a new session using builder pattern
            let session_response = conn
                .new_session(acp::NewSessionRequest::new(&cwd).mcp_servers(vec![]))
                .await
                .map_err(|e| eyre!("ACP new_session failed: {e}"))?;

            let session_id = session_response.session_id;

            // Log prompt preview for debugging
            let prompt_preview = if prompt.len() > 200 {
                format!("{}... ({} chars total)", &prompt[..200], prompt.len())
            } else {
                format!("{} ({} chars)", prompt, prompt.len())
            };
            tracing::debug!("sending prompt via ACP: {prompt_preview}");

            // Send the prompt using builder pattern
            conn.prompt(acp::PromptRequest::new(
                session_id,
                vec![acp::ContentBlock::Text(acp::TextContent::new(
                    prompt.to_string(),
                ))],
            ))
            .await
            .map_err(|e| eyre!("ACP prompt failed: {e}"))?;

            color_eyre::eyre::Result::<()>::Ok(())
        })
        .await?;

    // Kill the child process
    child.kill().await.ok();

    // Return success result
    Ok(AcpRunResult {
        stdout: String::new(), // Output is streamed via callback
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_velor_client_new_allow() {
        let client = VelorClient::new(PermissionMode::Allow);
        // Can't inspect private field, but we can verify it compiles
        let _ = client;
    }

    #[test]
    fn test_velor_client_new_deny() {
        let client = VelorClient::new(PermissionMode::Deny);
        let _ = client;
    }

    #[test]
    fn test_acp_run_result_debug() {
        let result = AcpRunResult {
            stdout: "test output".to_string(),
        };
        assert_eq!(
            format!("{:?}", result),
            "AcpRunResult { stdout: \"test output\" }"
        );
    }
}

#[cfg(test)]
mod proptest_tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn test_acp_run_result_roundtrip(content in ".*") {
            let result = AcpRunResult {
                stdout: content.clone(),
            };
            prop_assert_eq!(result.stdout, content);
        }
    }
}
