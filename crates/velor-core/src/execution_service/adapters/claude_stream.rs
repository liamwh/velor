//! Claude Code `stream-json` wire framing and parsing.
//!
//! This is the **single** place that knows about the Claude Code `stream-json`
//! protocol. The generic [`crate::execution_service::supervisor`] never sees
//! JSON, NDJSON, or any Claude semantics — it only carries [`bytes::Bytes`]. A
//! future Claude Code protocol change should be localised primarily to this
//! module.
//!
//! Responsibilities:
//!
//! - **Input framing** ([`frame_user_message`]): turn a [`SteeringText`] into
//!   exactly one compact JSON object followed by one newline.
//! - **Semantic text types** ([`SteeringText`], [`PersistentAppend`]): naked
//!   strings are never accepted where a semantically meaningful input is
//!   required, so empty/whitespace-only values cannot slip through.
//! - **Output parsing** ([`parse_output_event`]): a tolerant envelope that keeps
//!   the session running when Claude Code emits a new, unknown-but-valid event
//!   type, while still surfacing genuine protocol errors.
//!
//! This module is internal and version-sensitive: the wire types here mirror a
//! specific Claude Code schema and may need adjustment when that schema changes.

use bytes::Bytes;
use serde_json::Value;

// ── Semantic text types ─────────────────────────────────────────────────────

/// One-shot live-steering text sent to the active Claude session. Empty or
/// whitespace-only text is invalid (it would steer the agent with nothing), so
/// construction is fallible.
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

// ── Input framing ───────────────────────────────────────────────────────────

/// The internal, version-sensitive wire representation of one Claude
/// `stream-json` input message. Kept private: callers frame via
/// [`frame_user_message`] and never assemble raw JSON.
#[derive(Debug, serde::Serialize)]
struct ClaudeStreamInputMessage {
    #[serde(rename = "type")]
    kind: &'static str,
    message: InputMessage,
}

#[derive(Debug, serde::Serialize)]
struct InputMessage {
    role: &'static str,
    content: Vec<InputContentBlock>,
}

#[derive(Debug, serde::Serialize)]
struct InputContentBlock {
    #[serde(rename = "type")]
    kind: &'static str,
    text: String,
}

impl ClaudeStreamInputMessage {
    /// Builds a `user` turn carrying `text` as a single text content block.
    fn user_text(text: &SteeringText) -> Self {
        Self {
            kind: "user",
            message: InputMessage {
                role: "user",
                content: vec![InputContentBlock {
                    kind: "text",
                    text: text.as_str().to_string(),
                }],
            },
        }
    }
}

/// Frames `text` as exactly one Claude `stream-json` user message: one compact
/// JSON object followed by a single trailing newline, with no additional NDJSON
/// records.
///
/// # Errors
/// Returns [`ClaudeProtocolError::Serialisation`] if the message cannot be
/// serialised (the structure is fixed, so this is effectively impossible in
/// practice, but the fallibility is preserved for honesty).
pub fn frame_user_message(text: &SteeringText) -> Result<Bytes, ClaudeProtocolError> {
    let msg = ClaudeStreamInputMessage::user_text(text);
    let mut bytes =
        serde_json::to_vec(&msg).map_err(|source| ClaudeProtocolError::Serialisation { source })?;
    bytes.push(b'\n');
    Ok(Bytes::from(bytes))
}

// ── Output parsing (tolerant) ───────────────────────────────────────────────

/// A tolerant envelope around any single Claude `stream-json` output record. The
/// `event_type` is always captured; the remainder is kept verbatim so
/// unrecognised-but-valid events are preserved for diagnostics rather than
/// terminating the session.
#[derive(Debug, serde::Deserialize)]
struct ClaudeStreamEnvelope {
    #[serde(rename = "type")]
    event_type: String,
    #[serde(flatten)]
    payload: serde_json::Map<String, Value>,
}

/// A parsed Claude `stream-json` output event. Recognised frames become typed
/// variants; valid-but-unknown frames become [`ClaudeOutputEvent::Unknown`] so a
/// future Claude Code event type does not abort the run.
#[derive(Debug, Clone)]
pub enum ClaudeOutputEvent {
    /// A user message echoed back by `--replay-user-messages`. Used to
    /// acknowledge that a steering message reached Claude.
    ReplayedUserMessage(ReplayedUserMessage),
    /// A recognised event whose type we do not need to act on here (the adapter
    /// extracts display content from these via the shared parser).
    Other {
        /// The Claude event type string (e.g. `assistant`, `result`).
        event_type: String,
    },
    /// A valid JSON record whose `type` this version does not recognise.
    /// Preserved verbatim so diagnostics can show exactly what Claude sent.
    Unknown {
        /// The unrecognised event type string.
        event_type: String,
        /// The raw record, for diagnostics.
        record: Value,
    },
}

/// A user message echoed by Claude Code's `--replay-user-messages` flag — the
/// mechanism by which live-steering delivery can be acknowledged.
#[derive(Debug, Clone)]
pub struct ReplayedUserMessage {
    /// The text of the replayed user message.
    pub text: SteeringText,
}

/// Parses one NDJSON line of Claude `stream-json` output into a typed event.
///
/// Malformed JSON (a non-object record, or a record without a `type` field) is a
/// protocol error. A valid record with a known or unknown `type` yields a typed
/// [`ClaudeOutputEvent`] — unknown types never terminate the session.
///
/// # Errors
/// Returns [`ClaudeProtocolError::MalformedOutput`] if the line is not a valid
/// JSON object with a `type` field.
pub fn parse_output_event(line: &str) -> Result<ClaudeOutputEvent, ClaudeProtocolError> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Err(ClaudeProtocolError::MalformedOutput {
            line: line.to_string(),
            detail: "empty output line".to_string(),
        });
    }
    let value: Value =
        serde_json::from_str(trimmed).map_err(|e| ClaudeProtocolError::MalformedOutput {
            line: line.to_string(),
            detail: e.to_string(),
        })?;
    let record = match &value {
        Value::Object(_) => value,
        _ => {
            return Err(ClaudeProtocolError::MalformedOutput {
                line: line.to_string(),
                detail: "expected a JSON object".to_string(),
            });
        }
    };
    let envelope: ClaudeStreamEnvelope = serde_json::from_value(record.clone()).map_err(|e| {
        ClaudeProtocolError::MalformedOutput {
            line: line.to_string(),
            detail: format!("missing or invalid `type`: {e}"),
        }
    })?;

    match envelope.event_type.as_str() {
        "user" => {
            if let Some(replay) = extract_replayed_user_message(&envelope.payload) {
                Ok(ClaudeOutputEvent::ReplayedUserMessage(replay))
            } else {
                Ok(ClaudeOutputEvent::Other {
                    event_type: envelope.event_type,
                })
            }
        }
        // Known event types the adapter handles via the shared display parser.
        "assistant"
        | "system"
        | "result"
        | "content_block_start"
        | "content_block_delta"
        | "content_block_stop"
        | "message_start"
        | "message_delta"
        | "message_stop" => Ok(ClaudeOutputEvent::Other {
            event_type: envelope.event_type,
        }),
        _ => Ok(ClaudeOutputEvent::Unknown {
            event_type: envelope.event_type,
            record,
        }),
    }
}

/// Extracts a [`ReplayedUserMessage`] from a `user`-typed envelope's payload, if
/// it carries a textual user message (the shape echoed by
/// `--replay-user-messages`).
fn extract_replayed_user_message(
    payload: &serde_json::Map<String, Value>,
) -> Option<ReplayedUserMessage> {
    let message = payload.get("message")?;
    // `content` may be a string or an array of content blocks.
    let text = match message.get("content")? {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => {
            let mut joined = String::new();
            for block in blocks {
                if block.get("type").and_then(Value::as_str) == Some("text")
                    && let Some(t) = block.get("text").and_then(Value::as_str)
                {
                    joined.push_str(t);
                }
            }
            joined
        }
        _ => return None,
    };
    SteeringText::new(text)
        .ok()
        .map(|text| ReplayedUserMessage { text })
}

// ── Errors ──────────────────────────────────────────────────────────────────

/// An error in the Claude `stream-json` protocol (input framing or output
/// parsing).
#[derive(Debug, thiserror::Error)]
pub enum ClaudeProtocolError {
    /// Claude Code rejected the stream-json input schema (e.g. our user-message
    /// shape is no longer accepted).
    #[error("Claude Code rejected the stream-json input schema (version={version:?}): {detail}")]
    SchemaRejected {
        /// The Claude Code version that rejected it, if known.
        version: Option<String>,
        /// Why the schema was rejected.
        detail: String,
    },
    /// A Claude stream-json input message could not be serialised.
    #[error("failed to serialise a Claude stream-json input message")]
    Serialisation {
        /// The underlying serde error.
        #[source]
        source: serde_json::Error,
    },
    /// Claude Code emitted malformed stream-json output.
    #[error("received malformed Claude stream-json output: {detail}")]
    MalformedOutput {
        /// The offending line.
        line: String,
        /// Why it could not be parsed.
        detail: String,
    },
    /// Claude Code reported an explicit stream protocol error.
    #[error("Claude Code reported a stream protocol error: {detail}")]
    Remote {
        /// The reported error detail.
        detail: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(text: &str) -> Bytes {
        frame_user_message(&SteeringText::new(text).unwrap()).unwrap()
    }

    #[test]
    fn user_message_frame_is_valid_compact_json() {
        let bytes = frame("hello world");
        let s = std::str::from_utf8(&bytes).unwrap();
        // Compact: no pretty-printing whitespace between tokens.
        assert!(!s.contains("  "), "frame is compact");
        let without_newline = s.trim_end_matches('\n');
        let value: Value = serde_json::from_str(without_newline).expect("valid JSON object");
        assert_eq!(value["type"], "user");
        assert_eq!(value["message"]["role"], "user");
        assert_eq!(value["message"]["content"][0]["type"], "text");
        assert_eq!(value["message"]["content"][0]["text"], "hello world");
    }

    #[test]
    fn user_message_frame_has_exactly_one_trailing_newline() {
        let bytes = frame("x");
        assert!(bytes.ends_with(b"\n"), "ends with a newline");
        // Exactly one trailing newline, no extra records.
        assert_eq!(
            bytes.iter().filter(|&&b| b == b'\n').count(),
            1,
            "exactly one newline total"
        );
    }

    #[test]
    fn user_message_frame_has_no_extra_ndjson_records() {
        let bytes = frame("one");
        let s = std::str::from_utf8(&bytes).unwrap();
        let lines: Vec<&str> = s.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(lines.len(), 1, "exactly one NDJSON record");
    }

    #[test]
    fn user_message_frame_escapes_special_characters() {
        let bytes = frame("a\"b\nc\\d");
        let s = std::str::from_utf8(&bytes).unwrap();
        // The frame is still one line (newline escaped inside JSON).
        assert_eq!(s.lines().count(), 1);
        let value: Value = serde_json::from_str(s.trim_end()).unwrap();
        assert_eq!(value["message"]["content"][0]["text"], "a\"b\nc\\d");
    }

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
    fn parsed_replayed_user_message_array_content() {
        let line = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"steer this"}]}}"#;
        let event = parse_output_event(line).unwrap();
        match event {
            ClaudeOutputEvent::ReplayedUserMessage(ReplayedUserMessage { text }) => {
                assert_eq!(text.as_str(), "steer this");
            }
            other => panic!("expected ReplayedUserMessage, got {other:?}"),
        }
    }

    #[test]
    fn parsed_replayed_user_message_string_content() {
        let line = r#"{"type":"user","message":{"role":"user","content":"plain string"}}"#;
        let event = parse_output_event(line).unwrap();
        match event {
            ClaudeOutputEvent::ReplayedUserMessage(ReplayedUserMessage { text }) => {
                assert_eq!(text.as_str(), "plain string");
            }
            other => panic!("expected ReplayedUserMessage, got {other:?}"),
        }
    }

    #[test]
    fn user_frame_without_text_is_other_not_replay() {
        // A `user` frame without textual content is not a replay ack.
        let line = r#"{"type":"user","message":{"role":"user","content":[]}}"#;
        let event = parse_output_event(line).unwrap();
        assert!(matches!(event, ClaudeOutputEvent::Other { .. }));
    }

    #[test]
    fn known_event_types_are_other() {
        for ty in ["assistant", "result", "system", "content_block_delta"] {
            let line = format!(r#"{{"type":"{ty}"}}"#);
            let event = parse_output_event(&line).unwrap();
            assert!(
                matches!(event, ClaudeOutputEvent::Other { .. }),
                "{ty} is a known type"
            );
        }
    }

    #[test]
    fn unknown_valid_event_type_is_preserved_not_fatal() {
        let line = r#"{"type":"future_event","data":{"x":1}}"#;
        let event = parse_output_event(line).unwrap();
        match event {
            ClaudeOutputEvent::Unknown { event_type, record } => {
                assert_eq!(event_type, "future_event");
                assert_eq!(record["data"]["x"], 1);
            }
            other => panic!("unknown type preserved as Unknown, got {other:?}"),
        }
    }

    #[test]
    fn malformed_json_is_an_error() {
        let result = parse_output_event("not json at all");
        assert!(matches!(
            result,
            Err(ClaudeProtocolError::MalformedOutput { .. })
        ));
    }

    #[test]
    fn non_object_record_is_an_error() {
        let result = parse_output_event("[1, 2, 3]");
        assert!(matches!(
            result,
            Err(ClaudeProtocolError::MalformedOutput { .. })
        ));
    }

    #[test]
    fn record_without_type_field_is_an_error() {
        let result = parse_output_event(r#"{"no_type":true}"#);
        assert!(matches!(
            result,
            Err(ClaudeProtocolError::MalformedOutput { .. })
        ));
    }
}
