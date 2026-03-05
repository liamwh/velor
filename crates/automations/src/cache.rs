// Copyright (c) 2024 Liam S. (velor)
//
// This software is licensed under the terms of the UNLICENSE.
// You should have received a copy of the UNLICENSE with this program.
// If not, see https://unlicense.org/

//! Automation cache for file-based automation discovery.
//!
//! This module provides the [`AutomationCache`] for loading automations from
//! TOML files in both global (`XDG_CONFIG_HOME/velor/automations/`) and
//! project-specific (`{repo}/.velor/automations/`) locations.
//!
//! Project-level automations override global automations with the same name.

use color_eyre::Result;
use color_eyre::eyre::{WrapErr, eyre};
use std::collections::BTreeMap;
use std::path::Path;
use tokio::fs;

use crate::file_config::{AutomationEntry, AutomationFile, AutomationFileRaw, AutomationSource};

#[cfg(test)]
use tokio::io::AsyncWriteExt;

/// Cache for loading automations from global and project directories.
///
/// Automations are loaded fresh on each access (fast enough for typical usage).
/// Project-level automations override global automations with the same name.
#[derive(Debug, Clone)]
pub struct AutomationCache {
    /// Home directory path (`XDG_CONFIG_HOME/velor` or `~/.config/velor`).
    home_dir: std::path::PathBuf,
    /// Optional git repository root (for repo-level automations).
    repo_dir: Option<std::path::PathBuf>,
}

impl AutomationCache {
    /// Creates a new [`AutomationCache`].
    ///
    /// # Arguments
    ///
    /// * `home_dir` - Path to the home config directory (`XDG_CONFIG_HOME/velor` or `~/.config/velor`).
    /// * `repo_dir` - Optional path to the git repository root (for project-level automations).
    #[must_use]
    pub const fn new(home_dir: std::path::PathBuf, repo_dir: Option<std::path::PathBuf>) -> Self {
        Self { home_dir, repo_dir }
    }

    /// Returns all cached automations, loading them fresh each time.
    ///
    /// Home and repository automations are merged, with repository automations
    /// taking precedence over home automations with the same name.
    ///
    /// # Errors
    ///
    /// Returns an error if automation discovery fails.
    #[tracing::instrument(level = "debug", ret, err, skip(self))]
    pub async fn get(&self) -> Result<BTreeMap<String, AutomationEntry>> {
        let mut home_automations = self
            .discover_automations(&self.home_dir, AutomationSource::Global)
            .await?;

        let repo_automations = if let Some(ref repo_dir) = self.repo_dir {
            self.discover_automations(repo_dir, AutomationSource::Project)
                .await?
        } else {
            BTreeMap::new()
        };

        // Merge: project overrides global by name
        for (name, entry) in repo_automations {
            home_automations.insert(name, entry);
        }

        Ok(home_automations)
    }

    /// Fetches a single automation by name (respects override precedence).
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The automations cannot be loaded
    /// - The automation name is not found
    #[tracing::instrument(level = "debug", ret, err, skip(self))]
    pub async fn get_by_name(&self, name: &str) -> Result<AutomationEntry> {
        let all = self.get().await?;
        all.get(name)
            .cloned()
            .ok_or_else(|| eyre!("automation '{}' not found", name))
    }

    /// Lists all automations including duplicates (shows source of each).
    ///
    /// This returns all automations from both global and project sources,
    /// including duplicates (which can happen when an automation with the
    /// same name exists in both locations). Use `get()` for the merged view.
    ///
    /// # Errors
    ///
    /// Returns an error if automation discovery fails.
    #[tracing::instrument(level = "debug", ret, err, skip(self))]
    pub async fn list_all(&self) -> Result<Vec<AutomationEntry>> {
        let mut result = Vec::new();

        // Load home automations
        let home = self
            .discover_automations(&self.home_dir, AutomationSource::Global)
            .await?;
        result.extend(home.into_values());

        // Load repo automations
        if let Some(ref repo_dir) = self.repo_dir {
            let repo = self
                .discover_automations(repo_dir, AutomationSource::Project)
                .await?;
            result.extend(repo.into_values());
        }

        // Sort by name for consistent output
        result.sort_by(|a, b| a.automation.name.cmp(&b.automation.name));
        Ok(result)
    }

    /// Discovers automations in a directory.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be read or if any automation
    /// file fails to parse.
    async fn discover_automations(
        &self,
        base_dir: &Path,
        source: AutomationSource,
    ) -> Result<BTreeMap<String, AutomationEntry>> {
        let automations_dir = base_dir.join("automations");

        // Use async read_dir, not exists() check
        let mut entries = match fs::read_dir(&automations_dir).await {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
            Err(e) => {
                return Err(e).wrap_err_with(|| {
                    format!(
                        "Failed to read automations directory: {}",
                        automations_dir.display()
                    )
                });
            }
        };

        let mut automations = BTreeMap::new();

        while let Some(entry) = entries
            .next_entry()
            .await
            .wrap_err("Failed to read directory entry")?
        {
            let path = entry.path();

            // Skip directories
            if path.is_dir() {
                continue;
            }

            // Skip non-TOML files
            if path.extension().and_then(|s| s.to_str()) != Some("toml") {
                continue;
            }

            let (name, automation_file) = parse_automation_file(&path, source).await?;

            automations.insert(
                name,
                AutomationEntry {
                    automation: automation_file,
                    source,
                    path,
                },
            );
        }

        Ok(automations)
    }
}

/// Parses a single automation TOML file.
///
/// # Errors
///
/// Returns an error if:
/// - The file cannot be read
/// - The TOML cannot be parsed
/// - The automation configuration is invalid
/// - The project path does not exist
async fn parse_automation_file(
    path: &Path,
    _source: AutomationSource,
) -> Result<(String, AutomationFile)> {
    let content = fs::read_to_string(path)
        .await
        .wrap_err_with(|| format!("Failed to read automation file: {}", path.display()))?;

    // Extract name from filename (without .toml extension)
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| eyre!("Invalid automation filename: {}", path.display()))?
        .to_string();

    // Parse TOML
    let raw: AutomationFileRaw = toml::from_str(&content)
        .wrap_err_with(|| format!("Failed to parse automation file: {}", path.display()))?;

    // Validate and convert (project path validated here using async metadata)
    let automation = validate_and_convert(name.clone(), raw).await?;

    Ok((name, automation))
}

/// Validates project path using async metadata check.
///
/// # Errors
///
/// Returns an error if:
/// - The project path does not exist
/// - The project path cannot be accessed
async fn validate_and_convert(name: String, raw: AutomationFileRaw) -> Result<AutomationFile> {
    // Validate project path exists using async metadata
    if let Some(ref proj) = raw.project {
        match fs::metadata(proj).await {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(eyre!("project path {} does not exist", proj.display()));
            }
            Err(e) => {
                return Err(e).wrap_err_with(|| {
                    format!("Failed to access project path: {}", proj.display())
                });
            }
        }
    }

    AutomationFile::from_raw(name, raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_config::PromptSource;
    use tempfile::TempDir;
    use tokio::fs;

    const MSG_TEMPDIR: &str = "tempdir should be created";
    const MSG_CREATE_AUTOMATIONS_DIR: &str = "should create automations dir";
    const MSG_CREATE_FILE: &str = "should create file";
    const MSG_WRITE_CONTENT: &str = "should write content";
    const MSG_SUCCEED: &str = "should succeed";
    const MSG_PARSE_FAILED: &str = "Failed to parse automation file";
    const MSG_INVALID_CRON: &str = "Invalid cron expression";
    const MSG_PATH_NOT_EXIST: &str = "does not exist";

    /// Helper to create a minimal valid automation TOML.
    fn minimal_automation_toml() -> String {
        String::from(
            r#"
description = "Test automation"
schedule = "0 0 * * * *"
prompt = "once"
"#,
        )
    }

    /// Helper to create an automation with custom schedule.
    fn automation_with_schedule(schedule: &str) -> String {
        format!(
            r#"
description = "Test automation"
schedule = "{}"
prompt = "once"
"#,
            schedule
        )
    }

    /// Helper to create an automation with prompt_file.
    fn automation_with_prompt_file(prompt_file: &str) -> String {
        format!(
            r#"
description = "Test automation"
schedule = "0 0 * * * *"
prompt_file = "{}"
"#,
            prompt_file
        )
    }

    /// Helper to create an automation with project path.
    fn automation_with_project(project: &str) -> String {
        format!(
            r#"
description = "Test automation"
schedule = "0 0 * * * *"
prompt = "once"
project = "{}"
"#,
            project
        )
    }

    #[tokio::test]
    async fn test_cache_new() {
        let home_dir = std::path::PathBuf::from("/home/test/.config/velor");
        let cache = AutomationCache::new(home_dir.clone(), None);
        assert_eq!(cache.home_dir, home_dir);
        assert!(cache.repo_dir.is_none());
    }

    #[tokio::test]
    async fn test_cache_with_repo() {
        let home_dir = std::path::PathBuf::from("/home/test/.config/velor");
        let repo_dir = std::path::PathBuf::from("/home/test/project");
        let cache = AutomationCache::new(home_dir.clone(), Some(repo_dir.clone()));
        assert_eq!(cache.home_dir, home_dir);
        assert_eq!(cache.repo_dir, Some(repo_dir));
    }

    #[tokio::test]
    async fn test_discover_empty_directory() {
        let temp_dir = TempDir::new().expect(MSG_TEMPDIR);
        let cache = AutomationCache::new(temp_dir.path().to_path_buf(), None);

        // Don't create the automations directory - should return empty map
        let automations = cache.get().await.expect(MSG_SUCCEED);
        assert!(automations.is_empty());
    }

    #[tokio::test]
    async fn test_discover_single_automation() {
        let temp_dir = TempDir::new().expect(MSG_TEMPDIR);
        let automations_dir = temp_dir.path().join("automations");
        fs::create_dir_all(&automations_dir)
            .await
            .expect(MSG_CREATE_AUTOMATIONS_DIR);

        // Create a test automation file
        let automation_path = automations_dir.join("test.toml");
        let mut file = fs::File::create(&automation_path)
            .await
            .expect(MSG_CREATE_FILE);
        file.write_all(minimal_automation_toml().as_bytes())
            .await
            .expect(MSG_WRITE_CONTENT);

        let cache = AutomationCache::new(temp_dir.path().to_path_buf(), None);
        let automations = cache.get().await.expect(MSG_SUCCEED);

        assert_eq!(automations.len(), 1);
        assert!(automations.contains_key("test"));
        let entry = &automations["test"];
        assert_eq!(entry.automation.name, "test");
        assert_eq!(entry.automation.description, "Test automation");
        assert_eq!(entry.source, AutomationSource::Global);
        assert_eq!(entry.path, automation_path);
    }

    #[tokio::test]
    async fn test_discover_multiple_automations() {
        let temp_dir = TempDir::new().expect(MSG_TEMPDIR);
        let automations_dir = temp_dir.path().join("automations");
        fs::create_dir_all(&automations_dir)
            .await
            .expect(MSG_CREATE_AUTOMATIONS_DIR);

        // Create multiple automation files
        for name in &["first", "second", "third"] {
            let automation_path = automations_dir.join(format!("{}.toml", name));
            let mut file = fs::File::create(&automation_path)
                .await
                .expect(MSG_CREATE_FILE);
            file.write_all(minimal_automation_toml().as_bytes())
                .await
                .expect(MSG_WRITE_CONTENT);
        }

        let cache = AutomationCache::new(temp_dir.path().to_path_buf(), None);
        let automations = cache.get().await.expect(MSG_SUCCEED);

        assert_eq!(automations.len(), 3);
        assert!(automations.contains_key("first"));
        assert!(automations.contains_key("second"));
        assert!(automations.contains_key("third"));
    }

    #[tokio::test]
    async fn test_project_overrides_global() {
        let home_dir = TempDir::new().expect(MSG_TEMPDIR);
        let repo_dir = TempDir::new().expect(MSG_TEMPDIR);

        // Create global automations
        let home_auto_dir = home_dir.path().join("automations");
        fs::create_dir_all(&home_auto_dir)
            .await
            .expect(MSG_CREATE_AUTOMATIONS_DIR);
        let home_auto_path = home_auto_dir.join("test.toml");
        let mut file = fs::File::create(&home_auto_path)
            .await
            .expect(MSG_CREATE_FILE);
        file.write_all(
            r#"
description = "Global automation"
schedule = "0 0 * * * *"
prompt = "once-global"
"#
            .as_bytes(),
        )
        .await
        .expect(MSG_WRITE_CONTENT);

        // Create project automations with same name
        let repo_auto_dir = repo_dir.path().join("automations");
        fs::create_dir_all(&repo_auto_dir)
            .await
            .expect(MSG_CREATE_AUTOMATIONS_DIR);
        let repo_auto_path = repo_auto_dir.join("test.toml");
        let mut file = fs::File::create(&repo_auto_path)
            .await
            .expect(MSG_CREATE_FILE);
        file.write_all(
            r#"
description = "Project automation"
schedule = "0 0 * * * *"
prompt = "once-project"
"#
            .as_bytes(),
        )
        .await
        .expect(MSG_WRITE_CONTENT);

        let cache = AutomationCache::new(
            home_dir.path().to_path_buf(),
            Some(repo_dir.path().to_path_buf()),
        );
        let automations = cache.get().await.expect(MSG_SUCCEED);

        assert_eq!(automations.len(), 1);
        let entry = &automations["test"];
        assert_eq!(entry.automation.description, "Project automation");
        assert_eq!(entry.source, AutomationSource::Project);
        assert_eq!(entry.path, repo_auto_path);
    }

    #[tokio::test]
    async fn test_list_all_includes_duplicates() {
        let home_dir = TempDir::new().expect(MSG_TEMPDIR);
        let repo_dir = TempDir::new().expect(MSG_TEMPDIR);

        // Create global automations
        let home_auto_dir = home_dir.path().join("automations");
        fs::create_dir_all(&home_auto_dir)
            .await
            .expect(MSG_CREATE_AUTOMATIONS_DIR);
        let home_auto_path = home_auto_dir.join("test.toml");
        let mut file = fs::File::create(&home_auto_path)
            .await
            .expect(MSG_CREATE_FILE);
        file.write_all(
            r#"
description = "Global automation"
schedule = "0 0 * * * *"
prompt = "once"
"#
            .as_bytes(),
        )
        .await
        .expect(MSG_WRITE_CONTENT);

        // Create project automations with same name
        let repo_auto_dir = repo_dir.path().join("automations");
        fs::create_dir_all(&repo_auto_dir)
            .await
            .expect(MSG_CREATE_AUTOMATIONS_DIR);
        let repo_auto_path = repo_auto_dir.join("test.toml");
        let mut file = fs::File::create(&repo_auto_path)
            .await
            .expect(MSG_CREATE_FILE);
        file.write_all(
            r#"
description = "Project automation"
schedule = "0 0 * * * *"
prompt = "once"
"#
            .as_bytes(),
        )
        .await
        .expect(MSG_WRITE_CONTENT);

        let cache = AutomationCache::new(
            home_dir.path().to_path_buf(),
            Some(repo_dir.path().to_path_buf()),
        );
        let all = cache.list_all().await.expect(MSG_SUCCEED);

        // Should have both entries (with duplicates shown)
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].source, AutomationSource::Global);
        assert_eq!(all[1].source, AutomationSource::Project);
    }

    #[tokio::test]
    async fn test_get_by_name() {
        let temp_dir = TempDir::new().expect(MSG_TEMPDIR);
        let automations_dir = temp_dir.path().join("automations");
        fs::create_dir_all(&automations_dir)
            .await
            .expect(MSG_CREATE_AUTOMATIONS_DIR);

        let automation_path = automations_dir.join("my-auto.toml");
        let mut file = fs::File::create(&automation_path)
            .await
            .expect(MSG_CREATE_FILE);
        file.write_all(minimal_automation_toml().as_bytes())
            .await
            .expect(MSG_WRITE_CONTENT);

        let cache = AutomationCache::new(temp_dir.path().to_path_buf(), None);
        let entry = cache
            .get_by_name("my-auto")
            .await
            .expect("should find automation");

        assert_eq!(entry.automation.name, "my-auto");
    }

    #[tokio::test]
    async fn test_get_by_name_not_found() {
        let temp_dir = TempDir::new().expect(MSG_TEMPDIR);
        let cache = AutomationCache::new(temp_dir.path().to_path_buf(), None);

        let result = cache.get_by_name("nonexistent").await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("automation 'nonexistent' not found"));
    }

    #[tokio::test]
    async fn test_skips_non_toml_files() {
        let temp_dir = TempDir::new().expect(MSG_TEMPDIR);
        let automations_dir = temp_dir.path().join("automations");
        fs::create_dir_all(&automations_dir)
            .await
            .expect(MSG_CREATE_AUTOMATIONS_DIR);

        // Create a non-TOML file
        let readme_path = automations_dir.join("README.md");
        let mut file = fs::File::create(&readme_path).await.expect(MSG_CREATE_FILE);
        file.write_all(b"# README").await.expect(MSG_WRITE_CONTENT);

        // Create a valid automation
        let automation_path = automations_dir.join("test.toml");
        let mut file = fs::File::create(&automation_path)
            .await
            .expect(MSG_CREATE_FILE);
        file.write_all(minimal_automation_toml().as_bytes())
            .await
            .expect(MSG_WRITE_CONTENT);

        let cache = AutomationCache::new(temp_dir.path().to_path_buf(), None);
        let automations = cache.get().await.expect(MSG_SUCCEED);

        // Should only have the TOML file, not the README
        assert_eq!(automations.len(), 1);
        assert!(automations.contains_key("test"));
    }

    #[tokio::test]
    async fn test_skips_subdirectories() {
        let temp_dir = TempDir::new().expect(MSG_TEMPDIR);
        let automations_dir = temp_dir.path().join("automations");
        fs::create_dir_all(&automations_dir)
            .await
            .expect(MSG_CREATE_AUTOMATIONS_DIR);

        // Create a subdirectory
        let subdir = automations_dir.join("subdir");
        fs::create_dir(&subdir).await.expect("should create subdir");

        // Create a valid automation
        let automation_path = automations_dir.join("test.toml");
        let mut file = fs::File::create(&automation_path)
            .await
            .expect(MSG_CREATE_FILE);
        file.write_all(minimal_automation_toml().as_bytes())
            .await
            .expect(MSG_WRITE_CONTENT);

        let cache = AutomationCache::new(temp_dir.path().to_path_buf(), None);
        let automations = cache.get().await.expect(MSG_SUCCEED);

        // Should only have the TOML file, not the subdirectory
        assert_eq!(automations.len(), 1);
        assert!(automations.contains_key("test"));
    }

    #[tokio::test]
    async fn test_invalid_toml_returns_error() {
        let temp_dir = TempDir::new().expect(MSG_TEMPDIR);
        let automations_dir = temp_dir.path().join("automations");
        fs::create_dir_all(&automations_dir)
            .await
            .expect(MSG_CREATE_AUTOMATIONS_DIR);

        let automation_path = automations_dir.join("test.toml");
        let mut file = fs::File::create(&automation_path)
            .await
            .expect(MSG_CREATE_FILE);
        file.write_all(b"invalid toml content [[[")
            .await
            .expect(MSG_WRITE_CONTENT);

        let cache = AutomationCache::new(temp_dir.path().to_path_buf(), None);
        let result = cache.get().await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains(MSG_PARSE_FAILED));
    }

    #[tokio::test]
    async fn test_invalid_cron_expression_returns_error() {
        let temp_dir = TempDir::new().expect(MSG_TEMPDIR);
        let automations_dir = temp_dir.path().join("automations");
        fs::create_dir_all(&automations_dir)
            .await
            .expect(MSG_CREATE_AUTOMATIONS_DIR);

        let automation_path = automations_dir.join("test.toml");
        let mut file = fs::File::create(&automation_path)
            .await
            .expect(MSG_CREATE_FILE);
        file.write_all(
            r#"
description = "Test automation"
schedule = "invalid-cron"
prompt = "once"
"#
            .as_bytes(),
        )
        .await
        .expect(MSG_WRITE_CONTENT);

        let cache = AutomationCache::new(temp_dir.path().to_path_buf(), None);
        let result = cache.get().await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains(MSG_INVALID_CRON));
    }

    #[tokio::test]
    async fn test_project_path_not_exists_returns_error() {
        let temp_dir = TempDir::new().expect(MSG_TEMPDIR);
        let automations_dir = temp_dir.path().join("automations");
        fs::create_dir_all(&automations_dir)
            .await
            .expect(MSG_CREATE_AUTOMATIONS_DIR);

        let automation_path = automations_dir.join("test.toml");
        let mut file = fs::File::create(&automation_path)
            .await
            .expect(MSG_CREATE_FILE);
        file.write_all(automation_with_project("/nonexistent/path").as_bytes())
            .await
            .expect(MSG_WRITE_CONTENT);

        let cache = AutomationCache::new(temp_dir.path().to_path_buf(), None);
        let result = cache.get().await;

        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains(MSG_PATH_NOT_EXIST));
    }

    #[tokio::test]
    async fn test_valid_project_path_succeeds() {
        let temp_dir = TempDir::new().expect(MSG_TEMPDIR);
        let automations_dir = temp_dir.path().join("automations");
        fs::create_dir_all(&automations_dir)
            .await
            .expect(MSG_CREATE_AUTOMATIONS_DIR);

        // Create the project directory
        let project_dir = temp_dir.path().join("project");
        fs::create_dir(&project_dir)
            .await
            .expect("should create project dir");

        let automation_path = automations_dir.join("test.toml");
        let mut file = fs::File::create(&automation_path)
            .await
            .expect(MSG_CREATE_FILE);
        file.write_all(automation_with_project(project_dir.to_str().unwrap()).as_bytes())
            .await
            .expect(MSG_WRITE_CONTENT);

        let cache = AutomationCache::new(temp_dir.path().to_path_buf(), None);
        let automations = cache.get().await.expect(MSG_SUCCEED);

        assert_eq!(automations.len(), 1);
        assert_eq!(
            automations["test"].automation.project,
            Some(project_dir.to_path_buf())
        );
    }

    #[tokio::test]
    async fn test_five_field_cron_normalized() {
        let temp_dir = TempDir::new().expect(MSG_TEMPDIR);
        let automations_dir = temp_dir.path().join("automations");
        fs::create_dir_all(&automations_dir)
            .await
            .expect(MSG_CREATE_AUTOMATIONS_DIR);

        // Create automation with 5-field cron (minutes hours day month weekday)
        let automation_path = automations_dir.join("test.toml");
        let mut file = fs::File::create(&automation_path)
            .await
            .expect(MSG_CREATE_FILE);
        file.write_all(automation_with_schedule("0 9 * * *").as_bytes())
            .await
            .expect(MSG_WRITE_CONTENT);

        let cache = AutomationCache::new(temp_dir.path().to_path_buf(), None);
        let automations = cache.get().await.expect(MSG_SUCCEED);

        assert_eq!(automations.len(), 1);
        // The raw schedule should be preserved
        assert_eq!(automations["test"].automation.schedule_raw, "0 9 * * *");
        // But the parsed schedule should work
        let now = chrono::Utc::now();
        let next = automations["test"].automation.next_after(now);
        // Next run should be in the future
        assert!(next > now);
    }

    #[tokio::test]
    async fn test_six_field_cron_accepted() {
        let temp_dir = TempDir::new().expect(MSG_TEMPDIR);
        let automations_dir = temp_dir.path().join("automations");
        fs::create_dir_all(&automations_dir)
            .await
            .expect(MSG_CREATE_AUTOMATIONS_DIR);

        // Create automation with 6-field cron (seconds minutes hours day month weekday)
        let automation_path = automations_dir.join("test.toml");
        let mut file = fs::File::create(&automation_path)
            .await
            .expect(MSG_CREATE_FILE);
        file.write_all(automation_with_schedule("0 0 9 * * *").as_bytes())
            .await
            .expect(MSG_WRITE_CONTENT);

        let cache = AutomationCache::new(temp_dir.path().to_path_buf(), None);
        let automations = cache.get().await.expect(MSG_SUCCEED);

        assert_eq!(automations.len(), 1);
        assert_eq!(automations["test"].automation.schedule_raw, "0 0 9 * * *");
    }

    #[tokio::test]
    async fn test_prompt_file_strips_md_suffix() {
        let temp_dir = TempDir::new().expect(MSG_TEMPDIR);
        let automations_dir = temp_dir.path().join("automations");
        fs::create_dir_all(&automations_dir)
            .await
            .expect(MSG_CREATE_AUTOMATIONS_DIR);

        let automation_path = automations_dir.join("test.toml");
        let mut file = fs::File::create(&automation_path)
            .await
            .expect(MSG_CREATE_FILE);
        file.write_all(automation_with_prompt_file("my-prompt.md").as_bytes())
            .await
            .expect(MSG_WRITE_CONTENT);

        let cache = AutomationCache::new(temp_dir.path().to_path_buf(), None);
        let automations = cache.get().await.expect(MSG_SUCCEED);

        assert_eq!(automations.len(), 1);
        // The .md suffix should be stripped
        match &automations["test"].automation.prompt_source {
            PromptSource::PromptsDirFile(name) => {
                assert_eq!(name, "my-prompt");
            }
            _ => panic!("Expected PromptsDirFile variant"),
        }
    }
}
