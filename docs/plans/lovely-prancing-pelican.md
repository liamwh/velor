# Plan: Resolve ACP TODOs

## Context

The ACP (Agent Client Protocol) integration in velor has 3 TODOs that need to be resolved:

1. **Callback mechanism not implemented** (`src/acp.rs:175-177`) - The `_on_chunk` parameter is passed to `run_acp()` but never used; agent output is only logged instead of being collected and returned
2. **Callback storage TODO** (`src/acp.rs:199-205`) - The `tokio::task_local!` storage for callbacks is defined but not used
3. **Hardcoded 5-second sleep for completion** (`src/acp.rs:340-343`) - Uses `tokio::time::sleep()` instead of proper async completion detection

The core issue is that `conn.prompt()` returns a `PromptResponse` containing a `stop_reason` field, but the current code ignores this and just sleeps for 5 seconds before killing the child process.

## Files to Modify

- `src/acp.rs` - Main file containing all TODOs

## Implementation Plan

### 1. Add Output Collection to `VelorClient`

**Problem:** The `VelorClient` struct cannot currently access the output collection mechanism.

**Solution:** Add an `Arc<Mutex<String>>` field to `VelorClient` to collect output chunks.

```rust
struct VelorClient {
    permission_mode: PermissionMode,
    output: Arc<Mutex<String>>,  // NEW: Collect output chunks
}
```

Update `VelorClient::new()` and the constructor call in `run_acp()`.

### 2. Implement Callback in `session_notification`

**Problem:** Currently logs output with `tracing::info!()` instead of collecting it.

**Solution:** Append chunks to the shared output buffer.

```rust
async fn session_notification(&self, args: acp::SessionNotification) -> acp::Result<()> {
    if let acp::SessionUpdate::AgentMessageChunk(chunk) = args.update {
        let text = match &chunk.content {
            acp::ContentBlock::Text(text_content) => text_content.text.clone(),
            // ... other variants ...
        };
        let mut output = self.output.lock().map_err(|_| acp::Error::internal_error())?;
        output.push_str(&text);
    }
    Ok(())
}
```

### 3. Fix Completion Detection

**Problem:** Uses 5-second sleep instead of waiting for the actual prompt response.

**Solution:** Capture and use the `PromptResponse` returned by `conn.prompt()`.

```rust
// Before (lines 329-344):
conn.prompt(...).await?;
tokio::time::sleep(Duration::from_secs(5)).await; // TODO!

// After:
let prompt_response = conn.prompt(...).await?;
tracing::info!("Agent completed with stop_reason: {:?}", prompt_response.stop_reason());
// No sleep needed - prompt() returns when agent is done!
```

### 4. Return Collected Output

**Problem:** `AcpRunResult.stdout` currently returns a placeholder message.

**Solution:** Clone the collected output and return it.

```rust
// After local_set.run_until() completes:
let output = Arc::try_unwrap(client.output)
    .map_err(|_| eyre!("Failed to unwrap Arc"))?
    .into_inner();
Ok(AcpRunResult { stdout: output })
```

### 5. Remove Unused Code

**Delete:**
- The unused `tokio::task_local! { static CURRENT_CALLBACK: ... }` block (lines 197-207)
- The unused `ChunkCallback` type alias (line 22)
- The `_on_chunk` parameter from `run_acp()` signature

### 6. Update `AgentRunner::run` Signature

Since `_on_chunk` is being removed, update the call site in `src/claude.rs`:

```rust
// Change line 94 from:
acp::run_acp(binary, prompt, prompt_name, config, cwd, on_chunk).await?;

// To:
acp::run_acp(binary, prompt, prompt_name, config, cwd).await?;
```

## Verification

### Manual Testing

Run the ACP test command and verify:
1. Agent response is returned in the result (not just logged)
2. Command completes quickly (no 5-second delay)
3. `stop_reason` is logged for debugging

```bash
unset CLAUDECODE && RUST_LOG=info ./target/debug/velor once \
  --config .velor/velor-acp-test.toml \
  --prompt-text "What is 2+2? Answer with just the number."
```

Expected output should show:
- `Agent completed with stop_reason: EndTurn` (or similar)
- The result contains "4"
- No 5-second wait

### Automated Tests

The existing tests in `src/acp.rs` should continue to pass:
- `cargo test --package velor-agent-cli acp` should pass
- Integration tests may need updates if they expect the old signature

## Edge Cases Considered

1. **Empty output:** Agent sends no message chunks → returns empty string
2. **Non-text content:** Images, audio → logs placeholder but doesn't crash
3. **Agent error:** Prompt call returns error → propagates correctly via `?`
4. **Multiple chunks:** All chunks are concatenated in order
5. **Arc unwrap failure:** Returns descriptive error if Arc is still shared

## Notes

- The `tracing::info!("Agent output: {}", text)` logging will be removed since output is now properly returned
- This change is **not** a breaking change for the public API - `AgentRunner::run()` still returns `ClaudeRunResult` with `stdout: String`
- The 5-second sleep was a workaround that sometimes caused issues (too long for quick responses, too short for complex ones)
