//! Bounded output capture for finished processes.
//!
//! Provider errors usually appear in the *tail* of stdout (e.g. the `glm5`
//! wrapper's `echo "…"; exit 1`, or Claude Code's trailing `API Error:` lines).
//! [`CapturedOutput`] therefore retains a configurable head and tail rather than
//! the whole stream, while always reporting the true total byte count. Errors
//! and tracing records carry only [`CapturedOutput`] (counts + bounded slices);
//! the full retained bytes are never auto-printed.

use bytes::Bytes;
use std::process::ExitStatus;
use std::time::Duration;

use crate::execution_service::error::TimeoutKind;

/// Which standard stream a chunk originated from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputStream {
    /// The standard output stream.
    Stdout,
    /// The standard error stream.
    Stderr,
}

impl std::fmt::Display for OutputStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stdout => f.write_str("stdout"),
            Self::Stderr => f.write_str("stderr"),
        }
    }
}

/// Bounded captured output for one stream.
///
/// `retained_head` holds the first up-to-`cap` bytes; `retained_tail` holds the
/// last up-to-`cap` bytes; bytes in between are dropped but counted in
/// `total_bytes`. `truncated` is true when any bytes were dropped.
#[derive(Debug, Clone)]
pub struct CapturedOutput {
    /// Total number of bytes produced on this stream.
    pub total_bytes: u64,
    /// Retained leading bytes (up to the capture cap).
    pub retained_head: Bytes,
    /// Retained trailing bytes (up to the capture cap).
    pub retained_tail: Bytes,
    /// Whether any bytes were discarded between head and tail.
    pub truncated: bool,
}

impl CapturedOutput {
    /// Returns `true` when the stream produced no output at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.total_bytes == 0
    }

    /// Returns the tail as a UTF-8 string, lossily decoded.
    #[must_use]
    pub fn tail_str(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.retained_tail)
    }

    /// Returns the head as a UTF-8 string, lossily decoded.
    #[must_use]
    pub fn head_str(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.retained_head)
    }
}

/// Incremental builder for a [`CapturedOutput`] that retains a head and tail
/// within a byte cap with amortised O(1) per chunk.
#[derive(Debug)]
pub struct CaptureBuilder {
    cap: usize,
    total: u64,
    head: Vec<u8>,
    head_full: bool,
    tail: Vec<u8>,
}

impl CaptureBuilder {
    /// Creates a builder retaining up to `cap` head bytes and up to `cap` tail
    /// bytes.
    #[must_use]
    pub fn new(cap: usize) -> Self {
        Self {
            cap,
            total: 0,
            head: Vec::new(),
            head_full: cap == 0,
            tail: Vec::new(),
        }
    }

    /// Appends a chunk of bytes to the captured output.
    pub fn push(&mut self, bytes: &[u8]) {
        self.total = self.total.saturating_add(bytes.len() as u64);
        if !self.head_full {
            let space = self.cap.saturating_sub(self.head.len());
            if space > 0 {
                let take = bytes.len().min(space);
                self.head.extend_from_slice(&bytes[..take]);
            }
            if self.head.len() >= self.cap {
                self.head_full = true;
            }
        }
        self.tail.extend_from_slice(bytes);
        // Keep the tail from growing without bound: once it exceeds twice the
        // cap, drop everything except the most recent `cap` bytes.
        let double_cap = self.cap.saturating_mul(2);
        if self.tail.len() > double_cap {
            let keep_from = self.tail.len().saturating_sub(self.cap);
            self.tail.drain(..keep_from);
        }
    }

    /// Finalises the builder into a [`CapturedOutput`].
    #[must_use]
    pub fn finish(mut self) -> CapturedOutput {
        if self.tail.len() > self.cap {
            let keep_from = self.tail.len().saturating_sub(self.cap);
            self.tail.drain(..keep_from);
        }
        let retained: usize = self.head.len() + self.tail.len();
        CapturedOutput {
            total_bytes: self.total,
            truncated: (self.total as usize) > retained,
            retained_head: Bytes::from(std::mem::take(&mut self.head)),
            retained_tail: Bytes::from(std::mem::take(&mut self.tail)),
        }
    }
}

/// How a supervised process terminated.
#[derive(Debug, Clone)]
pub enum Termination {
    /// The child exited normally with this status.
    Exited(ExitStatus),
    /// The process group exceeded a configured deadline and was terminated.
    TimedOut {
        /// Which deadline was violated.
        which: TimeoutKind,
    },
    /// The process group was cancelled via the cancellation token.
    Cancelled,
}

impl Termination {
    /// Returns the exit status if the process exited normally.
    #[must_use]
    pub fn exit_status(&self) -> Option<ExitStatus> {
        match self {
            Self::Exited(status) => Some(*status),
            Self::TimedOut { .. } | Self::Cancelled => None,
        }
    }

    /// Returns `true` if the process was terminated due to a deadline.
    #[must_use]
    pub fn is_timeout(&self) -> bool {
        matches!(self, Self::TimedOut { .. })
    }

    /// Returns `true` if the process was cancelled.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled)
    }

    /// Which deadline fired, if this is a timeout termination.
    #[must_use]
    pub fn timeout_kind(&self) -> Option<TimeoutKind> {
        match self {
            Self::TimedOut { which } => Some(*which),
            _ => None,
        }
    }
}

/// The complete output of a finished (or killed) supervised process.
#[derive(Debug, Clone)]
pub struct ProcessOutput {
    /// The captured standard output.
    pub stdout: CapturedOutput,
    /// The captured standard error.
    pub stderr: CapturedOutput,
    /// How the process terminated.
    pub termination: Termination,
    /// Wall-clock duration from spawn to termination.
    pub duration: Duration,
    /// The direct child's process ID, if known.
    pub pid: Option<u32>,
}

impl ProcessOutput {
    /// Returns the exit status if the process exited normally.
    #[must_use]
    pub fn exit_status(&self) -> Option<ExitStatus> {
        self.termination.exit_status()
    }

    /// Returns `true` if the process exited with a zero status.
    #[must_use]
    pub fn is_success(&self) -> bool {
        self.exit_status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_under_cap_retains_everything() {
        let mut b = CaptureBuilder::new(16);
        b.push(b"hello ");
        b.push(b"world");
        let out = b.finish();
        assert_eq!(out.total_bytes, 11);
        assert!(!out.truncated);
        assert_eq!(out.retained_head.as_ref(), b"hello world");
        assert_eq!(out.retained_tail.as_ref(), b"hello world");
    }

    #[test]
    fn capture_over_cap_retains_head_and_tail() {
        let mut b = CaptureBuilder::new(4);
        b.push(b"AAAABBBBCCCCDDDD"); // 16 bytes, cap 4
        let out = b.finish();
        assert_eq!(out.total_bytes, 16);
        assert!(out.truncated);
        assert_eq!(out.retained_head.as_ref(), b"AAAA");
        assert_eq!(out.retained_tail.as_ref(), b"DDDD");
    }

    #[test]
    fn capture_tail_grows_then_trims() {
        let mut b = CaptureBuilder::new(3);
        for byte in b"abc" {
            b.push(&[*byte]);
        }
        b.push(b"defgh");
        let out = b.finish();
        assert_eq!(out.total_bytes, 8);
        assert!(out.truncated);
        assert_eq!(out.retained_head.as_ref(), b"abc");
        assert_eq!(out.retained_tail.as_ref(), b"fgh");
    }

    #[test]
    fn empty_capture_is_empty() {
        let out = CaptureBuilder::new(8).finish();
        assert!(out.is_empty());
        assert_eq!(out.total_bytes, 0);
        assert!(!out.truncated);
    }
}
