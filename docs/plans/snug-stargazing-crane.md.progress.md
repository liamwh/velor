# Progress: Add ACP (Agent Client Protocol) Support to Velor

## Completed Tasks

### Step 1: Add Dependencies ✅
- Added `agent-client-protocol = "0.9"` to Cargo.toml (ACP SDK)
- Added `tokio` with features: rt-multi-thread, process, io-util, net, fs, signal, macros, sync
- Added `async-trait = "0.1"` for async trait support
- Added `ctrlc = "3.4"` for Ctrl+C signal handling
- Added `tokio-util = "0.7"` for `CancellationToken` support

### Step 2: Switch to Async Main ✅

Converted `src/main.rs` to async architecture:

1. **Changed `main()` to async**:
   - Added `#[tokio::main]` attribute
   - Changed `fn main()` to `async fn main()`

2. **Made all run functions async**:
   - `run_init()` - async
   - `run_plan()` - async
   - `run_test_notification()` - async
   - `run_interactive_menu()` - async
   - `run_once()` - async
   - `run_auto()` - async

3. **Updated all call sites**:
   - Added `.await` to all async function calls in match arms
   - Main function dispatch now awaits all command handlers

**Note**: The subprocess `run_claude()` calls in `run_once()` and `run_auto()` remain synchronous. They will be wrapped in `spawn_blocking` when the ACP abstraction layer is implemented in Step 6. This hybrid approach keeps subprocess mode working while enabling async ACP mode.

### Step 3: Add Cancellation Support ✅

Implemented cancellation support in `src/main.rs`:

1. **Added tokio-util dependency** - `tokio-util = "0.7"` to Cargo.toml for `CancellationToken`

2. **Added CancellationToken to runtime**:
   - Created `CancellationToken` in `main()` via `CancellationToken::new()`
   - Imported `use tokio_util::sync::CancellationToken`

3. **Implemented Ctrl+C handler**:
   - Registered `ctrlc` handler that calls `token.cancel()`
   - Handler logs cancellation initiation
   - Graceful shutdown support for future ACP sessions

4. **Passed cancel token through to run_once/run_auto**:
   - Updated `run_once()` signature to accept `CancellationToken`
   - Updated `run_auto()` signature to accept `CancellationToken`
   - Updated `run_interactive_menu()` to create and pass token
   - Updated all call sites to pass token through

**Note**: The cancellation token is now available throughout the call chain but is not yet actively used for cancellation logic. This will be implemented in Step 5 when the ACP client is created, where the token will be checked during operations and used for graceful shutdown (`session/cancel` → wait → kill).

**All tests pass**: 154 tests passing, no clippy warnings

### Step 4: Add Configuration ✅

Added to `src/config.rs`:

1. **Protocol enum** - Communication protocol selection:
   - `Subprocess` (default) - original subprocess spawning behavior
   - `Acp` - ACP protocol via stdio

2. **PermissionMode enum** - ACP permission handling:
   - `Allow` (default) - automatically allow all requests
   - `Deny` - automatically deny all requests
   - `Ask` - future: interactive prompting

3. **AcpConfig struct** - ACP-specific configuration:
   - `api_key_env: String` - environment variable name for API key (default: "ANTHROPIC_API_KEY")
   - `permission_mode: PermissionMode` - permission handling mode (default: Allow)
   - `persist_adapter: bool` - keep adapter alive between prompts (default: true)

4. **Extended Defaults struct** with:
   - `protocol: Protocol` field
   - `acp: AcpConfig` field

5. **Updated Defaults::merge()** to handle new fields with overlay precedence

6. **Added comprehensive tests**:
   - Unit tests for default values
   - Property tests for merge behavior
   - Integration test for loading TOML config with ACP settings

## Remaining Tasks

### Step 5: Create src/acp.rs
- [ ] Implement ACP client with permission handling (`request_permission`)
- [ ] Implement file read support (`read_text_file`)
- [ ] Add callback-based output streaming
- [ ] Add graceful cancellation (`session/cancel` → wait → kill)
- [ ] Implement `VelorClient` trait for ACP client methods

### Step 6: Add Abstraction Layer
- [ ] Create `AgentRunner` enum with Subprocess/Acp variants
- [ ] Add async `run()` method to AgentRunner
- [ ] Handle subprocess mode via spawn_blocking

### Step 7: Wire Up in Main
- [ ] Update `run_once()` to use `AgentRunner`
- [ ] Update `run_auto()` to use `AgentRunner`
- [ ] Handle protocol selection from config

### Step 8: Add Tests
- [ ] Mock-based tests for ACP client
- [ ] Integration test with real claude-agent-acp adapter
- [ ] Process cleanup tests

### Step 9: Documentation
- [ ] Update README with ACP configuration examples
- [ ] Document claude-agent-acp setup steps

## Implementation Order Notes

The plan is designed to be implemented sequentially, with each step building on the previous:
1. Dependencies enable everything else ✅
2. Async main allows ACP client to work naturally ✅
3. Cancellation support enables graceful shutdown ✅
4. Config allows selecting protocol and ACP options ✅
5. ACP client implements the protocol
6. Abstraction layer provides clean API
7. Main integration wires everything together
8. Tests verify correctness
9. Documentation helps users

## Configuration Example (when complete)

```toml
[defaults]
# Protocol to use: "subprocess" (current) or "acp"
protocol = "acp"
binary = "claude-agent-acp"

# ACP-specific options
[defaults.acp]
api_key_env = "ANTHROPIC_API_KEY"
permission_mode = "allow"
persist_adapter = true
```

## Next Priority Task

**Step 5: Create src/acp.rs** - This is the next critical step because:
- Implements the actual ACP client functionality
- Will use the CancellationToken for graceful shutdown
- Provides the VelorClient trait implementation
- Enables permission handling and file access

## Date Completed

2025-02-19 - Completed dependencies (Step 1), async main (Step 2), cancellation support (Step 3), and configuration (Step 4)
