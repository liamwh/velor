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

### Status
- **Phase 1 (subprocess mode):** COMPLETE
- **Phase 2 (ACP mode):** COMPLETE
- **Phase 3 (Testing):** NOT STARTED

## What's Next

### Recommended Next Task

**Phase 3: End-to-End Testing**

Run a test prompt with multiple tool calls in both subprocess and ACP modes to verify:
- Tool names are displayed with `🔧` prefix
- Text output continues to display properly
- The output flow is readable and not overwhelming

## Blockers / Open Questions

None identified.

## Verification

- All tests pass
- `cargo check -q` passes (no warnings)
- `just check` passes (Svelte warnings unrelated)
- Tool calls now display in both subprocess mode (via stdout) and ACP mode (via stderr)

## Commit References

Previous session (Phase 1):
- Commit: 403391e feat(agent): display tool calls in subprocess mode output

This session (Phase 2):
- Changes to `crates/velor-core/src/acp.rs` - added eprintln for tool call display
- Progress file update
