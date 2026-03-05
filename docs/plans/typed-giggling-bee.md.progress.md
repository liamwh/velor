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
   - All tests pass

### Phase 2: Tool Call Display in ACP Mode - COMPLETED

**File: `/Users/liam/git/velor/crates/velor-core/src/acp.rs`**

1. **Updated `ext_method()` function:**
   - Added `eprintln!("🔧 {}", args.method)` to display tool calls to stderr
   - This provides visibility for MCP tool calls and other extension methods
   - Consistent with how tracing output is displayed (stderr)
   - Tool calls are now visible to users running in ACP mode

2. **Verification:**
   - `cargo check -q` passes (no compiler errors or warnings)
   - `just check` passes (all tests pass, Svelte warnings unrelated)

### Phase 3: Integration Testing - COMPLETED

**File: `/Users/liam/git/velor/crates/velor-core/src/agent.rs`**

1. **Added 4 new integration tests:**
   - `integration_multi_tool_conversation_sequence` - Simulates realistic conversation with multiple tools (Glob, Read, Grep) and verifies both tool calls and text are correctly extracted and formatted
   - `integration_all_common_tools_in_one_stream` - Tests all 6 common tool types (Read, Bash, Glob, Grep, Edit, Write) in a single stream and verifies emoji prefix on all
   - `integration_mixed_text_and_tools_stream` - Tests realistic interleaving of text chunks and tool calls, verifying both are captured correctly
   - `integration_long_command_truncation` - Verifies long Bash commands are truncated to 60 chars with "..." suffix

2. **Test Results:**
   - All 485 tests pass (253 in velor-core, including 31 tool call display tests)
   - `just check` passes with no new warnings

3. **Verification of Output Readability:**
   - Tool calls display with consistent `🔧 <ToolName>: <args>` format
   - Common tools (Read, Bash, Glob, Grep, Edit, Write) have specialized formatting
   - Long commands are truncated to avoid overwhelming output
   - Text and tool calls are properly separated in display

### Status
- **Phase 1 (subprocess mode):** COMPLETE
- **Phase 2 (ACP mode):** COMPLETE
- **Phase 3 (Testing):** COMPLETE

## What's Next

The plan is complete. All three phases have been implemented and tested:

1. **Subprocess mode**: Tool calls are parsed from stream-json output and displayed with `🔧` prefix
2. **ACP mode**: Tool calls are displayed via stderr in `ext_method()`
3. **Testing**: Comprehensive unit and integration tests verify the functionality

### Optional Enhancement (not implemented)

The plan mentioned an optional Phase 2 enhancement for displaying tool results (e.g., "✓ Read result" or "✗ Read failed"). This was not implemented because:
- The agent already explicitly reports tool results in its text output
- Adding separate result display would create redundant information
- The current implementation provides good visibility without clutter

## Blockers / Open Questions

None identified.

## Verification

- All 485 tests pass
- `cargo check -q` passes (no warnings)
- `just check` passes (Svelte warnings unrelated)
- Tool calls now display in both subprocess mode (via stdout) and ACP mode (via stderr)
- Integration tests verify multi-tool conversations work correctly

## Commit References

Previous sessions:
- Commit: 403391e feat(agent): display tool calls in subprocess mode output (Phase 1)
- Commit: d3eccf5 feat(acp): display tool calls in ACP mode output (Phase 2)

This session (Phase 3):
- Added 4 integration tests to `crates/velor-core/src/agent.rs`
- Created test script at `scripts/test-tool-display.sh` for manual verification
- Progress file update
