/**
 * Types for plan generation functionality.
 */

/**
 * Information about a discovered spec file.
 */
export interface SpecFileInfo {
	/** The file name (without extension). */
	name: string;
	/** The full file path. */
	path: string;
	/** The file content. */
	content: string;
}

/**
 * Request to generate a plan.
 */
export interface GeneratePlanRequest {
	/** Path to the specs directory. */
	specs_dir?: string;
	/** OpenAI API key (if not using environment variable). */
	api_key?: string;
	/** OpenAI model to use. */
	model?: string;
	/** Optional custom OpenAI base URL. */
	base_url?: string;
	/** Whether to use dry run (no API calls). */
	dry_run?: boolean;
}
