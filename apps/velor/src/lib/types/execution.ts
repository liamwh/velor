/**
 * Execution types for Velor GUI
 * These types mirror the Rust types from velor-core execution module
 */

/**
 * Execution state enum
 */
export enum ExecutionState {
	Pending = "pending",
	Rendering = "rendering",
	Running = "running",
	Retrying = "retrying",
	Completed = "completed",
	Failed = "failed",
	Cancelled = "cancelled"
}

/**
 * Execution event types
 */
export enum ExecutionEventType {
	StateChanged = "state_changed",
	OutputChunk = "output_chunk",
	Error = "error",
	IterationCompleted = "iteration_completed",
	MetricsUpdated = "metrics_updated",
	Activity = "activity"
}

/**
 * Structured provider activity item.
 */
export interface ExecutionActivity {
	provider: string;
	kind: "status" | "tool_call" | "tool_result" | "usage" | "provider";
	summary: string;
	detail?: string;
	success?: boolean;
}

/**
 * Execution ID (UUID v4)
 */
export type ExecutionId = string;

/**
 * Execution event
 */
export interface ExecutionEvent {
	/** Event type */
	event_type: ExecutionEventType;
	/** Timestamp of the event */
	timestamp: string;
	/** Associated state (for state_changed events) */
	state?: ExecutionState;
	/** Output chunk (for output_chunk events) */
	output?: string;
	/** Error message (for error events) */
	error?: string;
	/** Iteration number (for iteration_completed events) */
	iteration?: number;
	/** Execution metrics (for metrics_updated events) */
	metrics?: ExecutionMetrics;
	/** Provider activity payload (for activity events) */
	activity?: ExecutionActivity;
	/** Generic message payload */
	message?: string;
}

/**
 * Execution metrics
 */
export interface ExecutionMetrics {
	/** Current iteration number */
	iteration: number;
	/** Total retries attempted */
	retries: number;
	/** Total output characters */
	output_chars: number;
	/** Duration in milliseconds */
	duration_ms: number;
}

/**
 * Execution configuration
 */
export interface ExecutionConfig {
	/** Name of the prompt template to use */
	prompt_name: string;
	/** Template variables */
	vars: Record<string, string | number | boolean>;
	/** Maximum iterations */
	max_iterations?: number;
	/** Maximum retries */
	max_retries?: number;
	/** Completion token */
	complete_token?: string;
}

/**
 * Execution record (session)
 */
export interface ExecutionRecord {
	/** Unique execution ID */
	id: ExecutionId;
	/** Current state */
	state: ExecutionState;
	/** Prompt name used */
	prompt_name: string;
	/** Start timestamp */
	started_at: string;
	/** End timestamp (if completed) */
	completed_at?: string;
	/** Current iteration */
	iteration: number;
	/** All events */
	events: ExecutionEvent[];
	/** Current metrics */
	metrics: ExecutionMetrics;
	/** Error message if failed */
	error?: string;
	/** User-editable session name */
	name?: string;
	/** Whether this session is pinned in the sidebar */
	pinned: boolean;
	/** Git root path at time of session creation */
	project_path?: string;
}

/**
 * Execution list response
 */
export interface ExecutionList {
	/** List of execution records */
	executions: ExecutionRecord[];
	/** Total count */
	total: number;
}

/**
 * Start execution request
 */
export interface StartExecutionRequest {
	/** Execution configuration */
	config: ExecutionConfig;
}

/**
 * Start execution response
 */
export interface StartExecutionResponse {
	/** Execution ID */
	execution_id: ExecutionId;
	/** Initial state */
	state: ExecutionState;
}

/**
 * Statistics about execution sessions
 */
export interface SessionStats {
	/** Total number of sessions */
	total: number;
	/** Number of completed sessions */
	completed: number;
	/** Number of failed sessions */
	failed: number;
	/** Number of cancelled sessions */
	cancelled: number;
	/** Number of active (non-terminal) sessions */
	active: number;
}

/**
 * Project metadata for organizing sessions
 */
export interface Project {
	/** Unique path to the project (git root) */
	path: string;
	/** User-editable display name */
	display_name: string;
	/** Whether this project is hidden from the sidebar */
	hidden: boolean;
	/** Sort order for display (lower numbers appear first) */
	sort_order: number;
	/** Number of sessions associated with this project */
	session_count: number;
}
