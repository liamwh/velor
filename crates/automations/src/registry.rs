//! Project registry for multi-repo automation discovery.
//!
//! Stores a list of git repositories that should be scanned for
//! automations. Managed by ~/.config/velor/projects.toml

#![warn(missing_docs)]

use color_eyre::Result;
use color_eyre::eyre::eyre;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::instrument;

/// Project registry configuration.
///
/// Maintains a list of git repositories registered for automation discovery.
/// The registry is persisted to `~/.config/velor/projects.toml`.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ProjectRegistry {
    /// List of registered projects
    #[serde(default)]
    projects: Vec<ProjectEntry>,
}

/// Single project entry in the registry.
///
/// Each project represents a git repository that should be scanned
/// for automation definitions.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProjectEntry {
    /// Unique identifier (defaults to directory name).
    pub id: String,

    /// Absolute path to the git repository.
    pub path: PathBuf,

    /// Whether automations are enabled for this project.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

impl ProjectRegistry {
    /// Returns the path to the registry file: `~/.config/velor/projects.toml`.
    ///
    /// # Errors
    ///
    /// Returns an error if the home directory cannot be determined.
    #[instrument]
    pub fn registry_path() -> Result<PathBuf> {
        let home = dirs::home_dir().ok_or_else(|| eyre!("Cannot determine home directory"))?;
        Ok(home.join(".config").join("velor").join("projects.toml"))
    }

    /// Loads the registry from disk, returning an empty registry if the file doesn't exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the file exists but cannot be read or parsed.
    #[instrument(ret)]
    pub async fn load() -> Result<Self> {
        let path = Self::registry_path()?;
        if !path.exists() {
            return Ok(Self::default());
        }

        let content = tokio::fs::read_to_string(&path)
            .await
            .map_err(|e| eyre!("Failed to read projects.toml: {}", e))?;

        toml::from_str(&content).map_err(|e| eyre!("Failed to parse projects.toml: {}", e))
    }

    /// Saves the registry to disk, creating the parent directory if needed.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be created or the file cannot be written.
    #[instrument(skip(self), err)]
    pub async fn save(&self) -> Result<()> {
        let path = Self::registry_path()?;

        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| eyre!("Failed to create config directory: {}", e))?;
        }

        let content = toml::to_string_pretty(self)
            .map_err(|e| eyre!("Failed to serialize registry: {}", e))?;

        tokio::fs::write(&path, content)
            .await
            .map_err(|e| eyre!("Failed to write projects.toml: {}", e))?;

        Ok(())
    }

    /// Adds a project to the registry.
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the project directory (absolute or relative to current directory)
    /// * `id` - Optional unique identifier; defaults to the directory name if not provided
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The path cannot be resolved to an absolute path
    /// - The path is not a git repository (no `.git` directory)
    /// - A project with the same ID already exists
    #[instrument(skip(self), fields(id = id.as_deref().unwrap_or("<derived>")))]
    pub async fn add(&mut self, path: PathBuf, id: Option<String>) -> Result<()> {
        // Resolve to absolute path
        let path = if path.is_absolute() {
            path
        } else {
            std::env::current_dir()
                .map_err(|e| eyre!("Failed to get current directory: {}", e))?
                .join(&path)
        };

        // Verify it's a git repo
        if !path.join(".git").exists() {
            return Err(eyre!("Not a git repository: {}", path.display()));
        }

        // Use provided ID or derive from directory name
        let id = id.unwrap_or_else(|| {
            path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown")
                .to_string()
        });

        // Check for duplicate ID
        if self.projects.iter().any(|p| p.id == id) {
            return Err(eyre!("Project '{}' already registered", id));
        }

        self.projects.push(ProjectEntry {
            id,
            path,
            enabled: true,
        });

        Ok(())
    }

    /// Removes a project from the registry by ID.
    ///
    /// # Arguments
    ///
    /// * `id` - The unique identifier of the project to remove
    ///
    /// # Errors
    ///
    /// Returns an error if no project with the given ID exists.
    #[instrument(skip(self), fields(id = %id))]
    pub async fn remove(&mut self, id: &str) -> Result<()> {
        let original_len = self.projects.len();
        self.projects.retain(|p| p.id != id);

        if self.projects.len() == original_len {
            return Err(eyre!("Project '{}' not found", id));
        }

        Ok(())
    }

    /// Returns a reference to all projects in the registry.
    #[instrument(skip(self))]
    pub fn list(&self) -> &[ProjectEntry] {
        &self.projects
    }

    /// Returns only the enabled projects from the registry.
    #[instrument(skip(self))]
    pub fn enabled_projects(&self) -> Vec<&ProjectEntry> {
        self.projects.iter().filter(|p| p.enabled).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use tokio::fs;

    /// Creates a temporary git repository for testing.
    fn create_test_repo() -> Result<TempDir> {
        let dir = tempfile::tempdir()?;
        let git_dir = dir.path().join(".git");
        std::fs::create_dir(&git_dir)?;
        Ok(dir)
    }

    #[test]
    fn test_registry_path_returns_valid_path() {
        let path = ProjectRegistry::registry_path().expect("should get registry path");
        assert!(path.ends_with(".config/velor/projects.toml"));
    }

    #[tokio::test]
    async fn test_load_returns_empty_registry_when_file_missing() {
        // Use a temp dir to ensure no existing registry file
        let registry = ProjectRegistry::load()
            .await
            .expect("should load empty registry");
        assert!(registry.projects.is_empty());
    }

    #[tokio::test]
    async fn test_add_project_with_explicit_id() {
        let temp_dir = create_test_repo().expect("should create temp repo");
        let mut registry = ProjectRegistry::default();

        registry
            .add(
                temp_dir.path().to_path_buf(),
                Some("my-project".to_string()),
            )
            .await
            .expect("should add project");

        assert_eq!(registry.projects.len(), 1);
        assert_eq!(registry.projects[0].id, "my-project");
        assert_eq!(registry.projects[0].path, temp_dir.path());
        assert!(registry.projects[0].enabled);
    }

    #[tokio::test]
    async fn test_add_project_derives_id_from_directory_name() {
        let temp_dir = create_test_repo().expect("should create temp repo");
        let mut registry = ProjectRegistry::default();

        registry
            .add(temp_dir.path().to_path_buf(), None)
            .await
            .expect("should add project");

        let derived_id = temp_dir
            .path()
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown");
        assert_eq!(registry.projects[0].id, derived_id);
    }

    #[tokio::test]
    async fn test_add_project_fails_for_non_git_repo() {
        let temp_dir = tempfile::tempdir().expect("should create temp dir");
        let mut registry = ProjectRegistry::default();

        let result = registry.add(temp_dir.path().to_path_buf(), None).await;

        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("Not a git repository"));
    }

    #[tokio::test]
    async fn test_add_project_fails_for_duplicate_id() {
        let temp_dir1 = create_test_repo().expect("should create temp repo");
        let temp_dir2 = create_test_repo().expect("should create temp repo");
        let mut registry = ProjectRegistry::default();

        registry
            .add(
                temp_dir1.path().to_path_buf(),
                Some("duplicate".to_string()),
            )
            .await
            .expect("should add first project");

        let result = registry
            .add(
                temp_dir2.path().to_path_buf(),
                Some("duplicate".to_string()),
            )
            .await;

        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("already registered"));
    }

    #[tokio::test]
    async fn test_remove_project() {
        let temp_dir = create_test_repo().expect("should create temp repo");
        let mut registry = ProjectRegistry::default();

        registry
            .add(
                temp_dir.path().to_path_buf(),
                Some("test-project".to_string()),
            )
            .await
            .expect("should add project");

        assert_eq!(registry.projects.len(), 1);

        registry
            .remove("test-project")
            .await
            .expect("should remove project");

        assert!(registry.projects.is_empty());
    }

    #[tokio::test]
    async fn test_remove_project_fails_for_nonexistent_id() {
        let mut registry = ProjectRegistry::default();

        let result = registry.remove("nonexistent").await;

        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("not found"));
    }

    #[tokio::test]
    async fn test_list_returns_all_projects() {
        let temp_dir1 = create_test_repo().expect("should create temp repo");
        let temp_dir2 = create_test_repo().expect("should create temp repo");
        let mut registry = ProjectRegistry::default();

        registry
            .add(temp_dir1.path().to_path_buf(), Some("project1".to_string()))
            .await
            .expect("should add first project");
        registry
            .add(temp_dir2.path().to_path_buf(), Some("project2".to_string()))
            .await
            .expect("should add second project");

        let projects = registry.list();
        assert_eq!(projects.len(), 2);
        assert_eq!(projects[0].id, "project1");
        assert_eq!(projects[1].id, "project2");
    }

    #[tokio::test]
    async fn test_enabled_projects_filters_out_disabled() {
        let temp_dir1 = create_test_repo().expect("should create temp repo");
        let temp_dir2 = create_test_repo().expect("should create temp repo");
        let mut registry = ProjectRegistry::default();

        registry
            .add(
                temp_dir1.path().to_path_buf(),
                Some("enabled-project".to_string()),
            )
            .await
            .expect("should add enabled project");

        let entry = ProjectEntry {
            id: "disabled-project".to_string(),
            path: temp_dir2.path().to_path_buf(),
            enabled: false,
        };
        registry.projects.push(entry);

        let enabled = registry.enabled_projects();
        assert_eq!(enabled.len(), 1);
        assert_eq!(enabled[0].id, "enabled-project");
    }

    #[tokio::test]
    async fn test_save_and_load_persists_registry() {
        let temp_dir = create_test_repo().expect("should create temp repo");
        let mut registry = ProjectRegistry::default();

        registry
            .add(
                temp_dir.path().to_path_buf(),
                Some("persistent".to_string()),
            )
            .await
            .expect("should add project");

        registry.save().await.expect("should save registry");

        let loaded = ProjectRegistry::load().await.expect("should load registry");
        assert_eq!(loaded.projects.len(), 1);
        assert_eq!(loaded.projects[0].id, "persistent");
        assert_eq!(loaded.projects[0].path, temp_dir.path());

        // Clean up
        let path = ProjectRegistry::registry_path().expect("should get path");
        let _ = fs::remove_file(path).await;
    }

    #[tokio::test]
    async fn test_default_true_for_enabled_field() {
        let toml = r#"
            [[projects]]
            id = "test"
            path = "/fake/path"
        "#;

        let registry: ProjectRegistry =
            toml::from_str(toml).expect("should parse registry with missing enabled field");

        assert!(
            registry.projects[0].enabled,
            "enabled should default to true"
        );
    }

    #[tokio::test]
    async fn test_add_project_with_relative_path() {
        let temp_dir = create_test_repo().expect("should create temp repo");
        let mut registry = ProjectRegistry::default();

        // Change to temp dir's parent to test relative path resolution
        let parent = temp_dir.path().parent().expect("should have parent");
        let dir_name = temp_dir
            .path()
            .file_name()
            .and_then(|n| n.to_str())
            .expect("should have dir name");

        std::env::set_current_dir(parent).expect("should change dir");

        registry
            .add(PathBuf::from(dir_name), None)
            .await
            .expect("should add project with relative path");

        // Should resolve to absolute path (canonicalize to handle macOS /var -> /private/var symlinks)
        let registered_path = std::fs::canonicalize(&registry.projects[0].path)
            .expect("should canonicalize registered path");
        let expected_path =
            std::fs::canonicalize(temp_dir.path()).expect("should canonicalize temp dir path");
        assert_eq!(registered_path, expected_path);
    }
}
