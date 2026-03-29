// Copyright (c) 2024 Liam S. (velor)
//
// This software is licensed under the terms of the UNLICENSE.
// You should have received a copy of the UNLICENSE with this program.
// If not, see https://unlicense.org/

//! # Velor Core Library
//!
//! This library provides the shared business logic for the Velor Agent CLI and GUI.
//!
//! ## Modules
//!
//! - [`agent`] - Agent runner interface for subprocess and ACP protocols
//! - [`config`] - Configuration file loading and management
//! - [`execution`] - Execution state machine for tracking agent runs
//! - [`git`] - Git repository discovery utilities
//! - [`notification`] - Notification system (Telegram, macOS)
//! - [`retry`] - Crash resilience and retry logic
//! - [`rules`] - Project rules system for intelligent AI agent guidance
//! - [`prompts`] - File-based prompt system
//! - [`template`] - Template rendering utilities using MiniJinja
//! - [`acp`] - ACP (Agent Client Protocol) client implementation

#![warn(missing_docs)]
#![warn(clippy::unwrap_used)]

pub mod acp;
pub mod agent;
pub mod config;
pub mod execution;
pub mod git;
pub mod notification;
pub mod prompts;
pub mod retry;
pub mod rules;
pub mod template;

// Re-export commonly used types for convenience
pub use agent::{AgentEvent, AgentRunner, ClaudeRunResult};
pub use config::{
    AcpConfig, AgentProvider, AutomationsConfig, CodexConfig, CodexReasoningEffort,
    ConversationDbConfig, Defaults, FileConfig, MacOSConfig, NotificationsConfig, PermissionMode,
    PlanConfig, PromptDef, PromptsConfig, Protocol, TelegramConfig, TelegramParseMode,
};
pub use execution::{
    ExecutionActivity, ExecutionActivityKind, ExecutionConfig, ExecutionEvent, ExecutionId,
    ExecutionMetrics, ExecutionRecord, ExecutionState,
};
pub use notification::{
    MacOSNotifier, NotificationPayload, Notifier, RunStatus, TelegramNotifier, build_notifiers,
    format_macos_message, format_telegram_message, send_notifications, should_notify,
};
pub use retry::{ConversationHistory, RetryConfig, RetryError, calculate_backoff};
pub use template::{merge_vars, render_template};
