//! Concrete provider adapters.
//!
//! Each adapter owns its wire-protocol framing (supervisor bytes → UTF-8 →
//! newline-delimited frames → protocol messages), emits structured
//! [`crate::agent::AgentEvent`]s, and classifies the final [`ProcessOutput`].
//!
//! - [`claude`] — Claude Code (and GLM/Z.ai Claude-compatible wrappers) via
//!   `stream-json` over a subprocess.
//! - [`claude_stream`] — the internal Claude `stream-json` framing/parsing
//!   module (one place that knows the protocol; the supervisor stays JSON-blind).
//! - [`codex`] — Codex `codex exec --json`.
//! - [`omp`] — Oh My Pi (`omp --mode rpc`) newline-delimited JSON RPC over stdio.
//! - [`acp`] — Agent Client Protocol over stdio (driven on a dedicated worker).

pub mod acp;
pub mod claude;
pub mod claude_stream;
pub mod codex;
mod edit_capture;
pub mod omp;
