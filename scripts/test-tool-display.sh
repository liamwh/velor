#!/bin/bash
# Test script to verify tool call display in Velor output

set -e

echo "========================================="
echo "Testing Tool Call Display in Velor"
echo "========================================="
echo ""

# Test prompt that should trigger multiple tool calls
TEST_PROMPT="Please do the following tasks in the velor codebase:
1. Use Glob to find all Rust files in the crates/velor-core directory
2. Use Grep to search for 'fn process_stream_line' in the agent.rs file
3. Read the agent.rs file to understand the structure
4. List the files in the current directory using Bash

After each tool use, briefly describe what you found. Keep your responses concise."

echo "Running test prompt..."
echo "Prompt: $TEST_PROMPT"
echo ""
echo "========================================="
echo "Expected Output:"
echo "  - Tool calls should display with 🔧 emoji prefix"
echo "  - Format: 🔧 <ToolName>: <args>"
echo "  - Examples:"
echo "    🔧 Glob: crates/velor-core/**/*.rs"
echo "    🔧 Grep: pattern=\"fn process_stream_line\""
echo "    🔧 Read: crates/velor-core/src/agent.rs"
echo "    🔧 Bash: ls -la"
echo "========================================="
echo ""

# Run velor with the test prompt
# Using --dry-run first to see what would be sent, then actual run
echo "Running: velor once --prompt-text \"\$TEST_PROMPT\""
echo ""
echo "Press Ctrl+C to cancel, or wait for output..."
echo ""

# Run the actual test
cargo build --release -q --bin velor-cli 2>/dev/null || true
./target/release/velor-cli once --prompt-text "$TEST_PROMPT" 2>&1 | tee /tmp/velor-tool-test-output.txt

echo ""
echo "========================================="
echo "Test completed. Output saved to /tmp/velor-tool-test-output.txt"
echo "========================================="
echo ""
echo "Verification:"
echo "Checking for 🔧 emoji in output..."

if grep -q "🔧" /tmp/velor-tool-test-output.txt; then
    echo "✅ SUCCESS: Tool calls with 🔧 emoji found in output"
    echo ""
    echo "Tool calls found:"
    grep "🔧" /tmp/velor-tool-test-output.txt | head -20
else
    echo "❌ FAILURE: No tool calls with 🔧 emoji found in output"
    echo ""
    echo "Output preview (first 50 lines):"
    head -50 /tmp/velor-tool-test-output.txt
fi
