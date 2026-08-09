//! Capability and runtime-availability model for live agent steering.
//!
//! Distinguishes a *static capability* (does this adapter support live steering
//! at all?) from the *current runtime availability* (is a writable, active
//! steering session connected *right now*?). The two answer different questions:
//!
//! - [`AgentCapabilities`] is set by the adapter/profile and does not change for
//!   the life of an execution attempt.
//! - [`LiveSteeringStatus`] reflects the live process state and drives whether a
//!   steering command is forwarded (`Ready`) or rejected with a precise reason
//!   (`Unsupported` / `Inactive` / `Closing`).

/// Static capabilities advertised by an adapter/profile: which runtime session
/// operations it natively supports. `Copy` so callers can match on it cheaply
/// when deciding whether to even offer an affordance (e.g. the TUI's `i`/`f`
/// keys). A runner MUST NOT be offered an action whose capability is `false` —
/// there is no cross-runner emulation of a missing native capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentCapabilities {
    /// Steer the active turn with a one-shot message that interrupts it at the
    /// next safe boundary (Claude's `--replay-user-messages` streaming path;
    /// Oh My Pi's `steer` RPC command).
    pub live_steering: bool,
    /// Queue a message for *after* the active turn finishes, rather than
    /// interrupting it now (Oh My Pi's `follow_up` RPC command). Distinct from
    /// `live_steering`: a provider can support one without the other.
    pub follow_up: bool,
    /// Natively abort the active turn (Oh My Pi's `abort` RPC command) without
    /// tearing down the underlying process/session — as opposed to the
    /// universally-available hard cancellation (killing the subprocess), which
    /// every provider supports regardless of this flag.
    pub native_abort: bool,
    /// Query the model currently in effect for the active session.
    pub query_model: bool,
    /// Change the model for the active session mid-run.
    pub change_model: bool,
    /// Query broader session state (thinking level, queue modes, token usage,
    /// todo state, …) beyond the model.
    pub query_session_state: bool,
    /// Resume a previously-created native provider session by id (Claude's
    /// `--resume`, Codex's `resume`, Oh My Pi's `--resume`). ACP has none.
    pub native_resume: bool,
    /// Whether this runner can be driven as one long-lived persistent process
    /// spanning multiple turns (Oh My Pi's `omp --mode rpc` keeps stdin open
    /// between turns; Claude/Codex subprocesses exit after each turn).
    pub persistent_session: bool,
}
impl AgentCapabilities {
    /// No native session-operation support at all (Codex, ACP).
    #[must_use]
    pub const fn none() -> Self {
        Self {
            live_steering: false,
            follow_up: false,
            native_abort: false,
            query_model: false,
            change_model: false,
            query_session_state: false,
            native_resume: false,
            persistent_session: false,
        }
    }

    /// Claude's streaming path: one-shot live steering + native resume (via
    /// `--resume <id>`), but no persistent session (subprocess exits per turn).
    #[must_use]
    pub const fn with_live_steering() -> Self {
        Self {
            live_steering: true,
            native_resume: true,
            ..Self::none()
        }
    }

    /// Oh My Pi's RPC session: the full native surface — steer, follow-up,
    /// abort, model/session introspection, native resume, and a persistent
    /// session (one long-lived `omp --mode rpc` process spanning turns).
    #[must_use]
    pub const fn omp() -> Self {
        Self {
            live_steering: true,
            follow_up: true,
            native_abort: true,
            query_model: true,
            change_model: true,
            query_session_state: true,
            native_resume: true,
            persistent_session: true,
        }
    }

    /// Whether any runtime input (steer, follow-up, or native abort) can be
    /// forwarded to this execution. Drives whether the runner-side live-input
    /// channel is worth allocating at all.
    #[must_use]
    pub const fn accepts_live_input(&self) -> bool {
        self.live_steering || self.follow_up || self.native_abort
    }
}

/// The current runtime availability of a live-steering session. Independent of
/// the static [`AgentCapabilities`]: a Claude subprocess supports steering, but
/// is only `Ready` while its streaming stdin is open and the process is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveSteeringStatus {
    /// The adapter/profile does not support live steering at all (Codex, ACP).
    Unsupported,
    /// No active execution is connected (e.g. between iterations).
    Inactive,
    /// A writable streaming-input session is connected and the process is active.
    Ready,
    /// The active session is winding down (stdin closing or process terminating).
    Closing,
}

impl LiveSteeringStatus {
    /// Returns `true` only when steering can be forwarded right now.
    #[must_use]
    pub const fn is_ready(self) -> bool {
        matches!(self, Self::Ready)
    }
}
