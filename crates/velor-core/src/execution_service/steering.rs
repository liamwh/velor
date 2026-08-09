//! Provider-neutral live-session input types.
//!
//! These types are shared by every adapter capable of runtime session input
//! (currently the Claude streaming path and Oh My Pi's RPC session) and by the
//! TUI layer that composes them. Wire framing stays adapter-specific (see
//! [`crate::execution_service::adapters::claude_stream`] and
//! [`crate::execution_service::adapters::omp`]); this module only owns the
//! semantically-validated text and the behaviour discriminant.

/// One-shot live-session text: a steering message or a follow-up message.
/// Empty or whitespace-only text is invalid (it would steer/queue the agent
/// with nothing), so construction is fallible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SteeringText(String);

impl SteeringText {
    /// Creates steering text from any string-like input.
    ///
    /// # Errors
    /// Returns [`SteeringTextError::Empty`] if `value` is empty or
    /// whitespace-only.
    pub fn new(value: impl Into<String>) -> Result<Self, SteeringTextError> {
        let s = value.into();
        if s.trim().is_empty() {
            return Err(SteeringTextError::Empty);
        }
        Ok(Self(s))
    }

    /// Returns the steering text as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Error constructing [`SteeringText`].
#[derive(Debug, Clone, thiserror::Error)]
pub enum SteeringTextError {
    /// The supplied text was empty or whitespace-only.
    #[error("steering text must not be empty or whitespace-only")]
    Empty,
}

/// The persistent append — extra user instructions folded into the end of every
/// subsequent iteration prompt. Unlike [`SteeringText`], an empty value is
/// meaningful here: it clears the append. Construction therefore returns
/// `Option`, where `None` means "empty → clear".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistentAppend(String);

impl PersistentAppend {
    /// Creates a persistent append. Returns `None` for empty/whitespace-only
    /// input, signalling that the append should be cleared rather than stored.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Option<Self> {
        let s = value.into();
        if s.trim().is_empty() {
            return None;
        }
        Some(Self(s))
    }

    /// Returns the append text as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Which of the two runtime-input semantics a [`SteeringText`] carries.
///
/// The two are meaningfully different, not two names for the same action:
/// - `Steer` interrupts the active turn at the next safe boundary and
///   redirects it — no work is discarded, the session simply adjusts course.
/// - `FollowUp` queues the message to run *after* the active turn finishes,
///   leaving the current turn's course untouched.
///
/// Providers advertise support for each independently via
/// [`crate::execution_service::capabilities::AgentCapabilities`]; a provider
/// without native follow-up support never receives a `FollowUp`-behaviour
/// input (the TUI does not offer the affordance, and nothing emulates it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SteeringBehaviour {
    /// Redirect the active turn now.
    Steer,
    /// Queue for after the active turn finishes.
    FollowUp,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_steering_text_is_rejected() {
        assert!(SteeringText::new("").is_err());
        assert!(SteeringText::new("   ").is_err());
        assert!(SteeringText::new("\n\t").is_err());
        assert!(SteeringText::new("real").is_ok());
    }

    #[test]
    fn persistent_append_clears_on_empty() {
        assert!(PersistentAppend::new("").is_none());
        assert!(PersistentAppend::new("  \n").is_none());
        let append = PersistentAppend::new("do the thing").unwrap();
        assert_eq!(append.as_str(), "do the thing");
    }

    #[test]
    fn steering_behaviour_variants_are_distinct() {
        assert_ne!(SteeringBehaviour::Steer, SteeringBehaviour::FollowUp);
    }
}
