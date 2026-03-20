# Fix UTF-8 String Slicing Panic

## Context

The application crashes with a panic when displaying tool calls that contain multi-byte UTF-8 characters (like emoji '✔'). The panic occurs because Rust strings use byte indexing, but UTF-8 characters can be 1-4 bytes. When the code slices at a fixed byte position like `&command[..60]`, it may cut through a multi-byte character.

**Original crash:**
```
byte index 60 is not a char boundary; it is inside '✔' (bytes 58..61) of `just check 2>&1 | grep -E "(✓|✅|Error|error|FAIL|fail|✔|warning.*error)" | tail -30`
Location: crates/velor-core/src/agent.rs:115
```

## Affected Locations

The following locations use unsafe byte-based string slicing on arbitrary user content:

### crates/velor-core/src/agent.rs
- Line 115: `&command[..MAX_COMMAND_DISPLAY_LEN]` (Bash command display)
- Line 156: `&input_str[..MAX_COMMAND_DISPLAY_LEN]` (Generic tool input)
- Line 344: `&prompt[..200]` (Prompt preview)
- Line 440: `&stderr[..500]` (Stderr summary)
- Line 456: `&stdout[..200]` (Stdout preview)

### crates/velor-core/src/acp.rs
- Line 479: `&prompt[..200]` (Prompt preview in run_turn)
- Line 694: `&prompt[..200]` (Prompt preview in run_acp)

### crates/velor-core/src/rules.rs
- Line 934: `&task_preview[..500]` (Task preview capping)
- Line 972: `&output[..4096]` (Output capping)

## Implementation Plan

### Step 1: Add a helper function for safe UTF-8 truncation

Add to `crates/velor-core/src/agent.rs`:

```rust
/// Truncates a string to approximately `max_bytes` bytes.
///
/// Uses `floor_char_boundary` to avoid cutting through multi-byte UTF-8 sequences.
/// The actual result may be slightly shorter than `max_bytes` to ensure valid UTF-8.
fn truncate_str(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let safe_idx = s.floor_char_boundary(max_bytes);
    &s[..safe_idx]
}
```

This is cleaner than `.chars().count()` or `.char_indices().nth()` because:
- It's O(n) single-pass (scans to find the boundary)
- It uses the built-in `floor_char_boundary` method (stable since Rust 1.60)
- It keeps the limit in bytes (closer to the original intent)

### Step 2: Update all slicing sites in agent.rs

Replace unsafe byte slices with the helper:

```rust
// Line 115-117 (Bash command)
if command.len() > MAX_COMMAND_DISPLAY_LEN {
    format!("{}...", truncate_str(command, MAX_COMMAND_DISPLAY_LEN))
} else {
    command.to_string()
}

// Line 156 (Generic tool input)
if input_str.len() > MAX_COMMAND_DISPLAY_LEN {
    format!("{}...", truncate_str(input_str, MAX_COMMAND_DISPLAY_LEN))
} else {
    input_str.to_string()
}

// Line 344-347 (Prompt preview)
let prompt_preview = if prompt.len() > 200 {
    format!("{}... ({} chars total)", truncate_str(prompt, 200), prompt.len())
}

// Line 439-443 (Stderr summary)
let stderr_summary = if stderr.len() > 500 {
    format!("{}...", truncate_str(stderr, 500))
} else {
    stderr.clone()
};

// Line 455-459 (Stdout preview)
let stdout_preview = if stdout.len() > 200 {
    format!("{}... ({} chars total)", truncate_str(stdout, 200), stdout.len())
}
```

### Step 3: Update acp.rs

Add the same helper function (or export it from agent.rs) and update lines 479 and 694 similarly.

### Step 4: Update rules.rs

Add the helper function and update lines 934 and 972 similarly.

### Step 5: Add tests

Add a test for the truncation function with multi-byte characters:

```rust
#[test]
fn truncate_str_handles_multi_byte_chars() {
    // "✔" is 3 bytes, "🌍" is 4 bytes
    let s = "Hello ✔ World 🌍 Test";

    // Truncate to ~12 bytes (should include the ✔ fully)
    let truncated = truncate_str(s, 12);
    assert!(truncated.ends_with('✔'));
    assert!(!truncated.contains("World"));

    // No truncation when under limit
    assert_eq!(truncate_str(s, 1000), s);

    // Edge case: truncate in middle of multi-byte char
    // Byte 7 is in the middle of "✔" (bytes 6-8)
    let result = truncate_str(s, 7);
    assert_eq!(result, "Hello "); // Should stop at the boundary before ✔
}
```

## Verification

1. Run `cargo check -q` to ensure no compilation errors
2. Run `cargo test` to ensure all tests pass
3. Test with the original failing command: `just check 2>&1 | tail -100`
4. Add a specific test case that reproduces the original crash scenario

## Notes

- Lines that slice ASCII-only content (like hex strings in `velor-vault/src/keyring.rs:73` and ULIDs in `automations/src/runner.rs:190`) are safe and do not need changes.
- The `.chars().count()` approach is O(n) but acceptable for short strings used for display purposes.
