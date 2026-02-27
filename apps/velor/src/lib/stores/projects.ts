/**
 * Projects store for Velor GUI
 * Manages project metadata and preferences for organizing sessions
 */

import { writable, derived } from "svelte/store";
import type { Project } from "$lib/types";
import * as tauri from "$lib/services/tauri";

/**
 * Projects store state
 */
interface ProjectsState {
	/** List of all projects */
	projects: Project[];
	/** Loading state */
	loading: boolean;
	/** Error message if any */
	error: string | null;
}

/**
 * Initial state for the projects store
 */
const initialState: ProjectsState = {
	projects: [],
	loading: false,
	error: null,
};

/**
 * Create the projects store
 */
function createProjectsStore() {
	const { subscribe, update, set } = writable<ProjectsState>(initialState);

	/**
	 * Load all projects from the backend
	 */
	async function load(): Promise<void> {
		update((state) => ({ ...state, loading: true, error: null }));
		try {
			const projects = await tauri.listProjects();
			update((state) => ({
				...state,
				projects,
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
	 * Hide a project from the sidebar
	 */
	async function hide(path: string): Promise<void> {
		update((state) => ({ ...state, loading: true, error: null }));
		try {
			await tauri.hideProject(path);
			// Update local state
			update((state) => ({
				...state,
				projects: state.projects.map((p) =>
					p.path === path ? { ...p, hidden: true } : p,
				),
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
	 * Show a hidden project
	 */
	async function show(path: string): Promise<void> {
		update((state) => ({ ...state, loading: true, error: null }));
		try {
			await tauri.showProject(path);
			// Update local state
			update((state) => ({
				...state,
				projects: state.projects.map((p) =>
					p.path === path ? { ...p, hidden: false } : p,
				),
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
	 * Rename a project
	 */
	async function rename(path: string, displayName: string): Promise<void> {
		update((state) => ({ ...state, loading: true, error: null }));
		try {
			await tauri.renameProject(path, displayName);
			// Update local state
			update((state) => ({
				...state,
				projects: state.projects.map((p) =>
					p.path === path ? { ...p, display_name: displayName } : p,
				),
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
	 * Reorder projects by updating their sort order
	 */
	async function reorder(paths: string[]): Promise<void> {
		update((state) => ({ ...state, loading: true, error: null }));
		try {
			await tauri.reorderProjects(paths);
			// Update local state to match new order
			update((state) => {
				const pathIndex = new Map(paths.map((p, i) => [p, i]));
				const reordered = [...state.projects].sort((a, b) => {
					const aIndex = pathIndex.get(a.path) ?? Number.MAX_SAFE_INTEGER;
					const bIndex = pathIndex.get(b.path) ?? Number.MAX_SAFE_INTEGER;
					return aIndex - bIndex;
				});
				return {
					...state,
					projects: reordered.map((p, i) => ({ ...p, sort_order: i })),
					loading: false,
				};
			});
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

	return {
		subscribe,
		load,
		hide,
		show,
		rename,
		reorder,
		clearError,
		reset,
	};
}

export const projectsStore = createProjectsStore();

/**
 * Derived stores for convenience
 */
export const projects = derived(projectsStore, ($store) => $store.projects);
export const projectsLoading = derived(projectsStore, ($store) => $store.loading);
export const projectsError = derived(projectsStore, ($store) => $store.error);
