# Plan: Add ACP (Agent Client Protocol) Support to Velor

## Context

Velor currently controls Claude Code by spawning the configured binary as a subprocess, passing prompts via stdin, and parsing the `stream-json` output format. The user wants to add support for ACP (Agent Client Protocol) as an alternative communication method.

### What is ACP?

ACP (Agent Client Protocol) is a standardized JSON-RPC based protocol for AI agents to communicate with clients. It provides:
- Structured communication via JSON-RPC 2.0
- Session management (create, load, fork, resume)
- Real-time streaming content updates via `session/update` notifications
- Tool execution with permission requests
- Filesystem and terminal access via client methods
- Type-safe Rust SDK (`agent-client-protocol` crate)

### Architecture

```
┌─────────────────┐                    ┌──────────────────────┐
│     Velor       │   ACP Protocol     │  claude-agent-acp    │
│  (ACP Client)   │◄──────────────────►│  (ACP Agent/Server)  │
│                 │   stdio            │                      │
└─────────────────┘                    └──────────────────────┘
                                                │
                                                ▼
                                       ┌──────────────────┐
                                       │   Claude Agent   │
                                       │      SDK         │
                                       └──────────────────┘
```

Velor acts as an ACP **client**, communicating with the `@zed-industries/claude-agent-acp` adapter (or any ACP-compliant agent) which wraps the Claude Agent SDK.

### Why ACP?

1. **Standardization**: Common protocol across different AI agents
2. **Better Session Management**: Native support for conversation persistence
3. **Structured Communication**: Type-safe messages instead of parsing JSON lines
4. **Future Compatibility**: Works with any ACP-compliant agent
5. **Existing Adapter**: The `claude-agent-acp` npm package provides immediate Claude Code support

## Current Architecture

**Key files:**
- `src/claude.rs:54-240` - `run_claude()` spawns subprocess, handles stdio via threads, parses stream-json
- `src/config.rs:203-238` - `Defaults` struct with `binary` field
- `src/main.rs:516-545` - Synchronous `main()` function

**Current flow:**
1. Render template → prompt string
2. Spawn `{binary} --permission-mode {mode} -p --input-format text --output-format stream-json`
3. Write prompt to stdin
4. Parse JSON lines from stdout in a thread
5. Return collected output

## Proposed Changes

### 1. Async Architecture

Switch to `#[tokio::main]` in `src/main.rs`:

```rust
#[tokio::main]
async fn main() -> color_eyre::eyre::Result<()> {
    // ... existing setup ...

    match cli.command {
        Some(Commands::Once(args)) => run_once(args, home_cfg, git_root, cwd, &var_overrides).await,
        Some(Commands::Auto(args)) => run_auto(args, home_cfg, git_root, cwd, &var_overrides).await,
        // ... etc
    }
}
```

**Hybrid approach:**
- `main` is async (`#[tokio::main]`)
- Most code stays sync (templating, config, file reads)
- ACP runner is naturally async
- Subprocess runner stays sync via `std::process`, wrapped in `spawn_blocking` if needed from async context

### 2. Configuration

Add new config options to `Defaults` in `src/config.rs`:

```toml
[defaults]
# Protocol to use: "subprocess" (current) or "acp"
protocol = "subprocess"
binary = "claude-glm"

# ACP-specific options (only used when protocol = "acp")
[defaults.acp]
# Environment variable for Anthropic API key (passed to claude-agent-acp)
api_key_env = "ANTHROPIC_API_KEY"

# Permission handling: "deny", "allow", or "ask" (future)
permission_mode = "allow"

# Keep adapter process alive between prompts (recommended)
persist_adapter = true
```

**Notes:**
- MVP is stdio-only (no websocket/remote transport)
- If remote transport is added later, name it `websocket` to match ACP spec

### 3. New Module: `src/acp.rs`

```rust
use agent_client_protocol as acp;

/// ACP configuration from TOML
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AcpConfig {
    /// Environment variable name for API key
    pub api_key_env: String,
    /// Permission handling mode
    pub permission_mode: PermissionMode,
    /// Keep adapter alive between prompts
    pub persist_adapter: bool,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum PermissionMode {
    #[default]
    Allow,  // Always allow in auto mode
    Deny,   // Always deny
    // Ask, // Future: interactive prompting
}

impl Default for AcpConfig {
    fn default() -> Self {
        Self {
            api_key_env: "ANTHROPIC_API_KEY".to_string(),
            permission_mode: PermissionMode::Allow,
            persist_adapter: true,
        }
    }
}

/// Callback for streaming output
pub type ChunkCallback = Box<dyn Fn(&str) + Send>;

/// Run a prompt via ACP protocol
pub async fn run_acp(
    binary: &str,
    prompt: &str,
    prompt_name: &str,
    config: &AcpConfig,
    cwd: &Path,
    on_chunk: Option<ChunkCallback>,
) -> color_eyre::eyre::Result<String>
```

### 4. ACP Implementation Details

**Key requirements from ACP spec:**
- All file paths must be **absolute** - canonicalize `cwd` before sending
- Implement at least `session/request_permission` client method
- Optionally implement `fs/read_text_file` for agent filesystem access

```rust
use agent_client_protocol as acp;
use std::path::Path;

pub async fn run_acp(
    binary: &str,
    prompt: &str,
    _prompt_name: &str,
    config: &AcpConfig,
    cwd: &Path,
    on_chunk: Option<ChunkCallback>,
) -> Result<String> {
    // 1. Canonicalize cwd (ACP requires absolute paths)
    let cwd = cwd.canonicalize()?;

    // 2. Spawn the ACP adapter binary
    let mut child = tokio::process::Command::new(binary)
        .current_dir(&cwd)
        .env(&config.api_key_env, std::env::var(&config.api_key_env)?)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .kill_on_drop(true)  // Ensure cleanup on panic/cancel
        .spawn()?;

    // 3. Create ACP connection
    let client = VelorClient {
        permission_mode: config.permission_mode.clone(),
    };

    let (conn, handle) = acp::ClientSideConnection::new(
        client,
        child.stdin.take().unwrap(),
        child.stdout.take().unwrap(),
        tokio::spawn,
    );

    // 4. Initialize protocol (use actual crate API)
    conn.initialize(acp::InitializeRequest {
        protocol_version: acp::V1,
        client_capabilities: acp::ClientCapabilities {
            // Declare we support filesystem read
            fs: Some(acp::FsCapabilities {
                read_text_file: true,
                ..Default::default()
            }),
            ..Default::default()
        },
        client_info: Some(acp::Implementation {
            name: "velor".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }),
        meta: None,
    }).await?;

    // 5. Create session (method name per actual crate)
    let session = conn.new_session(acp::NewSessionRequest {
        cwd,
        mcp_servers: vec![],
        meta: None,
    }).await?;

    // 6. Send prompt
    conn.prompt(acp::PromptRequest {
        session_id: session.session_id.clone(),
        prompt: vec![acp::ContentBlock::Text(acp::TextContent {
            text: prompt.to_string(),
            annotations: None,
        })],
        meta: None,
    }).await?;

    // 7. Collect streaming output via callback
    let mut output = String::new();
    let mut receiver = conn.subscribe();

    loop {
        match receiver.recv().await {
            Ok(notification) => {
                match notification {
                    // Handle session/update notifications (per ACP spec)
                    acp::SessionNotification::Update(update) => {
                        if let Some(content) = &update.content {
                            for block in content {
                                if let acp::ContentBlock::Text(text) = block {
                                    if let Some(ref cb) = on_chunk {
                                        cb(&text.text);
                                    }
                                    output.push_str(&text.text);
                                }
                            }
                        }
                    }
                    acp::SessionNotification::PromptDone(_) => break,
                    _ => {}
                }
            }
            Err(e) => return Err(e.into()),
        }
    }

    // 8. Clean up (kill_on_drop handles this, but be explicit)
    child.kill().await.ok();

    Ok(output)
}

/// Velor's Client implementation
struct VelorClient {
    permission_mode: PermissionMode,
}

#[async_trait::async_trait]
impl acp::Client for VelorClient {
    /// Handle permission requests from the agent
    async fn request_permission(
        &self,
        _request: acp::PermissionRequest,
    ) -> acp::Result<acp::PermissionResponse> {
        match self.permission_mode {
            PermissionMode::Allow => Ok(acp::PermissionResponse::Granted),
            PermissionMode::Deny => Ok(acp::PermissionResponse::Denied),
        }
    }

    /// Allow agents to read files (read-only for MVP)
    async fn read_text_file(
        &self,
        request: acp::ReadTextFileRequest,
    ) -> acp::Result<acp::ReadTextFileResponse> {
        // Validate path is within cwd for security
        let path = std::path::PathBuf::from(&request.path);
        let content = tokio::fs::read_to_string(&path)
            .await
            .map_err(|e| acp::Error::internal(format!("Failed to read file: {e}")))?;

        Ok(acp::ReadTextFileResponse { content })
    }
}
```

### 5. Subprocess Lifecycle & Cancellation

**Graceful shutdown sequence (e.g., on Ctrl+C):**
1. Send `session/cancel` via ACP protocol
2. Wait briefly for final update / done notification
3. Kill the adapter if still running

```rust
use tokio_util::sync::CancellationToken;

pub struct AcpSession {
    conn: acp::ClientSideConnection,
    session_id: acp::SessionId,
    child: tokio::process::Child,
    cancel_token: CancellationToken,
}

impl AcpSession {
    /// Graceful shutdown: cancel → wait → kill
    pub async fn cancel(mut self) -> Result<()> {
        // 1. Send session/cancel
        let cancel_result = self.conn.cancel(acp::CancelRequest {
            session_id: self.session_id.clone(),
        }).await;

        // 2. Wait briefly for final update (with timeout)
        if cancel_result.is_ok() {
            tokio::time::timeout(
                std::time::Duration::from_secs(2),
                async {
                    while let Ok(notification) = self.conn.recv().await {
                        if matches!(notification, acp::SessionNotification::PromptDone(_)) {
                            break;
                        }
                    }
                }
            ).await.ok();
        }

        // 3. Kill adapter if still running
        self.child.kill().await.ok();

        Ok(())
    }
}

// In main, handle Ctrl+C:
async fn run_with_cancellation(session: AcpSession, cancel_token: CancellationToken) {
    tokio::select! {
        result = run_session(&session) => result,
        _ = cancel_token.cancelled() => {
            session.cancel().await
        }
    }
}
```

**Signal handling in main:**
```rust
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() -> Result<()> {
    let cancel_token = CancellationToken::new();

    // Register Ctrl+C handler
    let token_clone = cancel_token.clone();
    ctrlc::set_handler(move || {
        token_clone.cancel();
    })?;

    // Pass cancel_token through to run_auto / run_once
    run_auto(args, ..., cancel_token).await
}
```

**Add dependency:**
```toml
tokio-util = { version = "0.7", features = ["sync"] }
ctrlc = "3.4"  # Or use tokio signal handling
```

### 6. Output Handling

**Use callback pattern, not printing inside protocol layer:**

```rust
// In main.rs, when calling run_acp:
let output = run_acp(
    &binary,
    &rendered,
    &prompt_name,
    &acp_config,
    &cwd,
    Some(Box::new(|chunk| {
        print!("{}", chunk);
        std::io::stdout().flush().ok();
    })),
).await?;
```

This allows:
- Streaming to terminal (current behavior)
- Streaming to logs (future)
- Capturing for JSON output (future)

### 7. Dependencies

Add to `Cargo.toml`:
```toml
[dependencies]
# ACP SDK - use current published version (0.9.x, NOT schema version 0.10.x)
agent-client-protocol = "0.9"

# Async runtime
tokio = { version = "1", features = ["rt-multi-thread", "process", "io-util", "net", "fs", "signal"] }
tokio-util = { version = "0.7", features = ["sync"] }

# Async trait support
async-trait = "0.1"

# Ctrl+C handling (or use tokio signal)
ctrlc = "3.4"
```

**Note:** `agent-client-protocol` v0.9.x is the main crate. The schema crate is v0.10.x - don't confuse them.

### 8. Abstraction Layer

```rust
pub enum AgentRunner {
    Subprocess,
    Acp(AcpConfig),
}

impl AgentRunner {
    pub async fn run(
        &self,
        binary: &str,
        permission_mode: &str,
        prompt: &str,
        prompt_name: &str,
        cwd: &Path,
        on_chunk: Option<ChunkCallback>,
    ) -> Result<String> {
        match self {
            Self::Subprocess => {
                // Wrap sync subprocess in spawn_blocking
                let binary = binary.to_string();
                let permission_mode = permission_mode.to_string();
                let prompt = prompt.to_string();
                let prompt_name = prompt_name.to_string();

                tokio::task::spawn_blocking(move || {
                    let result = run_claude(&binary, &permission_mode, &prompt, &prompt_name)?;
                    Ok(result.stdout)
                }).await?
            }
            Self::Acp(config) => {
                run_acp(binary, prompt, prompt_name, config, cwd, on_chunk).await
            }
        }
    }
}
```

## Files to Modify

| File | Changes |
|------|---------|
| `Cargo.toml` | Add `agent-client-protocol = "0.9"`, `tokio`, `async-trait` |
| `src/main.rs` | Switch to `#[tokio::main]`, make `run_once`/`run_auto` async |
| `src/config.rs` | Add `protocol` field to `Defaults`, add `AcpConfig` struct |
| `src/acp.rs` | **NEW** - ACP client implementation with permission handling |
| `src/claude.rs` | Keep sync, add `AgentRunner` enum |

## Implementation Order

1. **Add dependencies** - Update `Cargo.toml` with correct crate versions
2. **Switch to async main** - Add `#[tokio::main]`, make entry points async
3. **Add cancellation support** - CancellationToken, Ctrl+C handler
4. **Add config** - Extend `Defaults` with `protocol` and `acp` sections
5. **Create `src/acp.rs`** - Implement ACP client with:
   - Permission handling (`request_permission`)
   - File read support (`read_text_file`)
   - Callback-based output streaming
   - Graceful cancellation (`session/cancel` → wait → kill)
6. **Add abstraction** - Create `AgentRunner` enum with async `run()` method
7. **Wire up in main** - Update `run_once`/`run_auto` to use `AgentRunner`
8. **Add tests** - Config parsing tests, mock ACP client tests
9. **Documentation** - Update README with ACP configuration examples

## Using with claude-agent-acp

1. Install the adapter:
   ```bash
   npm install -g @zed-industries/claude-agent-acp
   ```

2. Configure velor:
   ```toml
   [defaults]
   protocol = "acp"
   binary = "claude-agent-acp"

   [defaults.acp]
   api_key_env = "ANTHROPIC_API_KEY"
   permission_mode = "allow"
   ```

3. Run velor:
   ```bash
   export ANTHROPIC_API_KEY=sk-...
   velor auto
   ```

## Verification

1. **Config parsing**: Add tests for new config fields
2. **ACP client**: Mock-based tests for permission handling
3. **Integration test**: Manual test with `claude-agent-acp` adapter
4. **Backward compat**: Verify subprocess mode still works
5. **Process cleanup**: Test that orphaned processes are killed on panic

## Out of Scope

- Remote transport (websocket) - MVP is stdio only
- Full terminal subsystem implementation
- MCP server integration
- Interactive permission prompting (just allow/deny modes)
- Session persistence/loading (future)
