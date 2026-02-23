//! Automation configuration.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Configuration for the automations feature.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct AutomationsConfig {
    /// Directory for automation definitions (relative to git root).
    pub automations_dir: String,

    /// Path to the state database for tracking runs.
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
            state_db_path: ".velor/automations.db".to_string(),
            max_concurrent: 3,
            default_timezone: "UTC".to_string(),
            default_timeout_seconds: 3600, // 1 hour
            max_output_bytes: 100_000,     // 100 KB
        }
    }
}

/// Catch-up policy for missed runs.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CatchUpPolicy {
    /// Skip all missed runs, run only once on next tick.
    Skip,
    /// Run once regardless of how many were missed.
    RunOnce,
    /// Run all missed schedules (may be dangerous!).
    RunAll,
}

impl Default for CatchUpPolicy {
    fn default() -> Self {
        Self::Skip
    }
}

/// An automation definition loaded from TOML.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Automation {
    /// Unique name of the automation.
    pub name: String,

    /// Human-readable description.
    pub description: String,

    /// Cron schedule expression (6-field: seconds minutes hours day month weekday).
    pub schedule: String,

    /// Timezone for the schedule (IANA tz database name, e.g. "America/New_York").
    /// Defaults to config default_timezone.
    #[serde(default)]
    pub timezone: String,

    /// Prompt template name or inline content.
    pub prompt: String,

    /// Whether this automation is enabled.
    #[serde(default = "default_enabled")]
    pub enabled: bool,

    /// Variables to pass to the prompt.
    #[serde(default)]
    pub vars: BTreeMap<String, String>,

    /// Policy for handling missed runs.
    #[serde(default)]
    pub catch_up: CatchUpPolicy,

    /// Maximum number of catch-up runs to execute.
    #[serde(default)]
    pub max_catch_up: u32,

    /// Timeout for this automation (seconds).
    /// Defaults to config default_timeout_seconds.
    #[serde(default)]
    pub timeout_seconds: Option<u64>,

    /// Send notification on success.
    #[serde(default = "default_true")]
    pub notify_on_success: bool,

    /// Send notification on failure.
    #[serde(default = "default_true")]
    pub notify_on_failure: bool,
}

fn default_enabled() -> bool {
    true
}

fn default_true() -> bool {
    true
}

/// Load all automations from a directory.
pub async fn load_automations(
    dir: impl AsRef<std::path::Path>,
) -> color_eyre::Result<Vec<Automation>> {
    let dir = dir.as_ref();
    let mut automations = Vec::new();

    if !dir.exists() {
        tokio::fs::create_dir_all(dir).await?;
        return Ok(automations);
    }

    let mut entries = tokio::fs::read_dir(dir).await?;
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("toml") {
            let content = tokio::fs::read_to_string(&path).await?;

            // Validate cron expression format (6 fields expected)
            let automation: Automation = toml::from_str(&content)?;

            // Validate cron has 6 fields
            let parts: Vec<&str> = automation.schedule.split_whitespace().collect();
            if parts.len() != 6 {
                return Err(color_eyre::eyre::eyre!(
                    "Invalid cron expression '{}': expected 6 fields (seconds minutes hours day month weekday), got {}",
                    automation.schedule,
                    parts.len()
                ));
            }

            // Validate timezone
            if !automation.timezone.is_empty() {
                // chrono_tz::Tz doesn't have from_str_insensitive in 0.10
                // Use from_str which requires case-sensitive match
                automation.timezone.parse::<chrono_tz::Tz>().map_err(|_| {
                    color_eyre::eyre::eyre!("Invalid timezone: {}", automation.timezone)
                })?;
            }

            automations.push(automation);
        }
    }

    Ok(automations)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_automations_config_default() {
        let config = AutomationsConfig::default();
        assert_eq!(config.automations_dir, ".velor/automations.d");
        assert_eq!(config.state_db_path, ".velor/automations.db");
        assert_eq!(config.max_concurrent, 3);
        assert_eq!(config.default_timezone, "UTC");
        assert_eq!(config.default_timeout_seconds, 3600);
        assert_eq!(config.max_output_bytes, 100_000);
    }

    #[test]
    fn test_catch_up_policy_default() {
        let policy = CatchUpPolicy::default();
        assert_eq!(policy, CatchUpPolicy::Skip);
    }

    #[test]
    fn test_catch_up_policy_variants() {
        // Test that all variants exist and can be created
        let skip = CatchUpPolicy::Skip;
        let run_once = CatchUpPolicy::RunOnce;
        let run_all = CatchUpPolicy::RunAll;

        assert_ne!(skip, run_once);
        assert_ne!(skip, run_all);
        assert_ne!(run_once, run_all);
    }

    #[test]
    fn test_load_automations_empty_directory() {
        let temp_dir = TempDir::new().expect("tempdir should be created");
        let automations_dir = temp_dir.path().join("automations");

        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let result = load_automations(&automations_dir).await;
            assert!(result.is_ok());
            assert!(result.unwrap().is_empty());
        });
    }

    #[test]
    fn test_load_automations_valid() {
        let temp_dir = TempDir::new().expect("tempdir should be created");
        let automation_path = temp_dir.path().join("test.toml");

        let content = r#"
name = "test"
description = "Test automation"
schedule = "0 0 * * * *"
timezone = "UTC"
prompt = "once"
enabled = true

[vars]
key = "value"
"#;

        std::fs::write(&automation_path, content).expect("automation file should be written");

        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let result = load_automations(temp_dir.path()).await;
            assert!(result.is_ok());
            let automations = result.unwrap();
            assert_eq!(automations.len(), 1);
            assert_eq!(automations[0].name, "test");
            assert_eq!(automations[0].enabled, true);
        });
    }

    #[test]
    fn test_load_automations_invalid_cron_fields() {
        let temp_dir = TempDir::new().expect("tempdir should be created");
        let automation_path = temp_dir.path().join("invalid.toml");

        // Only 5 fields instead of 6
        let content = r#"
name = "invalid"
description = "Invalid automation"
schedule = "0 * * * *"
prompt = "once"
enabled = true
"#;

        std::fs::write(&automation_path, content).expect("automation file should be written");

        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let result = load_automations(temp_dir.path()).await;
            assert!(result.is_err());
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("expected 6 fields")
            );
        });
    }

    #[test]
    fn test_load_automations_invalid_timezone() {
        let temp_dir = TempDir::new().expect("tempdir should be created");
        let automation_path = temp_dir.path().join("invalid_tz.toml");

        let content = r#"
name = "invalid_tz"
description = "Invalid timezone automation"
schedule = "0 0 * * * *"
timezone = "Invalid/Timezone"
prompt = "once"
enabled = true
"#;

        std::fs::write(&automation_path, content).expect("automation file should be written");

        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let result = load_automations(temp_dir.path()).await;
            assert!(result.is_err());
            assert!(result.unwrap_err().to_string().contains("Invalid timezone"));
        });
    }
}
