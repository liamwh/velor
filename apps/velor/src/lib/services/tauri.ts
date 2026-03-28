/**
 * Tauri command wrappers for Velor GUI backend
 * These provide type-safe access to Tauri commands
 */

import { invoke } from "@tauri-apps/api/core";
import type {
	ConfigResponse,
	SaveConfigRequest,
	ExecutionEvent,
	ExecutionMetrics,
	ExecutionRecord,
	StartExecutionRequest,
	StartExecutionResponse,
	SessionStats,
	Automation,
	AutomationList,
	AutomationRunsList,
	ToggleAutomationRequest,
	CreateAutomationRequest,
	UpdateAutomationRequest,
	Project,
	SpecFileInfo,
	GeneratePlanRequest,
} from "$lib/types";
import { ExecutionEventType, ExecutionState } from "$lib/types";

interface RawDuration {
	secs?: number;
	nanos?: number;
}

interface RawExecutionMetrics {
	iteration: number;
	max_iterations?: number;
	retries: number;
	max_retries?: number;
	total_duration?: RawDuration;
	current_iteration_duration?: RawDuration;
	total_tokens?: number | null;
	total_cost?: number | null;
}

interface RawExecutionConfig {
	prompt_name?: string;
}

interface RawExecutionRecordLike {
	id: string;
	state: string;
	prompt_name?: string;
	config?: RawExecutionConfig;
	iteration?: number;
	is_active?: boolean;
	is_cancelled?: boolean;
	events: unknown[];
	metrics: RawExecutionMetrics;
	started_at?: string;
	completed_at?: string | null;
	ended_at?: string | null;
	error?: string | null;
	name?: string | null;
	pinned?: boolean;
	project_path?: string | null;
}

function toMs(duration?: RawDuration): number {
	if (!duration) return 0;
	const secs = duration.secs ?? 0;
	const nanos = duration.nanos ?? 0;
	return secs * 1000 + Math.floor(nanos / 1_000_000);
}

function normalizeState(state: string): ExecutionState {
	return state.toLowerCase() as ExecutionState;
}

function computeOutputChars(events: ExecutionEvent[]): number {
	return events
		.filter((e) => e.event_type === ExecutionEventType.OutputChunk)
		.reduce((sum, e) => sum + (e.output?.length ?? 0), 0);
}

function inferStartedAt(raw: RawExecutionRecordLike, events: ExecutionEvent[]): string {
	if (raw.started_at) return raw.started_at;
	const firstTimestamp = events[0]?.timestamp;
	return firstTimestamp ?? new Date().toISOString();
}

function inferCompletedAt(raw: RawExecutionRecordLike, events: ExecutionEvent[]): string | undefined {
	if (raw.completed_at) return raw.completed_at;
	if (raw.ended_at) return raw.ended_at;

	const normalized = normalizeState(raw.state);
	if (
		normalized === ExecutionState.Completed ||
		normalized === ExecutionState.Failed ||
		normalized === ExecutionState.Cancelled
	) {
		return events[events.length - 1]?.timestamp;
	}
	return undefined;
}

function normalizeEvent(raw: unknown): ExecutionEvent {
	if (typeof raw !== "object" || raw === null) {
		return {
			event_type: ExecutionEventType.StateChanged,
			timestamp: new Date().toISOString(),
			message: String(raw),
		};
	}

	const event = raw as Record<string, unknown>;
	if ("StateChanged" in event) {
		const payload = event.StateChanged as Record<string, unknown>;
		return {
			event_type: ExecutionEventType.StateChanged,
			timestamp: String(payload.timestamp ?? new Date().toISOString()),
			state: normalizeState(String(payload.state ?? "pending")),
		};
	}
	if ("OutputChunk" in event) {
		const payload = event.OutputChunk as Record<string, unknown>;
		return {
			event_type: ExecutionEventType.OutputChunk,
			timestamp: String(payload.timestamp ?? new Date().toISOString()),
			output: String(payload.text ?? ""),
		};
	}
	if ("Error" in event) {
		const payload = event.Error as Record<string, unknown>;
		return {
			event_type: ExecutionEventType.Error,
			timestamp: String(payload.timestamp ?? new Date().toISOString()),
			error: String(payload.message ?? "Unknown error"),
		};
	}
	if ("IterationCompleted" in event) {
		const payload = event.IterationCompleted as Record<string, unknown>;
		return {
			event_type: ExecutionEventType.IterationCompleted,
			timestamp: String(payload.timestamp ?? new Date().toISOString()),
			iteration: Number(payload.iteration ?? 0),
		};
	}
	if ("MetricsUpdated" in event) {
		const payload = event.MetricsUpdated as Record<string, unknown>;
		const metrics = (payload.metrics ?? {}) as RawExecutionMetrics;
		return {
			event_type: ExecutionEventType.MetricsUpdated,
			timestamp: String(payload.timestamp ?? new Date().toISOString()),
			metrics: {
				iteration: metrics.iteration ?? 1,
				retries: metrics.retries ?? 0,
				output_chars: 0,
				duration_ms: toMs(metrics.total_duration),
			},
		};
	}
	if ("Activity" in event) {
		const payload = event.Activity as Record<string, unknown>;
		const activity = (payload.activity ?? {}) as Record<string, unknown>;
		return {
			event_type: ExecutionEventType.Activity,
			timestamp: String(payload.timestamp ?? new Date().toISOString()),
			activity: {
				provider: String(activity.provider ?? "unknown"),
				kind: String(activity.kind ?? "provider") as
					| "status"
					| "tool_call"
					| "tool_result"
					| "usage"
					| "provider",
				summary: String(activity.summary ?? ""),
				detail: activity.detail ? String(activity.detail) : undefined,
				success:
					typeof activity.success === "boolean" ? Boolean(activity.success) : undefined,
			},
			message: String(activity.summary ?? ""),
		};
	}

	return {
		event_type: ExecutionEventType.StateChanged,
		timestamp: new Date().toISOString(),
		message: JSON.stringify(raw),
	};
}

function normalizeExecution(raw: RawExecutionRecordLike): ExecutionRecord {
	const events = raw.events.map(normalizeEvent);
	const outputChars = computeOutputChars(events);
	const promptName = raw.prompt_name ?? raw.config?.prompt_name ?? "unknown";
	const startedAt = inferStartedAt(raw, events);
	const completedAt = inferCompletedAt(raw, events);

	const metrics: ExecutionMetrics = {
		iteration: raw.metrics.iteration ?? raw.iteration ?? 1,
		retries: raw.metrics.retries ?? 0,
		output_chars: outputChars,
		duration_ms: toMs(raw.metrics.total_duration),
	};

	return {
		id: raw.id,
		state: normalizeState(raw.state),
		prompt_name: promptName,
		started_at: startedAt,
		completed_at: completedAt,
		iteration: raw.iteration ?? metrics.iteration,
		events,
		metrics,
		error: raw.error ?? undefined,
		name: raw.name ?? undefined,
		pinned: Boolean(raw.pinned),
		project_path: raw.project_path ?? undefined,
	};
}

/**
 * Config Commands
 */

export async function getConfig(): Promise<ConfigResponse> {
	return await invoke<ConfigResponse>("get_config");
}

export async function getHomeConfig(): Promise<string> {
	return await invoke<string>("get_home_config");
}

export async function getRepoConfig(): Promise<string> {
	return await invoke<string>("get_repo_config");
}

export async function saveConfig(request: SaveConfigRequest): Promise<void> {
	await invoke("save_config", { request });
}

/**
 * Execution Commands
 */

export async function startExecution(
	request: StartExecutionRequest,
): Promise<StartExecutionResponse> {
	return await invoke<StartExecutionResponse>("start_execution", { request });
}

export async function cancelExecution(executionId: string): Promise<void> {
	await invoke("cancel_execution", { id: executionId });
}

export async function getExecutionStatus(executionId: string): Promise<ExecutionRecord> {
	const raw = await invoke<RawExecutionRecordLike | null>("get_execution_status", {
		id: executionId,
	});
	if (!raw) {
		throw new Error(`Execution ${executionId} not found`);
	}
	return normalizeExecution(raw);
}

export async function getExecutionHistory(limit?: number): Promise<{ executions: ExecutionRecord[] }> {
	const raw = await invoke<RawExecutionRecordLike[]>("get_execution_history", { limit });
	return { executions: raw.map(normalizeExecution) };
}

/**
 * Session Commands
 */

export async function listSessions(limit?: number, offset?: number): Promise<ExecutionRecord[]> {
	const raw = await invoke<RawExecutionRecordLike[]>("list_sessions", { limit, offset });
	return raw.map(normalizeExecution);
}

export async function getSession(id: string): Promise<ExecutionRecord | null> {
	const raw = await invoke<RawExecutionRecordLike | null>("get_session", { id });
	return raw ? normalizeExecution(raw) : null;
}

export async function deleteSession(id: string): Promise<void> {
	await invoke("delete_session", { id });
}

export async function getSessionStats(): Promise<SessionStats> {
	return await invoke<SessionStats>("get_session_stats");
}

export async function renameSession(id: string, name: string | null): Promise<void> {
	await invoke("rename_session", { id, name });
}

export async function toggleSessionPin(id: string): Promise<boolean> {
	return await invoke<boolean>("toggle_session_pin", { id });
}

/**
 * Automation Commands
 */

export async function listAutomations(): Promise<AutomationList> {
	return await invoke<AutomationList>("list_automations");
}

export async function getAutomation(name: string): Promise<Automation> {
	return await invoke<Automation>("get_automation", { name });
}

export async function toggleAutomation(request: ToggleAutomationRequest): Promise<void> {
	await invoke("toggle_automation", { name: request.name, enabled: request.enabled });
}

export async function runAutomationNow(name: string): Promise<void> {
	await invoke("run_automation_now", { name });
}

export async function getAutomationRuns(name: string, limit?: number): Promise<AutomationRunsList> {
	return await invoke<AutomationRunsList>("get_automation_runs", { name, limit });
}

export async function createAutomation(request: CreateAutomationRequest): Promise<void> {
	await invoke("create_automation", { request });
}

export async function updateAutomation(request: UpdateAutomationRequest): Promise<void> {
	await invoke("update_automation", { request });
}

export async function deleteAutomation(name: string): Promise<void> {
	await invoke("delete_automation", { name });
}

export async function startDaemon(): Promise<void> {
	await invoke("start_daemon");
}

export async function stopDaemon(): Promise<void> {
	await invoke("stop_daemon");
}

/**
 * Notification Commands
 */

export async function testNotification(): Promise<void> {
	await invoke("test_notification");
}

/**
 * System Commands
 */

export async function discoverGitRoot(path?: string): Promise<string | null> {
	return await invoke<string | null>("discover_git_root", { path });
}

export async function checkBinaryAvailable(): Promise<boolean> {
	return await invoke<boolean>("check_binary_available");
}

/**
 * Project Commands
 */

export async function listProjects(): Promise<Project[]> {
	return await invoke<Project[]>("list_projects");
}

export async function hideProject(path: string): Promise<void> {
	await invoke("hide_project", { path });
}

export async function showProject(path: string): Promise<void> {
	await invoke("show_project", { path });
}

export async function renameProject(path: string, displayName: string): Promise<void> {
	await invoke("rename_project", { path, displayName });
}

export async function reorderProjects(paths: string[]): Promise<void> {
	await invoke("reorder_projects", { paths });
}

/**
 * Plan Commands
 */

export async function discoverSpecs(specsDir?: string): Promise<SpecFileInfo[]> {
	return await invoke<SpecFileInfo[]>("discover_specs", { specsDir });
}

export async function buildPlanPrompt(specs: SpecFileInfo[]): Promise<string> {
	return await invoke<string>("build_plan_prompt", { specs });
}

export async function generatePlan(request: GeneratePlanRequest): Promise<string> {
	return await invoke<string>("generate_plan", { request });
}
