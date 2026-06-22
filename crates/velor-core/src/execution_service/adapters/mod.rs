//! Concrete provider adapters.
//!
//! Each adapter owns its wire-protocol framing (supervisor bytes → UTF-8 →
//! newline-delimited frames → protocol messages), emits structured
//! [`crate::agent::AgentEvent`]s, and classifies the final [`ProcessOutput`].
//!
//! - [`claude`] — Claude Code (and GLM/Z.ai Claude-compatible wrappers) via
//!   `stream-json` over a subprocess.
//! - [`codex`] — Codex `codex exec --json`.
//! - [`acp`] — Agent Client Protocol over stdio (driven on a dedicated worker).

pub mod claude;
pub mod codex;
