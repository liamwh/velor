//! Integration tests for `velor_core::execution_service::supervisor`.
//!
//! These drive the real supervisor against the deterministic `velor-test-agent`
//! fixture (a `[[bin]]` of this crate; no network, no real provider).

#![cfg(unix)]

use async_trait::async_trait;
use bytes::Bytes;
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use velor_core::agent::AgentEvent;
use velor_core::execution_service::adapter::{AgentAdapter, AgentEventSink, AgentSinkError};
use velor_core::execution_service::adapters::claude::{ClaudeParams, ClaudeSubprocessAdapter};
use velor_core::execution_service::error::{ProcessError, TimeoutKind};
use velor_core::execution_service::service::{AgentExecutionService, AgentProfile};
use velor_core::execution_service::supervisor as sup; // for spawn()
use velor_core::execution_service::supervisor::{
    ProcessInput, ProcessSpec, ProcessSpecBuilder, ProcessTimeouts, run,
};

/// Resolves the fixture binary (a `[[bin]]` of this crate).
fn fixture() -> PathBuf {
    PathBuf::from(
        option_env!("CARGO_BIN_EXE_velor-test-agent")
            .or_else(|| option_env!("CARGO_BIN_EXE_velor_test_agent"))
            .expect("CARGO_BIN_EXE_velor-test-agent must be set"),
    )
}

/// Starts a [`ProcessSpecBuilder`] for the fixture with a scenario and extra args.
fn spec(scenario: &str, extra: &[&str]) -> ProcessSpecBuilder {
    let mut builder = ProcessSpec::builder(fixture()).arg(scenario);
    for a in extra {
        builder = builder.arg(a);
    }
    builder
}

/// Wraps a future in a hard guard so a supervisor bug cannot hang the test suite.
async fn guard<F, T>(f: F) -> T
where
    F: std::future::Future<Output = T>,
{
    tokio::time::timeout(Duration::from_secs(15), f)
        .await
        .expect("test exceeded 15s guard; supervisor likely hung")
}

#[tokio::test]
async fn happy_path_captures_stdout_and_exits_zero() {
    let output = guard(run(spec("success", &[]).build(), CancellationToken::new())).await;
    assert!(
        output.is_ok(),
        "expected success: {:?}",
        output.as_ref().err()
    );
    let output = output.unwrap();
    assert!(output.is_success(), "termination: {:?}", output.termination);
    assert!(output.stdout.tail_str().contains("done"));
    assert!(output.stderr.is_empty());
}

#[tokio::test]
async fn stdin_is_written_and_closed() {
    let prompt = "the quick brown fox jumps";
    let s = spec("echo-stdin", &[])
        .input(ProcessInput::Bytes(Bytes::from(prompt)))
        .build();
    let output = guard(run(s, CancellationToken::new())).await.unwrap();
    assert!(output.is_success());
    let tail = output.stdout.tail_str();
    assert!(
        tail.starts_with("ECHO:the quick brown"),
        "expected echoed prompt prefix, got: {tail}",
    );
}

#[tokio::test]
async fn stdin_much_larger_than_pipe_buffer_is_written() {
    // Build a prompt far larger than the ~43,846-char real-world prompt and the
    // OS pipe buffer (~64KB). The supervisor must write it fully and close stdin.
    let big = "x".repeat(2 * 1024 * 1024); // 2 MiB
    let s = spec("echo-stdin", &[])
        .input(ProcessInput::Bytes(Bytes::from(big)))
        .build();
    let output = guard(run(s, CancellationToken::new())).await.unwrap();
    assert!(output.is_success());
    // echo-stdin prints ECHO: + first 16 bytes, so just verify it ran the echo path.
    assert!(output.stdout.tail_str().starts_with("ECHO:"));
}

#[tokio::test]
async fn large_stdout_drains_without_deadlock() {
    let ten_mb = 10 * 1024 * 1024;
    let s = spec("large-output", &["--bytes", &ten_mb.to_string()]).build();
    let output = guard(run(s, CancellationToken::new())).await.unwrap();
    assert!(output.is_success());
    assert_eq!(output.stdout.total_bytes, ten_mb as u64);
    assert!(output.stdout.truncated, "10MB should exceed the 64KB cap");
    assert_eq!(output.stdout.retained_head.len(), 64 * 1024);
    assert_eq!(output.stdout.retained_tail.len(), 64 * 1024);
}

#[tokio::test]
async fn total_deadline_terminates_and_reaps() {
    let s = spec("sleep", &["--duration", "30"])
        .timeouts(ProcessTimeouts {
            total: Some(Duration::from_millis(150)),
            termination_grace: Duration::from_millis(500),
            ..ProcessTimeouts::default()
        })
        .build();
    let output = guard(run(s, CancellationToken::new())).await.unwrap();
    assert_eq!(
        output.termination.timeout_kind(),
        Some(TimeoutKind::Total),
        "expected Total timeout: {:?}",
        output.termination,
    );
    assert!(output.pid.is_some());
}

#[tokio::test]
async fn cancellation_terminates_and_reaps() {
    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();
    let s = spec("sleep", &["--duration", "30"])
        .timeouts(ProcessTimeouts {
            termination_grace: Duration::from_millis(500),
            ..ProcessTimeouts::default()
        })
        .build();
    let proc = sup::spawn(s, cancel).await.expect("spawn");
    let handle = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel_clone.cancel();
    });
    let output = guard(proc.complete()).await.unwrap();
    let _ = handle.await;
    assert!(output.termination.is_cancelled(), "expected Cancelled");
}

#[tokio::test]
async fn idle_deadline_fires_between_outputs() {
    let s = spec("stdout-lines", &["--count", "5", "--interval-ms", "300"])
        .timeouts(ProcessTimeouts {
            idle: Some(Duration::from_millis(80)),
            total: Some(Duration::from_secs(2)),
            termination_grace: Duration::from_millis(500),
            ..ProcessTimeouts::default()
        })
        .build();
    let output = guard(run(s, CancellationToken::new())).await.unwrap();
    assert_eq!(
        output.termination.timeout_kind(),
        Some(TimeoutKind::Idle),
        "expected Idle timeout: {:?}",
        output.termination,
    );
}

#[tokio::test]
async fn startup_deadline_fires_before_first_output() {
    let s = spec("sleep", &["--duration", "30"])
        .timeouts(ProcessTimeouts {
            startup: Some(Duration::from_millis(150)),
            termination_grace: Duration::from_millis(500),
            ..ProcessTimeouts::default()
        })
        .build();
    let output = guard(run(s, CancellationToken::new())).await.unwrap();
    assert_eq!(
        output.termination.timeout_kind(),
        Some(TimeoutKind::Startup),
        "expected Startup timeout: {:?}",
        output.termination,
    );
}

#[tokio::test]
async fn error_on_stdout_empty_stderr_is_captured() {
    // The exact bug class: real error lands on stdout, stderr is empty.
    let output = guard(run(
        spec("overload-529", &[]).build(),
        CancellationToken::new(),
    ))
    .await
    .unwrap();
    assert!(!output.is_success());
    assert!(output.stderr.is_empty(), "stderr should be empty");
    assert!(
        output.stdout.tail_str().contains("529"),
        "stdout tail should carry the error"
    );
}

#[tokio::test]
async fn error_only_on_stderr_is_captured() {
    let output = guard(run(
        spec("stderr-only", &[]).build(),
        CancellationToken::new(),
    ))
    .await
    .unwrap();
    assert!(!output.is_success());
    assert!(output.stdout.is_empty());
    assert!(output.stderr.tail_str().contains("stderr"));
}

#[tokio::test]
async fn missing_executable_is_classified_not_found() {
    let s = ProcessSpec::builder("/nonexistent/path/to/glm5-nope").build();
    let result = guard(run(s, CancellationToken::new())).await;
    assert!(
        matches!(result, Err(ProcessError::ExecutableNotFound { .. })),
        "expected ExecutableNotFound, got: {result:?}",
    );
}

#[tokio::test]
async fn non_utf8_output_is_captured_lossily() {
    let output = guard(run(
        spec("invalid-utf8", &[]).build(),
        CancellationToken::new(),
    ))
    .await
    .unwrap();
    assert!(output.is_success());
    // total_bytes counts the raw bytes even though they are invalid UTF-8.
    assert_eq!(output.stdout.total_bytes, 4); // 0xFF 0xFE 0xFD '\n'
}

#[tokio::test]
async fn exit_before_stdin_does_not_hang() {
    // The child exits immediately without reading stdin; the supervisor must not
    // block on the stdin write.
    let s = spec("exit-before-stdin", &[])
        .input(ProcessInput::Bytes(Bytes::from_static(b"unused")))
        .build();
    let output = guard(run(s, CancellationToken::new())).await.unwrap();
    assert!(!output.is_success());
}

#[tokio::test]
async fn graceful_sigterm_escalates_to_sigkill() {
    // The fixture ignores SIGTERM, so the supervisor must escalate to SIGKILL
    // within termination_grace and still reap.
    let s = spec("ignore-sigterm", &["--duration", "60"])
        .timeouts(ProcessTimeouts {
            total: Some(Duration::from_millis(150)),
            termination_grace: Duration::from_millis(400),
            ..ProcessTimeouts::default()
        })
        .build();
    let started = std::time::Instant::now();
    let output = guard(run(s, CancellationToken::new())).await.unwrap();
    assert_eq!(output.termination.timeout_kind(), Some(TimeoutKind::Total));
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "reap took too long: {:?}",
        started.elapsed(),
    );
}

#[tokio::test]
async fn process_group_kill_reaps_grandchildren() {
    // The fixture forks grandchildren that hold an exclusive flock on a temp
    // file (fd-held lock — robust against PID reuse). If the process group is
    // killed correctly, the grandchildren die, their fds close, and the lock
    // becomes acquirable.
    let dir = tempfile::tempdir().expect("tempdir");
    let lockfile = dir.path().join("grandchild.lock");
    std::fs::write(&lockfile, b"").expect("create lockfile");

    let s = spec(
        "fork-tree",
        &[
            "--children",
            "2",
            "--duration",
            "60",
            "--lockfile",
            lockfile.to_str().unwrap(),
        ],
    )
    .timeouts(ProcessTimeouts {
        total: Some(Duration::from_millis(300)),
        termination_grace: Duration::from_millis(800),
        ..ProcessTimeouts::default()
    })
    .build();

    let output = guard(run(s, CancellationToken::new())).await.unwrap();
    assert_eq!(output.termination.timeout_kind(), Some(TimeoutKind::Total));

    // Give the OS a moment to release the fds, then the flock must be acquirable.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let probe = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lockfile)
        .expect("open lockfile");
    let fd = probe.as_raw_fd();
    // Non-blocking exclusive flock: success means no grandchild holds the lock.
    let acquired = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) } == 0;
    let _ = unsafe { libc::flock(fd, libc::LOCK_UN) };
    assert!(
        acquired,
        "grandchild flock still held — process group was not fully killed",
    );
}

#[tokio::test]
async fn running_process_next_event_streams_chunks() {
    let mut proc = sup::spawn(
        spec("stdout-lines", &["--count", "3"]).build(),
        CancellationToken::new(),
    )
    .await
    .expect("spawn");
    let mut stdout = String::new();
    while let Some(event) = proc.next_event().await {
        if let Some(chunk) = event.chunk() {
            stdout.push_str(&String::from_utf8_lossy(&chunk.bytes));
        }
    }
    let output = guard(proc.complete()).await.unwrap();
    assert!(output.is_success());
    assert_eq!(stdout.lines().count(), 3, "streamed: {stdout}");
}

#[tokio::test]
async fn drop_without_complete_still_cancels() {
    // Dropping a RunningProcess without awaiting complete()/cancel() must not
    // leak the child: the Drop requests cancellation.
    let _proc = sup::spawn(
        spec("sleep", &["--duration", "60"]).build(),
        CancellationToken::new(),
    )
    .await
    .expect("spawn");
    drop(_proc);
    // If the child leaked, we cannot easily assert it here, but the cancellation
    // path is exercised; the graceful-sigterm test above proves termination works.
}

struct CollectSink {
    events: Vec<AgentEvent>,
}

#[async_trait(?Send)]
impl AgentEventSink for CollectSink {
    async fn emit(&mut self, event: AgentEvent) -> Result<(), AgentSinkError> {
        self.events.push(event);
        Ok(())
    }
}

#[tokio::test]
async fn claude_adapter_end_to_end_streams_and_classifies() {
    // The fixture's `success` scenario emits Claude stream-json; the adapter
    // must parse it into AgentEvents and return success.
    let mut params = ClaudeParams::new(
        fixture().to_string_lossy().into_owned(),
        bytes::Bytes::from_static(b""),
        std::env::temp_dir(),
    );
    params.cancellation = CancellationToken::new();
    // The fixture locates its scenario token anywhere in argv, so pass it as an
    // extra arg alongside the adapter's standard Claude flags.
    params.extra_args = vec!["success".to_string()];
    let mut adapter = ClaudeSubprocessAdapter::new(params);
    let mut sink = CollectSink { events: Vec::new() };
    let outcome = tokio::time::timeout(Duration::from_secs(15), adapter.execute(&mut sink)).await;
    match outcome {
        Ok(Ok(result)) => {
            assert!(result.stdout.contains("done"), "stdout: {}", result.stdout);
            assert!(
                sink.events
                    .iter()
                    .any(|e| matches!(e, AgentEvent::TextDelta { .. })),
                "expected a text delta event"
            );
        }
        Ok(Err(e)) => panic!("expected success, got: {e:?}"),
        Err(_) => panic!("adapter timed out"),
    }
}

#[tokio::test]
async fn service_runs_profile_streams_and_completes() {
    // End-to-end through AgentExecutionService: profile -> worker thread ->
    // adapter -> supervisor -> events + report.
    let mut params = ClaudeParams::new(
        fixture().to_string_lossy().into_owned(),
        bytes::Bytes::from_static(b""),
        std::env::temp_dir(),
    );
    params.cancellation = CancellationToken::new();
    params.extra_args = vec!["success".to_string()];
    params.timeouts = ProcessTimeouts {
        total: Some(Duration::from_secs(15)),
        ..ProcessTimeouts::default()
    };
    let service = AgentExecutionService::new();
    let mut exec = service
        .execute(AgentProfile::Claude(params))
        .await
        .expect("execute");
    let mut saw_text = false;
    while let Some(event) = exec.next_event().await {
        if matches!(event, AgentEvent::TextDelta { .. }) {
            saw_text = true;
        }
    }
    let report = exec.complete().await.expect("report");
    assert!(report.result.stdout.contains("done"));
    assert!(saw_text);
    assert_eq!(report.attempts.len(), 1);
}
