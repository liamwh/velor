//! Continuation-tier engine for mid-session model and provider switches.
//!
//! When the user asks to switch model or provider mid-run, Velor picks the
//! cheapest continuation strategy that preserves context fidelity, in priority
//! order:
//!
//! 1. **Live in-session** (Oh My Pi only): the persistent `omp --mode rpc`
//!    process is still alive and advertises a native model-switch command, so we
//!    swap the model in place without tearing down the session.
//! 2. **Native resume**: the from- and to-runners are the same provider and we
//!    hold its native session id, so the to-runner resumes it with
//!    `--resume <id>` (Claude/Codex/OMP).
//! 3. **Structured handoff**: nothing native is available, so we ask the
//!    from-runner to emit a structured Markdown handoff document and feed it to
//!    the to-runner's fresh session. See [`handoff`].
//!
//! [`decide_tier`] is a pure function over [`ContinuationContext`] and the
//! target runner's [`AgentCapabilities`]; the chosen [`ContinuationTier`]
//! drives the rest of the switch flow in the caller.

use crate::agent::AgentRunnerKind;
use crate::execution_service::capabilities::AgentCapabilities;

pub mod handoff;

pub use handoff::{HANDOFF_PROMPT_TEMPLATE, degraded_handoff, request_handoff};

/// Everything [`decide_tier`] needs to pick a continuation strategy. Collected
/// by the caller from the live run before a switch and treated as immutable
/// input by the pure decision function.
#[derive(Debug, Clone)]
pub struct ContinuationContext {
    /// The runner kind that produced the session being switched away from.
    pub from_kind: AgentRunnerKind,
    /// The runner kind the switch is targeting.
    pub to_kind: AgentRunnerKind,
    /// The provider-native session id created by the from-runner, if any
    /// (Claude/Codex/OMP `--resume` id). Drives the NativeResume tier.
    pub from_native_session_id: Option<String>,
    /// Whether the from-runner's underlying process is still alive. The
    /// Fall back to a structured Markdown handoff document: ask the from-runner
    /// to write one, then feed it to a fresh to-runner session. The weakest
    /// fidelity but universally available.
    pub omp_process_alive: bool,
    /// The target model in `provider/modelId` form (e.g.
    /// `anthropic/claude-sonnet-4`). Tier 1 requires this exact slashed form so
    /// the native `set_model` command receives a well-formed provider+model pair;
    /// a bare model id or [`None`] does not qualify.
    pub target_model: Option<String>,
}

/// The continuation strategy [`decide_tier`] selects for a switch. Ordered from
/// highest context fidelity (live in-session) to lowest (structured handoff).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContinuationTier {
    /// Keep the same live persistent process and switch its model in place via
    /// the provider's native model-switch command (Oh My Pi only).
    LiveInSession,
    /// Resume the provider-native session by id on the to-runner
    /// (`--resume <id>`). Same provider on both sides and a known session id.
    NativeResume {
        /// The provider-native session id the to-runner should resume.
        session_id: String,
    },
    /// Fall back to a structured Markdown handoff document: ask the from-runner
        /// to write one, then feed it to a fresh to-runner session. The weakest
        /// fidelity but universally available.
    StructuredHandoff,
}

impl ContinuationTier {
    /// Returns a short, human-readable label for TUI display (e.g. on the
    /// model-switch confirmation line).
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::LiveInSession => "live in-session",
            Self::NativeResume { .. } => "native resume",
            Self::StructuredHandoff => "structured handoff",
        }
    }
}

impl std::fmt::Display for ContinuationTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// Picks the highest-fidelity continuation tier available for `ctx` given the
/// to-runner's static `capabilities`.
///
/// # Decision rules (in priority order)
///
/// 1. [`ContinuationTier::LiveInSession`] — the from- and to-runners are both
///    Oh My Pi, the persistent process is still alive, the adapter advertises
///    `persistent_session`, and `target_model` is in `provider/modelId` form.
/// 2. [`ContinuationTier::NativeResume`] — the from- and to-runners are the same
///    provider and we hold the from-runner's native session id.
/// 3. [`ContinuationTier::StructuredHandoff`] — anything else.
///
/// Pure: no I/O; the only allocation is the carried `session_id` clone on the
/// NativeResume tier.
#[must_use]
#[tracing::instrument(level = "debug", skip(capabilities))]
pub fn decide_tier(
    ctx: &ContinuationContext,
    capabilities: AgentCapabilities,
) -> ContinuationTier {
    // Tier 1: live in-session model switch. Only OMP keeps a long-lived
    // persistent process whose model can be swapped without a teardown, and
    // only a `provider/modelId` target gives the native command a well-formed
    // provider+model pair.
    if ctx.from_kind == AgentRunnerKind::Omp
        && ctx.to_kind == AgentRunnerKind::Omp
        && ctx.omp_process_alive
        && capabilities.persistent_session
        && ctx.target_model.as_deref().is_some_and(|m| m.contains('/'))
    {
        return ContinuationTier::LiveInSession;
    }

    // Tier 2: same provider on both sides with a known native session id. The
    // to-runner resumes it with its provider-native `--resume <id>` flag.
    if ctx.from_kind == ctx.to_kind
        && let Some(session_id) = ctx.from_native_session_id.as_deref()
    {
        return ContinuationTier::NativeResume {
            session_id: session_id.to_string(),
        };
    }

    // Tier 3: structured handoff fallback.
    ContinuationTier::StructuredHandoff
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Session id used across the decision-table tests.
    const TEST_SESSION_ID: &str = "sess-123";

    /// All four runner kinds, for exhaustive enumeration.
    const KINDS: [AgentRunnerKind; 4] = [
        AgentRunnerKind::ClaudeSubprocess,
        AgentRunnerKind::ClaudeAcp,
        AgentRunnerKind::Codex,
        AgentRunnerKind::Omp,
    ];

    /// `None` target model.
    const MODEL_NONE: Option<&str> = None;
    /// Target model in `provider/modelId` form (qualifies for tier 1).
    const MODEL_SLASH: Option<&str> = Some("anthropic/claude-sonnet-4");
    /// Bare model id with no provider (does NOT qualify for tier 1).
    const MODEL_NO_SLASH: Option<&str> = Some("claude-sonnet-4");

    /// Builds a [`ContinuationContext`] for the decision-table tests.
    fn ctx(
        from: AgentRunnerKind,
        to: AgentRunnerKind,
        alive: bool,
        has_id: bool,
        model: Option<&str>,
    ) -> ContinuationContext {
        ContinuationContext {
            from_kind: from,
            to_kind: to,
            from_native_session_id: has_id.then(|| TEST_SESSION_ID.to_string()),
            omp_process_alive: alive,
            target_model: model.map(str::to_string),
        }
    }

    // --- focused positive/negative cases ------------------------------------

    #[test]
    fn live_in_session_when_all_conditions_hold() {
        // OMP->OMP, alive, persistent-session capability, slashed target model.
        let ctx = ctx(
            AgentRunnerKind::Omp,
            AgentRunnerKind::Omp,
            true,
            true,
            MODEL_SLASH,
        );
        assert_eq!(
            decide_tier(&ctx, AgentCapabilities::omp()),
            ContinuationTier::LiveInSession
        );
        // Tier 1 wins even though tier 2 preconditions also hold (same kind +
        // id): highest fidelity short-circuits.
    }

    #[test]
    fn live_in_session_label_and_display() {
        assert_eq!(ContinuationTier::LiveInSession.label(), "live in-session");
        assert_eq!(
            ContinuationTier::NativeResume {
                session_id: "x".to_string()
            }
            .label(),
            "native resume"
        );
        assert_eq!(
            ContinuationTier::StructuredHandoff.label(),
            "structured handoff"
        );
        assert_eq!(
            ContinuationTier::LiveInSession.to_string(),
            "live in-session"
        );
    }

    #[test]
    fn live_in_session_requires_omp_on_both_sides() {
        let caps = AgentCapabilities::omp();
        // OMP->Codex must not be live in-session even if alive + slashed model.
        let ctx = ctx(
            AgentRunnerKind::Omp,
            AgentRunnerKind::Codex,
            true,
            false,
            MODEL_SLASH,
        );
        assert_ne!(decide_tier(&ctx, caps), ContinuationTier::LiveInSession);
    }

    #[test]
    fn live_in_session_requires_alive_process() {
        let ctx = ctx(
            AgentRunnerKind::Omp,
            AgentRunnerKind::Omp,
            false,
            true,
            MODEL_SLASH,
        );
        // Alive=false knocks out tier 1; falls through to tier 2 (same kind +
        // id present).
        assert_eq!(
            decide_tier(&ctx, AgentCapabilities::omp()),
            ContinuationTier::NativeResume {
                session_id: TEST_SESSION_ID.to_string()
            }
        );
    }

    #[test]
    fn live_in_session_requires_slashed_model() {
        let caps = AgentCapabilities::omp();
        // Bare model id: tier 1 does not qualify.
        let bare = ctx(
            AgentRunnerKind::Omp,
            AgentRunnerKind::Omp,
            true,
            false,
            MODEL_NO_SLASH,
        );
        assert_ne!(decide_tier(&bare, caps), ContinuationTier::LiveInSession);
        // Missing model: tier 1 does not qualify.
        let none = ctx(
            AgentRunnerKind::Omp,
            AgentRunnerKind::Omp,
            true,
            false,
            MODEL_NONE,
        );
        assert_ne!(decide_tier(&none, caps), ContinuationTier::LiveInSession);
    }

    #[test]
    fn live_in_session_requires_persistent_session_capability() {
        // Same OMP->OMP, alive, slashed model, but NO persistent_session
        // capability: tier 1 is out, falls to tier 2 (same kind + id).
        let ctx = ctx(
            AgentRunnerKind::Omp,
            AgentRunnerKind::Omp,
            true,
            true,
            MODEL_SLASH,
        );
        let caps = AgentCapabilities {
            persistent_session: false,
            ..AgentCapabilities::omp()
        };
        assert_eq!(
            decide_tier(&ctx, caps),
            ContinuationTier::NativeResume {
                session_id: TEST_SESSION_ID.to_string()
            }
        );
        // With the capability restored, tier 1 applies.
        assert_eq!(
            decide_tier(&ctx, AgentCapabilities::omp()),
            ContinuationTier::LiveInSession
        );
    }

    #[test]
    fn native_resume_same_kind_with_id() {
        // Same kind, id present, but NOT an OMP->OMP alive+slashed scenario, so
        // tier 2 is selected and carries the session id through verbatim.
        let ctx = ctx(
            AgentRunnerKind::Codex,
            AgentRunnerKind::Codex,
            false,
            true,
            MODEL_NONE,
        );
        assert_eq!(
            decide_tier(&ctx, AgentCapabilities::none()),
            ContinuationTier::NativeResume {
                session_id: TEST_SESSION_ID.to_string()
            }
        );
    }

    #[test]
    fn native_resume_absent_when_no_session_id() {
        // Same kind but no native session id: cannot resume.
        let ctx = ctx(
            AgentRunnerKind::ClaudeSubprocess,
            AgentRunnerKind::ClaudeSubprocess,
            false,
            false,
            MODEL_NONE,
        );
        assert_eq!(
            decide_tier(&ctx, AgentCapabilities::with_live_steering()),
            ContinuationTier::StructuredHandoff
        );
    }

    #[test]
    fn structured_handoff_when_kinds_differ() {
        let ctx = ctx(
            AgentRunnerKind::ClaudeSubprocess,
            AgentRunnerKind::Codex,
            false,
            true,
            MODEL_NONE,
        );
        assert_eq!(
            decide_tier(&ctx, AgentCapabilities::none()),
            ContinuationTier::StructuredHandoff
        );
    }

    #[test]
    fn structured_handoff_when_everything_is_missing() {
        let ctx = ctx(
            AgentRunnerKind::ClaudeAcp,
            AgentRunnerKind::ClaudeAcp,
            false,
            false,
            MODEL_NONE,
        );
        assert_eq!(
            decide_tier(&ctx, AgentCapabilities::none()),
            ContinuationTier::StructuredHandoff
        );
    }

    // --- exhaustive decision table -----------------------------------------
    //
    // The reference below restates the documented rules (the *specification*),
    // not the implementation: it pins the entire decision table across the
    // five input dimensions so any drift in `decide_tier` fails loudly. We use
    // `AgentCapabilities::omp()` (persistent_session == true) so the capability
    // gate is satisfied and tier 1 reduces to a pure function of the five
    // enumerated dimensions.

    #[test]
    fn decide_tier_exhaustive_decision_table() {
        let caps = AgentCapabilities::omp();
        for &from in &KINDS {
            for &to in &KINDS {
                for &alive in [false, true] {
                    for &has_id in [false, true] {
                        for &model in [MODEL_NONE, MODEL_SLASH, MODEL_NO_SLASH] {
                            let ctx = ctx(from, to, alive, has_id, model);
                            let tier = decide_tier(&ctx, caps);

                            // Reference rule for tier 1: both sides OMP,
                            // alive, slashed model (capability gate is
                            // satisfied by the fixed `caps`).
                            let tier1 = from == AgentRunnerKind::Omp
                                && to == AgentRunnerKind::Omp
                                && alive
                                && model == MODEL_SLASH;

                            if tier1 {
                                assert_eq!(
                                    tier,
                                    ContinuationTier::LiveInSession,
                                    "expected LiveInSession for {ctx:?}"
                                );
                            } else {
                                // Reference rule for tier 2: same kind and a
                                // known native session id.
                                let tier2 = from == to && has_id;
                                if tier2 {
                                    match tier {
                                        ContinuationTier::NativeResume { session_id } => {
                                            assert_eq!(
                                                session_id, TEST_SESSION_ID,
                                                "NativeResume carried wrong id for {ctx:?}"
                                            );
                                        }
                                        other => panic!(
                                            "expected NativeResume for {ctx:?}, got {other:?}"
                                        ),
                                    }
                                } else {
                                    assert_eq!(
                                        tier,
                                        ContinuationTier::StructuredHandoff,
                                        "expected StructuredHandoff for {ctx:?}"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// Invariant: tier 1 is never returned unless every individual precondition
    /// holds, regardless of which other dimension varies. Checked across the
    /// full cross-product as an independent guard on the decision table.
    #[test]
    fn live_in_session_only_when_all_preconditions_hold() {
        let caps = AgentCapabilities::omp();
        for &from in &KINDS {
            for &to in &KINDS {
                for &alive in [false, true] {
                    for &model in [MODEL_NONE, MODEL_SLASH, MODEL_NO_SLASH] {
                        let ctx = ctx(from, to, alive, false, model);
                        let tier = decide_tier(&ctx, caps);
                        let all_hold = from == AgentRunnerKind::Omp
                            && to == AgentRunnerKind::Omp
                            && alive
                            && model == MODEL_SLASH;
                        assert_eq!(
                            tier == ContinuationTier::LiveInSession,
                            all_hold,
                            "LiveInSession gating mismatch for {ctx:?}"
                        );
                    }
                }
            }
        }
    }
}
