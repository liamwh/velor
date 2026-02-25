/**
 * Automation types for Velor GUI
 * These types mirror the Rust types from velor-automations
 */

/**
 * Catch-up policy for missed runs
 */
export type CatchUpPolicy = "Skip" | "RunOnce" | "RunAll";

/**
 * Automation configuration
 * Matches the Rust Automation struct from velor-automations
 */
export interface Automation {
	/** Unique name of the automation (used as identifier) */
	name: string;
	/** Human-readable description */
	description: string;
	/** Cron schedule expression (6-field: seconds minutes hours day month weekday) */
	schedule: string;
	/** Timezone for the schedule (IANA tz database name) */
	timezone: string;
	/** Prompt template name or inline content */
	prompt: string;
	/** Whether this automation is enabled */
	enabled: boolean;
	/** Variables to pass to the prompt */
	vars: Record<string, string>;
	/** Policy for handling missed runs */
	catch_up: CatchUpPolicy;
	/** Maximum number of catch-up runs to execute */
	max_catch_up: number;
	/** Timeout for this automation (seconds, optional) */
	timeout_seconds?: number;
	/** Send notification on success */
	notify_on_success: boolean;
	/** Send notification on failure */
	notify_on_failure: boolean;
}

/**
 * Automation run status
 */
export type AutomationRunStatus = "Pending" | "Running" | "Completed" | "Failed" | "Cancelled";

/**
 * Automation run record
 * Matches the Rust AutomationRun struct from velor-automations
 */
export interface AutomationRun {
	/** Unique identifier for this run */
	id: number;
	/** Name of the automation that was run */
	automation_name: string;
	/** When this run was scheduled to occur */
	scheduled_for: string;
	/** When this run actually started */
	started_at: string;
	/** When this run completed (if terminal) */
	completed_at?: string;
	/** The current status of this run */
	status: AutomationRunStatus;
	/** Number of iterations completed before termination */
	iterations_completed: number;
	/** Exit code from the automation process (if available) */
	exit_code?: number;
	/** Duration of the run in milliseconds (if terminal) */
	duration_ms?: number;
	/** Standard output from the automation run (truncated if needed) */
	output?: string;
	/** Standard error from the automation run (if any) */
	error?: string;
}

/**
 * Automation list response
 * The backend returns a Vec<Automation> directly
 */
export type AutomationList = Automation[];

/**
 * Automation runs list response
 */
export type AutomationRunsList = AutomationRun[];

/**
 * Create automation request
 */
export interface CreateAutomationRequest {
	/** Unique name of the automation */
	name: string;
	/** Human-readable description */
	description?: string;
	/** Cron schedule expression (6-field) */
	schedule: string;
	/** Timezone for the schedule */
	timezone?: string;
	/** Prompt template name */
	prompt: string;
	/** Whether this automation is enabled */
	enabled?: boolean;
	/** Variables to pass to the prompt */
	vars?: Record<string, string>;
	/** Policy for handling missed runs */
	catch_up?: CatchUpPolicy;
	/** Maximum number of catch-up runs */
	max_catch_up?: number;
	/** Timeout in seconds */
	timeout_seconds?: number;
	/** Send notification on success */
	notify_on_success?: boolean;
	/** Send notification on failure */
	notify_on_failure?: boolean;
}

/**
 * Update automation request
 */
export interface UpdateAutomationRequest {
	/** Current automation name (used as identifier) */
	current_name: string;
	/** New unique name of the automation */
	name?: string;
	/** Human-readable description */
	description?: string;
	/** Cron schedule expression (6-field) */
	schedule?: string;
	/** Timezone for the schedule */
	timezone?: string;
	/** Prompt template name */
	prompt?: string;
	/** Whether this automation is enabled */
	enabled?: boolean;
	/** Variables to pass to the prompt */
	vars?: Record<string, string>;
	/** Policy for handling missed runs */
	catch_up?: CatchUpPolicy;
	/** Maximum number of catch-up runs */
	max_catch_up?: number;
	/** Timeout in seconds */
	timeout_seconds?: number;
	/** Send notification on success */
	notify_on_success?: boolean;
	/** Send notification on failure */
	notify_on_failure?: boolean;
}

/**
 * Toggle automation request
 */
export interface ToggleAutomationRequest {
	/** Automation name (used as identifier) */
	name: string;
	/** Whether to enable or disable */
	enabled: boolean;
}
