//! Execution state machine for tracking agent runs.
//!
//! This module provides the state machine and event types needed for the GUI
//! to track and display the progress of agent executions.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::time::Duration;

/// Unique identifier for an execution.
///
/// Uses a newtype wrapper for type safety and to prevent mixing up IDs.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ExecutionId(String);

impl ExecutionId {
    /// Creates a new unique execution ID.
    #[must_use]
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }

    /// Creates an execution ID from a string (for testing/rehydration).
    #[must_use]
    pub const fn from_string(s: String) -> Self {
        Self(s)
    }

    /// Returns the inner string value.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for ExecutionId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for ExecutionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// State of an execution in the state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecutionState {
    /// Execution is pending start.
    Pending,
    /// Template is being rendered.
    Rendering,
    /// Agent is running.
    Running,
    /// Execution is being retried after a failure.
    Retrying,
    /// Execution completed successfully.
    Completed,
    /// Execution failed after all retries.
    Failed,
    /// Execution was cancelled by the user.
    Cancelled,
}

impl ExecutionState {
    /// Returns true if the execution is in a terminal state.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }

    /// Returns true if the execution is active (not terminal).
    #[must_use]
    pub const fn is_active(self) -> bool {
        !self.is_terminal()
    }

    /// Returns a human-readable label for this state.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Pending => "Pending",
            Self::Rendering => "Rendering",
            Self::Running => "Running",
            Self::Retrying => "Retrying",
            Self::Completed => "Completed",
            Self::Failed => "Failed",
            Self::Cancelled => "Cancelled",
        }
    }
}

/// Event emitted during execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ExecutionEvent {
    /// The execution state changed.
    StateChanged {
        /// New state.
        state: ExecutionState,
        /// Timestamp of the change.
        timestamp: Timestamp,
    },
    /// A chunk of output was received from the agent.
    OutputChunk {
        /// Text content.
        text: String,
        /// Timestamp of the chunk.
        timestamp: Timestamp,
    },
    /// An error occurred.
    Error {
        /// Error message.
        message: String,
        /// Whether this is a retryable error.
        retryable: bool,
        /// Timestamp of the error.
        timestamp: Timestamp,
    },
    /// An iteration completed.
    IterationCompleted {
        /// Iteration number.
        iteration: u32,
        /// Completion status.
        completed: bool,
        /// Timestamp of completion.
        timestamp: Timestamp,
    },
    /// Execution metrics were updated.
    MetricsUpdated {
        /// Updated metrics.
        metrics: ExecutionMetrics,
        /// Timestamp of the update.
        timestamp: Timestamp,
    },
    /// Provider activity update (status/tool/progress/usage).
    Activity {
        /// Structured activity payload.
        activity: ExecutionActivity,
        /// Timestamp of the activity.
        timestamp: Timestamp,
    },
}

/// Provider activity payload for rich execution streaming.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionActivity {
    /// Provider identifier (e.g. `claude`, `codex`).
    pub provider: String,
    /// Activity kind.
    pub kind: ExecutionActivityKind,
    /// Short human-readable summary.
    pub summary: String,
    /// Optional structured detail string.
    pub detail: Option<String>,
    /// Optional success value for result-like activities.
    pub success: Option<bool>,
}

/// Activity kind for provider updates.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionActivityKind {
    /// Lifecycle/status update.
    Status,
    /// Tool/action started.
    ToolCall,
    /// Tool/action finished.
    ToolResult,
    /// Usage or accounting update.
    Usage,
    /// Provider-specific generic update.
    Provider,
}

/// Metrics for an execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionMetrics {
    /// Current iteration number (1-indexed).
    pub iteration: u32,
    /// Maximum iterations allowed.
    pub max_iterations: u32,
    /// Number of retries attempted for current iteration.
    pub retries: u32,
    /// Maximum retries allowed per iteration.
    pub max_retries: u32,
    /// Total duration since start.
    pub total_duration: Duration,
    /// Duration of the current iteration.
    pub current_iteration_duration: Duration,
    /// Total tokens used (if available).
    pub total_tokens: Option<u64>,
    /// Total cost in USD (if available).
    pub total_cost: Option<f64>,
}

impl Default for ExecutionMetrics {
    fn default() -> Self {
        Self {
            iteration: 1,
            max_iterations: 1000,
            retries: 0,
            max_retries: 5,
            total_duration: Duration::ZERO,
            current_iteration_duration: Duration::ZERO,
            total_tokens: None,
            total_cost: None,
        }
    }
}

/// Configuration for an execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionConfig {
    /// Name of the prompt template to use.
    pub prompt_name: String,
    /// Template variables for rendering.
    pub template_vars: BTreeMap<String, String>,
    /// Maximum iterations to run.
    pub max_iterations: u32,
    /// Completion token to detect.
    pub complete_token: String,
    /// Binary to invoke (e.g., "claude-glm").
    pub binary: String,
    /// Permission mode for the agent.
    pub permission_mode: String,
    /// Current working directory for execution.
    pub cwd: String,
    /// Whether rules are enabled.
    pub rules_enabled: bool,
    /// Rules directory path.
    pub rules_dir: String,
}

impl ExecutionConfig {
    /// Creates a new execution configuration.
    #[must_use]
    pub fn new(prompt_name: String) -> Self {
        Self {
            prompt_name,
            template_vars: BTreeMap::new(),
            max_iterations: 1000,
            complete_token: "<promise>COMPLETE</promise>".to_string(),
            binary: "claude-glm".to_string(),
            permission_mode: "acceptEdits".to_string(),
            cwd: std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| ".".to_string()),
            rules_enabled: false,
            rules_dir: ".agents/rules".to_string(),
        }
    }

    /// Sets the template variables.
    #[must_use]
    pub fn with_template_vars(mut self, vars: BTreeMap<String, String>) -> Self {
        self.template_vars = vars;
        self
    }

    /// Sets the maximum iterations.
    #[must_use]
    pub fn with_max_iterations(mut self, max: u32) -> Self {
        self.max_iterations = max;
        self
    }

    /// Sets the completion token.
    #[must_use]
    pub fn with_complete_token(mut self, token: String) -> Self {
        self.complete_token = token;
        self
    }

    /// Sets the binary.
    #[must_use]
    pub fn with_binary(mut self, binary: String) -> Self {
        self.binary = binary;
        self
    }

    /// Sets the permission mode.
    #[must_use]
    pub fn with_permission_mode(mut self, mode: String) -> Self {
        self.permission_mode = mode;
        self
    }

    /// Sets the working directory.
    #[must_use]
    pub fn with_cwd(mut self, cwd: String) -> Self {
        self.cwd = cwd;
        self
    }

    /// Enables rules with the given directory.
    #[must_use]
    pub fn with_rules(mut self, enabled: bool, dir: String) -> Self {
        self.rules_enabled = enabled;
        self.rules_dir = dir;
        self
    }
}

/// Complete execution record for history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionRecord {
    /// Unique execution ID.
    pub id: ExecutionId,
    /// Configuration used for this execution.
    pub config: ExecutionConfig,
    /// Current state.
    pub state: ExecutionState,
    /// Current metrics.
    pub metrics: ExecutionMetrics,
    /// Accumulated output.
    pub output: String,
    /// Error message if failed.
    pub error: Option<String>,
    /// When the execution started.
    pub started_at: Timestamp,
    /// When the execution ended (if terminal).
    pub ended_at: Option<Timestamp>,
    /// Events log.
    pub events: Vec<ExecutionEvent>,
    /// User-editable session name (optional, defaults to prompt_name).
    pub name: Option<String>,
    /// Whether this session is pinned in the sidebar.
    pub pinned: bool,
    /// Git root path at time of session creation.
    pub project_path: Option<String>,
}

impl ExecutionRecord {
    /// Creates a new execution record.
    #[must_use]
    pub fn new(config: ExecutionConfig) -> Self {
        let started_at = Timestamp::now();
        let id = ExecutionId::new();
        let initial_event = ExecutionEvent::StateChanged {
            state: ExecutionState::Pending,
            timestamp: started_at,
        };
        Self {
            id,
            config,
            state: ExecutionState::Pending,
            metrics: ExecutionMetrics::default(),
            output: String::new(),
            error: None,
            started_at,
            ended_at: None,
            events: vec![initial_event],
            name: None,
            pinned: false,
            project_path: None,
        }
    }

    /// Records a state change.
    pub fn set_state(&mut self, state: ExecutionState) {
        let timestamp = Timestamp::now();
        self.events
            .push(ExecutionEvent::StateChanged { state, timestamp });
        self.state = state;
        if state.is_terminal() {
            self.ended_at = Some(timestamp);
        }
    }

    /// Appends output text.
    pub fn append_output(&mut self, text: &str) {
        let timestamp = Timestamp::now();
        self.events.push(ExecutionEvent::OutputChunk {
            text: text.to_string(),
            timestamp,
        });
        self.output.push_str(text);
    }

    /// Records an error.
    pub fn record_error(&mut self, message: String, retryable: bool) {
        let timestamp = Timestamp::now();
        self.events.push(ExecutionEvent::Error {
            message: message.clone(),
            retryable,
            timestamp,
        });
        if !retryable {
            self.error = Some(message);
        }
    }

    /// Records an iteration completion.
    pub fn complete_iteration(&mut self, iteration: u32, completed: bool) {
        let timestamp = Timestamp::now();
        self.events.push(ExecutionEvent::IterationCompleted {
            iteration,
            completed,
            timestamp,
        });
    }

    /// Updates metrics.
    pub fn update_metrics(&mut self, metrics: ExecutionMetrics) {
        let timestamp = Timestamp::now();
        self.events.push(ExecutionEvent::MetricsUpdated {
            metrics: metrics.clone(),
            timestamp,
        });
        self.metrics = metrics;
    }

    /// Records a provider activity event.
    pub fn record_activity(&mut self, activity: ExecutionActivity) {
        let timestamp = Timestamp::now();
        self.events.push(ExecutionEvent::Activity {
            activity,
            timestamp,
        });
    }

    /// Returns the duration of the execution so far.
    #[must_use]
    pub fn duration(&self) -> Duration {
        let end = self.ended_at.unwrap_or_else(jiff::Timestamp::now);
        Duration::try_from(end.duration_since(self.started_at)).unwrap_or(Duration::ZERO)
    }

    /// Returns true if the execution is complete (detected completion token).
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        matches!(self.state, ExecutionState::Completed)
    }

    /// Returns true if the execution failed.
    #[must_use]
    pub const fn is_failed(&self) -> bool {
        matches!(self.state, ExecutionState::Failed)
    }

    /// Returns true if the execution was cancelled.
    #[must_use]
    pub const fn is_cancelled(&self) -> bool {
        matches!(self.state, ExecutionState::Cancelled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_execution_id_new_is_unique() {
        let id1 = ExecutionId::new();
        let id2 = ExecutionId::new();
        assert_ne!(id1, id2, "IDs should be unique");
    }

    #[test]
    fn test_execution_id_from_string() {
        let s = "test-id-123".to_string();
        let id = ExecutionId::from_string(s.clone());
        assert_eq!(id.as_str(), &s);
    }

    #[test]
    fn test_execution_id_display() {
        let id = ExecutionId::from_string("test-id".to_string());
        assert_eq!(format!("{id}"), "test-id");
        assert_eq!(id.as_str(), "test-id");
    }

    #[test]
    fn test_execution_state_is_terminal() {
        assert!(!ExecutionState::Pending.is_terminal());
        assert!(!ExecutionState::Rendering.is_terminal());
        assert!(!ExecutionState::Running.is_terminal());
        assert!(!ExecutionState::Retrying.is_terminal());
        assert!(ExecutionState::Completed.is_terminal());
        assert!(ExecutionState::Failed.is_terminal());
        assert!(ExecutionState::Cancelled.is_terminal());
    }

    #[test]
    fn test_execution_state_is_active() {
        assert!(ExecutionState::Pending.is_active());
        assert!(ExecutionState::Rendering.is_active());
        assert!(ExecutionState::Running.is_active());
        assert!(ExecutionState::Retrying.is_active());
        assert!(!ExecutionState::Completed.is_active());
        assert!(!ExecutionState::Failed.is_active());
        assert!(!ExecutionState::Cancelled.is_active());
    }

    #[test]
    fn test_execution_state_label() {
        assert_eq!(ExecutionState::Pending.label(), "Pending");
        assert_eq!(ExecutionState::Rendering.label(), "Rendering");
        assert_eq!(ExecutionState::Running.label(), "Running");
        assert_eq!(ExecutionState::Retrying.label(), "Retrying");
        assert_eq!(ExecutionState::Completed.label(), "Completed");
        assert_eq!(ExecutionState::Failed.label(), "Failed");
        assert_eq!(ExecutionState::Cancelled.label(), "Cancelled");
    }

    #[test]
    fn test_execution_config_new() {
        let config = ExecutionConfig::new("test-prompt".to_string());
        assert_eq!(config.prompt_name, "test-prompt");
        assert_eq!(config.max_iterations, 1000);
        assert_eq!(config.complete_token, "<promise>COMPLETE</promise>");
        assert_eq!(config.binary, "claude-glm");
        assert_eq!(config.permission_mode, "acceptEdits");
        assert!(!config.rules_enabled);
    }

    #[test]
    fn test_execution_config_builder() {
        let mut vars = BTreeMap::new();
        vars.insert("key".to_string(), "value".to_string());

        let config = ExecutionConfig::new("test".to_string())
            .with_template_vars(vars.clone())
            .with_max_iterations(100)
            .with_complete_token("DONE".to_string())
            .with_binary("custom-binary".to_string())
            .with_permission_mode("deny".to_string())
            .with_cwd("/tmp".to_string())
            .with_rules(true, ".custom/rules".to_string());

        assert_eq!(config.prompt_name, "test");
        assert_eq!(config.template_vars, vars);
        assert_eq!(config.max_iterations, 100);
        assert_eq!(config.complete_token, "DONE");
        assert_eq!(config.binary, "custom-binary");
        assert_eq!(config.permission_mode, "deny");
        assert_eq!(config.cwd, "/tmp");
        assert!(config.rules_enabled);
        assert_eq!(config.rules_dir, ".custom/rules");
    }

    #[test]
    fn test_execution_record_new() {
        let config = ExecutionConfig::new("test".to_string());
        let record = ExecutionRecord::new(config);

        assert_eq!(record.state, ExecutionState::Pending);
        assert_eq!(record.metrics.iteration, 1);
        assert!(record.output.is_empty());
        assert!(record.error.is_none());
        assert!(record.ended_at.is_none());
        assert_eq!(record.events.len(), 1); // Initial Pending state event
        assert!(matches!(
            record.events[0],
            ExecutionEvent::StateChanged {
                state: ExecutionState::Pending,
                ..
            }
        ));
    }

    #[test]
    fn test_execution_record_set_state() {
        let config = ExecutionConfig::new("test".to_string());
        let mut record = ExecutionRecord::new(config);

        assert!(!record.events.is_empty());
        assert_eq!(record.events.len(), 1); // Initial state change

        record.set_state(ExecutionState::Running);
        assert_eq!(record.state, ExecutionState::Running);
        assert!(record.ended_at.is_none());
        assert_eq!(record.events.len(), 2);

        record.set_state(ExecutionState::Completed);
        assert_eq!(record.state, ExecutionState::Completed);
        assert!(record.ended_at.is_some());
        assert_eq!(record.events.len(), 3);
    }

    #[test]
    fn test_execution_record_append_output() {
        let config = ExecutionConfig::new("test".to_string());
        let mut record = ExecutionRecord::new(config);

        record.append_output("Hello, ");
        record.append_output("world!");

        assert_eq!(record.output, "Hello, world!");
        assert_eq!(record.events.len(), 3); // Initial + 2 output chunks
    }

    #[test]
    fn test_execution_record_error() {
        let config = ExecutionConfig::new("test".to_string());
        let mut record = ExecutionRecord::new(config);

        record.record_error("Temporary error".to_string(), true);
        assert!(record.error.is_none()); // Retryable errors don't set error field
        assert_eq!(record.events.len(), 2); // Initial + first error

        record.record_error("Permanent error".to_string(), false);
        assert_eq!(record.error, Some("Permanent error".to_string()));
        assert_eq!(record.events.len(), 3); // Initial + 2 errors
    }

    #[test]
    fn test_execution_record_complete_iteration() {
        let config = ExecutionConfig::new("test".to_string());
        let mut record = ExecutionRecord::new(config);

        record.complete_iteration(1, false);
        record.complete_iteration(2, true);

        assert_eq!(record.events.len(), 3); // Initial + 2 iteration completions
    }

    #[test]
    fn test_execution_record_update_metrics() {
        let config = ExecutionConfig::new("test".to_string());
        let mut record = ExecutionRecord::new(config);

        let metrics = ExecutionMetrics {
            iteration: 5,
            max_iterations: 50,
            retries: 1,
            max_retries: 5,
            total_duration: Duration::from_secs(60),
            current_iteration_duration: Duration::from_secs(10),
            total_tokens: Some(1000),
            total_cost: Some(0.05),
        };

        record.update_metrics(metrics.clone());
        assert_eq!(record.metrics.iteration, 5);
        assert_eq!(record.metrics.total_tokens, Some(1000));
        assert_eq!(record.events.len(), 2); // Initial state + metrics update
    }

    #[test]
    fn test_execution_record_duration() {
        let config = ExecutionConfig::new("test".to_string());
        let mut record = ExecutionRecord::new(config);

        // Duration should be near zero for a new record
        let duration = record.duration();
        assert!(duration.as_secs() < 1);

        // After setting terminal state, duration should reflect elapsed time
        record.set_state(ExecutionState::Completed);
        let duration = record.duration();
        assert!(duration.as_secs() < 1);
    }

    #[test]
    fn test_execution_metrics_default_is_valid() {
        let metrics = ExecutionMetrics::default();
        assert!(metrics.iteration >= 1);
        assert!(metrics.max_iterations >= 1);
        assert!(metrics.iteration <= metrics.max_iterations);
        assert!(metrics.retries <= metrics.max_retries);
    }

    #[test]
    fn test_execution_record_status_checks() {
        let config = ExecutionConfig::new("test".to_string());
        let mut record = ExecutionRecord::new(config);

        assert!(!record.is_complete());
        assert!(!record.is_failed());
        assert!(!record.is_cancelled());

        record.set_state(ExecutionState::Completed);
        assert!(record.is_complete());
        assert!(!record.is_failed());

        let config2 = ExecutionConfig::new("test".to_string());
        let mut record2 = ExecutionRecord::new(config2);
        record2.set_state(ExecutionState::Failed);
        assert!(record2.is_failed());
        assert!(!record2.is_complete());

        let config3 = ExecutionConfig::new("test".to_string());
        let mut record3 = ExecutionRecord::new(config3);
        record3.set_state(ExecutionState::Cancelled);
        assert!(record3.is_cancelled());
        assert!(!record3.is_complete());
        assert!(!record3.is_failed());
    }
}

#[cfg(test)]
mod proptest_tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        fn test_execution_id_display_roundtrip(s in "[a-zA-Z0-9-]+") {
            let id = ExecutionId::from_string(s.clone());
            prop_assert_eq!(id.as_str(), &s);
            prop_assert_eq!(format!("{id}"), s);
        }

        fn test_append_output_preserves_content(text in ".*") {
            let config = ExecutionConfig::new("test".to_string());
            let mut record = ExecutionRecord::new(config);

            record.append_output(&text);
            prop_assert_eq!(record.output, text);
        }

        fn test_multiple_appends_accumulate(
            part1 in ".*",
            part2 in ".*",
            part3 in ".*"
        ) {
            let config = ExecutionConfig::new("test".to_string());
            let mut record = ExecutionRecord::new(config);

            record.append_output(&part1);
            record.append_output(&part2);
            record.append_output(&part3);

            let expected = format!("{}{}{}", part1, part2, part3);
            prop_assert_eq!(record.output, expected);
        }

        fn test_execution_config_max_iterations_positive(max in 1u32..1000u32) {
            let config = ExecutionConfig::new("test".to_string())
                .with_max_iterations(max);

            prop_assert_eq!(config.max_iterations, max);
        }

        fn test_template_vars_preserved(
            vars in prop::collection::btree_map("[a-z]{1,10}", ".*", 0..20)
        ) {
            let config = ExecutionConfig::new("test".to_string())
                .with_template_vars(vars.clone());

            prop_assert_eq!(config.template_vars, vars);
        }
    }
}
