//! Unified agent execution substrate.
//!
//! This module is the single place that owns spawning, draining, deadline, and
//! cancellation semantics for external agent processes (Claude Code/GLM, Codex).
//! It is intentionally provider-agnostic: provider-specific interpretation
//! (overload, auth, context-too-large, …) lives in the adapter/classifier layer
//! added in later phases, never in the generic supervisor.
//!
//! - [`supervisor`] — the generic process supervisor (one owner per child, byte
//!   chunks, deadlock-free drain, whole-group lifecycle).
//! - [`output`] — bounded head/tail captured output.
//! - [`error`] — typed process-execution errors.

pub mod adapter;
pub mod adapters;
pub mod classify;
pub mod error;
pub mod output;
pub mod supervisor;
