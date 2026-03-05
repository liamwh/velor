# Plan: Display Tool Calls in Velor Output

## Context

When running Velor with the glm4 binary, users see text outputs from Claude but tool calls (Read, Bash, Edit, etc.) are silently executed without visual feedback. This makes it difficult to understand what actions the agent is taking.

Currently, Velor parses Claude's `stream-json` output but only extracts **text content** via the `extract_text_chunk()` function in `/Users/liam/git/velor/crates/velor-core/src/agent.rs`. Tool calls are present in the JSON stream but are ignored.

## Stream-JSON Format Reference

Based on [Claude stream-json cheatsheet](https://takopi.dev/reference/runners/claude/stream-json-cheatsheet/), the format is:

**Top-level event types:**
- `system` - init events (session_id, tools, cwd, model)
- `assistant` - assistant messages containing content blocks
- `user` - user messages (tool results)
- `result` - final results

**Tool use structure (assistant event):**
```json
{
  "type": "assistant",
  "message": {
    "content": [
      {
        "type": "tool_use",
        "id": "toolu_1",
        "name": "Bash",
        "input": {"command": "ls -la"}
      }
    ]
  }
}
```

**Tool result structure (user event):**
```json
{
  "type": "user",
  "message": {
    "content": [
      {
        "type": "tool_result",
        "tool_use_id": "toolu_1",
        "content": "output text"
      }
    ]
  }
}
```

## Goal

Display tool calls to the user when they are invoked by the agent, showing:
- Tool name (e.g., `Read`, `Bash`, `Edit`)
- Key parameters (file path, command, etc.)

**Display format:** Compact log - tool calls on their own lines with minimal detail (e.g., `🔧 Read: src/main.rs`)

## Implementation Approach

### Phase 1: Typesafe Event Parsing (subprocess mode)

**File: `/Users/liam/git/velor/crates/velor-core/src/agent.rs`**

Instead of ad-hoc JSON parsing, define proper Rust types for the stream-json events:

1. **Add event type definitions:**
   ```rust
   #[derive(Deserialize)]
   struct StreamEvent {
       r#type: String,  // "assistant", "user", "system", "result"
       message: Option<Message>,
   }

   #[derive(Deserialize)]
   struct Message {
       content: Vec<ContentBlock>,
   }

   #[derive(Deserialize)]
   #[serde(tag = "type")]
   enum ContentBlock {
       Text { text: String },
       ToolUse { id: String, name: String, input: serde_json::Value },
       ToolResult { tool_use_id: String, content: serde_json::Value },
   }
   ```

2. **Replace `extract_text_chunk()` with a proper event handler** that:
   - Parses each line as `StreamEvent`
   - Matches on `event.r#type` and `ContentBlock` enum
   - Extracts and displays text chunks
   - **NEW:** Detects `ContentBlock::ToolUse` and formats for display

3. **Tool call display:** Compact log on its own line
   - Format: `🔧 <ToolName>: <brief_args>`
   - Examples:
     - `🔧 Read: src/main.rs`
     - `🔧 Bash: cargo test`
     - `🔧 Glob: **/*.rs`

4. **Tool-specific formatting logic:**
   - `Read`: show `file_path` or `file_name`
   - `Bash`: show `command` (truncated if long)
   - `Glob`: show `pattern`
   - `Grep`: show `pattern` and optional `path`
   - `Edit`: show `file_path` with `(replace)` indicator
   - `Write`: show `file_path` with `(new)` indicator

### Phase 2: Add Tool Result Display (optional enhancement)

After implementing Phase 1, consider adding visual feedback for tool results:
- Format: `✓ <ToolName> result` or `✗ <ToolName> failed`
- Only show error/failure results to avoid clutter

### Phase 3: ACP Mode (deferred if not needed)

The ACP mode already logs extension methods in `ext_method()` (line 227 of acp.rs). If tool calls aren't visible, add handling for the appropriate content block type.

## Key Files to Modify

- `/Users/liam/git/velor/crates/velor-core/src/agent.rs` - Primary change location
  - Add event type definitions (structs/enums)
  - Replace `extract_text_chunk()` with typesafe event parsing
  - Add tool call formatting logic

## Verification

After implementation, run a test prompt that triggers multiple tool calls and verify:
- Tool names are displayed (e.g., `🔧 Read: src/main.rs`)
- Arguments are visible and appropriately truncated
- Text output continues to display properly
- The output flow is readable and not overwhelming
- All common tools (Read, Bash, Glob, Grep, Edit, Write) are handled

## Example Expected Output

```
I'll examine the codebase structure.
🔧 Glob: **/*.rs
🔧 Read: src/main.rs
Analyzing the main function...
🔧 Grep: pattern="test" path=src/
Found 3 test files.
🔧 Bash: cargo test
```

### Phase 2: Add Tool Call Display to ACP Mode (if needed)

**File: `/Users/liam/git/velor/crates/velor-core/src/acp.rs`**

The ACP mode handles content blocks in `session_notification` (lines 197-219). Currently it only extracts text from `ContentBlock::Text` and returns placeholders for other types.

If tool calls aren't already visible in ACP mode:
1. Add handling for `ContentBlock::ToolUse` (or equivalent) to extract and display tool information

### Phase 3: Testing

1. Run Velor with a prompt that triggers tool use
2. Verify tool calls are displayed with their names and arguments
3. Ensure text output still flows correctly alongside tool call display

## Key Files to Modify

- `/Users/liam/git/velor/crates/velor-core/src/agent.rs` - Primary change location
- `/Users/liam/git/velor/crates/velor-core/src/acp.rs` - Secondary (may already work via ext_method logging)

## Verification

After implementation, run a test prompt that triggers multiple tool calls and verify:
- Tool names are displayed (e.g., `📖 Read: path/to/file`)
- Arguments are visible
- Text output continues to display properly
- The output flow is readable and not overwhelming
