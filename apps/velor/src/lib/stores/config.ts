/**
 * Configuration store for Velor GUI
 */

import { writable, derived } from "svelte/store";
import type { VelorConfig } from "$lib/types";
import * as tauri from "$lib/services/tauri";

/**
 * Config store state
 */
interface ConfigState {
	config: VelorConfig | null;
	configToml: string | null;
	homeConfig: string | null;
	repoConfig: string | null;
	gitRoot: string | null;
	loading: boolean;
	error: string | null;
}

/**
 * Create the config store
 */
function createConfigStore() {
	const { subscribe, update } = writable<ConfigState>({
		config: null,
		configToml: null,
		homeConfig: null,
		repoConfig: null,
		gitRoot: null,
		loading: false,
		error: null,
	});

	/**
	 * Load all configurations
	 */
	async function load(): Promise<void> {
		update((state) => ({ ...state, loading: true, error: null }));
		try {
			const configResponse = await tauri.getConfig();
			update((state) => ({
				...state,
				config: configResponse.merged,
				configToml: configResponse.merged_toml,
				homeConfig: configResponse.home_toml || null,
				repoConfig: configResponse.repo_toml || null,
				gitRoot: null,
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
	 * Save configuration to home or repo
	 */
	async function save(configType: "home" | "repo", content: string): Promise<void> {
		update((state) => ({ ...state, loading: true, error: null }));
		try {
			await tauri.saveConfig({ config_type: configType, content });
			// Reload configs after save
			await load();
		} catch (e) {
			update((state) => ({
				...state,
				loading: false,
				error: e instanceof Error ? e.message : String(e),
			}));
			throw e;
		}
	}

	return {
		subscribe,
		load,
		save,
	};
}

export const configStore = createConfigStore();

/**
 * Derived stores for convenience
 */
export const config = derived(configStore, ($store) => $store.config);
export const configToml = derived(configStore, ($store) => $store.configToml);
export const homeConfig = derived(configStore, ($store) => $store.homeConfig);
export const repoConfig = derived(configStore, ($store) => $store.repoConfig);
export const gitRoot = derived(configStore, ($store) => $store.gitRoot);
export const configLoading = derived(configStore, ($store) => $store.loading);
export const configError = derived(configStore, ($store) => $store.error);
