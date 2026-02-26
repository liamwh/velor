/**
 * Sessions store for Velor GUI
 * Manages execution session history from SQLite persistence
 */

import { writable, derived } from "svelte/store";
import type { ExecutionRecord, SessionStats } from "$lib/types";
import * as tauri from "$lib/services/tauri";

/** Default page size for session listings */
const DEFAULT_PAGE_SIZE = 20;

/**
 * Sessions store state
 */
interface SessionsState {
	/** List of sessions */
	sessions: ExecutionRecord[];
	/** Currently selected session for detail view */
	selectedSession: ExecutionRecord | null;
	/** Aggregated session statistics */
	stats: SessionStats | null;
	/** Loading state */
	loading: boolean;
	/** Error message if any */
	error: string | null;
	/** Whether more sessions are available for pagination */
	hasMore: boolean;
	/** Current offset for pagination */
	offset: number;
}

/**
 * Initial state for the sessions store
 */
const initialState: SessionsState = {
	sessions: [],
	selectedSession: null,
	stats: null,
	loading: false,
	error: null,
	hasMore: false,
	offset: 0,
};

/**
 * Create the sessions store
 */
function createSessionsStore() {
	const { subscribe, update, set } = writable<SessionsState>(initialState);

	/**
	 * Load sessions from the backend with pagination
	 */
	async function load(limit: number = DEFAULT_PAGE_SIZE): Promise<void> {
		update((state) => ({ ...state, loading: true, error: null }));
		try {
			const sessions = await tauri.listSessions(limit, 0);
			const stats = await tauri.getSessionStats();
			update((state) => ({
				...state,
				sessions,
				stats,
				offset: limit,
				hasMore: sessions.length === limit,
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
	 * Load more sessions (pagination)
	 */
	async function loadMore(limit: number = DEFAULT_PAGE_SIZE): Promise<void> {
		const state = getState();
		if (!state.hasMore || state.loading) {
			return;
		}

		update((s) => ({ ...s, loading: true, error: null }));
		try {
			const newSessions = await tauri.listSessions(limit, state.offset);
			update((s) => ({
				...s,
				sessions: [...s.sessions, ...newSessions],
				offset: s.offset + limit,
				hasMore: newSessions.length === limit,
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
	 * Get a single session by ID
	 */
	async function getSession(id: string): Promise<ExecutionRecord | null> {
		update((state) => ({ ...state, loading: true, error: null }));
		try {
			const session = await tauri.getSession(id);
			update((state) => ({
				...state,
				selectedSession: session,
				loading: false,
			}));
			return session;
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
	 * Delete a session by ID
	 */
	async function deleteSession(id: string): Promise<void> {
		update((state) => ({ ...state, loading: true, error: null }));
		try {
			await tauri.deleteSession(id);
			// Remove from local state
			update((state) => ({
				...state,
				sessions: state.sessions.filter((s) => s.id !== id),
				selectedSession: state.selectedSession?.id === id ? null : state.selectedSession,
				loading: false,
			}));
			// Refresh stats after deletion
			await refreshStats();
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
	 * Refresh the session list (reload first page)
	 */
	async function refresh(limit: number = DEFAULT_PAGE_SIZE): Promise<void> {
		await load(limit);
	}

	/**
	 * Refresh only the stats
	 */
	async function refreshStats(): Promise<void> {
		try {
			const stats = await tauri.getSessionStats();
			update((state) => ({ ...state, stats }));
		} catch (e) {
			// Silently fail stats refresh - don't disrupt user
			console.error("Failed to refresh session stats:", e);
		}
	}

	/**
	 * Select a session for detail view
	 */
	function select(session: ExecutionRecord | null): void {
		update((state) => ({ ...state, selectedSession: session }));
	}

	/**
	 * Clear the selected session
	 */
	function clearSelected(): void {
		update((state) => ({ ...state, selectedSession: null }));
	}

	/**
	 * Clear any error
	 */
	function clearError(): void {
		update((state) => ({ ...state, error: null }));
	}

	/**
	 * Reset the store to initial state
	 */
	function reset(): void {
		set(initialState);
	}

	/**
	 * Get the current state value
	 */
	function getState(): SessionsState {
		let value: SessionsState | null = null;
		subscribe((v) => {
			value = v;
		})();
		return value!;
	}

	return {
		subscribe,
		load,
		loadMore,
		get: getSession,
		delete: deleteSession,
		refresh,
		refreshStats,
		select,
		clearSelected,
		clearError,
		reset,
	};
}

export const sessionsStore = createSessionsStore();

/**
 * Derived stores for convenience
 */
export const sessions = derived(sessionsStore, ($store) => $store.sessions);
export const selectedSession = derived(sessionsStore, ($store) => $store.selectedSession);
export const sessionStats = derived(sessionsStore, ($store) => $store.stats);
export const sessionsLoading = derived(sessionsStore, ($store) => $store.loading);
export const sessionsError = derived(sessionsStore, ($store) => $store.error);
export const sessionsHasMore = derived(sessionsStore, ($store) => $store.hasMore);
