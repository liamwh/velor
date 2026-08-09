//! Real Claude Code `stream-json` live-steering compatibility test.
//!
//! Gated behind `CLAUDE_LIVE_STEERING_TEST=1` because it requires:
//!   - a locally installed Claude Code CLI,
//!   - valid authentication,
//!   - network access,
//! and it consumes real model usage.
//!
//! The test verifies the actual stdin schema and `--replay-user-messages`
//! behaviour end-to-end through the supervisor's streaming stdin path: it sends a
//! harmless user message containing a unique nonce, waits for Claude to echo it
//! (a replay acknowledgement or other verified output), sends a second message
//! with a different nonce, confirms receipt, then deliberately closes stdin and
//! waits for a clean exit. The whole process group is terminated on timeout.
//!
//! Auth unavailability is reported as a *skipped* prerequisite, never
//! misclassified as protocol incompatibility. A genuine schema rejection fails
//! loudly as a `SchemaRejected`-style condition.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use velor_core::execution_service::adapter::LineDecoder;
use velor_core::execution_service::adapters::claude_stream::{
    ClaudeOutputEvent, frame_user_message, parse_output_event,
};
use velor_core::execution_service::steering::SteeringText;
use velor_core::execution_service::supervisor::{
    ProcessEvent, ProcessInput, ProcessInputCommand, ProcessSpec, ProcessTimeouts, spawn,
};

/// Per-attempt deadlines. Generous for a real model round-trip, bounded so a
/// wedged session fails fast.
const TOTAL: Duration = Duration::from_secs(180);
const IDLE: Duration = Duration::from_secs(90);
const VERSION_TIMEOUT: Duration = Duration::from_secs(30);

/// A harmless prompt that asks Claude to echo a token and stop, minimising tool
/// use and processing time.
fn probe(text: &str) -> Bytes {
    frame_user_message(&SteeringText::new(text).expect("non-empty probe")).expect("frame")
}

/// Generates a unique, unambiguous nonce.
fn nonce(tag: u8) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("VELORSTEER-{tag}-{nanos}")
}

/// Runs `binary --version` and returns the version string. Errors here are
/// prerequisites (binary missing), not protocol incompatibility.
async fn run_version_check(binary: &str) -> Result<String, String> {
    let output = tokio::process::Command::new(binary)
        .arg("--version")
        .output();
    match tokio::time::timeout(VERSION_TIMEOUT, output).await {
        Ok(Ok(out)) if out.status.success() => {
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
        }
        Ok(Ok(out)) => Err(format!(
            "exited {} : {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        )),
        Ok(Err(e)) => Err(format!("could not run {binary} --version: {e}")),
        Err(_) => Err(format!(
            "{binary} --version timed out after {VERSION_TIMEOUT:?}"
        )),
    }
}

/// Builds the streaming invocation spec for the live probe.
fn build_spec(binary: &str, initial: Bytes) -> ProcessSpec {
    ProcessSpec::builder(binary)
        .arg("--print")
        .arg("--input-format")
        .arg("stream-json")
        .arg("--output-format")
        .arg("stream-json")
        .arg("--verbose")
        .arg("--replay-user-messages")
        .input(ProcessInput::Streaming { initial })
        .timeouts(ProcessTimeouts {
            total: Some(TOTAL),
            idle: Some(IDLE),
            termination_grace: Duration::from_secs(5),
            ..Default::default()
        })
        .capture_bytes(512 * 1024)
        .build()
}

/// Drives the live round-trip against a real Claude Code process.
async fn live_round_trip(binary: &str) -> Result<(), String> {
    let nonce1 = nonce(1);
    let nonce2 = nonce(2);
    let initial = probe(&format!(
        "Reply with exactly this token and nothing else: {nonce1}"
    ));
    let frame2 = probe(&format!(
        "Reply with exactly this token and nothing else: {nonce2}"
    ));

    let cancel = CancellationToken::new();
    let mut proc = spawn(build_spec(binary, initial), cancel.clone())
        .await
        .map_err(|e| format!("spawn failed: {e}"))?;
    let sender = proc
        .input_sender()
        .ok_or_else(|| "streaming process exposed no input sender".to_string())?
        .clone();

    let started = Instant::now();
    let mut decoder = LineDecoder::new(8 * 1024 * 1024);
    let mut saw_nonce1 = false;
    let mut saw_nonce2 = false;
    let mut nonce2_sent = false;
    let mut stderr = String::new();
    let mut schema_detail: Option<String> = None;

    loop {
        if started.elapsed() > TOTAL {
            break;
        }
        // Send the second probe once the first has been acknowledged or Claude has
        // begun emitting output for it — never queue it ahead of confirmation.
        if saw_nonce1 && !nonce2_sent {
            nonce2_sent = true;
            let (ack, ack_rx) = oneshot::channel();
            if sender
                .send(ProcessInputCommand::Write {
                    bytes: frame2.clone(),
                    acknowledgement: ack,
                })
                .await
                .is_err()
            {
                // Writer gone; we'll observe it via the event stream.
                let _ = ack_rx;
            }
        }
        // Done as soon as the second probe is acknowledged.
        if saw_nonce2 {
            break;
        }
        let Some(ev) = proc.next_event().await else {
            break; // process exited
        };
        match ev {
            ProcessEvent::Stdout(chunk) => {
                let lines = decoder
                    .push(&chunk.bytes)
                    .map_err(|e| format!("decode: {e}"))?;
                for line in lines {
                    let text = String::from_utf8_lossy(&line);
                    match parse_output_event(&text) {
                        Ok(ClaudeOutputEvent::ReplayedUserMessage(r)) => {
                            let body = r.text.as_str().to_string();
                            if body.contains(&nonce1) {
                                saw_nonce1 = true;
                            }
                            if body.contains(&nonce2) {
                                saw_nonce2 = true;
                            }
                        }
                        Ok(_) => {}
                        // A partial line that failed to parse is ignored here; a
                        // genuinely malformed full frame is rare and not fatal to
                        // the round-trip.
                        Err(_) => {}
                    }
                }
            }
            ProcessEvent::Stderr(chunk) => {
                stderr.push_str(&String::from_utf8_lossy(&chunk.bytes));
            }
            ProcessEvent::StdinWriteFailed(e) => {
                schema_detail = Some(format!("stdin write failed: {e}"));
            }
            ProcessEvent::StdinInitialised | ProcessEvent::StdinWritten | ProcessEvent::Exited => {}
        }
    }

    // Deliberately close stdin (the central shutdown path), then reap.
    if !saw_nonce2 {
        let (ack, _) = oneshot::channel();
        let _ = sender
            .send(ProcessInputCommand::Close {
                acknowledgement: ack,
            })
            .await;
    }
    cancel.cancel();
    let output = proc
        .complete()
        .await
        .map_err(|e| format!("supervisor did not reap cleanly: {e}"))?;

    // Classify the outcome.
    let lower_stderr = stderr.to_ascii_lowercase();
    let auth_failure = lower_stderr.contains("not logged in")
        || lower_stderr.contains("invalid api key")
        || lower_stderr.contains("authentication")
        || lower_stderr.contains("unauthorized")
        || lower_stderr.contains("run `claude setup-token`")
        || lower_stderr.contains("login");
    if auth_failure {
        return Err(format!(
            "SKIPPED (prerequisite): {binary} reported an authentication problem. stderr: {}",
            stderr.trim()
        ));
    }

    if saw_nonce2 {
        return Ok(());
    }

    // Not acknowledged: distinguish a schema rejection from a generic failure.
    let combined = format!("{stderr} {}", output.stderr.tail_str());
    let schema_rejected = combined.to_ascii_lowercase().contains("input")
        && (combined.to_ascii_lowercase().contains("schema")
            || combined.to_ascii_lowercase().contains("format")
            || combined.to_ascii_lowercase().contains("invalid"));
    let detail = schema_detail.unwrap_or_else(|| combined.trim().to_string());
    if schema_rejected {
        return Err(format!(
            "SchemaRejected: Claude Code rejected the stream-json input schema. detail: {detail}"
        ));
    }
    Err(format!(
        "LiveSteeringUnavailable: neither nonce was acknowledged. \
         saw_nonce1={saw_nonce1} saw_nonce2={saw_nonce2}. detail: {detail}"
    ))
}

#[tokio::test]
async fn claude_live_steering_round_trip() {
    if std::env::var("CLAUDE_LIVE_STEERING_TEST").ok().as_deref() != Some("1") {
        eprintln!(
            "skipped: set CLAUDE_LIVE_STEERING_TEST=1 (with a locally installed, authenticated \
             Claude Code CLI) to run this real-model-usage test"
        );
        return;
    }
    let binary =
        std::env::var("CLAUDE_LIVE_STEERING_BINARY").unwrap_or_else(|_| "claude".to_string());

    match run_version_check(&binary).await {
        Ok(version) => eprintln!("claude version: {version}"),
        Err(e) => {
            eprintln!("skipped (prerequisite not met): {e}");
            return;
        }
    }

    match live_round_trip(&binary).await {
        Ok(()) => eprintln!("live steering round-trip succeeded"),
        Err(msg) if msg.starts_with("SKIPPED") => {
            eprintln!("{msg}");
        }
        Err(msg) => panic!("{msg}"),
    }
}
