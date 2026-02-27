/**
 * Configuration types for Velor GUI
 * These types mirror the Rust types from velor-core
 */

/**
 * Prompt template configuration
 */
export interface Prompt {
	/** The prompt content (can be a string or a table with template/complete_token) */
	[key: string]: string | { template: string; complete_token?: string } | undefined;
}

/**
 * Executable prompt template for the GUI
 * This is a more structured type for prompt templates that can be executed
 */
export interface PromptTemplate {
	/** Unique name/identifier of the prompt */
	name: string;
	/** Human-readable description */
	description?: string;
	/** The prompt template content or table */
	template: string;
	/** Custom completion token (optional) */
	complete_token?: string;
	/** Whether this is a template or an executable prompt */
	is_template?: boolean;
	/** Default variables for this prompt */
	vars?: Record<string, string | number | boolean>;
}

/**
 * Prompts section of config
 */
export interface Prompts {
	[key: string]: Prompt;
}

/**
 * Variables section of config
 */
export interface Vars {
	[key: string]:
		| string
		| number
		| boolean
		| string[]
		| { [key: string]: string | number | boolean }
		| undefined;
}

/**
 * Notifications configuration
 */
export interface NotificationConfig {
	enabled?: boolean;
	notify_on_success?: boolean;
	notify_on_max_iterations?: boolean;
	notify_on_failure?: boolean;
	output_preview_chars?: number;
}

/**
 * Telegram notification configuration
 */
export interface TelegramConfig extends NotificationConfig {
	bot_token_env?: string;
	chat_id?: string;
	api_base_url?: string;
	parse_mode?: "MarkdownV2" | "Html";
}

/**
 * macOS notification configuration
 */
export interface MacOSConfig extends NotificationConfig {
	enabled?: boolean;
	sound?: string;
}

/**
 * Notifications section
 */
export interface Notifications {
	enabled?: boolean;
	notify_on_success?: boolean;
	notify_on_max_iterations?: boolean;
	notify_on_failure?: boolean;
	output_preview_chars?: number;
	telegram?: TelegramConfig;
	macos?: MacOSConfig;
}

/**
 * Complete Velor configuration
 */
export interface VelorConfig {
	/** Path to config file */
	_config_path?: string;
	/** Claude binary to use */
	binary?: string;
	/** Permission mode for Claude */
	permission_mode?: string;
	/** Maximum iterations for auto mode */
	max_iterations?: number;
	/** Maximum retries on failure */
	max_retries?: number;
	/** Completion token to detect completion */
	complete_token?: string;
	/** Template variables */
	vars?: Vars;
	/** Prompt templates */
	prompts?: Prompts;
	/** Notifications configuration */
	notifications?: Notifications;
	/** Agent rules configuration */
	rules?: {
		enabled?: boolean;
		path?: string;
	};
	/** ACP (Agent Client Protocol) configuration */
	acp?: {
		enabled?: boolean;
		base_url?: string;
		api_key_env?: string;
	};
}

/**
 * Config response with separate home/repo configs
 */
export interface ConfigResponse {
	/** Merged effective configuration */
	merged: VelorConfig;
	/** Pre-serialized TOML for the merged config */
	merged_toml: string;
	/** Home directory config */
	home?: VelorConfig;
	/** Pre-serialized TOML for the home config */
	home_toml?: string;
	/** Repository config */
	repo?: VelorConfig;
	/** Pre-serialized TOML for the repo config */
	repo_toml?: string;
}

/**
 * Config file type for saving
 */
export type ConfigFileType = "home" | "repo";

/**
 * Save config request
 */
export interface SaveConfigRequest {
	/** Which config file to save to */
	config_type: ConfigFileType;
	/** The config content to save (TOML string) */
	content: string;
}
