/**
 * Tauri command wrappers for Velor GUI backend
 * These provide type-safe access to Tauri commands
 */

import { invoke } from "@tauri-apps/api/core";
import type {
	ConfigResponse,
	ConfigFileType,
	SaveConfigRequest,
	ExecutionRecord,
	ExecutionList,
	StartExecutionRequest,
	StartExecutionResponse,
	Automation,
	AutomationList,
	AutomationRun,
	AutomationRunsList,
	ToggleAutomationRequest,
	UpdateAutomationRequest,
} from "$lib/types";

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
	await invoke("cancel_execution", { executionId });
}

export async function getExecutionStatus(executionId: string): Promise<ExecutionRecord> {
	return await invoke<ExecutionRecord>("get_execution_status", { executionId });
}

export async function getExecutionHistory(limit?: number): Promise<ExecutionList> {
	return await invoke<ExecutionList>("get_execution_history", { limit });
}

/**
 * Automation Commands
 */

export async function listAutomations(): Promise<AutomationList> {
	return await invoke<AutomationList>("list_automations");
}

export async function getAutomation(id: string): Promise<Automation> {
	return await invoke<Automation>("get_automation", { id });
}

export async function toggleAutomation(request: ToggleAutomationRequest): Promise<Automation> {
	return await invoke<Automation>("toggle_automation", { request });
}

export async function runAutomationNow(id: string): Promise<void> {
	await invoke("run_automation_now", { id });
}

export async function getAutomationRuns(id: string, limit?: number): Promise<AutomationRunsList> {
	return await invoke<AutomationRunsList>("get_automation_runs", { id, limit });
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
