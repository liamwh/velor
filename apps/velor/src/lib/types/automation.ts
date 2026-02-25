/**
 * Automation types for Velor GUI
 * These types mirror the Rust types from velor-automations
 */

/**
 * Automation ID (UUID v4)
 */
export type AutomationId = string;

/**
 * Automation schedule
 */
export interface AutomationSchedule {
	/** Cron expression */
	cron: string;
	/** Timezone (IANA) */
	timezone: string;
}

/**
 * Automation status
 */
export enum AutomationStatus {
	Enabled = "enabled",
	Disabled = "disabled",
	Running = "running",
	Failed = "failed"
}

/**
 * Automation configuration
 */
export interface Automation {
	/** Unique automation ID */
	id: AutomationId;
	/** Human-readable name */
	name: string;
	/** Description */
	description?: string;
	/** Schedule configuration */
	schedule: AutomationSchedule;
	/** Prompt template name */
	prompt_name: string;
	/** Template variables */
	vars: Record<string, string | number | boolean>;
	/** Whether the automation is enabled */
	enabled: boolean;
	/** Catch-up policy */
	catch_up_policy: "skip" | "run_once" | "run_all";
	/** Created timestamp */
	created_at: string;
	/** Updated timestamp */
	updated_at: string;
}

/**
 * Automation run status
 */
export enum AutomationRunStatus {
	Pending = "pending",
	Running = "running",
	Completed = "completed",
	Failed = "failed",
	Skipped = "skipped"
}

/**
 * Automation run record
 */
export interface AutomationRun {
	/** Unique run ID */
	id: string;
	/** Automation ID */
	automation_id: AutomationId;
	/** Scheduled timestamp */
	scheduled_at: string;
	/** Started timestamp */
	started_at?: string;
	/** Completed timestamp */
	completed_at?: string;
	/** Run status */
	status: AutomationRunStatus;
	/** Output preview */
	output_preview?: string;
	/** Error message if failed */
	error?: string;
}

/**
 * Automation list response
 */
export interface AutomationList {
	/** List of automations */
	automations: Automation[];
	/** Total count */
	total: number;
}

/**
 * Automation runs list response
 */
export interface AutomationRunsList {
	/** List of runs */
	runs: AutomationRun[];
	/** Total count */
	total: number;
}

/**
 * Create automation request
 */
export interface CreateAutomationRequest {
	/** Human-readable name */
	name: string;
	/** Description */
	description?: string;
	/** Schedule configuration */
	schedule: AutomationSchedule;
	/** Prompt template name */
	prompt_name: string;
	/** Template variables */
	vars: Record<string, string | number | boolean>;
	/** Catch-up policy */
	catch_up_policy?: "skip" | "run_once" | "run_all";
}

/**
 * Update automation request
 */
export interface UpdateAutomationRequest {
	/** Automation ID */
	id: AutomationId;
	/** Human-readable name */
	name?: string;
	/** Description */
	description?: string;
	/** Schedule configuration */
	schedule?: AutomationSchedule;
	/** Prompt template name */
	prompt_name?: string;
	/** Template variables */
	vars?: Record<string, string | number | boolean>;
	/** Catch-up policy */
	catch_up_policy?: "skip" | "run_once" | "run_all";
}

/**
 * Toggle automation request
 */
export interface ToggleAutomationRequest {
	/** Automation ID */
	id: AutomationId;
	/** Whether to enable or disable */
	enabled: boolean;
}
