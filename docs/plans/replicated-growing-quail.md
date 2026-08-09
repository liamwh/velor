# Mid-session provider/model switching for `vel auto`

## Context

`vel auto` picks one provider/model (`AgentRunner`) at startup and drives it for the
whole run. The user hits usage limits on one provider (e.g. z.ai/GLM 5.2 via `omp`)
mid-session and wants to swap to another (e.g. Anthropic Sonnet 5, also via `omp`)
without losing context — "changing gears," not starting a new conversation.

Two assumptions from the original ask turned out to be wrong, discovered via research
(see below) and corrected with the user directly:

1. **`/handoff` is not an omp feature.** It's a third-party skill
   (`mattpocock/skills`) symlinked into this user's `~/.pi/agent/skills/` from their
   personal dotfiles — not guaranteed to exist for anyone else, no fixed output
   schema, writes to the OS temp dir. Velor will own its own structured handoff
   prompt/schema instead of depending on it.
2. Native OMP↔OMP continuity requires the omp session to already be *persisted*
   before a switch is requested (velor currently forces `--no-session` on every
   iteration). The user chose, explicitly: **persist OMP sessions by default**,
   lifecycle-managed by a Velor-owned registry (never touching sessions Velor didn't
   create), over the ephemeral-by-default status quo — because the flagship scenario
   (rate-limited *mid-turn*) only works if the session was already durable when the
   limit hit.

The user also asked for a **long-lived native OMP RPC session** (one `omp --mode rpc`
process spanning many iterations) rather than today's one-process-per-iteration
model, as groundwork for future model-inspection/steering work — and for **one
unified `m` modal** (info + picker combined, not two separate modals), designed to
grow into a fuller "Model & Session" panel over time.

**Continuity hierarchy** (strict priority, never skip a tier to reach a lower one
unless the higher one is genuinely unavailable):

1. **Live in-session switch** — same OMP process still running, native RPC command
   changes model without restarting (existence unverified — see Phase 0).
2. **Native session resume** — same runner kind, process restarted, but the
   provider's own session id is passed back in (`--resume`/`resume_session`).
3. **Velor-owned structured handoff** — different runner, or same runner with no
   resumable session: ask the *current* live agent (full context) to produce a
   structured continuation doc via a prompt template Velor controls, inject it into
   the new runner's first prompt.
4. Raw transcript replay — never automatic, last-resort only.

## Research grounding (do not re-derive — verified facts)

- `AgentRunner` (`crates/velor-core/src/agent.rs:147-158`) is a stateless `Clone`
  config enum (`ClaudeSubprocess | ClaudeAcp(AcpConfig) | Codex(CodexConfig) |
  Omp(OmpConfig)`). `run()`/`run_with_events()`/`run_with_events_and_steering()`
  (agent.rs:288-, all `#[allow(clippy::too_many_arguments)]`) each call private
  `build_profile()` (agent.rs:217-) fresh per call → stateless
  `shared_service().execute(profile)`. `build_profile` currently hardcodes
  `resume_session: None` for Claude/Codex; `OmpParams`/`AcpParams` have no resume
  field at all.
- **Claude** (`adapters/claude.rs`): `ClaudeParams{model, resume_session, ...}`,
  `--model`/`--resume <id>` already work; adapter already parses the stream-json
  `"system"` frame → `AgentEvent::Status{message: "session: {id}"}`.
- **Codex** (`adapters/codex.rs`): `CodexParams{config: CodexConfig, resume_session,
  ...}`, `--model`/`resume` subcommand already work; parses `"thread.started"` →
  `AgentEvent::Status{message: "thread started: {id}"}`.
- **Omp** (`adapters/omp.rs`): `OmpParams` has no resume field; adapter always passes
  `--no-session`; one `omp --mode rpc` process per `execute()` call, torn down after
  `agent_end`. No live steering. `omp` v17.2.11 (installed, verified via `--help` +
  its embedded changelog string table): `--model=<fuzzy>`, `-r/--resume=<id>`,
  `-c/--continue`, `--from-claude`/`--from-codex` (session import, unverified exact
  semantics), `--mode=rpc`. Changelog shows the RPC wrapper changed from
  `{"type":"prompt","message":...}` to `{"role":"user","content":...}` in v0.12.0 —
  velor's adapter doc-comment claims the old form was "verified against v17.1.5,"
  which post-dates that changelog entry — **unresolved discrepancy, needs a live
  spike** (Phase 0). No documented RPC command for live model switching was found
  (only `set_fast_mode`, `get_state`, `compact`, `print`).
- **ACP** (`adapters/acp.rs`): no `model`/`resume_session` field at all; runs on a
  dedicated `!Send` `LocalSet` thread; fresh session every call.
- **Supervisor** (`execution_service/supervisor.rs`): `ProcessInput::Streaming
  {initial}` keeps stdin open via `RunningProcess::input_sender()` →
  `ProcessInputCommand::Write/Close`. **Verified directly (read the source + its own
  test suite, supervisor.rs:995-1150): an empty `initial: Bytes::new()` is already
  exercised by existing tests and works fine** — `OmpPersistentSession::spawn` can
  spawn with an empty initial frame and send every real turn (including the first)
  via `input_sender()`, no synthetic no-op frame needed. This resolves one of Phase
  2's open risks.
- **`run_auto`/`run_auto_loop`** (`apps/velor-cli/src/main.rs`): `runner`/`binary`
  built once (main.rs:1354-, 1231) and passed as **immutable borrows** all the way
  down through `run_auto_loop` → `execute_with_retry` (loops `for attempt in
  1..=max_retries`, calling `run_with_events_and_steering` fresh each attempt).
  `live_capable`/`use_acp_session` are computed **once before the iteration loop**
  (main.rs:2125, 2130), not per-iteration. No native session/turn-history object
  spans iterations today — continuity is on-disk PRD/progress files (primary) plus
  `ConversationHistory` (`retry.rs:38-120`, only populated on iteration *failure*,
  cleared on next success, flattened into a `crash_recovery_context` template var,
  main.rs:2179-2200).
- **Live-steering round-trip** (the pattern to model the new switch command on):
  `TuiSteeringCommand` (`streaming_tui.rs:56-73`, `SendOnce`/`ReplacePersistent`,
  each with its own `oneshot` ack) → `steering_cmd_tx`/`rx` (main.rs:1410-1411) →
  consumed via **`steer_during`** (main.rs:2028-2062, `tokio::select!` racing the
  iteration future against `tui_command_rx.recv()`) → **`handle_steering_command`**
  (main.rs:1933-2010), called from *two* sites (mid-iteration via `steer_during`,
  and a direct drain loop between iterations, main.rs:2496-2501) — both need the new
  variant. TUI side: `InputMode` enum (streaming_tui.rs:233-250,
  `Normal|Steering{..}|EditingPersistentAppend{..}`), `handle_modal_key`
  (736-791, checked first in `handle_key`), `submit_steering`/`submit_append`
  (795-842) build the command + spawn the send + store a `oneshot::Receiver` polled
  non-blockingly every render tick by `TuiState::poll_pending_submission`.
- **`vel serve`** (`apps/velor-cli/src/serve.rs`) already has a working, separate
  same-provider resume prototype: `SessionResumeStore` (serve.rs:760-881) —
  versioned JSON, atomic tmp-write+rename persistence — **this exact pattern is what
  Phase 3's OMP session registry mirrors**, confirmed by reading `persist()`
  directly (serve.rs:853-880).
- **`ProviderInfo`/`m` modal** (added this session, `streaming_tui.rs:140-153`,
  `show_provider_info: bool`) is read-only, static startup config — doesn't reflect
  the actually-resolved model at runtime (flagged by the user as "useless" for Omp
  when no override is configured). This gets superseded by Phase 6, not patched
  separately.

## Phase 0 — OMP RPC live spike — RESOLVED

Ran live against the real installed `omp` v17.2.11 (`--mode rpc`), bypassing the
adapter, from a scratch cwd/session-dir (throwaway, no production code). All six
questions are now answered:

1. **Prompt wire format**: `{"id":"vel","type":"prompt","message":"..."}` — velor's
   current adapter format — **works as-is**, got a `success:true` ack followed by
   the full `agent_start`→…→`agent_end` turn stream. No migration needed; the
   changelog's claimed replacement doesn't block this form.
2. **`get_state`**: returns `{"success":true,"data":{"model":{"id","name","api",
   "provider","baseUrl","contextWindow","maxTokens","cost",...}}}` — the exact
   resolved model info (not just config), perfect for Phase 6's TUI and for
   replacing the "(binary default)" gap from the `m` modal shipped earlier this
   session. No session id in this payload — not needed, see next point.
3. **On-disk shape** (confirmed by direct filesystem read, no RPC needed):
   `~/.omp/agent/sessions/<sanitized-cwd>/<ISO-timestamp>_<uuid-v7>.jsonl` (one
   file per session) + a same-named directory alongside it. **The session id is the
   UUID-v7 suffix**, also recorded explicitly as the second line of the `.jsonl`
   file: `{"type":"session","id":"<uuid>","cwd":"...","title":"...",...}`. No mtime-
   scan fallback needed — the id is directly readable. A `"model_change"` line
   (`{"type":"model_change","model":"anthropic/claude-sonnet-5","resolvedModelIsFallback":false}`)
   is also recorded whenever the model changes — a second, file-based way to learn
   the resolved model.
4. **`--resume <id>` continuity**: **confirmed** — a brand-new process (`omp --mode
   rpc -r <uuid>`), no relation to the process that created the session, correctly
   recalled a fact from a turn run by the *original*, now-exited process. This is
   tier 2, fully working today.
5. **Live model-switch RPC (tier 1)**: **exists and works** —
   `{"type":"set_model","modelId":"<exact-id>","provider":"<exact-provider>"}` →
   `{"type":"model_changed"}` followed by a `success:true` response carrying the new
   model's full metadata. Requires **exact** `provider`+`modelId` (confirmed via the
   `get_state` response's shape) — fuzzy names alone (`"opus"`, `"sonnet-5"`) or a
   combined `"provider/modelId"` string are rejected ("Model not found:
   undefined/undefined" or similar). Unlike the CLI's `--model` flag, this RPC
   command does **not** fuzzy-resolve.
6. **Mid-turn interrupt RPC**: **does not exist** — `{"type":"interrupt"}` →
   `"Unknown command: interrupt"`, and the full `available_commands_update` command
   registry (~95 entries, dumped and inspected in full) has no turn-abort/cancel
   command. A mid-turn switch away from Omp must kill the process and rely on tier
   2's respawn-with-resume, exactly as the plan already assumed as a fallback.

**Design consequence for `SwitchTarget` (Phase 1):** since tier 1's `set_model`
needs an *exact* pair but the config's `model: Option<String>` field is meant to
double as the CLI `--model` flag's value (which *does* fuzzy-resolve, e.g.
`"opus"`), tier-1 support for a given target requires that string to be in
`"provider/modelId"` form (e.g. `"anthropic/claude-sonnet-5"` — the same form
`omp --help` documents for its own `--model` flag, and the same form the session log
already uses for `model_change`). `try_switch_model_live` splits on the first `/`;
no `/` present → tier 1 isn't attempted for that target (falls through to tier 2),
no new config field needed.

Gates Phase 2 and tier 1 of Phase 4 — both now unblocked, tier 1 stays in the design
(not stubbed/deleted). Did not gate Phase 1 or Phase 5's Claude/Codex logic.

## Phase 1 — Foundational types (behavior-neutral)

**`crates/velor-core/src/agent.rs`**
- `AgentRunnerKind` — `Copy` identity-only mirror of `AgentRunner` (`ClaudeSubprocess
  | ClaudeAcp | Codex | Omp`), plus `AgentRunner::kind()`,
  `supports_native_resume()`, `configured_model()`. One small match per method — this
  is the extensibility point requirement 5 asks for; a 5th runner adds one arm to
  each.
- `ResumeHandle{ session_id: Option<String> }` — new trailing parameter on
  `build_profile`/`run`/`run_with_events`/`run_with_events_and_steering` (consistent
  with their existing `#[allow(too_many_arguments)]`). `build_profile` stops
  hardcoding `resume_session: None` and reads `resume.session_id.clone()` for
  Claude/Codex/Omp arms. Every existing call site (main.rs, serve.rs) passes
  `ResumeHandle::default()` except the new switch-time call in Phase 5.

**`crates/velor-core/src/execution_service/adapters/omp.rs`**
- `OmpParams` gains `resume_session: Option<String>`. `build_spec`: `--resume <id>`
  when set, else `--no-session` as today (keeps the one-shot adapter correct for any
  caller other than `vel auto`'s new persistent path, e.g. the disk-cleanup pass).

**`crates/velor-core/src/config.rs`**
- `SwitchTarget{label, provider: AgentProvider, protocol: Protocol, model:
  Option<String>, binary: Option<String>}`, `Vec<SwitchTarget>` on `FileConfig` as
  `[[switch_targets]]` (`#[serde(default)]`). Merge rule: repo config's array, if
  non-empty, replaces home's wholesale (no natural per-key merge for a `Vec` —
  document this explicitly, it differs from the `BTreeMap` overlay semantics used
  for `[vars]`/`[prompts]`).
- `OmpSessionsConfig{retention_days: u32 = 14, cleanup_on_startup: bool = true}`.

**`crates/velor-core/src/execution_service/capabilities.rs`**
- Extend `AgentCapabilities` with `native_resume: bool`, `persistent_session: bool`.
  `AgentRunner::capabilities()` populates per-kind.

Mechanical but wide — touches every `run*`/`build_profile` call site. Land as its
own PR first, no behavior change.

## Phase 2 — OMP persistent-session architecture

Depends on Phase 0 + Phase 1's `resume_session`.

**New: `crates/velor-core/src/execution_service/adapters/omp_session.rs`**
- Extract the shared frame-decode/dispatch (today's `parse_omp_line`/event handling
  in `omp.rs`) to `pub(crate)` so both the one-shot adapter and the new persistent
  session reuse it without duplication.
- `OmpPersistentSession{ process: RunningProcess, input_tx, native_session_id:
  Option<String>, ... }`:
  - `spawn(binary, config, cwd, resume_session_id, cancellation, timeouts) -> Result<Self,
    _>` — spawns with `ProcessInput::Streaming{initial: Bytes::new()}` (confirmed
    safe, see Research Grounding), never `--no-session`, `--resume <id>` when
    resuming.
  - `send_turn(&mut self, prompt, sink) -> Result<AgentRunResult, _>` — writes the
    prompt via `input_tx`/`ProcessInputCommand::Write` (not the initial frame),
    drives events until that turn's `agent_end`, does **not** close stdin (process
    stays alive). On first successful call, captures `native_session_id` by reading
    the freshly-written `.jsonl` session file's `{"type":"session","id":...}` line
    under `~/.omp/agent/sessions/<sanitized-cwd>/` (confirmed format, Phase 0) —
    the newest file matching this process's cwd immediately after the first
    `agent_start`.
  - `is_alive() -> bool`, `native_session_id() -> Option<&str>`.
  - `try_switch_model_live(&mut self, provider: &str, model_id: &str) -> Result<bool, _>`
    — tier 1 hook, confirmed real (Phase 0): sends `{"type":"set_model","provider":
    provider,"modelId":model_id}`, returns `Ok(true)` on a `success:true` response
    (after observing `{"type":"model_changed"}`), `Ok(false)`/`Err` otherwise so the
    caller falls back to tier 2. Only callable when the target's `model` string is
    in exact `"provider/modelId"` form (see Phase 1's `SwitchTarget` note) — the
    caller splits it before calling this.
  - `shutdown(self) -> Result<(), _>` — EOF, grace period, force group-kill, mirrors
    `omp.rs`'s existing `finalize_streaming`.

**`apps/velor-cli/src/main.rs`**
- New `run_auto_iteration_omp(...)` (parallel role to the existing
  `run_auto_iteration_acp`), lazily spawns `OmpPersistentSession` on first use,
  reuses thereafter. A new small Omp-specific retry wrapper (same
  `RetryConfig`/`BackoffPolicy` types, no new retry primitives) retries via
  `send_turn` on the *same* live session when the error is retryable-but-the-process-
  survived; if the process itself died, drops the session and respawns with
  `resume_session_id: session.native_session_id()` on the next attempt — this **is**
  tier 2's crash-recovery path, satisfying "if an OMP process dies... prefer
  resuming the persisted native session."
- New third branch in `run_auto_loop`'s iteration dispatch, parallel to the existing
  ACP branch (main.rs:2203-2248): `if let AgentRunner::Omp(cfg) = &state.runner {
  run_auto_iteration_omp(...) } else { /* existing execute_with_retry path */ }`.
- Cancellation: `OmpPersistentSession::spawn` takes a **run-scoped** child token
  (`cancel_handler.token().child_token()`, created once when first spawned, stored
  alongside the session) so it survives iteration boundaries; each `send_turn` call
  derives a further per-call child token for scoped mid-turn cancellation (mirrors
  Claude/Codex's existing per-attempt token pattern).
- `run_disk_cleanup_pass` (main.rs ~2805): **exempt from the persistent-session
  path** — keep using the one-shot `OmpSubprocessAdapter` via
  `run_with_events_and_steering`, passing `ResumeHandle{session_id:
  session.native_session_id()}` so it still resumes the same native conversation
  without touching the live session object's lifecycle. Simpler and lower-risk than
  threading the disk-cleanup pass into Phase 2's new object.

## Phase 3 — Velor-owned OMP session registry

Depends on Phase 2 (needs a `native_session_id` to record).

**New: `crates/velor-core/src/omp_session_registry.rs`**
- `OmpSessionRecord{session_id, repo_root, run_id, created_at, last_touched_at,
  status: OmpSessionStatus, binary, model}`; `OmpSessionStatus{Active, Completed,
  Abandoned}`.
- `OmpSessionRegistry` — home-scoped (`~/.velor/omp_sessions.json`, not per-repo,
  mirroring where `omp` itself stores sessions). Persistence: **copy
  `serve.rs`'s `SessionResumeStore::persist()` pattern verbatim** (versioned JSON,
  atomic tmp-write + rename — confirmed working pattern, serve.rs:853-880).
- API: `load_default()`, `record_created(...)`, `touch(session_id)`,
  `mark_completed`, `mark_abandoned`, `get(session_id) -> Option<&Record>`,
  `reclassify_stale_active(staleness: Duration)`, `prune(omp_sessions_dir,
  retention) -> PruneReport` — **hard invariant: `prune` only ever deletes entries
  present in the registry; a directory under `~/.omp/agent/sessions/` with no
  matching registry record is never touched.**

**Wiring**
- `record_created`/`touch`: called from `run_auto_iteration_omp` after each
  successful `send_turn` (first call captures + records; later calls touch).
- `mark_completed`: at `run_auto`'s end-of-run summary block.
- `reclassify_stale_active` + `prune`: a startup sweep in `run_auto`, gated by
  `defaults.omp_sessions.cleanup_on_startup`, placed right after
  `require_agent_on_path(&binary)?` (main.rs ~1298) — logs via `tracing::info!`/`warn!`,
  never fails the run on error.

## Phase 4 — Continuation-tier engine + structured handoff

Depends on Phases 1–3.

**New: `crates/velor-core/src/continuation/mod.rs`**
- `ContinuationContext{from_kind, to_kind, from_native_session_id: Option<String>,
  omp_process_alive: bool}` — built fresh by the caller each time, never cached.
- `ContinuationTier{ LiveInSession, NativeResume{session_id}, StructuredHandoff }`
  (no automatic `RawTranscriptReplay` constructor — per the user's explicit
  requirement, that tier is never chosen automatically).
- `decide_tier(ctx, capabilities) -> ContinuationTier` — pure function: tier 1 when
  `from==to==Omp && omp_process_alive && capabilities.persistent_session &&` the
  target's model string is in exact `"provider/modelId"` form (Phase 0 confirmed
  `set_model` is real but needs an exact pair, not a fuzzy name); tier 2 when
  `from_kind == to_kind` and a native session id is known; else tier 3. Adding a 5th
  runner needs no change here — tier 2 is generic over `AgentRunnerKind` equality.

**New: `crates/velor-core/src/continuation/handoff.rs`**
- `HANDOFF_PROMPT_TEMPLATE` — a fixed Velor-owned Markdown schema (Objective,
  Completed Work, Remaining Work, Architectural Decisions, Important
  Assumptions/Constraints, Unresolved Questions, Relevant Files/Code Locations,
  Additional Context) rendered via the **existing** `crate::template::render_template`
  (same MiniJinja engine every other prompt uses — no new templating machinery).
- `request_handoff(from_runner, binary, permission_mode, cwd, resume, timeouts,
  cancellation) -> Result<String, _>` — one bounded, no-retry call to the *current*
  (still-context-holding) runner via its existing `run(...)` method, requesting the
  document as the entire response. If this call itself fails (e.g. the provider is
  fully unreachable, not just slow), fall back to a degraded handoff synthesized
  from `ConversationHistory::get_previous_context()` + the current iteration's
  rendered prompt — tag it as degraded so the new runner's first prompt can note the
  gap.
- Injection reuses the **existing** `crash_recovery_context` pattern
  (main.rs:2179-2200) — a new `switch_handoff_context` template var, inserted only
  when non-empty (same convention), consumed by the next rendered prompt on the new
  runner. No new templating mechanism.
- Optional, never required: if a `handoff`-style skill is detected for the *from*
  runner's tooling, try invoking it first as a bonus enhancement, falling back to
  the Velor-owned template on any failure or absence. Skip in v1 unless time
  permits — never the canonical path.

## Phase 5 — `run_auto_loop` control flow: swappable runner + mid-iteration switch

Depends on Phases 1–4.

**`apps/velor-cli/src/main.rs`**
- New `RunnerState{runner: AgentRunner, binary: String, native_sessions:
  HashMap<AgentRunnerKind, String>, omp_session: Option<OmpPersistentSession>,
  omp_registry: OmpSessionRegistry, last_handoff: Option<String>}` — owned,
  constructed once in `run_auto`, moved into `run_auto_loop` (replaces today's `&
  AgentRunner`/`&str` borrows). `live_capable`/`use_acp_session`/`use_omp_session`
  move from "computed once before the loop" to **recomputed at the top of every
  iteration** (required now that `state.runner` can change between iterations, not
  just at startup).
- `TuiSteeringCommand::SwitchRunner{target: SwitchTarget, acknowledgement:
  oneshot::Sender<Result<SwitchOutcome, TuiSteeringError>>}` — new variant,
  `streaming_tui.rs:56-73`. `SwitchOutcome{Applied{tier: ContinuationTierLabel} |
  Failed{message}}`.
- **Mid-iteration** (`steer_during`, main.rs:2028-2062): gains a per-iteration
  `iteration_cancel: &CancellationToken` param (a child of
  `cancel_handler.token()`, freshly derived each iteration, replacing today's direct
  use of the parent token in `execute_with_retry`). On `SwitchRunner`, `steer_during`
  intercepts it directly (not delegated to `handle_steering_command`, which lacks
  the cancellation/state access needed): stashes the pending switch, cancels
  `iteration_cancel`, keeps racing until the iteration future unwinds (so the
  subprocess is fully reaped, not abandoned), then returns
  `SteerOutcome::SwitchRequested(pending)` instead of `SteerOutcome::Completed`. A
  real Ctrl+C still works unchanged (it cancels the parent token, which
  `iteration_cancel` — as a child — also observes).
- **Between iterations** (main.rs:2496-2501 drain loop): `handle_steering_command`
  gains a `&mut RunnerState` param; `SwitchRunner` there calls `apply_switch`
  directly (no in-flight future to cancel).
- New `apply_switch(state, target, cwd, permission_mode, timeouts, cancel_handler,
  tui_tx, logger) -> SwitchOutcome`: builds the target `AgentRunner`, builds
  `ContinuationContext`, calls `decide_tier`, and per tier: (1) calls
  `try_switch_model_live` and falls back to tier 2/3 on `Ok(false)`/`Err`; (2) shuts
  down any live Omp session if leaving Omp, updates `native_sessions`, swaps
  `state.runner`; (3) calls `request_handoff` against the *current* (pre-switch)
  runner, stores the doc in `state.last_handoff`, then swaps `state.runner`. Same
  iteration's prompt is retried against the new runner (`current_iteration` does not
  advance) — consistent with existing retry-on-failure semantics and the
  "changing gears, not skipping the task" framing.

## Phase 6 — Unified "Model & Session" TUI modal

Depends on Phase 5. Replaces the `ProviderInfo`/`show_provider_info` read-only modal
added this session (not a second modal alongside it).

**`apps/velor-cli/src/streaming_tui.rs`**
- `ModelSessionRow{label, provider, binary, model: Option<String>, is_current: bool,
  native_session_id: Option<String>, capabilities: Option<AgentCapabilities>,
  selectable: bool, target: Option<SwitchTarget>}` — deliberately extensible
  (`Option` fields) so future rows (context usage, pricing) slot in later without a
  redesign, per the user's explicit ask. `ModelSessionModel{rows: Vec<Row>, cursor}`.
- `TuiMessage::SetModelSessionInfo(ModelSessionRow)` (current row, sent at startup
  and after every switch) + `SetSwitchTargets(Vec<ModelSessionRow>)` (static, sent
  once at startup from config) — replace `SetProviderInfo`.
- `InputMode::ModelSession{cursor: usize, submission: SubmissionState}` — new
  variant alongside `Steering`/`EditingPersistentAppend`, same
  `handle_modal_key`/`SubmissionState` lifecycle. `open_model_session_modal()`
  replaces the bare `show_provider_info = true` flip. Up/`k`, Down/`j` move cursor;
  `Enter` on a `selectable && !is_current` row calls `submit_switch_target` (mirrors
  `submit_steering`: builds `TuiSteeringCommand::SwitchRunner`, spawns the send,
  stores the ack receiver in a new `pending_switch_ack`, polled by the existing
  `poll_pending_submission` loop — closes the modal on success with a transient
  status naming the tier used, shows `Failed{message}` inline on error, same as
  today's editors); `Esc` cancels via the existing `cancel_modal`.
- `render_model_session_modal` replaces `render_provider_info_modal`: current row
  pinned/highlighted first, switch targets below, cursor-row highlight reusing
  whatever selection style convention already exists in the theme; each row shows
  label/provider/binary/model (or dim "(binary default)", same convention as today)
  and, for the current row, native session id (truncated) + capability badges.
- `m` key: `handle_key` calls `state.open_model_session_modal()` instead of setting
  the old flag; delete the standalone `show_provider_info` close-on-any-key blocks.
- Update the existing `show_provider_info`-based tests to the new
  `InputMode::ModelSession`/`model_session` fields.

## Phase 7 — `velor.toml` config

Add `[[switch_targets]]` entries matching the motivating scenario (GLM 5.2 via omp ↔
Sonnet 5 via omp ↔ Codex) and `[defaults.omp_sessions]` to the repo's example config
and docs. Add (de)serialization round-trip tests for `SwitchTarget` alongside
existing config tests in `crates/velor-core/src/config.rs`.

## Phase 8 — Testing & verification

- **Unit (velor-core):** `decide_tier` exhaustively over `(from_kind, to_kind,
  omp_process_alive, native_session_id present/absent)`; `OmpPersistentSession`
  multi-turn frame handling against a fake protocol-speaking test double (not the
  real binary, keep CI hermetic) — spawn once, `send_turn` twice, assert stdin isn't
  closed between turns; `OmpSessionRegistry::prune`'s "never touch an unknown
  session" invariant via a seeded directory with both registry-known and
  registry-unknown entries.
- **Integration (velor-cli):** extend `steer_during`/`handle_steering_command`
  coverage with a mid-iteration `SwitchRunner` scenario using fake runners —
  assert the in-flight future is actually cancelled (not just ignored) and the same
  prompt is retried against the new runner.
- **Manual, end-to-end** (per the `verify` skill — exercise the real flow): run `vel
  auto` with `provider = omp` against a real repo; mid-run press `m`, switch to a
  second `[[switch_targets]]` omp entry with a different model; confirm the picker
  renders both rows, the switch completes and the modal auto-closes, the next turn's
  output shows real continuity (references something from before the switch),
  `~/.velor/omp_sessions.json` gained an entry, and after run completion it's
  `Completed`. Also manually verify a cross-runner switch (Omp → Codex) produces a
  sensible structured handoff document and the new runner's first turn actually uses
  it.
- `cargo check -q` / `just check` after every phase, per project convention.

## Suggested ordering / parallelization

0 (spike, blocking) → 1 (parallel with 0) → 2 (needs 0+1) → 3 (needs 2) → 4 (needs
1–3; tier-1 arm stubbed/omitted until 0 confirms it) → 5 (needs 1+4; the Claude/Codex
tiers 2/3 half of this can start as soon as Phase 1 lands, independent of 2/3) → 6
(needs 5's types) → 7 (parallel, any time after Phase 1's `SwitchTarget` exists) → 8
(continuous, gates merge of 2/5/6).

## Critical files

- `crates/velor-core/src/agent.rs` — `AgentRunner`, `build_profile`, new
  `ResumeHandle`/`AgentRunnerKind`/capability queries.
- `crates/velor-core/src/execution_service/adapters/omp.rs` +
  new `omp_session.rs` — persistent-session architecture.
- `crates/velor-core/src/continuation/` (new) — tier decision + structured handoff.
- `crates/velor-core/src/omp_session_registry.rs` (new) — ownership/retention.
- `apps/velor-cli/src/main.rs` — `run_auto`, `run_auto_loop`, `steer_during`,
  `handle_steering_command`, `execute_with_retry`, new `RunnerState`/`apply_switch`.
- `apps/velor-cli/src/streaming_tui.rs` — `TuiSteeringCommand`, `InputMode`,
  `TuiState`, the unified Model & Session modal.
- `crates/velor-core/src/config.rs` — `SwitchTarget`, `OmpSessionsConfig`.
- Reuse, don't reinvent: `serve.rs`'s `SessionResumeStore::persist()` (atomic
  tmp+rename) for the registry; `main.rs`'s existing `crash_recovery_context`
  injection pattern for the handoff doc; `steer_during`/`handle_steering_command`'s
  existing round-trip for the new switch command; `template::render_template` for
  the handoff prompt.
