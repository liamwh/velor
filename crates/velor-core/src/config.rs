//! Configuration file loading and management.
//!
//! This module handles loading the TOML configuration file from the `.velor` directory
//! in the git repository root.

use color_eyre::eyre::WrapErr;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// Global persistence configuration (shared by multiple commands).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ConversationDbConfig {
    /// Path to the shared SQLite database (relative to git root).
    pub path: String,

    /// Enable encryption for stored message content.
    pub encrypt_content: bool,

    /// Environment variable containing a 32-byte key (base64) for encryption.
    /// Only used when encrypt_content = true.
    pub encryption_key_env: String,

    /// Optional retention: delete sessions older than N days (0 = disabled).
    pub retention_days: u32,
}

impl Default for ConversationDbConfig {
    fn default() -> Self {
        Self {
            path: ".velor/conversations.db".to_string(),
            encrypt_content: false,
            encryption_key_env: "VELOR_CONVERSATIONS_KEY".to_string(),
            retention_days: 0,
        }
    }
}

/// Configuration for the plan subcommand.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PlanConfig {
    /// Directory for spec files (relative to git root).
    pub specs_dir: String,

    /// Maximum review iterations.
    pub plan_max_iterations: u32,

    /// Environment variable name for OpenAI API key.
    pub openai_api_key_env: String,

    /// OpenAI model to use for reviews.
    pub openai_model: String,

    /// OpenAI base URL (optional, for custom endpoints).
    pub openai_base_url: Option<String>,
}

impl Default for PlanConfig {
    fn default() -> Self {
        Self {
            specs_dir: "specs".to_string(),
            plan_max_iterations: 10,
            openai_api_key_env: "OPENAI_API_KEY".to_string(),
            openai_model: "gpt-4o".to_string(),
            openai_base_url: None,
        }
    }
}

/// Parse mode for Telegram messages.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum TelegramParseMode {
    /// MarkdownV2 formatting.
    #[default]
    MarkdownV2,
    /// HTML formatting.
    Html,
}

/// Configuration for Telegram notifications.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TelegramConfig {
    /// Whether Telegram notifications are enabled.
    pub enabled: bool,

    /// Environment variable name containing the bot token.
    pub bot_token_env: String,

    /// Telegram chat ID to send messages to.
    pub chat_id: String,

    /// Optional API base URL (for proxies).
    pub api_base_url: Option<String>,

    /// Parse mode for messages (MarkdownV2, HTML, or None).
    pub parse_mode: Option<TelegramParseMode>,
}

impl Default for TelegramConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bot_token_env: "TELEGRAM_BOT_TOKEN".to_string(),
            chat_id: String::new(),
            api_base_url: None,
            parse_mode: Some(TelegramParseMode::MarkdownV2),
        }
    }
}

/// Configuration for macOS notifications.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MacOSConfig {
    /// Whether macOS notifications are enabled.
    pub enabled: bool,

    /// Sound to play (e.g., "default", "Basso", "Sosumi", or empty for silent).
    pub sound: Option<String>,
}

impl Default for MacOSConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            sound: Some("default".to_string()),
        }
    }
}

/// Configuration for the rules system.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RulesConfig {
    /// Whether the rules system is enabled.
    pub enabled: bool,

    /// Directory for rule definitions (relative to git root).
    pub directory: String,

    /// Maximum number of follow-up prompts per iteration for glob-based rule injection.
    ///
    /// This prevents infinite loops when rules trigger other rules in a chain.
    /// Default: 2
    pub max_mid_iteration_injections: u32,

    /// Whether to use intelligent selection for rules without always_apply or globs.
    ///
    /// When enabled, rules that don't have always_apply: true or glob patterns
    /// are selected via ACP based on the current task.
    /// Default: false
    pub intelligent_selection: bool,

    /// Maximum number of intelligent rules to select per iteration.
    ///
    /// Prevents overwhelming the agent with too many rules.
    /// Default: 5
    pub intelligent_selection_max_rules: usize,
}

impl Default for RulesConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            directory: ".agents/rules".to_string(),
            max_mid_iteration_injections: 2,
            intelligent_selection: false,
            intelligent_selection_max_rules: 5,
        }
    }
}

/// Configuration for the file-based prompts system.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PromptsConfig {
    /// Whether file-based prompts are enabled.
    pub enabled: bool,

    /// Directory for prompt definitions (relative to .velor/).
    pub directory: String,
}

impl Default for PromptsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            directory: "prompts".to_string(),
        }
    }
}

/// Configuration for notifications.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct NotificationsConfig {
    /// Whether notifications are globally enabled.
    pub enabled: bool,

    /// Send notification on successful completion.
    pub notify_on_success: bool,

    /// Send notification when max iterations are reached.
    pub notify_on_max_iterations: bool,

    /// Send notification on failure.
    pub notify_on_failure: bool,

    /// Number of characters to include in output preview.
    pub output_preview_chars: u32,

    /// Telegram notification configuration.
    pub telegram: Option<TelegramConfig>,

    /// macOS notification configuration.
    pub macos: Option<MacOSConfig>,
}

/// Configuration for the automations feature.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AutomationsConfig {
    /// Directory for automation definitions (relative to git root).
    pub automations_dir: String,

    /// Path to the state database for tracking runs (relative to git root).
    pub state_db_path: String,

    /// Default maximum concurrent automations.
    pub max_concurrent: u32,

    /// Default timezone for schedule parsing (IANA tz database name).
    pub default_timezone: String,

    /// Default timeout for automation runs (seconds).
    pub default_timeout_seconds: u64,

    /// Maximum output size to store (bytes).
    pub max_output_bytes: usize,
}

impl Default for AutomationsConfig {
    fn default() -> Self {
        Self {
            automations_dir: ".velor/automations.d".to_string(),
            state_db_path: ".velor/velor.db".to_string(),
            max_concurrent: 3,
            default_timezone: "UTC".to_string(),
            default_timeout_seconds: 3600, // 1 hour
            max_output_bytes: 100_000,     // 100 KB
        }
    }
}

impl Default for NotificationsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            notify_on_success: true,
            notify_on_max_iterations: true,
            notify_on_failure: true,
            output_preview_chars: 500,
            telegram: None,
            macos: None,
        }
    }
}

/// Communication protocol for agent interaction.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    /// Spawn subprocess with stdin/stdout (original behavior).
    #[default]
    Subprocess,
    /// ACP (Agent Client Protocol) via stdio.
    Acp,
}

/// Permission handling mode for ACP.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PermissionMode {
    /// Automatically allow all permission requests.
    #[default]
    Allow,
    /// Automatically deny all permission requests.
    Deny,
    // Future: Interactive prompting for each request.
    // Ask,
}

/// Configuration for ACP (Agent Client Protocol) mode.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AcpConfig {
    /// Environment variable name for Anthropic API key.
    pub api_key_env: String,

    /// Permission handling mode.
    pub permission_mode: PermissionMode,

    /// Keep adapter process alive between prompts (recommended).
    pub persist_adapter: bool,
}

impl Default for AcpConfig {
    fn default() -> Self {
        Self {
            api_key_env: "ANTHROPIC_API_KEY".to_string(),
            permission_mode: PermissionMode::Allow,
            persist_adapter: true,
        }
    }
}

/// Configuration loaded from the TOML file.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FileConfig {
    /// Default values for CLI options.
    #[serde(default)]
    pub defaults: Defaults,

    /// Global variables available to all prompt templates.
    #[serde(default)]
    pub vars: BTreeMap<String, String>,

    /// Named prompt templates.
    #[serde(default)]
    pub prompts: BTreeMap<String, PromptDef>,

    /// Shared conversation database configuration.
    #[serde(default)]
    pub conversation_db: ConversationDbConfig,

    /// Plan subcommand configuration.
    #[serde(default)]
    pub plan: PlanConfig,

    /// Notifications configuration.
    #[serde(default)]
    pub notifications: NotificationsConfig,

    /// Rules system configuration.
    #[serde(default)]
    pub rules: RulesConfig,

    /// Prompts system configuration.
    #[serde(default)]
    pub prompts_config: PromptsConfig,

    /// Automations configuration.
    #[serde(default)]
    pub automations: AutomationsConfig,
}

/// Default values that can be overridden by CLI arguments.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Defaults {
    /// Communication protocol for agent interaction.
    #[serde(default)]
    pub protocol: Protocol,

    /// Default permission mode for Claude (e.g. "acceptEdits").
    pub permission_mode: Option<String>,

    /// Default path to the PRD file.
    pub prd_path: Option<String>,

    /// Default path to the progress file.
    pub progress_path: Option<String>,

    /// Default number of iterations for auto-mode.
    pub iterations: Option<u32>,

    /// Default prompt name to use.
    pub prompt: Option<String>,

    /// Default completion token that signals PRD completion.
    pub complete_token: Option<String>,

    /// Default Claude binary to use (e.g. "claude" or "claude-glm").
    #[serde(default = "default_binary")]
    pub binary: String,

    /// Maximum retry attempts per auto-loop iteration.
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,

    /// Base backoff duration in milliseconds for exponential backoff.
    #[serde(default = "default_base_backoff_ms")]
    pub base_backoff_ms: u32,

    /// Absolute timeout for all retries combined in milliseconds.
    #[serde(default = "default_absolute_timeout_ms")]
    pub absolute_timeout_ms: u32,

    /// ACP-specific configuration (only used when protocol = "acp").
    #[serde(default)]
    pub acp: AcpConfig,
}

/// Default value for the binary field.
fn default_binary() -> String {
    "claude-glm".to_string()
}

/// Default value for max_retries field.
fn default_max_retries() -> u32 {
    5
}

/// Default value for base_backoff_ms field.
fn default_base_backoff_ms() -> u32 {
    100
}

/// Default value for absolute_timeout_ms field (5 hours in milliseconds).
fn default_absolute_timeout_ms() -> u32 {
    5 * 60 * 60 * 1000 // 5 hours
}

/// A named prompt template definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PromptDef {
    /// Inline string format: `prompt = "template string"`
    Inline(String),
    /// Table format with optional complete_token override.
    #[allow(dead_code)]
    Table {
        /// The MiniJinja template string.
        template: String,
        /// Optional override of the completion token for this prompt.
        #[serde(default)]
        complete_token: Option<String>,
    },
    /// File-based prompt with optional complete_token override.
    File {
        /// Path to the prompt file relative to the prompts directory.
        path: String,
        /// Optional override of the completion token for this prompt.
        #[serde(default)]
        complete_token: Option<String>,
    },
}

impl PromptDef {
    /// Returns the template string.
    ///
    /// For `PromptDef::File` variants, this returns the path placeholder
    /// that should be resolved through the prompt cache.
    #[must_use]
    pub fn template(&self) -> &str {
        match self {
            Self::Inline(s) => s,
            Self::Table { template, .. } => template,
            Self::File { path, .. } => path,
        }
    }

    /// Returns the optional completion token override.
    #[must_use]
    #[allow(dead_code)]
    pub fn complete_token(&self) -> Option<&String> {
        match self {
            Self::Inline(_) => None,
            Self::Table { complete_token, .. } => complete_token.as_ref(),
            Self::File { complete_token, .. } => complete_token.as_ref(),
        }
    }

    /// Returns whether this is a file-based prompt.
    #[must_use]
    pub const fn is_file(&self) -> bool {
        matches!(self, Self::File { .. })
    }
}

impl Defaults {
    /// Merges two Defaults, with `overlay` taking precedence.
    ///
    /// For each field, if `overlay` has a `Some` value, it is used;
    /// otherwise, the value from `self` (base) is used.
    #[must_use]
    #[tracing::instrument(level = "debug", ret)]
    pub fn merge(self, overlay: Self) -> Self {
        Self {
            // For protocol, overlay takes precedence (Subprocess is default)
            protocol: overlay.protocol,
            permission_mode: overlay.permission_mode.or(self.permission_mode),
            prd_path: overlay.prd_path.or(self.prd_path),
            progress_path: overlay.progress_path.or(self.progress_path),
            iterations: overlay.iterations.or(self.iterations),
            prompt: overlay.prompt.or(self.prompt),
            complete_token: overlay.complete_token.or(self.complete_token),
            binary: overlay.binary,
            max_retries: overlay.max_retries,
            base_backoff_ms: overlay.base_backoff_ms,
            absolute_timeout_ms: overlay.absolute_timeout_ms,
            // For acp, overlay takes precedence
            acp: overlay.acp,
        }
    }
}

/// Merges two BTreeMaps of vars, with `overlay` taking precedence.
///
/// Keys present in `overlay` replace those in `base`; keys only in `base`
/// are preserved.
#[must_use]
fn merge_vars(
    base: &BTreeMap<String, String>,
    overlay: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut result = base.clone();
    for (k, v) in overlay {
        result.insert(k.clone(), v.clone());
    }
    result
}

/// Merges two BTreeMaps of prompts, with `overlay` taking precedence.
///
/// Keys present in `overlay` replace those in `base`; keys only in `base`
/// are preserved.
#[must_use]
fn merge_prompts(
    base: &BTreeMap<String, PromptDef>,
    overlay: &BTreeMap<String, PromptDef>,
) -> BTreeMap<String, PromptDef> {
    let mut result = base.clone();
    for (k, v) in overlay {
        result.insert(k.clone(), v.clone());
    }
    result
}

impl FileConfig {
    /// Merges two configs, with `overlay` taking precedence over `base`.
    ///
    /// For each field:
    /// - `defaults`: overlay values replace base values via `Defaults::merge`
    /// - `vars`: overlay vars extend and replace base vars
    /// - `prompts`: overlay prompts extend and replace base prompts
    /// - `conversation_db`: overlay config takes precedence
    /// - `plan`: overlay config takes precedence
    /// - `notifications`: overlay config takes precedence
    /// - `rules`: overlay config takes precedence
    /// - `automations`: overlay config takes precedence
    #[must_use]
    #[tracing::instrument(level = "debug", ret)]
    pub fn merge(base: Self, overlay: Self) -> Self {
        Self {
            defaults: base.defaults.merge(overlay.defaults),
            vars: merge_vars(&base.vars, &overlay.vars),
            prompts: merge_prompts(&base.prompts, &overlay.prompts),
            conversation_db: overlay.conversation_db,
            plan: overlay.plan,
            notifications: overlay.notifications,
            rules: overlay.rules,
            prompts_config: overlay.prompts_config,
            automations: overlay.automations,
        }
    }

    /// Returns the global config path in the user's home directory.
    ///
    /// Checks for `velor.toml` first, then falls back to `agent-cli.toml` for backward compatibility.
    ///
    /// # Errors
    ///
    /// Returns an error if the home directory cannot be determined.
    #[tracing::instrument(level = "debug", ret)]
    pub fn home_config_path() -> color_eyre::eyre::Result<std::path::PathBuf> {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .wrap_err("failed to determine home directory")?;
        let velor_dir = std::path::PathBuf::from(home).join(".velor");

        let velor_toml = velor_dir.join("velor.toml");
        let agent_cli_toml = velor_dir.join("agent-cli.toml");

        // Prefer velor.toml, but fall back to agent-cli.toml for backward compatibility
        if velor_toml.exists() {
            Ok(velor_toml)
        } else {
            Ok(agent_cli_toml)
        }
    }

    /// Loads configuration from the given path if it exists.
    ///
    /// # Errors
    ///
    /// Returns an error if the file exists but cannot be read or parsed.
    #[tracing::instrument(level = "debug", ret)]
    pub fn load_if_exists(path: &Path) -> color_eyre::eyre::Result<Option<Self>> {
        if !path.exists() {
            return Ok(None);
        }
        let raw = std::fs::read_to_string(path)?;
        let parsed: Self = toml::from_str(&raw)?;
        Ok(Some(parsed))
    }

    /// Returns the default configuration path for a given git root.
    ///
    /// Checks for `velor.toml` first, then falls back to `agent-cli.toml` for backward compatibility.
    #[must_use]
    #[tracing::instrument(level = "trace", ret)]
    pub fn default_config_path(git_root: &Path) -> std::path::PathBuf {
        let velor_toml = git_root.join(".velor").join("velor.toml");
        let agent_cli_toml = git_root.join(".velor").join("agent-cli.toml");

        // Prefer velor.toml, but fall back to agent-cli.toml for backward compatibility
        if velor_toml.exists() {
            velor_toml
        } else {
            agent_cli_toml
        }
    }

    /// Resolves the configured binary to an absolute path.
    ///
    /// For automations, this returns the path to the vel binary itself (using current_exe),
    /// not the Claude binary that vel uses internally.
    ///
    /// This ensures the automations can invoke `vel once` correctly when running under launchd.
    ///
    /// # Errors
    ///
    /// Returns an error if the current executable path cannot be determined.
    #[tracing::instrument(level = "debug", ret, err)]
    pub fn resolve_binary_path(&self) -> color_eyre::eyre::Result<String> {
        // For automations, we need the path to the vel binary itself, not the Claude binary
        // Use current_exe() to get the path to the currently running vel binary
        let current_exe =
            std::env::current_exe().wrap_err("Failed to determine current executable path")?;

        // Convert to absolute path string
        let binary_path = current_exe
            .canonicalize()
            .wrap_err_with(|| format!("Failed to canonicalize path: {}", current_exe.display()))?
            .to_str()
            .ok_or_else(|| color_eyre::eyre::eyre!("Executable path contains invalid UTF-8"))?
            .to_string();

        Ok(binary_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Defaults::merge tests
    #[test]
    fn test_defaults_merge_overlay_takes_precedence() {
        let base = Defaults {
            permission_mode: Some("base_mode".to_string()),
            prd_path: Some("base_prd".to_string()),
            progress_path: Some("base_progress".to_string()),
            iterations: Some(10),
            prompt: Some("base_prompt".to_string()),
            complete_token: Some("base_token".to_string()),
            binary: "base_binary".to_string(),
            max_retries: 3,
            base_backoff_ms: 50,
            absolute_timeout_ms: 3600000, // 1 hour
            ..Default::default()
        };

        let overlay = Defaults {
            permission_mode: Some("overlay_mode".to_string()),
            prd_path: None, // Keep base value
            progress_path: Some("overlay_progress".to_string()),
            iterations: Some(5),
            prompt: None,
            complete_token: None,
            binary: "overlay_binary".to_string(),
            max_retries: 7,
            base_backoff_ms: 200,
            absolute_timeout_ms: 7200000, // 2 hours
            ..Default::default()
        };

        let result = base.clone().merge(overlay);

        assert_eq!(result.permission_mode, Some("overlay_mode".to_string()));
        assert_eq!(result.prd_path, Some("base_prd".to_string())); // base preserved
        assert_eq!(result.progress_path, Some("overlay_progress".to_string()));
        assert_eq!(result.iterations, Some(5));
        assert_eq!(result.prompt, Some("base_prompt".to_string()));
        assert_eq!(result.complete_token, Some("base_token".to_string()));
        assert_eq!(result.binary, "overlay_binary".to_string()); // overlay wins
        assert_eq!(result.max_retries, 7); // overlay wins
        assert_eq!(result.base_backoff_ms, 200); // overlay wins
        assert_eq!(result.absolute_timeout_ms, 7200000); // overlay wins
    }

    #[test]
    fn test_defaults_merge_base_only() {
        let base = Defaults {
            permission_mode: Some("mode".to_string()),
            ..Default::default()
        };

        let result = base.clone().merge(Defaults::default());

        assert_eq!(result.permission_mode, Some("mode".to_string()));
    }

    #[test]
    fn test_defaults_merge_overlay_only() {
        let overlay = Defaults {
            permission_mode: Some("mode".to_string()),
            ..Default::default()
        };

        let result = Defaults::default().merge(overlay);

        assert_eq!(result.permission_mode, Some("mode".to_string()));
    }

    // merge_vars tests
    #[test]
    fn test_merge_vars_overlay_overwrites_base() {
        let mut base = BTreeMap::new();
        base.insert("a".to_string(), "base_a".to_string());
        base.insert("b".to_string(), "base_b".to_string());

        let mut overlay = BTreeMap::new();
        overlay.insert("a".to_string(), "overlay_a".to_string());
        overlay.insert("c".to_string(), "overlay_c".to_string());

        let result = merge_vars(&base, &overlay);

        assert_eq!(result.get("a"), Some(&"overlay_a".to_string()));
        assert_eq!(result.get("b"), Some(&"base_b".to_string()));
        assert_eq!(result.get("c"), Some(&"overlay_c".to_string()));
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn test_merge_vars_empty_base() {
        let base = BTreeMap::new();
        let mut overlay = BTreeMap::new();
        overlay.insert("key".to_string(), "value".to_string());

        let result = merge_vars(&base, &overlay);

        assert_eq!(result.get("key"), Some(&"value".to_string()));
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn test_merge_vars_empty_overlay() {
        let mut base = BTreeMap::new();
        base.insert("key".to_string(), "value".to_string());
        let overlay = BTreeMap::new();

        let result = merge_vars(&base, &overlay);

        assert_eq!(result.get("key"), Some(&"value".to_string()));
        assert_eq!(result.len(), 1);
    }

    // merge_prompts tests
    #[test]
    fn test_merge_prompts_overlay_overwrites_base() {
        let mut base = BTreeMap::new();
        base.insert(
            "prompt1".to_string(),
            PromptDef::Inline("base_template".to_string()),
        );

        let mut overlay = BTreeMap::new();
        overlay.insert(
            "prompt1".to_string(),
            PromptDef::Table {
                template: "overlay_template".to_string(),
                complete_token: Some("token".to_string()),
            },
        );
        overlay.insert(
            "prompt2".to_string(),
            PromptDef::Inline("new_template".to_string()),
        );

        let result = merge_prompts(&base, &overlay);

        assert_eq!(
            result
                .get("prompt1")
                .expect("prompt1 should exist")
                .template(),
            "overlay_template"
        );
        assert_eq!(
            result
                .get("prompt2")
                .expect("prompt2 should exist")
                .template(),
            "new_template"
        );
        assert_eq!(result.len(), 2);
    }

    // FileConfig::merge tests
    #[test]
    fn test_file_config_merge_full() {
        let base = FileConfig {
            defaults: Defaults {
                permission_mode: Some("acceptEdits".to_string()),
                iterations: Some(10),
                ..Default::default()
            },
            vars: {
                let mut vars = BTreeMap::new();
                vars.insert("base_var".to_string(), "base_value".to_string());
                vars.insert("shared_var".to_string(), "base_shared".to_string());
                vars
            },
            prompts: {
                let mut prompts = BTreeMap::new();
                prompts.insert(
                    "base_prompt".to_string(),
                    PromptDef::Inline("base template".to_string()),
                );
                prompts
            },
            conversation_db: ConversationDbConfig {
                path: ".velor/base.db".to_string(),
                encrypt_content: false,
                encryption_key_env: "BASE_KEY".to_string(),
                retention_days: 30,
            },
            plan: PlanConfig {
                specs_dir: "base-specs".to_string(),
                plan_max_iterations: 5,
                ..Default::default()
            },
            notifications: NotificationsConfig::default(),
            rules: RulesConfig::default(),
            prompts_config: PromptsConfig::default(),
            automations: AutomationsConfig::default(),
        };

        let overlay = FileConfig {
            defaults: Defaults {
                iterations: Some(5),
                ..Default::default()
            },
            vars: {
                let mut vars = BTreeMap::new();
                vars.insert("shared_var".to_string(), "overlay_shared".to_string());
                vars.insert("overlay_var".to_string(), "overlay_value".to_string());
                vars
            },
            prompts: {
                let mut prompts = BTreeMap::new();
                prompts.insert(
                    "overlay_prompt".to_string(),
                    PromptDef::Inline("overlay template".to_string()),
                );
                prompts
            },
            conversation_db: ConversationDbConfig {
                path: ".velor/overlay.db".to_string(),
                ..Default::default()
            },
            plan: PlanConfig {
                openai_model: "gpt-4o-mini".to_string(),
                ..Default::default()
            },
            notifications: NotificationsConfig {
                enabled: true,
                ..Default::default()
            },
            rules: RulesConfig::default(),
            prompts_config: PromptsConfig::default(),
            automations: AutomationsConfig::default(),
        };

        let result = FileConfig::merge(base, overlay);

        // Check defaults merge
        assert_eq!(
            result.defaults.permission_mode,
            Some("acceptEdits".to_string())
        );
        assert_eq!(result.defaults.iterations, Some(5));

        // Check vars merge
        assert_eq!(result.vars.get("base_var"), Some(&"base_value".to_string()));
        assert_eq!(
            result.vars.get("shared_var"),
            Some(&"overlay_shared".to_string())
        );
        assert_eq!(
            result.vars.get("overlay_var"),
            Some(&"overlay_value".to_string())
        );

        // Check prompts merge
        assert_eq!(
            result
                .prompts
                .get("base_prompt")
                .expect("base_prompt should exist")
                .template(),
            "base template"
        );
        assert_eq!(
            result
                .prompts
                .get("overlay_prompt")
                .expect("overlay_prompt should exist")
                .template(),
            "overlay template"
        );

        // Check conversation_db overlay wins
        assert_eq!(result.conversation_db.path, ".velor/overlay.db".to_string());

        // Check plan overlay wins
        assert_eq!(result.plan.openai_model, "gpt-4o-mini".to_string());
    }

    // home_config_path test
    #[test]
    fn test_home_config_path() {
        let path = FileConfig::home_config_path();
        assert!(path.is_ok(), "home_config_path should return Ok");
        let path = path.expect("home_config_path should return Ok");
        // The function returns velor.toml if it exists, otherwise agent-cli.toml for backward compatibility
        assert!(
            path.ends_with(".velor/velor.toml")
                || path.ends_with(".velor\\velor.toml")
                || path.ends_with(".velor/agent-cli.toml")
                || path.ends_with(".velor\\agent-cli.toml"),
            "path should end with .velor/velor.toml or .velor/agent-cli.toml, got: {}",
            path.display()
        );
    }

    // Integration test with tempfile
    #[test]
    fn test_load_and_merge_configs() {
        let temp_dir = tempfile::TempDir::new().expect("tempdir should be created");
        let home_dir = temp_dir.path().join("home");
        let repo_dir = temp_dir.path().join("repo");
        std::fs::create_dir_all(&home_dir).expect("home_dir should be created");
        std::fs::create_dir_all(&repo_dir).expect("repo_dir should be created");

        let home_config = home_dir.join(".velor").join("velor.toml");
        let repo_config = repo_dir.join(".velor").join("velor.toml");
        std::fs::create_dir_all(
            home_config
                .parent()
                .expect("home_config should have parent"),
        )
        .expect("home_config parent dir should be created");
        std::fs::create_dir_all(
            repo_config
                .parent()
                .expect("repo_config should have parent"),
        )
        .expect("repo_config parent dir should be created");

        // Write home config
        std::fs::write(
            &home_config,
            r#"
        [vars]
        home_var = "from_home"
        shared = "home_shared"

        [defaults]
        permission_mode = "acceptEdits"
        iterations = 10
    "#,
        )
        .expect("home config should be written");

        // Write repo config
        std::fs::write(
            &repo_config,
            r#"
        [vars]
        repo_var = "from_repo"
        shared = "repo_shared"

        [defaults]
        iterations = 5
    "#,
        )
        .expect("repo config should be written");

        // Load and merge
        let home_cfg = FileConfig::load_if_exists(&home_config)
            .expect("home config should load")
            .expect("home config should exist");
        let repo_cfg = FileConfig::load_if_exists(&repo_config)
            .expect("repo config should load")
            .expect("repo config should exist");
        let merged = FileConfig::merge(home_cfg, repo_cfg);

        // Verify merge results
        assert_eq!(merged.vars.get("home_var"), Some(&"from_home".to_string()));
        assert_eq!(merged.vars.get("repo_var"), Some(&"from_repo".to_string()));
        assert_eq!(merged.vars.get("shared"), Some(&"repo_shared".to_string()));
        assert_eq!(
            merged.defaults.permission_mode,
            Some("acceptEdits".to_string())
        );
        assert_eq!(merged.defaults.iterations, Some(5));
    }

    // ConversationDbConfig tests
    #[test]
    fn test_conversation_db_config_default() {
        let config = ConversationDbConfig::default();
        assert_eq!(config.path, ".velor/conversations.db");
        assert!(!config.encrypt_content);
        assert_eq!(config.encryption_key_env, "VELOR_CONVERSATIONS_KEY");
        assert_eq!(config.retention_days, 0);
    }

    #[test]
    fn test_conversation_db_config_full() {
        let config = ConversationDbConfig {
            path: ".velor/custom.db".to_string(),
            encrypt_content: true,
            encryption_key_env: "CUSTOM_KEY".to_string(),
            retention_days: 90,
        };
        assert_eq!(config.path, ".velor/custom.db");
        assert!(config.encrypt_content);
        assert_eq!(config.encryption_key_env, "CUSTOM_KEY");
        assert_eq!(config.retention_days, 90);
    }

    // PlanConfig tests
    #[test]
    fn test_plan_config_default() {
        let config = PlanConfig::default();
        assert_eq!(config.specs_dir, "specs");
        assert_eq!(config.plan_max_iterations, 10);
        assert_eq!(config.openai_api_key_env, "OPENAI_API_KEY");
        assert_eq!(config.openai_model, "gpt-4o");
        assert!(config.openai_base_url.is_none());
    }

    #[test]
    fn test_plan_config_custom() {
        let config = PlanConfig {
            specs_dir: "custom-specs".to_string(),
            plan_max_iterations: 20,
            openai_api_key_env: "CUSTOM_API_KEY".to_string(),
            openai_model: "gpt-4o-mini".to_string(),
            openai_base_url: Some("https://api.example.com".to_string()),
        };
        assert_eq!(config.specs_dir, "custom-specs");
        assert_eq!(config.plan_max_iterations, 20);
        assert_eq!(config.openai_api_key_env, "CUSTOM_API_KEY");
        assert_eq!(config.openai_model, "gpt-4o-mini");
        assert_eq!(
            config.openai_base_url,
            Some("https://api.example.com".to_string())
        );
    }

    // Test loading TOML with new sections
    #[test]
    fn test_load_full_config_with_new_sections() {
        let temp_dir = tempfile::TempDir::new().expect("tempdir should be created");
        let config_path = temp_dir.path().join("test.toml");

        std::fs::write(
            &config_path,
            r#"
[conversation_db]
path = ".velor/test.db"
encrypt_content = true
encryption_key_env = "TEST_KEY"
retention_days = 60

[plan]
specs_dir = "test-specs"
plan_max_iterations = 15
openai_api_key_env = "TEST_OPENAI_KEY"
openai_model = "gpt-4o-mini"
openai_base_url = "https://test.example.com"

[defaults]
iterations = 5

[vars]
test_var = "test_value"
"#,
        )
        .expect("config should be written");

        let config = FileConfig::load_if_exists(&config_path)
            .expect("config should load")
            .expect("config should exist");

        // Verify conversation_db section
        assert_eq!(config.conversation_db.path, ".velor/test.db");
        assert!(config.conversation_db.encrypt_content);
        assert_eq!(config.conversation_db.encryption_key_env, "TEST_KEY");
        assert_eq!(config.conversation_db.retention_days, 60);

        // Verify plan section
        assert_eq!(config.plan.specs_dir, "test-specs");
        assert_eq!(config.plan.plan_max_iterations, 15);
        assert_eq!(config.plan.openai_api_key_env, "TEST_OPENAI_KEY");
        assert_eq!(config.plan.openai_model, "gpt-4o-mini");
        assert_eq!(
            config.plan.openai_base_url,
            Some("https://test.example.com".to_string())
        );

        // Verify other sections still work
        assert_eq!(config.defaults.iterations, Some(5));
        assert_eq!(config.vars.get("test_var"), Some(&"test_value".to_string()));
    }

    // Test merging configs with new sections
    #[test]
    fn test_merge_new_config_sections() {
        let base = FileConfig {
            conversation_db: ConversationDbConfig {
                path: ".velor/base.db".to_string(),
                ..Default::default()
            },
            plan: PlanConfig {
                specs_dir: "base-specs".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };

        let overlay = FileConfig {
            conversation_db: ConversationDbConfig {
                path: ".velor/overlay.db".to_string(),
                encrypt_content: true,
                ..Default::default()
            },
            plan: PlanConfig {
                plan_max_iterations: 20,
                ..Default::default()
            },
            ..Default::default()
        };

        let result = FileConfig::merge(base, overlay);

        // Overlay should win for conversation_db
        assert_eq!(result.conversation_db.path, ".velor/overlay.db");
        assert!(result.conversation_db.encrypt_content);

        // Overlay should win for plan
        assert_eq!(result.plan.plan_max_iterations, 20);
        // Base values should be lost (overlay wins completely)
        assert_eq!(result.plan.specs_dir, "specs"); // default
    }
}

#[cfg(test)]
mod proptest_tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
            fn test_merge_vars_idempotent(
                    base_vars in prop::collection::btree_map(".*", ".*", 0..10),
                    overlay_vars in prop::collection::btree_map(".*", ".*", 0..10)
            ) {
                    let merged1 = merge_vars(&base_vars, &overlay_vars);
                    let merged2 = merge_vars(&merged1, &BTreeMap::new());

                    prop_assert_eq!(merged1, merged2);
            }

            fn test_merge_vars_overlay_wins(
                    base_vars in prop::collection::btree_map(".*", ".*", 0..10),
                    mut overlay_vars in prop::collection::btree_map(".*", ".*", 0..10)
            ) {
                    // Ensure at least one common key
                    if !base_vars.is_empty() && !overlay_vars.is_empty() {
                            let common_key = base_vars
                                    .keys()
                                    .next()
                                    .expect("base_vars should have at least one key")
                                    .clone();
                            overlay_vars.insert(common_key, "overlay_value".to_string());
                    }

                    let merged = merge_vars(&base_vars, &overlay_vars);

                    // All overlay keys should be in result
                    for (k, v) in &overlay_vars {
                            prop_assert_eq!(merged.get(k), Some(v));
                    }
                    // Base-only keys should be preserved
                    for (k, v) in &base_vars {
                            if !overlay_vars.contains_key(k) {
                                    prop_assert_eq!(merged.get(k), Some(v));
                            }
                    }
            }

            fn test_defaults_merge_preserves_somes(
                    base_mode in prop::option::of(".*"),
                    overlay_mode in prop::option::of(".*")
            ) {
                    let base = Defaults {
                            permission_mode: base_mode.clone(),
                            ..Default::default()
                    };
                    let overlay = Defaults {
                            permission_mode: overlay_mode.clone(),
                            ..Default::default()
                    };

                    let result = base.merge(overlay);

                    // Overlay Some should win, base Some should be used if overlay is None
                    prop_assert_eq!(result.permission_mode, overlay_mode.or(base_mode));
            }

            fn test_conversation_db_config_roundtrip(
                    path in "[a-zA-Z0-9_/\\.-]+",
                    encrypt_content in prop::bool::ANY,
                    encryption_key_env in "[A-Z_]+",
                    retention_days in 0u32..3650,
            ) {
                    let config = ConversationDbConfig {
                            path: path.clone(),
                            encrypt_content,
                            encryption_key_env: encryption_key_env.clone(),
                            retention_days,
                    };

                    // Verify fields match
                    prop_assert_eq!(config.path, path);
                    prop_assert_eq!(config.encrypt_content, encrypt_content);
                    prop_assert_eq!(config.encryption_key_env, encryption_key_env);
                    prop_assert_eq!(config.retention_days, retention_days);
            }

            fn test_plan_config_roundtrip(
                    specs_dir in "[a-zA-Z0-9_/\\-]+",
                    plan_max_iterations in 1u32..100,
                    openai_api_key_env in "[A-Z_]+",
                    openai_model in "[a-z0-9-\\.]+",
            ) {
                    let config = PlanConfig {
                            specs_dir: specs_dir.clone(),
                            plan_max_iterations,
                            openai_api_key_env: openai_api_key_env.clone(),
                            openai_model: openai_model.clone(),
                            openai_base_url: None, // Keep it simple for prop test
                    };

                    // Verify fields match
                    prop_assert_eq!(config.specs_dir, specs_dir);
                    prop_assert_eq!(config.plan_max_iterations, plan_max_iterations);
                    prop_assert_eq!(config.openai_api_key_env, openai_api_key_env);
                    prop_assert_eq!(config.openai_model, openai_model);
                    prop_assert!(config.openai_base_url.is_none());
            }

            fn test_file_config_merge_preserves_defaults_merge_semantics(
                    base_iterations in prop::option::of(1u32..100),
                    overlay_iterations in prop::option::of(1u32..100),
            ) {
                    let base = FileConfig {
                            defaults: Defaults {
                                    iterations: base_iterations,
                                    ..Default::default()
                            },
                            ..Default::default()
                    };

                    let overlay = FileConfig {
                            defaults: Defaults {
                                    iterations: overlay_iterations,
                                    ..Default::default()
                            },
                            ..Default::default()
                    };

                    let result = FileConfig::merge(base, overlay);

                    // Should follow Defaults::merge semantics
                    prop_assert_eq!(
                            result.defaults.iterations,
                            overlay_iterations.or(base_iterations)
                    );
            }
    }

    // Unit tests that don't require property testing
    #[test]
    fn test_conversation_db_config_default_is_valid() {
        let config = ConversationDbConfig::default();
        // Verify all defaults are sensible
        assert!(!config.path.is_empty());
        // retention_days is u64, so always >= 0
    }

    #[test]
    fn test_plan_config_default_is_valid() {
        let config = PlanConfig::default();
        // Verify all defaults are sensible
        assert!(!config.specs_dir.is_empty());
        assert!(config.plan_max_iterations > 0);
        assert!(!config.openai_api_key_env.is_empty());
        assert!(!config.openai_model.is_empty());
    }

    // NotificationsConfig tests
    #[test]
    fn test_notifications_config_default() {
        let config = NotificationsConfig::default();
        assert!(!config.enabled);
        assert!(config.notify_on_success);
        assert!(config.notify_on_max_iterations);
        assert!(config.notify_on_failure);
        assert_eq!(config.output_preview_chars, 500);
        assert!(config.telegram.is_none());
        assert!(config.macos.is_none());
    }

    #[test]
    fn test_macos_config_default() {
        let config = MacOSConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.sound, Some("default".to_string()));
    }

    #[test]
    fn test_telegram_config_default() {
        let config = TelegramConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.bot_token_env, "TELEGRAM_BOT_TOKEN");
        assert!(config.chat_id.is_empty());
        assert!(config.api_base_url.is_none());
        assert_eq!(config.parse_mode, Some(TelegramParseMode::MarkdownV2));
    }

    #[test]
    fn test_telegram_parse_mode_default() {
        let mode = TelegramParseMode::default();
        assert_eq!(mode, TelegramParseMode::MarkdownV2);
    }

    #[test]
    fn test_notifications_config_custom() {
        let config = NotificationsConfig {
            enabled: true,
            notify_on_success: false,
            notify_on_max_iterations: true,
            notify_on_failure: true,
            output_preview_chars: 1000,
            telegram: Some(TelegramConfig {
                enabled: true,
                bot_token_env: "CUSTOM_TOKEN".to_string(),
                chat_id: "-1001234567890".to_string(),
                api_base_url: Some("https://proxy.example.com".to_string()),
                parse_mode: Some(TelegramParseMode::Html),
            }),
            macos: Some(MacOSConfig {
                enabled: true,
                sound: Some("Sosumi".to_string()),
            }),
        };

        assert!(config.enabled);
        assert!(!config.notify_on_success);
        assert!(config.notify_on_max_iterations);
        assert!(config.notify_on_failure);
        assert_eq!(config.output_preview_chars, 1000);

        let tg = config.telegram.as_ref().expect("telegram should be set");
        assert!(tg.enabled);
        assert_eq!(tg.bot_token_env, "CUSTOM_TOKEN");
        assert_eq!(tg.chat_id, "-1001234567890");
        assert_eq!(
            tg.api_base_url,
            Some("https://proxy.example.com".to_string())
        );
        assert_eq!(tg.parse_mode, Some(TelegramParseMode::Html));

        let macos = config.macos.as_ref().expect("macos should be set");
        assert!(macos.enabled);
        assert_eq!(macos.sound, Some("Sosumi".to_string()));
    }

    // Test loading TOML with notifications section
    #[test]
    fn test_load_config_with_notifications() {
        let temp_dir = tempfile::TempDir::new().expect("tempdir should be created");
        let config_path = temp_dir.path().join("test.toml");

        std::fs::write(
            &config_path,
            r#"
[notifications]
enabled = true
notify_on_success = true
notify_on_max_iterations = true
notify_on_failure = true
output_preview_chars = 750

[notifications.telegram]
enabled = true
bot_token_env = "MY_BOT_TOKEN"
chat_id = "-1001234567890"
parse_mode = "MarkdownV2"
"#,
        )
        .expect("config should be written");

        let config = FileConfig::load_if_exists(&config_path)
            .expect("config should load")
            .expect("config should exist");

        assert!(config.notifications.enabled);
        assert_eq!(config.notifications.output_preview_chars, 750);

        let tg = config
            .notifications
            .telegram
            .as_ref()
            .expect("telegram should be set");
        assert!(tg.enabled);
        assert_eq!(tg.bot_token_env, "MY_BOT_TOKEN");
        assert_eq!(tg.chat_id, "-1001234567890");
        assert_eq!(tg.parse_mode, Some(TelegramParseMode::MarkdownV2));
    }

    #[test]
    fn test_notifications_merge_overlay_wins() {
        let base = FileConfig {
            notifications: NotificationsConfig {
                enabled: false,
                output_preview_chars: 500,
                ..Default::default()
            },
            ..Default::default()
        };

        let overlay = FileConfig {
            notifications: NotificationsConfig {
                enabled: true,
                output_preview_chars: 1000,
                ..Default::default()
            },
            ..Default::default()
        };

        let result = FileConfig::merge(base, overlay);

        // Overlay should win
        assert!(result.notifications.enabled);
        assert_eq!(result.notifications.output_preview_chars, 1000);
    }

    // Test loading TOML with notifications including macOS section
    #[test]
    fn test_load_config_with_notifications_and_macos() {
        let temp_dir = tempfile::TempDir::new().expect("tempdir should be created");
        let config_path = temp_dir.path().join("test.toml");

        std::fs::write(
            &config_path,
            r#"
[notifications]
enabled = true
notify_on_success = true
notify_on_max_iterations = true
notify_on_failure = true
output_preview_chars = 500

[notifications.telegram]
enabled = true
bot_token_env = "MY_BOT_TOKEN"
chat_id = "-1001234567890"
parse_mode = "MarkdownV2"

[notifications.macos]
enabled = true
sound = "Sosumi"
"#,
        )
        .expect("config should be written");

        let config = FileConfig::load_if_exists(&config_path)
            .expect("config should load")
            .expect("config should exist");

        assert!(config.notifications.enabled);

        let tg = config
            .notifications
            .telegram
            .as_ref()
            .expect("telegram should be set");
        assert!(tg.enabled);
        assert_eq!(tg.bot_token_env, "MY_BOT_TOKEN");

        let macos = config
            .notifications
            .macos
            .as_ref()
            .expect("macos should be set");
        assert!(macos.enabled);
        assert_eq!(macos.sound, Some("Sosumi".to_string()));
    }

    // ACP configuration tests
    #[test]
    fn test_protocol_default() {
        let protocol = Protocol::default();
        assert_eq!(protocol, Protocol::Subprocess);
    }

    #[test]
    fn test_permission_mode_default() {
        let mode = PermissionMode::default();
        assert_eq!(mode, PermissionMode::Allow);
    }

    #[test]
    fn test_acp_config_default() {
        let config = AcpConfig::default();
        assert_eq!(config.api_key_env, "ANTHROPIC_API_KEY");
        assert_eq!(config.permission_mode, PermissionMode::Allow);
        assert!(config.persist_adapter);
    }

    #[test]
    fn test_acp_config_custom() {
        let config = AcpConfig {
            api_key_env: "CUSTOM_API_KEY".to_string(),
            permission_mode: PermissionMode::Deny,
            persist_adapter: false,
        };
        assert_eq!(config.api_key_env, "CUSTOM_API_KEY");
        assert_eq!(config.permission_mode, PermissionMode::Deny);
        assert!(!config.persist_adapter);
    }

    #[test]
    fn test_protocol_subprocess_variant() {
        // Test that Subprocess variant exists and equals itself
        assert_eq!(Protocol::Subprocess, Protocol::Subprocess);
        assert_ne!(Protocol::Subprocess, Protocol::Acp);
    }

    #[test]
    fn test_protocol_acp_variant() {
        // Test that Acp variant exists and equals itself
        assert_eq!(Protocol::Acp, Protocol::Acp);
        assert_ne!(Protocol::Acp, Protocol::Subprocess);
    }

    #[test]
    fn test_permission_mode_allow_variant() {
        // Test that Allow variant exists and equals itself
        assert_eq!(PermissionMode::Allow, PermissionMode::Allow);
        assert_ne!(PermissionMode::Allow, PermissionMode::Deny);
    }

    #[test]
    fn test_permission_mode_deny_variant() {
        // Test that Deny variant exists and equals itself
        assert_eq!(PermissionMode::Deny, PermissionMode::Deny);
        assert_ne!(PermissionMode::Deny, PermissionMode::Allow);
    }

    #[test]
    fn test_load_config_with_acp_settings() {
        let temp_dir = tempfile::TempDir::new().expect("tempdir should be created");
        let config_path = temp_dir.path().join("test.toml");

        std::fs::write(
            &config_path,
            r#"
[defaults]
protocol = "acp"
binary = "claude-agent-acp"

[defaults.acp]
api_key_env = "CUSTOM_KEY"
permission_mode = "deny"
persist_adapter = false
"#,
        )
        .expect("config should be written");

        let config = FileConfig::load_if_exists(&config_path)
            .expect("config should load")
            .expect("config should exist");

        assert_eq!(config.defaults.protocol, Protocol::Acp);
        assert_eq!(config.defaults.binary, "claude-agent-acp");
        assert_eq!(config.defaults.acp.api_key_env, "CUSTOM_KEY");
        assert_eq!(config.defaults.acp.permission_mode, PermissionMode::Deny);
        assert!(!config.defaults.acp.persist_adapter);
    }

    #[test]
    fn test_defaults_merge_protocol_acp_wins() {
        let base = Defaults {
            protocol: Protocol::Subprocess,
            ..Default::default()
        };

        let overlay = Defaults {
            protocol: Protocol::Acp,
            ..Default::default()
        };

        let result = base.clone().merge(overlay);
        assert_eq!(result.protocol, Protocol::Acp);
    }

    #[test]
    fn test_defaults_merge_acp_config_overlay_wins() {
        let base = Defaults {
            acp: AcpConfig {
                api_key_env: "BASE_KEY".to_string(),
                permission_mode: PermissionMode::Allow,
                persist_adapter: true,
            },
            ..Default::default()
        };

        let overlay = Defaults {
            acp: AcpConfig {
                api_key_env: "OVERLAY_KEY".to_string(),
                permission_mode: PermissionMode::Deny,
                persist_adapter: false,
            },
            ..Default::default()
        };

        let result = base.clone().merge(overlay);
        assert_eq!(result.acp.api_key_env, "OVERLAY_KEY");
        assert_eq!(result.acp.permission_mode, PermissionMode::Deny);
        assert!(!result.acp.persist_adapter);
    }
}
