# Progress: Display Tool Calls in Velor Output

## Session Date: 2025-03-05

## What Changed (Facts)

### Phase 1: Typesafe Event Parsing (subprocess mode) - COMPLETED

**File: `/Users/liam/git/velor/crates/velor-core/src/agent.rs`**

1. **Added stream-json event type definitions:**
   - `StreamEvent` enum with variants: `Assistant`, `User`, `System`, `Result`, `ContentBlockDelta`, `ContentBlockStart`, `Unknown`
   - `Message` struct containing `Vec<ContentBlock>`
   - `ContentBlockDelta` struct for streaming text
   - `ContentBlock` enum with variants: `Text`, `ToolUse`, `ToolResult`, `Unknown`
   - `ToolCall` struct for formatted display with `format_display()` method

2. **Implemented tool call extraction and formatting:**
   - `extract_tool_call()` - extracts tool calls from assistant events
   - `format_tool_args()` - tool-specific formatting logic:
     - `Read`: shows `file_path` or `file_name`
     - `Bash`: shows `command` (truncated to 60 chars with "..." suffix)
     - `Glob`: shows `pattern`
     - `Grep`: shows `pattern` and optional `path`
     - `Edit`: shows `file_path` with `(replace)` indicator
     - `Write`: shows `file_path` with `(new)` indicator
     - Unknown tools: shows JSON input

3. **Added new stream processing function:**
   - `process_stream_line()` - processes stream-json lines returning `(Option<String>, Option<ToolCall>)`
   - Extracts both text content and tool calls from stream events
   - Falls back to legacy `extract_text_chunk()` for unhandled formats

4. **Updated stdout handler thread in `run_claude()`:**
   - Now uses `process_stream_line()` instead of `extract_text_chunk()`
   - Displays tool calls on their own lines with `🔧` emoji prefix
   - Tool calls are displayed to user but NOT included in collected output

5. **Added comprehensive tests:**
   - 16 new tests for tool call formatting (all tool types)
   - 11 new tests for stream line processing
   - All 249 tests pass

### Status
- **Phase 1 (subprocess mode):** COMPLETE
- **Phase 2 (ACP mode):** NOT STARTED
- **Phase 3 (Testing):** NOT STARTED

## What's Next

### Recommended Next Task

**Phase 2: Add Tool Call Display to ACP Mode**

The ACP mode may need similar tool call display handling. Check if tool calls are already visible via `ext_method()` logging in `/Users/liam/git/velor/crates/velor-core/src/acp.rs`. If not, add handling for `ContentBlock::ToolUse` in the `session_notification` function.

### Remaining Plan Items

1. **Phase 2:** Add Tool Call Display to ACP Mode (if needed)
2. **Phase 3:** Testing - Run a test prompt with multiple tool calls to verify output

## Blockers / Open Questions

None identified.

## Verification

- All 249 tests pass
- `just check` passes (only unrelated Svelte CSS warnings remain)
- Commit will include this progress file update

## Commit References

This session's work will be committed as a single detailed commit covering:
- New stream-json event type definitions
- Tool call extraction and formatting logic
- Updated stdout handler for subprocess mode
- Comprehensive tests for the new functionality
- Progress file update
