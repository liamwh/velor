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
	MetricsUpdated = "metrics_updated"
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
 * Execution record
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
