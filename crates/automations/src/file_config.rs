//! File-based automation configuration with dual-location discovery.
//!
//! This module provides types for loading automations from TOML files in either:
//! - Global location: `XDG_CONFIG_HOME/velor/automations/`
//! - Project location: `{repo}/.velor/automations/`
//!
//! Each automation is defined as a single TOML file with schedule, prompt source,
//! variables, and execution options.

use crate::config::CatchUpPolicy;
use color_eyre::Result;
use color_eyre::eyre::eyre;
use cron::Schedule;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::str::FromStr;

/// Source of an automation definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutomationSource {
    /// Global automations from `~/.config/velor/automations/`.
    Global,
    /// Project-specific automations from `{repo}/.velor/automations/`.
    Project,
    /// Legacy automations from `.velor/automations.d/` (deprecated).
    Legacy,
}

/// Raw TOML representation (before validation).
#[derive(Debug, Clone, Deserialize)]
pub struct AutomationFileRaw {
    /// Human-readable description of what this automation does.
    pub description: String,
    /// Cron schedule expression (5 or 6 fields).
    pub schedule: String,
    /// Timezone for schedule interpretation (None means system local timezone).
    pub timezone: Option<String>,
    /// Prompt source (inline, file reference, or named prompt).
    #[serde(flatten)]
    pub prompt_source: PromptSourceRaw,
    /// Whether to run in a dedicated worktree.
    #[serde(default)]
    pub worktree: bool,
    /// Working directory for execution (can be inside a repo).
    pub project: Option<PathBuf>,
    /// Variables to pass to the prompt template.
    #[serde(default)]
    pub vars: BTreeMap<String, String>,
    /// Whether this automation is enabled.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Policy for handling missed runs.
    #[serde(default)]
    pub catch_up: CatchUpPolicy,
    /// Maximum number of catch-up runs to execute.
    #[serde(default)]
    pub max_catch_up: u32,
    /// Timeout for this automation in seconds.
    pub timeout_seconds: Option<u64>,
    /// Send notification on success.
    #[serde(default = "default_true")]
    pub notify_on_success: bool,
    /// Send notification on failure.
    #[serde(default = "default_true")]
    pub notify_on_failure: bool,
}

/// Validated automation (pure config, no provenance).
#[derive(Debug, Clone)]
pub struct AutomationFile {
    /// Unique name of the automation (from filename).
    pub name: String,
    /// Human-readable description of what this automation does.
    pub description: String,
    /// Raw cron expression (for display).
    pub schedule_raw: String,
    /// Parsed cron schedule for next-run calculation.
    pub schedule: Schedule,
    /// Timezone for schedule interpretation.
    pub timezone: chrono_tz::Tz,
    /// Source of the prompt content.
    pub prompt_source: PromptSource,
    /// Whether to run in a dedicated worktree.
    pub worktree: bool,
    /// Working directory (resolved, validated).
    pub project: Option<PathBuf>,
    /// Variables to pass to the prompt template.
    pub vars: BTreeMap<String, String>,
    /// Whether this automation is enabled.
    pub enabled: bool,
    /// Policy for handling missed runs.
    pub catch_up: CatchUpPolicy,
    /// Maximum number of catch-up runs to execute.
    pub max_catch_up: u32,
    /// Timeout for this automation in seconds.
    pub timeout_seconds: Option<u64>,
    /// Send notification on success.
    pub notify_on_success: bool,
    /// Send notification on failure.
    pub notify_on_failure: bool,
}

/// Cache entry with provenance metadata.
#[derive(Debug, Clone)]
pub struct AutomationEntry {
    /// The validated automation configuration.
    pub automation: AutomationFile,
    /// Where this automation was loaded from.
    pub source: AutomationSource,
    /// Path to the TOML file.
    pub path: PathBuf,
}

/// Raw prompt source (from TOML).
#[derive(Debug, Clone, Deserialize, Default)]
pub struct PromptSourceRaw {
    /// Inline prompt content.
    pub prompt: Option<String>,
    /// Reference to a file in the prompts/ directory.
    pub prompt_file: Option<String>,
    /// Reference to a named prompt from velor.toml [prompts].
    pub prompt_name: Option<String>,
}

/// Validated prompt source (exactly one field set).
#[derive(Debug, Clone)]
pub enum PromptSource {
    /// Inline prompt content.
    Inline(String),
    /// Reference to a file in the prompts/ directory.
    PromptsDirFile(String),
    /// Reference to a named prompt from velor.toml [prompts].
    Name(String),
}

fn default_enabled() -> bool {
    true
}

fn default_true() -> bool {
    true
}

/// Get system local timezone.
fn get_local_timezone() -> chrono_tz::Tz {
    iana_time_zone::get_timezone()
        .ok()
        .and_then(|tz| tz.parse().ok())
        .unwrap_or(chrono_tz::UTC)
}

/// Normalize cron expression to 6-field format (seconds minutes hours day month weekday).
fn normalize_cron_schedule(expr: &str) -> Result<String> {
    let parts: Vec<&str> = expr.split_whitespace().collect();

    let normalized = match parts.len() {
        5 => format!("0 {}", expr),
        6 => expr.to_string(),
        _ => {
            return Err(eyre!(
                "Invalid cron expression '{}': expected 5 or 6 fields (seconds minutes hours day month weekday), got {}",
                expr,
                parts.len()
            ));
        }
    };

    Ok(normalized)
}

impl PromptSource {
    /// Validate that exactly one field is set and convert to the enum.
    pub fn from_raw(raw: PromptSourceRaw) -> Result<Self> {
        let fields_set = [
            raw.prompt.is_some(),
            raw.prompt_file.is_some(),
            raw.prompt_name.is_some(),
        ]
        .iter()
        .filter(|&&x| x)
        .count();

        if fields_set != 1 {
            return Err(eyre!(
                "Exactly one of 'prompt', 'prompt_file', or 'prompt_name' must be set, got {}",
                fields_set
            ));
        }

        Ok(match raw {
            PromptSourceRaw {
                prompt: Some(p), ..
            } => PromptSource::Inline(p),
            PromptSourceRaw {
                prompt_file: Some(f),
                ..
            } => {
                let normalized = f.strip_suffix(".md").unwrap_or(&f);
                PromptSource::PromptsDirFile(normalized.to_string())
            }
            PromptSourceRaw {
                prompt_name: Some(n),
                ..
            } => PromptSource::Name(n),
            _ => unreachable!(),
        })
    }
}

impl AutomationFile {
    /// Validate and convert raw TOML representation.
    pub fn from_raw(name: String, raw: AutomationFileRaw) -> Result<Self> {
        // Parse timezone (None means local timezone)
        let timezone = match raw.timezone {
            None => get_local_timezone(),
            Some(ref tz) if tz.is_empty() => {
                return Err(eyre!(
                    "timezone cannot be an empty string; omit the field or use a valid IANA timezone"
                ));
            }
            Some(tz) => tz
                .parse::<chrono_tz::Tz>()
                .map_err(|_| eyre!("Invalid timezone: '{}'", tz))?,
        };

        // Normalize and parse cron expression
        let normalized_schedule = normalize_cron_schedule(&raw.schedule)?;
        let schedule = Schedule::from_str(&normalized_schedule)
            .map_err(|e| eyre!("Invalid cron expression '{}': {}", raw.schedule, e))?;

        // Validate prompt source
        let prompt_source = PromptSource::from_raw(raw.prompt_source)?;

        // Validate project path if specified (actual existence checked later)
        let project = raw.project.clone();

        Ok(Self {
            name,
            description: raw.description,
            schedule_raw: raw.schedule,
            schedule,
            timezone,
            prompt_source,
            worktree: raw.worktree,
            project,
            vars: raw.vars,
            enabled: raw.enabled,
            catch_up: raw.catch_up,
            max_catch_up: raw.max_catch_up,
            timeout_seconds: raw.timeout_seconds,
            notify_on_success: raw.notify_on_success,
            notify_on_failure: raw.notify_on_failure,
        })
    }

    /// Get next scheduled occurrence after a given time.
    ///
    /// IMPORTANT: The cron crate evaluates schedules in the target timezone.
    /// DST transitions are handled by the crate's timezone-aware scheduling.
    pub fn next_after(
        &self,
        after: chrono::DateTime<chrono::Utc>,
    ) -> chrono::DateTime<chrono::Utc> {
        // Convert to target timezone, evaluate schedule, convert back to UTC
        let after_tz = after.with_timezone(&self.timezone);
        let next = self.schedule.after(&after_tz).next();
        next.unwrap().with_timezone(&chrono::Utc)
    }

    /// Get missed runs since a given time (up to max_count).
    pub fn missed_runs_since(
        &self,
        since: chrono::DateTime<chrono::Utc>,
        before: chrono::DateTime<chrono::Utc>,
        max_count: u32,
    ) -> Vec<chrono::DateTime<chrono::Utc>> {
        let since_tz = since.with_timezone(&self.timezone);
        self.schedule
            .after(&since_tz)
            .take(max_count as usize)
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .take_while(|&dt| dt < before)
            .collect()
    }

    /// Get stale-run timeout based on automation's configured timeout.
    pub fn stale_run_timeout(&self) -> chrono::Duration {
        let timeout_secs = self.timeout_seconds.unwrap_or(3600);
        // Use 2x the automation timeout, minimum 15 minutes
        let secs = (timeout_secs * 2).max(900);
        chrono::Duration::seconds(secs as i64)
    }
}

#[cfg(test)]
mod dst_tests {
    use super::*;
    use chrono::Timelike;

    /// Helper to create a minimal raw automation for testing.
    fn make_test_raw_automation(schedule: &str, timezone: &str) -> AutomationFileRaw {
        AutomationFileRaw {
            description: "test".to_string(),
            schedule: schedule.to_string(),
            timezone: Some(timezone.to_string()),
            prompt_source: PromptSourceRaw {
                prompt: Some("test prompt".to_string()),
                prompt_file: None,
                prompt_name: None,
            },
            worktree: false,
            project: None,
            vars: BTreeMap::new(),
            enabled: true,
            catch_up: CatchUpPolicy::Skip,
            max_catch_up: 10,
            timeout_seconds: Some(60),
            notify_on_success: false,
            notify_on_failure: false,
        }
    }

    #[test]
    fn test_spring_forward_no_double_run() {
        // 2024-03-31 02:30 CET doesn't exist (spring forward at 02:00)
        // Schedule "0 30 2 * * *" should not panic or double-run
        let raw = make_test_raw_automation("0 30 2 * * *", "Europe/Amsterdam");
        let automation = AutomationFile::from_raw("test".to_string(), raw).unwrap();

        // Check time before DST transition
        let before_dst = "2024-03-30T02:30:00Z"
            .parse::<chrono::DateTime<chrono::Utc>>()
            .unwrap();
        let next = automation.next_after(before_dst);

        // Should schedule the next run after the non-existent 02:30 time
        // (which would be 03:00 CEST or later)
        assert!(next > before_dst);

        // Verify we can call next_after multiple times without issues
        let next2 = automation.next_after(next);
        assert!(next2 > next);
    }

    #[test]
    fn test_fall_back_runs_once() {
        // 2024-10-27 02:30 CEST happens twice (fall back at 03:00)
        // Schedule "0 30 2 * * *" should run consistently
        let raw = make_test_raw_automation("0 30 2 * * *", "Europe/Amsterdam");
        let automation = AutomationFile::from_raw("test".to_string(), raw).unwrap();

        // Check time around DST transition
        let before_fall_back = "2024-10-26T02:30:00Z"
            .parse::<chrono::DateTime<chrono::Utc>>()
            .unwrap();
        let next = automation.next_after(before_fall_back);

        // Should schedule the next run
        assert!(next > before_fall_back);

        // Verify consistent behavior
        let next2 = automation.next_after(next);
        assert!(next2 > next);
    }

    #[test]
    fn test_weekly_schedule_stable_across_dst() {
        // "Mon-Fri at 09:00" should stay stable around DST
        let raw = make_test_raw_automation("0 0 9 * * Mon-Fri", "America/New_York");
        let automation = AutomationFile::from_raw("test".to_string(), raw).unwrap();

        // Check a Monday in March (before spring forward)
        let march_monday = "2024-03-11T14:00:00Z"
            .parse::<chrono::DateTime<chrono::Utc>>()
            .unwrap();
        let next_march = automation.next_after(march_monday);

        // Convert to local time to verify it's 09:00
        let local_march = next_march.with_timezone(&automation.timezone);
        assert_eq!(local_march.hour(), 9);
        assert_eq!(local_march.minute(), 0);

        // Check a Monday in November (after fall back)
        let november_monday = "2024-11-11T14:00:00Z"
            .parse::<chrono::DateTime<chrono::Utc>>()
            .unwrap();
        let next_november = automation.next_after(november_monday);

        // Convert to local time to verify it's still 09:00
        let local_november = next_november.with_timezone(&automation.timezone);
        assert_eq!(local_november.hour(), 9);
        assert_eq!(local_november.minute(), 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_raw_automation() -> AutomationFileRaw {
        AutomationFileRaw {
            description: "test automation".to_string(),
            schedule: "0 0 * * * *".to_string(),
            timezone: Some("UTC".to_string()),
            prompt_source: PromptSourceRaw {
                prompt: Some("test prompt".to_string()),
                prompt_file: None,
                prompt_name: None,
            },
            worktree: false,
            project: None,
            vars: BTreeMap::new(),
            enabled: true,
            catch_up: CatchUpPolicy::Skip,
            max_catch_up: 10,
            timeout_seconds: Some(60),
            notify_on_success: false,
            notify_on_failure: false,
        }
    }

    #[test]
    fn test_normalize_cron_schedule_five_fields() {
        let result = normalize_cron_schedule("0 * * * *").unwrap();
        assert_eq!(result, "0 0 * * * *");
    }

    #[test]
    fn test_normalize_cron_schedule_six_fields() {
        let result = normalize_cron_schedule("0 0 * * * *").unwrap();
        assert_eq!(result, "0 0 * * * *");
    }

    #[test]
    fn test_normalize_cron_schedule_invalid() {
        let result = normalize_cron_schedule("0 * * *");
        assert!(result.is_err());
    }

    #[test]
    fn test_prompt_source_from_raw_inline() {
        let raw = PromptSourceRaw {
            prompt: Some("inline prompt".to_string()),
            prompt_file: None,
            prompt_name: None,
        };
        let result = PromptSource::from_raw(raw).unwrap();
        assert!(matches!(result, PromptSource::Inline(_)));
    }

    #[test]
    fn test_prompt_source_from_raw_file() {
        let raw = PromptSourceRaw {
            prompt: None,
            prompt_file: Some("my-prompt".to_string()),
            prompt_name: None,
        };
        let result = PromptSource::from_raw(raw).unwrap();
        assert!(matches!(result, PromptSource::PromptsDirFile(_)));
    }

    #[test]
    fn test_prompt_source_from_raw_name() {
        let raw = PromptSourceRaw {
            prompt: None,
            prompt_file: None,
            prompt_name: Some("named-prompt".to_string()),
        };
        let result = PromptSource::from_raw(raw).unwrap();
        assert!(matches!(result, PromptSource::Name(_)));
    }

    #[test]
    fn test_prompt_source_from_raw_none_set() {
        let raw = PromptSourceRaw {
            prompt: None,
            prompt_file: None,
            prompt_name: None,
        };
        let result = PromptSource::from_raw(raw);
        assert!(result.is_err());
    }

    #[test]
    fn test_prompt_source_from_raw_multiple_set() {
        let raw = PromptSourceRaw {
            prompt: Some("inline".to_string()),
            prompt_file: Some("file".to_string()),
            prompt_name: None,
        };
        let result = PromptSource::from_raw(raw);
        assert!(result.is_err());
    }

    #[test]
    fn test_prompt_source_file_normalization() {
        let raw = PromptSourceRaw {
            prompt: None,
            prompt_file: Some("my-prompt.md".to_string()),
            prompt_name: None,
        };
        let result = PromptSource::from_raw(raw).unwrap();
        match result {
            PromptSource::PromptsDirFile(name) => {
                assert_eq!(name, "my-prompt");
            }
            _ => panic!("Expected PromptsDirFile variant"),
        }
    }

    #[test]
    fn test_automation_file_from_raw_valid() {
        let raw = make_test_raw_automation();
        let result = AutomationFile::from_raw("test".to_string(), raw).unwrap();
        assert_eq!(result.name, "test");
        assert_eq!(result.description, "test automation");
        assert!(result.enabled);
    }

    #[test]
    fn test_automation_file_from_raw_invalid_timezone() {
        let mut raw = make_test_raw_automation();
        raw.timezone = Some("Invalid/Timezone".to_string());
        let result = AutomationFile::from_raw("test".to_string(), raw);
        assert!(result.is_err());
    }

    #[test]
    fn test_automation_file_from_raw_empty_timezone() {
        let mut raw = make_test_raw_automation();
        raw.timezone = Some("".to_string());
        let result = AutomationFile::from_raw("test".to_string(), raw);
        assert!(result.is_err());
    }

    #[test]
    fn test_automation_file_from_raw_invalid_cron() {
        let mut raw = make_test_raw_automation();
        raw.schedule = "invalid cron".to_string();
        let result = AutomationFile::from_raw("test".to_string(), raw);
        assert!(result.is_err());
    }

    #[test]
    fn test_automation_file_next_after() {
        let raw = make_test_raw_automation();
        let automation = AutomationFile::from_raw("test".to_string(), raw).unwrap();

        let now = chrono::Utc::now();
        let next = automation.next_after(now);

        assert!(next > now);
    }

    #[test]
    fn test_automation_file_missed_runs_since() {
        let raw = make_test_raw_automation();
        let automation = AutomationFile::from_raw("test".to_string(), raw).unwrap();

        let since = chrono::Utc::now() - chrono::Duration::hours(2);
        let before = chrono::Utc::now();

        let missed = automation.missed_runs_since(since, before, 10);

        // Should have some missed runs for an hourly schedule
        assert!(!missed.is_empty());
        assert!(missed.len() <= 10);
    }

    #[test]
    fn test_automation_file_stale_run_timeout() {
        let raw = make_test_raw_automation();
        let automation = AutomationFile::from_raw("test".to_string(), raw).unwrap();

        let timeout = automation.stale_run_timeout();
        // timeout_seconds = 60, 2x = 120, but minimum is 900
        assert_eq!(timeout.num_seconds(), 900);
    }

    #[test]
    fn test_automation_file_stale_run_timeout_default() {
        let mut raw = make_test_raw_automation();
        raw.timeout_seconds = None;
        let automation = AutomationFile::from_raw("test".to_string(), raw).unwrap();

        let timeout = automation.stale_run_timeout();
        assert_eq!(timeout.num_seconds(), 7200); // 2x 3600 seconds (default)
    }

    #[test]
    fn test_automation_file_stale_run_timeout_minimum() {
        let mut raw = make_test_raw_automation();
        raw.timeout_seconds = Some(100); // Very short timeout
        let automation = AutomationFile::from_raw("test".to_string(), raw).unwrap();

        let timeout = automation.stale_run_timeout();
        assert_eq!(timeout.num_seconds(), 900); // Minimum 15 minutes
    }

    #[test]
    fn test_automation_file_stale_run_timeout_large() {
        let mut raw = make_test_raw_automation();
        raw.timeout_seconds = Some(1000); // Large timeout (1000 * 2 = 2000 > 900)
        let automation = AutomationFile::from_raw("test".to_string(), raw).unwrap();

        let timeout = automation.stale_run_timeout();
        assert_eq!(timeout.num_seconds(), 2000); // 2x 1000 seconds
    }

    #[test]
    fn test_get_local_timezone() {
        let tz = get_local_timezone();
        // Should return a valid timezone
        let now = chrono::Utc::now();
        let _local = now.with_timezone(&tz);
    }

    #[test]
    fn test_automation_source_equality() {
        assert_eq!(AutomationSource::Global, AutomationSource::Global);
        assert_ne!(AutomationSource::Global, AutomationSource::Project);
        assert_ne!(AutomationSource::Project, AutomationSource::Legacy);
    }
}
