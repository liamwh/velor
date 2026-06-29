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

/// Static capabilities advertised by an adapter/profile. `Copy` so callers can
/// match on it cheaply when deciding whether to even offer a steering affordance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentCapabilities {
    /// Whether the adapter supports sending live steering input to an active
    /// session (only the Claude subprocess adapter's streaming path does).
    pub live_steering: bool,
}

impl AgentCapabilities {
    /// Capabilities for an adapter that supports live steering.
    #[must_use]
    pub const fn with_live_steering() -> Self {
        Self {
            live_steering: true,
        }
    }

    /// Capabilities for an adapter with no live steering support.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            live_steering: false,
        }
    }
}

impl Default for AgentCapabilities {
    fn default() -> Self {
        Self::none()
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
