/**
 * Execution store for Velor GUI
 */

import { writable, derived } from "svelte/store";
import type { ExecutionRecord, ExecutionConfig, StartExecutionRequest } from "$lib/types";
import * as tauri from "$lib/services/tauri";

/**
 * Execution store state
 */
interface ExecutionState {
	current: ExecutionRecord | null;
	history: ExecutionRecord[];
	loading: boolean;
	error: string | null;
}

/**
 * Create the execution store
 */
function createExecutionStore() {
	const { subscribe, set, update } = writable<ExecutionState>({
		current: null,
		history: [],
		loading: false,
		error: null,
	});

	/**
	 * Start a new execution
	 */
	async function start(config: ExecutionConfig): Promise<string | null> {
		update((state) => ({ ...state, loading: true, error: null }));
		try {
			const response = await tauri.startExecution({ config });
			// Load the execution status to get the full record
			const record = await tauri.getExecutionStatus(response.execution_id);
			update((state) => ({
				...state,
				current: record,
				loading: false,
			}));
			return response.execution_id;
		} catch (e) {
			update((state) => ({
				...state,
				loading: false,
				error: e instanceof Error ? e.message : String(e),
			}));
			return null;
		}
	}

	/**
	 * Cancel the current execution
	 */
	async function cancel(): Promise<void> {
		const state = get();
		if (!state.current) return;

		update((s) => ({ ...s, loading: true, error: null }));
		try {
			await tauri.cancelExecution(state.current.id);
			// Refresh the execution status
			const record = await tauri.getExecutionStatus(state.current.id);
			update((s) => ({
				...s,
				current: record,
				loading: false,
			}));
		} catch (e) {
			update((s) => ({
				...s,
				loading: false,
				error: e instanceof Error ? e.message : String(e),
			}));
		}
	}

	/**
	 * Load execution status
	 */
	async function loadStatus(executionId: string): Promise<void> {
		update((state) => ({ ...state, loading: true, error: null }));
		try {
			const record = await tauri.getExecutionStatus(executionId);
			update((state) => ({
				...state,
				current: record,
				loading: false,
			}));
		} catch (e) {
			update((state) => ({
				...state,
				loading: false,
				error: e instanceof Error ? e.message : String(e),
			}));
		}
	}

	/**
	 * Load execution history
	 */
	async function loadHistory(limit?: number): Promise<void> {
		update((state) => ({ ...state, loading: true, error: null }));
		try {
			const response = await tauri.getExecutionHistory(limit);
			update((state) => ({
				...state,
				history: response.executions,
				loading: false,
			}));
		} catch (e) {
			update((state) => ({
				...state,
				loading: false,
				error: e instanceof Error ? e.message : String(e),
			}));
		}
	}

	/**
	 * Clear the current execution
	 */
	function clearCurrent(): void {
		update((state) => ({ ...state, current: null }));
	}

	/**
	 * Update the current execution record (called by event listeners)
	 */
	function updateCurrent(record: ExecutionRecord): void {
		update((state) => ({
			...state,
			current: record,
		}));
	}

	/**
	 * Get the current state value
	 */
	function get(): ExecutionState {
		let value: ExecutionState | null = null;
		subscribe((v) => {
			value = v;
		})();
		return value!;
	}

	return {
		subscribe,
		start,
		cancel,
		loadStatus,
		loadHistory,
		clearCurrent,
		updateCurrent,
		get,
	};
}

export const executionStore = createExecutionStore();

/**
 * Derived stores for convenience
 */
export const currentExecution = derived(executionStore, ($store) => $store.current);
export const executionHistory = derived(executionStore, ($store) => $store.history);
export const executionLoading = derived(executionStore, ($store) => $store.loading);
export const executionError = derived(executionStore, ($store) => $store.error);
