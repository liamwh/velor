//! Adapter contracts: how protocol adapters consume supervisor byte-streams and
//! emit structured [`AgentEvent`]s.
//!
//! Adapters own framing: supervisor bytes → UTF-8 → newline-delimited frames →
//! protocol messages (stream-json for Claude, JSONL for Codex). The generic
//! [`crate::execution_service::supervisor`] never sees lines.

use async_trait::async_trait;

use crate::execution_service::error::AgentProtocolError;

/// A sink for adapter-emitted [`AgentEvent`]s.
///
/// Implemented by consumers (CLI terminal renderer, serve `RunnerProgressEvent`
/// adapter, Tauri `ExecutionRecord` adapter). Sinks must not block process
/// draining: the supervisor drains unconditionally; a slow sink sees coalesced or
/// dropped verbose events.
#[async_trait(?Send)]
pub trait AgentEventSink {
    /// Emits one event. Returns `Err` only if the consumer wishes to abort.
    ///
    /// # Errors
    /// Implementations return an error to signal the consumer has gone away.
    async fn emit(&mut self, event: crate::agent::AgentEvent) -> Result<(), AgentSinkError>;
}

/// Error returned by an [`AgentEventSink`] when the consumer has gone away.
#[derive(Debug, thiserror::Error)]
#[error("agent event sink closed")]
pub struct AgentSinkError;

/// A provider adapter that drives one agent invocation over the supervisor.
///
/// `?Send` because the ACP adapter is `!Send` (it runs on the service's
/// dedicated `LocalSet`); subprocess/codex adapters are natively `Send`.
#[async_trait(?Send)]
pub trait AgentAdapter {
    /// Runs one invocation, emitting events to `sink` and returning the result.
    ///
    /// # Errors
    /// Returns [`crate::execution_service::error::AgentExecutionError`] on any
    /// failure (process, protocol, provider, unsuccessful exit, cancellation).
    async fn execute(
        &mut self,
        sink: &mut dyn AgentEventSink,
    ) -> Result<crate::agent::AgentRunResult, crate::execution_service::error::AgentExecutionError>;
}

/// Incremental newline-delimited frame decoder with a maximum frame length.
///
/// Feeds raw [`bytes::Bytes`] chunks (from the supervisor) and yields complete
/// lines. A line longer than `max_frame_bytes` (without a newline) yields
/// [`AgentProtocolError::FrameTooLong`], bounding memory against runaway output.
pub struct LineDecoder {
    buf: Vec<u8>,
    max_frame_bytes: usize,
}

impl LineDecoder {
    /// Creates a decoder with the given maximum frame (line) length.
    #[must_use]
    pub fn new(max_frame_bytes: usize) -> Self {
        Self {
            buf: Vec::new(),
            max_frame_bytes,
        }
    }

    /// Feeds a chunk and returns any complete (newline-terminated) frames. The
    /// returned frames exclude the trailing newline.
    ///
    /// # Errors
    /// Returns [`AgentProtocolError::FrameTooLong`] if a frame exceeds the cap.
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<Vec<u8>>, AgentProtocolError> {
        self.buf.extend_from_slice(chunk);
        let mut lines = Vec::new();
        loop {
            let Some(nl) = self.buf.iter().position(|&b| b == b'\n') else {
                // No complete line yet; guard against unbounded growth.
                if self.buf.len() > self.max_frame_bytes {
                    self.buf.clear();
                    return Err(AgentProtocolError::FrameTooLong {
                        max: self.max_frame_bytes,
                    });
                }
                break;
            };
            if nl > self.max_frame_bytes {
                self.buf.clear();
                return Err(AgentProtocolError::FrameTooLong {
                    max: self.max_frame_bytes,
                });
            }
            // Own the frame (newline excluded) before mutating the buffer.
            let line: Vec<u8> = self.buf.drain(..=nl).take(nl).collect();
            lines.push(line);
        }
        Ok(lines)
    }

    /// Flushes any trailing bytes (no terminating newline) as a final owned
    /// frame. Terminal: call once after EOF.
    ///
    /// # Errors
    /// Returns [`AgentProtocolError::FrameTooLong`] if the remainder exceeds the cap.
    pub fn flush_remainder(&mut self) -> Result<Option<Vec<u8>>, AgentProtocolError> {
        if self.buf.is_empty() {
            return Ok(None);
        }
        if self.buf.len() > self.max_frame_bytes {
            self.buf.clear();
            return Err(AgentProtocolError::FrameTooLong {
                max: self.max_frame_bytes,
            });
        }
        Ok(Some(std::mem::take(&mut self.buf)))
    }
}

/// Decoded text frame (owned) to hand to protocol parsers without lifetime
/// entanglement with the decoder buffer.
#[derive(Debug, Clone)]
pub struct TextFrame {
    /// The frame bytes, valid UTF-8.
    pub text: String,
}

impl TextFrame {
    /// Lossily decodes bytes to a UTF-8 frame.
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self {
            text: String::from_utf8_lossy(bytes).into_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_on_newlines() {
        let mut d = LineDecoder::new(1024);
        let lines = d.push(b"one\ntwo\n").unwrap();
        assert_eq!(lines, vec![b"one".to_vec(), b"two".to_vec()]);
    }

    #[test]
    fn accumulates_partial_lines() {
        let mut d = LineDecoder::new(1024);
        assert!(d.push(b"hel").unwrap().is_empty());
        assert!(d.push(b"lo").unwrap().is_empty());
        let lines = d.push(b" world\n").unwrap();
        assert_eq!(lines, vec![b"hello world".to_vec()]);
    }

    #[test]
    fn handles_carriage_returns() {
        let mut d = LineDecoder::new(1024);
        let lines = d.push(b"a\r\nb\r\n").unwrap();
        // '\n' delimits; the trailing '\r' remains in the frame for parsers to trim.
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], b"a\r");
        assert_eq!(lines[1], b"b\r");
    }

    #[test]
    fn frame_too_long_errors() {
        let mut d = LineDecoder::new(4);
        let err = d.push(b"abcdefgh").unwrap_err();
        assert!(matches!(err, AgentProtocolError::FrameTooLong { max: 4 }));
    }

    #[test]
    fn flush_emits_trailing_frame() {
        let mut d = LineDecoder::new(1024);
        d.push(b"complete\ntrailing-no-newline").unwrap();
        let last = d.flush_remainder().unwrap();
        assert_eq!(last, Some(b"trailing-no-newline".to_vec()));
    }
}
