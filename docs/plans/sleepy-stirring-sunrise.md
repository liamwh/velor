# Add `velor test-notification` Command

## Context

The macOS notification feature was just implemented. The user wants a CLI command to easily test that notifications are working correctly. This is useful for:
- Verifying notification configuration is correct
- Testing on a real macOS machine (CI can't test GUI notifications)
- Debugging notification issues

## Implementation

### 1. `src/main.rs` - Add CLI subcommand

Add `TestNotification` variant to the `Commands` enum:

```rust
#[derive(Debug, Subcommand)]
enum Commands {
    // ... existing commands ...

    /// Send a test notification to verify notification configuration
    TestNotification,
}
```

Add dispatch in `main()`:

```rust
Some(Commands::TestNotification) => run_test_notification(&merged_cfg),
```

Add `run_test_notification` function:

```rust
#[tracing::instrument(level = "debug", ret, err)]
fn run_test_notification(config: &FileConfig) -> color_eyre::eyre::Result<()> {
    use crate::notification::{build_notifiers, send_notifications, NotificationPayload, RunStatus};
    use std::time::Duration;

    let notifiers = build_notifiers(&config.notifications)?;

    if notifiers.is_empty() {
        println!("No notifications enabled. Configure [notifications.telegram] or [notifications.macos] in velor.toml");
        return Ok(());
    }

    let payload = NotificationPayload {
        mode: "test",
        iterations_completed: 1,
        max_iterations: 1,
        duration: Duration::from_secs(0),
        status: RunStatus::Completed,
        output_preview: Some("This is a test notification from velor.".to_string()),
        prompt_name: "test-notification".to_string(),
    };

    println!("Sending test notification via: {}",
        notifiers.iter().map(|n| n.name()).collect::<Vec<_>>().join(", "));

    send_notifications(&notifiers, &payload);

    println!("Test notification sent!");
    Ok(())
}
```

### 2. `justfile` - Add convenience command

```just
# Test notification configuration
test-notification:
    cargo build -q && ./target/debug/velor test-notification
```

## Files to Modify

1. **`src/main.rs`** - Add `TestNotification` command variant and handler function
2. **`justfile`** - Add `test-notification` recipe

## Verification

1. Run `cargo check -q` to verify compilation
2. Run `cargo nextest run` to verify tests still pass
3. Test manually:
   ```bash
   # Add to velor.toml:
   # [notifications.macos]
   # enabled = true

   cargo run -- test-notification
   ```
4. Verify notification appears in macOS Notification Center

## Notes

- No additional dependencies needed
- Works with both Telegram and macOS notifiers
- Gracefully handles case when no notifiers are configured
- Uses existing `send_notifications` which logs errors but doesn't fail
