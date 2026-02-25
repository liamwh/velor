/**
 * Event listeners for Velor GUI
 * Handles Tauri events from the backend
 */

import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { ExecutionRecord, Automation } from "$lib/types";

/**
 * Execution event payload
 */
export interface ExecutionEventPayload {
	execution: ExecutionRecord;
}

/**
 * Automation event payload
 */
export interface AutomationEventPayload {
	automation: Automation;
}

/**
 * Daemon event payload
 */
export interface DaemonEventPayload {
	running: boolean;
}

/**
 * Error event payload
 */
export interface ErrorEventPayload {
	error: string;
}

/**
 * Event type constants
 */
export const EVENTS = {
	// Execution events
	EXECUTION_STARTED: "velor://execution_started",
	EXECUTION_UPDATED: "velor://execution_updated",
	EXECUTION_COMPLETED: "velor://execution_completed",
	EXECUTION_FAILED: "velor://execution_failed",

	// Automation events
	AUTOMATION_TRIGGERED: "velor://automation_triggered",
	AUTOMATION_COMPLETED: "velor://automation_completed",
	AUTOMATION_FAILED: "velor://automation_failed",

	// Daemon events
	DAEMON_STARTED: "velor://daemon_started",
	DAEMON_STOPPED: "velor://daemon_stopped",

	// Error events
	ERROR: "velor://error",
} as const;

/**
 * Event listener type
 */
export type EventListener<T = unknown> = (payload: T) => void;

/**
 * Event service for managing Tauri event listeners
 */
export class EventService {
	private listeners: Map<string, UnlistenFn[]> = new Map();

	/**
	 * Listen to execution started events
	 */
	async onExecutionStarted(callback: (payload: ExecutionEventPayload) => void): Promise<void> {
		const unlisten = await listen<ExecutionEventPayload>(EVENTS.EXECUTION_STARTED, (event) => {
			callback(event.payload);
		});
		this.track(EVENTS.EXECUTION_STARTED, unlisten);
	}

	/**
	 * Listen to execution updated events
	 */
	async onExecutionUpdated(callback: (payload: ExecutionEventPayload) => void): Promise<void> {
		const unlisten = await listen<ExecutionEventPayload>(EVENTS.EXECUTION_UPDATED, (event) => {
			callback(event.payload);
		});
		this.track(EVENTS.EXECUTION_UPDATED, unlisten);
	}

	/**
	 * Listen to execution completed events
	 */
	async onExecutionCompleted(callback: (payload: ExecutionEventPayload) => void): Promise<void> {
		const unlisten = await listen<ExecutionEventPayload>(EVENTS.EXECUTION_COMPLETED, (event) => {
			callback(event.payload);
		});
		this.track(EVENTS.EXECUTION_COMPLETED, unlisten);
	}

	/**
	 * Listen to execution failed events
	 */
	async onExecutionFailed(callback: (payload: ExecutionEventPayload) => void): Promise<void> {
		const unlisten = await listen<ExecutionEventPayload>(EVENTS.EXECUTION_FAILED, (event) => {
			callback(event.payload);
		});
		this.track(EVENTS.EXECUTION_FAILED, unlisten);
	}

	/**
	 * Listen to automation triggered events
	 */
	async onAutomationTriggered(callback: (payload: AutomationEventPayload) => void): Promise<void> {
		const unlisten = await listen<AutomationEventPayload>(EVENTS.AUTOMATION_TRIGGERED, (event) => {
			callback(event.payload);
		});
		this.track(EVENTS.AUTOMATION_TRIGGERED, unlisten);
	}

	/**
	 * Listen to automation completed events
	 */
	async onAutomationCompleted(callback: (payload: AutomationEventPayload) => void): Promise<void> {
		const unlisten = await listen<AutomationEventPayload>(EVENTS.AUTOMATION_COMPLETED, (event) => {
			callback(event.payload);
		});
		this.track(EVENTS.AUTOMATION_COMPLETED, unlisten);
	}

	/**
	 * Listen to automation failed events
	 */
	async onAutomationFailed(callback: (payload: AutomationEventPayload) => void): Promise<void> {
		const unlisten = await listen<AutomationEventPayload>(EVENTS.AUTOMATION_FAILED, (event) => {
			callback(event.payload);
		});
		this.track(EVENTS.AUTOMATION_FAILED, unlisten);
	}

	/**
	 * Listen to daemon started events
	 */
	async onDaemonStarted(callback: (payload: DaemonEventPayload) => void): Promise<void> {
		const unlisten = await listen<DaemonEventPayload>(EVENTS.DAEMON_STARTED, (event) => {
			callback(event.payload);
		});
		this.track(EVENTS.DAEMON_STARTED, unlisten);
	}

	/**
	 * Listen to daemon stopped events
	 */
	async onDaemonStopped(callback: (payload: DaemonEventPayload) => void): Promise<void> {
		const unlisten = await listen<DaemonEventPayload>(EVENTS.DAEMON_STOPPED, (event) => {
			callback(event.payload);
		});
		this.track(EVENTS.DAEMON_STOPPED, unlisten);
	}

	/**
	 * Listen to error events
	 */
	async onError(callback: (payload: ErrorEventPayload) => void): Promise<void> {
		const unlisten = await listen<ErrorEventPayload>(EVENTS.ERROR, (event) => {
			callback(event.payload);
		});
		this.track(EVENTS.ERROR, unlisten);
	}

	/**
	 * Track an unlisten function for cleanup
	 */
	private track(event: string, unlisten: UnlistenFn): void {
		if (!this.listeners.has(event)) {
			this.listeners.set(event, []);
		}
		this.listeners.get(event)?.push(unlisten);
	}

	/**
	 * Remove all listeners for a specific event
	 */
	unlisten(event: string): void {
		const unlisteners = this.listeners.get(event);
		if (unlisteners) {
			for (const unlisten of unlisteners) {
				unlisten();
			}
			this.listeners.delete(event);
		}
	}

	/**
	 * Remove all event listeners
	 */
	unlistenAll(): void {
		for (const [event, unlisteners] of this.listeners.entries()) {
			for (const unlisten of unlisteners) {
				unlisten();
			}
		}
		this.listeners.clear();
	}
}

/**
 * Global event service instance
 */
export const eventService = new EventService();
