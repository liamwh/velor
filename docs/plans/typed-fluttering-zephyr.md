# Notification Improvements for Cancelled Runs

## Context

The current notification system has two issues:

1. **Notifications are sent on Ctrl+C cancellation**: When a user cancels a run with Ctrl+C, a notification is still being sent (if `notify_on_failure` is enabled). This is undesirable since cancellation is a deliberate user action, not a failure.

2. **Notifications show the start of the conversation instead of the end**: The output preview truncates to show the first N characters, but users want to see the last N characters (the summary/end of the conversation).

## Changes

### 1. Don't Send Notifications on Cancellation

**File**: `crates/velor-core/src/notification.rs`

**Change**: Modify the `should_notify` function (line 479) to return `false` for `RunStatus::Cancelled`.

**Current code**:
```rust
match status {
    RunStatus::Completed => config.notify_on_success,
    RunStatus::MaxIterationsReached => config.notify_on_max_iterations,
    RunStatus::Failed => config.notify_on_failure,
    RunStatus::Cancelled => config.notify_on_failure,  // <-- Change this
}
```

**New code**:
```rust
match status {
    RunStatus::Completed => config.notify_on_success,
    RunStatus::MaxIterationsReached => config.notify_on_max_iterations,
    RunStatus::Failed => config.notify_on_failure,
    RunStatus::Cancelled => false,  // Never notify on cancellation
}
```

### 2. Send End of Conversation Instead of Start

**File**: `crates/velor-core/src/notification.rs`

**Change**: Modify the truncation logic to return the **last** N characters (suffix) instead of the first N characters (prefix).

**Current implementation**: The `truncate_str` function (lines 417-428) returns the prefix of the string.

**New implementation**: Replace the prefix-based truncation with suffix-based truncation in the message formatting functions.

**In `format_telegram_message` (line 336)**:
```rust
// Current: truncate_str(&preview, output_preview_chars as usize)
// New: Take the last N characters instead

let preview = if preview.len() > output_preview_chars as usize {
    // Find char boundary and take suffix
    let start = preview.len() - output_preview_chars as usize;
    let mut start = start;
    while !preview.is_char_boundary(start) && start < preview.len() {
        start += 1;
    }
    &preview[start..]
} else {
    preview
};
```

**Similarly in `format_macos_message` (line 398)**: Apply the same suffix-based logic.

### Helper Function Refactoring

For cleaner code, create a new helper function to replace the prefix-based `truncate_str`:

```rust
/// Takes the last `max_len` characters from a string, respecting character boundaries.
#[must_use]
fn take_suffix(s: &str, max_len: usize) -> &str {
    if s.len() <= max_len {
        return s;
    }

    // Find the smallest valid char boundary at (len - max_len)
    let mut start = s.len() - max_len;
    while !s.is_char_boundary(start) && start < s.len() {
        start += 1;
    }
    &s[start..]
}
```

## Critical Files

- `crates/velor-core/src/notification.rs` - Main notification logic
  - `should_notify()` function (line 470)
  - `format_telegram_message()` function (line 313)
  - `format_macos_message()` function (line 384)
  - `truncate_str()` helper (line 417) - replace or create new `take_suffix()`

## Testing

1. Test that Ctrl+C cancellation does NOT send a notification
   - Run a long auto mode
   - Press Ctrl+C to cancel
   - Verify no notification is sent

2. Test that the Telegram notification shows the END of the conversation
   - Run an agent that produces long output
   - Check the notification
   - Verify the preview contains the last N characters (the conclusion), not the first N characters

3. Verify existing tests still pass
   - Run `cargo test -p velor-core`
   - Check notification-related tests

4. Update test expectations for `truncate_str` or create new tests for `take_suffix`
