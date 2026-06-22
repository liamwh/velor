# Velor agent-execution substrate overhaul (GLM/Claude Code reliability)

## Context

Velor invokes the `glm5` Claude Code wrapper and fails (`API Error: 529 [1305]`,
`ECONNRESET`, exit 1, empty stderr) while a direct interactive `glm5` session
works. **Verified** root causes (from `crates/velor-core/src/agent.rs`, the
`~/bin/glm5` wrapper, and the consumer paths):

1. **Errors land on stdout, not stderr.** The wrapper (`echo "…"; exit 1`) and
   Claude Code emit API errors to **stdout**. `run_claude` (agent.rs:687-754)
   checks stderr first and, on empty stderr, prints the misleading *"binary may
   not be installed"* hint — a **classification** bug, not a missing-binary case.
2. **Backoff is 100 ms base** (main.rs:1171 → `retry.rs::calculate_backoff`) →
   observed 200 ms/400 ms retries.
3. **Duplicate logging.** `AgentRunner::run` (agent.rs:362) **and** `run_claude`
   (553)/`run_codex` (765)/`run_codex_with_events` (820) all carry
   `#[instrument(..., ret, err)]`; since `run` calls `run_claude`, every error is
   emitted twice, both attributed to `velor_core::agent`.
4. **Core path: no timeout/cancellation/group-kill.** `run_claude` is sync
   `std::process::Command` in `spawn_blocking`; `child.wait()` blocks forever, and
   `spawn_blocking` can't be cancelled — so serve.rs's `tokio::time::timeout`
   (serve.rs:3053) leaves the child **and its grandchildren** running → overlapping
   requests against an overloaded provider.
5. **Errors are flat `eyre!` strings**, forcing `is_permanent_error` (retry.rs:187)
   to re-parse strings — provider-blind matching in the orchestration layer.
6. **Execution is fragmented** across three independent implementations
   (`agent.rs::run_claude` sync; `serve.rs::run_claude_like_profile`/`run_codex_profile`
   async; `acp.rs::run_acp` JSON-RPC) each with its own parsing/lifecycle. The bug
   exists *because* the semantics diverge.

**Invocation discrepancy (honest):** interactive vs Velor differ in non-TTY
execution, stdin-delivered prompt, `--include-partial-messages`+`stream-json`, and
a single ~43,846-char prompt vs incremental context. No single cause is *proven*
without the live provider, so this ships a **diagnostic `InvocationRecord`** and a
**sanitised replay manifest** to isolate it (§Diagnostics, §Replay), and fixes the
verified defects regardless.

## Target architecture — one execution substrate

```
AgentExecutionService            ← concurrency policy + circuit state + retry orchestration
        │  execute(AgentRunRequest) → AgentExecution { events, completion }
   AgentAdapter trait             ← protocol framing + Classifier + emits AgentEvent
   ├── ClaudeAdapter  ├── CodexAdapter  ├── AcpAdapter (!Send, runs on service's LocalSet)
        │
   ProcessSupervisor              ← owns Child for its whole life; stdin writer + concurrent
        │                           stdout/stderr drain (byte chunks) + deadline + process-group
        │                           lifecycle (SIGTERM→grace→SIGKILL→reap) + no async-in-Drop
        │
   ProcessExecutionRecord → Classifier (with evidence) + diagnostics + event stream
```

All three consumers call the same service with different **event sinks**:
CLI → terminal renderer; `vel serve` → `RunnerProgressEvent` adapter; Tauri →
`ExecutionRecord` adapter. No frontend constructs a raw Claude/Codex subprocess.

### Public surface (new, in `crates/velor-core/src/execution_service/`)

```rust
pub struct AgentRunRequest {
    pub profile: AgentProfile,          // kind + binary/args/env/model/permission/acp/codex
    pub prompt: ProcessInput,           // Bytes(Bytes) | Null | Inherit
    pub working_directory: PathBuf,
    pub deadline: Option<Instant>,      // replaces wrapper timeouts
    pub cancellation: CancellationToken,// request-level token (parent of attempt/process tokens)
    pub diagnostics: DiagnosticLevel,   // Off | Human | Json
    pub timeouts: ProcessTimeouts,      // startup/stdin_write/idle/total/grace
}

pub struct AgentExecution {             // Send-safe handle
    pub events: mpsc::Receiver<AgentEvent>,
    pub completion: JoinHandle<Result<AgentRunReport, AgentExecutionError>>,
}
pub struct AgentRunReport {
    pub result: AgentRunResult,         // stdout + files_read + session
    pub attempts: Vec<AttemptRecord>,   // per-attempt timing, classification, retry decision
    pub diagnostics: Option<InvocationRecord>,
}
impl AgentExecutionService {
    pub fn new(cfg: ServiceConfig) -> Self;            // owns concurrency + circuit registries + LocalSet worker for !Send ACP
    pub async fn execute(&self, req: AgentRunRequest) -> AgentExecution;
}
```

`AgentEvent` (agent.rs:194, reused) is the one event type all consumers adapt from.
`RunnerProgressEvent` (serve.rs:1297) and the Tauri `apply_agent_event` mapping
(commands.rs:764) become thin `AgentEvent`→consumer-event adapters.

## Layered error model (`execution_service/error.rs`)

No flattening; provenance preserved. `Retryability` is derived at the layer that
owns it.

```rust
#[derive(Debug, thiserror::Error)]
pub enum AgentExecutionError {
    #[error(transparent)] Process(#[from] ProcessError),
    #[error(transparent)] Protocol(#[from] AgentProtocolError),   // framing/parse, adapter-internal
    #[error(transparent)] Provider(#[from] ProviderError),        // upstream LLM failures
    #[error("agent exited unsuccessfully")] UnsuccessfulExit(#[from] UnsuccessfulExit),
    #[error(transparent)] Acp(#[from] AcpError),
    #[error("execution cancelled")] Cancelled,
    #[error("execution deadline exceeded")] DeadlineExceeded { deadline: Duration },
    #[error("concurrency limit reached and queue deadline exceeded")] ConcurrencyExhausted,
    #[error("circuit open for {scope} until {until:?}")] CircuitOpen { scope: ExecutionScope, until: Instant },
}

pub enum ProcessError {
    ExecutableNotFound { executable: PathBuf }, PermissionDenied { executable: PathBuf },
    Spawn { executable: PathBuf, source: std::io::Error },
    Stdin { source: std::io::Error }, Output { stream: OutputStream, source: std::io::Error },
    TimedOut { which: TimeoutKind }, Cancelled,
    Termination { source: std::io::Error }, Reap { source: std::io::Error },
}

pub enum ProviderError {                                   // carries Classification evidence
    Overloaded { status: Option<u16>, provider_code: Option<String>, retry_after: Option<Duration>, evidence: Classification },
    RateLimited { retry_after: Option<Duration>, evidence: Classification },
    ConnectionReset { evidence: Classification },
    Authentication { evidence: Classification },
    ContextTooLarge { evidence: Classification },
    InvalidConfiguration { evidence: Classification },
    Other { summary: String, retryability: Retryability, evidence: Classification },
}

pub enum Retryability { Retryable { floor: Option<Duration> }, Permanent }
```

`UnsuccessfulExit` carries bounded `CapturedOutput` (below), **not** large strings.

## CapturedOutput — bounded, head+tail (`execution_service/output.rs`)

```rust
pub struct CapturedOutput { pub total_bytes: u64, pub retained_head: Bytes, pub retained_tail: Bytes, pub truncated: bool }
pub struct ProcessOutput { pub status: Option<ExitStatus>, pub stdout: CapturedOutput, pub stderr: CapturedOutput,
                            pub duration: Duration, pub termination: Termination }
```
Retention is configurable head+tail bytes (tail is where provider errors usually
sit). Errors and tracing records carry only `CapturedOutput` (counts + bounded
tail); the full retained bytes live on `ProcessOutput` for the classifier and the
opt-in diagnostic renderer, never auto-printed.

## ProcessSupervisor — single owned future, byte chunks (`execution_service/supervisor.rs`)

Ownership is unambiguous: **one supervisor task owns the `Child` for its entire
life.** Consumers only get `ProcessEvent`s + a completion handle; there is **no
async work in `Drop`** (Rust cannot prove reaping inside Drop).

```rust
pub enum ProcessEvent { Stdout(ProcessChunk), Stderr(ProcessChunk), StdinWritten, Exited }  // bytes, NOT lines
pub struct ProcessChunk { pub sequence: u64, pub observed_at: Instant, pub stream: OutputStream, pub bytes: Bytes }
pub enum ProcessInput { Inherit, Null, Bytes(Bytes) }
pub struct ProcessTimeouts { pub startup: Option<Duration>, pub stdin_write: Option<Duration>,
    pub idle: Option<Duration>, pub total: Option<Duration>, pub termination_grace: Duration }
pub struct RunningProcess { events: Receiver<ProcessEvent>, completion: JoinHandle<Result<ProcessOutput, ProcessError>>, cancellation: CancellationToken }
impl RunningProcess { pub async fn next_event(&mut self) -> Option<ProcessEvent>;
                      pub async fn complete(self) -> Result<ProcessOutput, ProcessError>;
                      pub async fn cancel(self) -> Result<ProcessOutput, ProcessError>; }
```
Internals: spawn in a **new process group** (Unix: `setpgid`/new session before
exec via `command-group`); concurrently write stdin + drain stdout+stderr (3
tasks, deadlock-free even at 10 MB+); `tokio::select!` over {exit, total/startup/
idle deadlines, cancellation token}. On cancel/timeout: **SIGTERM the group →
wait `termination_grace` → SIGKILL the group → reap the direct child**; on `Drop`
of `RunningProcess`, cancel the token and **detach the supervisor** so it still
terminates+reaps (deterministic cleanup still requires awaiting `complete()`/
`cancel()`; documented). Grandchildren verified dead via a fixture-held pipe whose
closure proves termination (avoids PID-reuse races). Newlines/framing/UTF-8
decoding are the **adapter's** job — the supervisor emits raw `Bytes`. A max
protocol-frame length bounds unbounded lines.

## AgentAdapter trait + classifiers (`execution_service/adapters/`)

```rust
#[async_trait::async_trait(?Send)]
pub trait AgentAdapter {
    async fn execute(&self, req: &AdapterRequest, events: &mut dyn AgentEventSink) -> Result<AdapterResult, AgentExecutionError>;
}
```
- `ClaudeAdapter`: builds the Claude `ProcessSpec` (moves serve.rs:5340-5356 args),
  feeds supervisor bytes through a **UTF-8 → newline-frame → stream-json** decoder
  (max frame length), emits `AgentEvent`s, extracts `session_id` mid-stream
  (reuses the logic now in `serve.rs::parse_claude_stream_line` at 5468 + agent.rs
  `process_stream_line` at 1056 — **consolidated to one decoder**), and on exit
  classifies via `ClaudeClassifier`.
- `CodexAdapter`: codex JSONL framing (from `parse_codex_stream_line` 5180).
- `AcpAdapter`: drives the `agent-client-protocol` JSON-RPC session; `!Send`, runs
  on the service's dedicated `LocalSet` worker so the public `AgentExecution` stays
  `Send`. Keeps the multi-turn `AcpSession` capability (acp.rs:283) behind the
  adapter, exposing only one-shot `execute` initially.

**Classifier returns evidence, with explicit precedence** (`execution_service/classify.rs`):
```rust
pub struct Classification { pub error: ProviderError, pub source: ClassificationSource, pub matched_rule: &'static str, pub confidence: ClassificationConfidence }
pub enum ClassificationSource { Spawn, VelorDeadline, VelorCancel, StructuredEvent, StdoutTail, StderrTail, ExitStatus }
```
Precedence: spawn error → Velor-owned cancel/timeout → **structured protocol error
event** → known provider error in stdout-tail → stderr-tail → exit status → generic
unsuccessful exit. "Scan stdout first" was the *bug-fix minimum*; structured events
come first so a model that *generates* the text `"API Error: 529"` is **not**
misclassified (a dedicated false-positive test covers this). `BinaryNotFound` is
emitted **only** from `ProcessError::ExecutableNotFound` — never from empty stderr.
`Retry-After` is **opportunistic**: accepted only in structured/known textual forms
(`retry_after:`, `Retry-After:`), never by extracting arbitrary numbers. Claude's
stream-json structured error events are preferred over rendered text.

## Retry policy — stateless per execution (`execution_service/retry.rs`)

`RetryPolicy` is **stateless**; orchestration state (attempts, sleep) lives in the
service. Jitter is **decorrelated with a non-zero floor** (not full-jitter-from-zero):

```rust
pub struct RetryPolicy { pub initial: Duration, pub max: Duration, pub multiplier: f64,
    pub floor: Duration, pub strategy: JitterStrategy, pub max_attempts: u32, pub retry_after_floor: Duration }
pub enum JitterStrategy { Decorrelated, EqualJitter }   // both non-zero floors
impl RetryPolicy { pub fn delay(&self, attempt: u32, rng: &mut dyn JitterSource, retry_after: Option<Duration>) -> SleepDecision }
```
- **Per-class initial delays**: overload (`529`) starts ~5 s; rate-limit honors
  `Retry-After` floored; transport resets start ~2 s. `max_attempts` = **total
  executions incl. the initial** → terminal text reads "attempt 2 of 5".
- The **old** `RetryConfig`/`RetryError`/`calculate_backoff`/`is_permanent_error`
  are **replaced, not deprecated** (single in-repo consumer = clean break). The
  service returns a typed `ExecutionOutcome` consumed by `run_auto_loop`; **no**
  flattening back into `RetryError`. `run_auto_loop`'s iteration-level
  crash-recovery (history/context) stays there — it's a different layer.
- Time + randomness are injectable via internal traits (`Sleeper`, `JitterSource`)
  defined under `#[cfg(test)]`/an internal module — **no public `test-util` feature**.

## Concurrency + circuit breaker — in the service, scoped (`execution_service/policy.rs`)

Lives in the long-lived `AgentExecutionService`, keyed by `ExecutionScope { provider, profile, credential_scope }`
— **not** per "agent" (ambiguous) and **not** inside the retry loop (retries are
already sequential). `ConcurrencyLimit` defines queue behaviour (max wait, cancel
while waiting, fairness, queue depth). Default is **not** a global 1 — GLM may use
1, but it's intentional/configurable.

`CircuitBreaker` has a real state model:
```rust
pub enum CircuitState { Closed, Open { until: Instant }, HalfOpen { probes_in_flight: usize } }
```
Only **transient upstream** failures count (overload, rate-limit, upstream 5xx,
upstream reset, upstream timeout); Velor-local deadlines, context-too-large, and
auth are excluded (one bad prompt can't open the provider breaker). **Explicitly
noted**: an in-memory breaker only helps the long-lived `vel serve` path; isolated
`vel` CLI invocations gain little (documented; optional).

## Cancellation hierarchy — one source of truth

`tokio_util::sync::CancellationToken` tree: **request → attempt → process**. A
cancel: stops retry sleeps → launches no further attempts → terminates the active
process group → drains/reaps → returns `Cancelled` (not `TimedOut`). A deadline
cancels through the same tree but preserves the `DeadlineExceeded` cause. serve.rs's
**wrapper `tokio::time::timeout` (3053) is removed** — the deadline is passed into
`AgentRunRequest` and enforced inside the supervisor. Backpressure: a slow event
**sink never stalls process draining** — supervisor drains to completion
unconditionally; the bounded `events` channel coalesces/drops verbose partial-delta
events under pressure and records dropped counts in diagnostics.

## Diagnostics — derived records, JSON form (`execution_service/diagnostics.rs`)

Not a hand-maintained mega-struct; produced from `ProcessSpec`+`ProcessOutput`+
`AgentExecutionError`+attempt decisions:
```rust
pub struct InvocationRecord { pub specification: SanitisedProcessSpec, pub timing: InvocationTiming,
    pub outcome: InvocationOutcome, pub output_metrics: OutputMetrics, pub classification: Option<ClassificationRecord> }
```
Rendered as structured tracing fields, **`--diagnose=json`** machine-readable form,
or human terminal. Env diagnostics: **allowlisted safe values** (PATH, HOME, TERM,
locale, NO_COLOR, cwd) shown directly; **sensitive** vars (name matches
KEY/TOKEN/SECRET/PASSWORD/AUTH, or `ANTHROPIC_*`/`ZAI_*`) → presence + byte length
only (no digests in normal logs — a stable hash can itself be sensitive; fingerprints
only under explicit local diagnostic). Resolved-exe path reported via same PATH
semantics + symlink target; **version probing is optional, short-timeout, cached by
exe identity, never on every invocation** (it can network/hang/leak). Prompt metrics:
chars, UTF-8 bytes, and a **broad estimate range** (e.g. "11k–18k"), never used for
policy. Previews off by default; opt-in, head+tail, redacted.

## Replay manifest — real A/B comparison (deliverable #1)

`--diagnose=manifest` writes a **sanitised** invocation manifest + the exact prompt
bytes to a temp file and emits a replay command: same exe, args, cwd, prompt file,
sanitised env (**no secrets in the script**). This is the genuine Velor-vs-direct-`glm5`
comparison; the fixture only proves mechanics/lifecycle/classification.

## Config — typed durations + validation (`config.rs`)

Add to `Defaults` with **human-readable durations** (`"5s"`, `"60s"`, `"30m"`) via a
`HumantimeSerde`-style newtype (workspace already has `humantime`): `initial_backoff`,
`max_backoff`, `backoff_multiplier`, `backoff_floor`, `attempt_timeout` (total),
`idle_timeout`, `termination_grace`, `startup_timeout`, `per_agent_concurrency`,
`output_capture_bytes`, `event_frame_bytes`, plus `circuit_breaker.{threshold,cooldown,
half_open_probes}`, `diagnostic_format`. All `Option`+`#[serde(default)]` → existing
configs keep parsing. **Validation** at load: multiplier ≥ 1.0 & finite, max ≥ initial,
attempts/concurrency/threshold non-zero, total > grace. `Defaults::merge` (config.rs:593)
extended. Migration notes in README + `docs/`.

## Logging policy

- Supervisor/adapter: record spans + safe fields, **never** terminal errors, raw
  prompts, env, stdout, or stderr.
- Retry orchestration: one event per retry decision (classification, attempt N of M,
  chosen delay).
- Application boundary (CLI auto-loop / serve `RunOutcome` / Tauri): **one** final
  failure log. `#[instrument]` uses `skip_all` + explicit safe fields; **`ret`/`err`
  removed from inner methods** throughout (fixes dup-logging durably, not just for
  `run`/`run_claude`).

## Terminal output

Distinguishes invocation-failure / transient-provider / retry-scheduling /
exhaustion / local-config-failure. Transient overload reads like:
> GLM request failed: upstream reported temporary overload (529/1305). Retrying
> attempt 2 of 5 after ~7.4 s.
ECONNRESET:
> The upstream connection was reset after 3m 25s; the child exited cleanly and
> will be retried after backoff.
Final error: classification, attempts made, total elapsed, last provider error,
sanitised command metadata, and a next diagnostic command (`--diagnose=json` or
`--diagnose=manifest`). Never claims the binary is missing unless spawn failed.

## Fixture — dedicated unpublished crate

`crates/velor-test-agent/` (workspace member, `publish = false`, not a `[[bin]]` of
velor-core). **Argument-driven** (not env-mega-fixture), e.g.
`velor-test-agent overload-529`, `… large-output --bytes 10485760`, `… sleep --duration 30s`,
`… fork-tree --children 3` (each child holds an open pipe fd for group-kill proof),
`… echo-stdin`, `… stderr-blind`, `… ignore-sigterm`. Tests use `CARGO_BIN_EXE_velor-test-agent`.

## Tests (expanded matrix — fixture + injected clock/jitter)

Executor/lifecycle: happy path; 10 MB stdout no-deadlock; stdin written+EOF
(`echo-stdin`); stdin larger than 43,846 chars; child exits before stdin fully
written; child closes stdout early but keeps running; closes stderr early; invalid
UTF-8 split across chunks; JSON object split across reads; multiple JSON objects in
one chunk; final line without newline; line over max frame size; timeout reaps
**whole group incl. grandchildren** (pipe-closure proof); cancel reaps group; child
ignores SIGTERM then dies in grace; race normal-exit vs cancel; race timeout vs
cancel; signal-exit vs numeric-exit; exec-not-found → `ExecutableNotFound`;
permission-denied; cwd-doesn't-exist at spawn.

Classifier: stdout-only error; stderr-only; split across streams; exit-1 with
`529[1305]` → `Overloaded`; `ECONNRESET` → `ConnectionReset`; invalid-key →
`Authentication` (permanent); prompt-too-long → `ContextTooLarge` (permanent);
`429`+structured retry-after → `RateLimited{retry_after}`; **false-positive**: model
text containing "API Error: 529" → NOT classified as overload; structured success
then non-zero exit; malformed stream-json then valid provider error; no-output +
non-zero; no-output + zero; truncation preserves tail evidence.

Retry/policy: decorrelated-jitter bounds (injected `JitterSource`) with non-zero
floor; `Retry-After` precedence (floored); overload-class ~5 s initial; exhaustion
→ `AttemptsExhausted` with attempt records; first-success → no further attempts;
no-retry for permanent; cancellation during backoff sleep; cancellation while
awaiting concurrency permit; breaker opens after N transient, half-open probe,
isolated between two scopes, ignores local-deadline/auth/context.

Diagnostics: redaction strips each secret pattern; safe allowlist values retained;
previews off by default; records pid/elapsed/exit/classification; **arguments
containing inline credentials are not exposed**; manifest contains no secrets.

Integration: serve.rs `run_claude_like_profile`/`run_codex_profile` via service +
classifier (fixture); session-id still extracted mid-stream; Tauri `AgentEvent`→
`ExecutionRecord` mapping intact; one `AgentError` → exactly one `velor_core` log
event (tracing test subscriber); ACP returns `AgentExecutionError::Acp` without
going through `ProcessSupervisor`.

## Phased execution (each phase ends green: `cargo check --workspace` + `cargo test -p velor-core`)

Per the requested reorder — **consolidate execution before adding policies:**
1. `ProcessSupervisor` + `CapturedOutput` + `ProcessError` + fixture crate + executor/classifier unit tests.
2. `AgentAdapter` trait + Claude/Codex/Acp adapters + `Classification` (evidence/precedence) + consolidated stream decoder.
3. **Unify all three consumers** through `AgentExecutionService` (replace `agent.rs::run_claude`/`run_codex`, serve.rs profile runners, Tauri call); remove serve.rs wrapper timeout → `AgentRunRequest.deadline`; remove dup-log instruments. ⚠️ largest/highest-risk phase — Tauri + serve must keep compiling and behavior-equivalent.
4. Cancellation hierarchy + process-tree cleanup verification (pipe-closure grandchild test).
5. Typed `RetryPolicy` (stateless, decorrelated w/ floor) replacing old retry API; `run_auto_loop` migrated to `ExecutionOutcome` (no `RetryError` round-trip).
6. `ConcurrencyLimit` + optional `CircuitBreaker` at service scope.
7. Derived `InvocationRecord` + JSON/manifest diagnostics + replay harness.
8. Config (typed durations + validation + merge) + docs + migration notes.
9. Real direct-vs-Velor replay validation against `glm5` (honest findings: which differences isolated, which remain unproven).

Cross-cutting: `#![warn(missing_docs)]` + `#![warn(clippy::unwrap_used)]` honored
(document all public items; no `unwrap`/`expect`/blanket allows in non-test code).
New dep: `command-group` (workspace + velor-core) for process-group kill; `humantime`
already present.

## Critical files

- new crate dir `crates/velor-core/src/execution_service/` (`mod.rs`, `error.rs`, `output.rs`, `supervisor.rs`, `adapters/{claude,codex,acp}.rs`, `classify.rs`, `retry.rs`, `policy.rs`, `diagnostics.rs`, `service.rs`)
- new crate `crates/velor-test-agent/` (`Cargo.toml`, `src/main.rs`)
- `crates/velor-core/src/agent.rs` (adapters consume its stream types; `AgentRunner` becomes a thin facade over the service or is removed)
- `crates/velor-core/src/{retry.rs,config.rs,lib.rs}` + `Cargo.toml`s + root `Cargo.toml`
- `apps/velor-cli/src/main.rs` (`execute_with_retry`→service, `run_auto_loop` `ExecutionOutcome`, retry-config build, `--diagnose`)
- `apps/velor-cli/src/serve.rs` (`ProcessExecutionRunner`→service; `AgentEvent`→`RunnerProgressEvent` adapter; remove wrapper timeout; `RunOutcome` from `AgentRunReport`)
- `apps/velor/src-tauri/src/commands.rs` (`run_execution_task`→service; `AgentEvent`→`ExecutionRecord` unchanged)
- docs: `README.md`, `docs/codex-telegram-server.md`, migration notes

## Verification

- `just check` (fmt + clippy `-D warnings` + svelte-check); `cargo check --workspace` after each phase; specifically `cargo check -p velor` after Phase 3.
- `cargo test -p velor-core` + `cargo test -p velor-test-agent` — the expanded matrix (fixture + injected clock/jitter; no real provider).
- `vel auto --prompt <small> --diagnose=json` smoke (diagnostic dump + new terminal wording; one error → one log line).
- `vel auto --diagnose=manifest` → run the emitted replay command directly against `glm5` to produce the A/B comparison for the PR (deliverable #1 + #9); document which differences were isolated and which remain unproven.
