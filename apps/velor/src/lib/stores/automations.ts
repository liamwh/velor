/**
 * Automations store for Velor GUI
 */

import { writable, derived } from "svelte/store";
import type { Automation, AutomationRun, ToggleAutomationRequest } from "$lib/types";
import * as tauri from "$lib/services/tauri";

/**
 * Automations store state
 */
interface AutomationsState {
	automations: Automation[];
	selectedAutomation: Automation | null;
	runs: AutomationRun[];
	daemonRunning: boolean;
	loading: boolean;
	error: string | null;
}

/**
 * Create the automations store
 */
function createAutomationsStore() {
	const { subscribe, set, update } = writable<AutomationsState>({
		automations: [],
		selectedAutomation: null,
		runs: [],
		daemonRunning: false,
		loading: false,
		error: null,
	});

	/**
	 * Load all automations
	 */
	async function load(): Promise<void> {
		update((state) => ({ ...state, loading: true, error: null }));
		try {
			const response = await tauri.listAutomations();
			update((state) => ({
				...state,
				automations: response.automations,
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
	 * Get a single automation by ID
	 */
	async function get(id: string): Promise<Automation | null> {
		update((state) => ({ ...state, loading: true, error: null }));
		try {
			const automation = await tauri.getAutomation(id);
			update((state) => ({
				...state,
				selectedAutomation: automation,
				loading: false,
			}));
			return automation;
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
	 * Toggle automation enabled state
	 */
	async function toggle(request: ToggleAutomationRequest): Promise<void> {
		update((state) => ({ ...state, loading: true, error: null }));
		try {
			const updated = await tauri.toggleAutomation(request);
			update((state) => ({
				...state,
				automations: state.automations.map((a) => (a.id === updated.id ? updated : a)),
				selectedAutomation:
					state.selectedAutomation?.id === updated.id ? updated : state.selectedAutomation,
				loading: false,
			}));
		} catch (e) {
			update((state) => ({
				...state,
				loading: false,
				error: e instanceof Error ? e.message : String(e),
			}));
			throw e;
		}
	}

	/**
	 * Run an automation manually
	 */
	async function runNow(id: string): Promise<void> {
		update((state) => ({ ...state, loading: true, error: null }));
		try {
			await tauri.runAutomationNow(id);
			update((state) => ({ ...state, loading: false }));
		} catch (e) {
			update((state) => ({
				...state,
				loading: false,
				error: e instanceof Error ? e.message : String(e),
			}));
			throw e;
		}
	}

	/**
	 * Load automation run history
	 */
	async function loadRuns(id: string, limit?: number): Promise<void> {
		update((state) => ({ ...state, loading: true, error: null }));
		try {
			const response = await tauri.getAutomationRuns(id, limit);
			update((state) => ({
				...state,
				runs: response.runs,
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
	 * Start the daemon
	 */
	async function startDaemon(): Promise<void> {
		update((state) => ({ ...state, loading: true, error: null }));
		try {
			await tauri.startDaemon();
			update((state) => ({ ...state, daemonRunning: true, loading: false }));
		} catch (e) {
			update((state) => ({
				...state,
				loading: false,
				error: e instanceof Error ? e.message : String(e),
			}));
			throw e;
		}
	}

	/**
	 * Stop the daemon
	 */
	async function stopDaemon(): Promise<void> {
		update((state) => ({ ...state, loading: true, error: null }));
		try {
			await tauri.stopDaemon();
			update((state) => ({ ...state, daemonRunning: false, loading: false }));
		} catch (e) {
			update((state) => ({
				...state,
				loading: false,
				error: e instanceof Error ? e.message : String(e),
			}));
			throw e;
		}
	}

	/**
	 * Set daemon running state (called by event listeners)
	 */
	function setDaemonRunning(running: boolean): void {
		update((state) => ({ ...state, daemonRunning: running }));
	}

	/**
	 * Clear the selected automation
	 */
	function clearSelected(): void {
		update((state) => ({ ...state, selectedAutomation: null }));
	}

	return {
		subscribe,
		load,
		get,
		toggle,
		runNow,
		loadRuns,
		startDaemon,
		stopDaemon,
		setDaemonRunning,
		clearSelected,
	};
}

export const automationsStore = createAutomationsStore();

/**
 * Derived stores for convenience
 */
export const automations = derived(automationsStore, ($store) => $store.automations);
export const selectedAutomation = derived(automationsStore, ($store) => $store.selectedAutomation);
export const automationRuns = derived(automationsStore, ($store) => $store.runs);
export const daemonRunning = derived(automationsStore, ($store) => $store.daemonRunning);
export const automationsLoading = derived(automationsStore, ($store) => $store.loading);
export const automationsError = derived(automationsStore, ($store) => $store.error);
