# Rules System Implementation Status

## Summary

The `.agents/rules` system from `docs/plans/wiggly-mixing-stearns.md` (Phase 4) is **partially implemented**:

### ✅ Working
1. **Rule discovery and parsing** - 5 rules discovered correctly
2. **Always-apply rule injection** - `always-test` rule is injected in every iteration
3. **Glob pattern matching** - Unit tests pass (`test_check_new_glob_matches_with_tracing_md`)
4. **Auto mode with ACP sessions** - Fixed LocalSet issue, sessions persist correctly
5. **Once mode with ACP** - Works perfectly (227/227 tests pass)

### ❌ Not Working
1. **Glob-based mid-iteration injection** - Rules are NOT being injected when files are read
2. **File read tracking** - `files_read=0` even though agent clearly reads files

## Root Cause Analysis

The issue is that **the agent is NOT using the ACP `read_text_file` callback** for file operations.

### Evidence
```log
[INFO] Turn A completed: files_read=0, output_len=213
```

The agent correctly responds to "Read the file CLAUDE.md" with the right answer, but `files_read` remains 0. This means:
- Agent reads files through **MCP (Model Context Protocol) tools** built into `claude-agent-acp`
- MCP tools **bypass the ACP client callbacks** we've implemented
- Our `read_text_file` callback in `VelorClient` is never called

### Technical Details

**File read tracking location:** `src/acp.rs:115-140`
```rust
async fn read_text_file(&self, request: acp::ReadTextFileRequest) -> acp::Result<acp::ReadTextFileResponse> {
    // This callback is NEVER called because agent uses MCP tools instead
    if let Some(relative) = normalize_file_path_if_safe(&self.git_root, path) {
        self.files_read_this_turn.lock().await.push(relative);
    }
    // ...
}
```

**Extension method tracking (attempted):** `src/acp.rs:221-234`
```rust
async fn ext_method(&self, args: acp::ExtRequest) -> acp::Result<acp::ExtResponse> {
    tracing::info!("🔧 Agent called extension method: {}", args.method);
    // This also never fires - MCP tools don't go through ACP extension methods
    Err(acp::Error::method_not_found())
}
```

## What Was Fixed

### 1. LocalSet Persistence Issue (FIXED)
**Problem:** `AcpSession` was dropping the `LocalSet` after initialization, killing the background I/O handler.

**Solution:** Store `LocalSet` as part of `AcpSession` struct and use the same instance for all turns.

**File:** `src/acp.rs:256-286`
```rust
pub struct AcpSession {
    // ... other fields
    local_set: tokio::task::LocalSet,  // Now lives as long as the session
    conn: Arc<Mutex<Option<acp::ClientSideConnection>>>,  // Wrapped for access
}
```

### 2. CLAUDECODE Nested Session Issue (FIXED)
**Problem:** ACP adapter refused to run when `CLAUDECODE` env var was set.

**Solution:** Unset `CLAUDECODE` and `CLAUDE_CODE_ENTRY_POINT` before spawning ACP adapter.

**File:** `src/acp.rs:314-315, 580-581`
```rust
.env("CLAUDECODE", "")
.env("CLAUDE_CODE_ENTRY_POINT", "")
```

## Current State

### Test Configuration
`/tmp/velor-test-auto-glob.toml`:
```toml
[defaults]
protocol = "acp"
binary = "claude-agent-acp"
iterations = 1

[defaults.acp]
api_key_env = "ZAI_API_KEY"
permission_mode = "allow"

[rules]
enabled = true
directory = ".agents/rules"
max_mid_iteration_injections = 2

[prompts]
test-read-file = """
Read the file CLAUDE.md and tell me the first line of it.
This should trigger the glob-based rule injection for .md files.
"""
```

### Test Rule
`.agents/rules/glob-test.mdc`:
```yaml
---
description: Test rule for Markdown files - verify glob-based injection works
globs:
  - "**/*.md"
  - "**/*.markdown"
alwaysApply: false
---
# Markdown File Rule (TEST)
This rule should ONLY appear when you open Markdown files.
```

## Next Steps to Complete Implementation

### Option 1: Disable MCP File System Tools
Find out how to disable built-in MCP tools in `claude-agent-acp` to force the agent to use ACP callbacks.

**Research needed:**
- Check `claude-agent-acp` documentation/flags for disabling MCP
- Look for session configuration options

### Option 2: Implement MCP Tool Interception
Intercept MCP tool calls through a different mechanism.

**Approaches:**
1. Check if ACP SDK has MCP-specific callbacks
2. Implement custom MCP server that proxies file reads
3. Use ACP's `mcp_servers` parameter to inject a custom server

### Option 3: Parse Agent Output for File Reads
Add pattern matching to detect when agent reads files based on its output.

**Pros:** Works regardless of how agent accesses files
**Cons:** Unreliable, fragile to agent's response format

### Option 4: Use Different Agent Adapter
Consider using a different ACP adapter without built-in MCP tools.

## Files Modified

1. **`src/acp.rs`** - Fixed LocalSet persistence and CLAUDECODE issues
2. **`src/main.rs`** - Added debug logging for file reads
3. **`.agents/rules/glob-test.mdc`** - Created test rule for .md files

## Test Commands

```bash
# Run auto mode test
RUST_LOG=velor=info ./target/debug/velor auto --config /tmp/velor-test-auto-glob.toml --prompt test-read-file

# Run unit test for glob matching
cargo test test_check_new_glob_matches_with_tracing_md

# Build
cargo build

# Full test suite
cargo nextest run
```

## Key Code Locations

- **ACP Session:** `src/acp.rs:256-550`
- **File read tracking:** `src/acp.rs:115-140`
- **Glob matching logic:** `src/rules.rs:764-785`
- **Auto iteration loop:** `src/main.rs:1150-1300`
- **Mid-iteration injection:** `src/main.rs:1211-1250`

## Debugging Tips

1. **Check if files are being tracked:** Look for `Turn A completed: files_read=N` in logs
2. **Check for extension methods:** Look for `🔧 Agent called extension method` logs
3. **Verify glob matching:** Run `test_check_new_glob_matches_with_tracing_md` unit test
4. **Session lifecycle:** Look for `🤖 Starting ACP session` and `✅ ACP session established` messages

## Contact/Questions

- The rules discovery and injection framework is solid and well-tested
- The issue is specifically with **file read tracking** through ACP callbacks
- Need to either disable MCP tools or implement MCP-level interception
- All unit tests pass (227/227)
