//! Velor Automations - Cron job capability for Velor agents.
//!
//! This crate provides scheduled, recurring execution of prompts,
//! similar to OpenAI Codex automations.

#![warn(missing_docs)]

pub mod cache;
pub mod config;
pub mod file_config;
pub mod runner;
pub mod scheduler;
pub mod store;

// Re-exports for convenience
pub use cache::AutomationCache;
pub use config::{Automation, AutomationsConfig, CatchUpPolicy, load_automations};
pub use file_config::{
    AutomationEntry, AutomationFile, AutomationFileRaw, AutomationSource, PromptSource,
    PromptSourceRaw,
};
pub use runner::{AutomationResult, AutomationRunner, WorktreeCleanup};
pub use scheduler::Scheduler;
pub use store::{AutomationRun, AutomationRunStatus, AutomationStore};
