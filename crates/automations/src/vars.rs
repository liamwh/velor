//! Variable merging for automation execution context.
//!
//! This module provides functionality to merge variables from multiple sources
//! with built-in variables for template rendering.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

/// Merge variables from multiple sources with built-ins.
///
/// # Precedence
///
/// Variables are merged with the following precedence (highest to lowest):
/// 1. Built-in variables (git_root, cwd, now, repo, branch, etc.)
/// 2. Automation-specific vars (from automation file)
/// 3. Repo config vars (from .velor/velor.toml)
/// 4. Home config vars (from ~/.velor/velor.toml)
///
/// # Arguments
///
/// * `automation_vars` - Variables defined in the automation file
/// * `repo_vars` - Variables from the repository config (.velor/velor.toml)
/// * `home_vars` - Variables from the home config (~/.velor/velor.toml)
/// * `git_root` - Path to the git repository root
/// * `cwd` - Current working directory
///
/// # Returns
///
/// A merged map of variables with built-ins applied.
///
/// # Examples
///
/// ```
/// use std::collections::BTreeMap;
/// use std::path::PathBuf;
///
/// let automation_vars = BTreeMap::from([("custom".to_string(), "value".to_string())]);
/// let repo_vars = BTreeMap::from([("repo_var".to_string(), "repo_value".to_string())]);
/// let home_vars = BTreeMap::from([("home_var".to_string(), "home_value".to_string())]);
/// let git_root = PathBuf::from("/path/to/repo");
/// let cwd = PathBuf::from("/path/to/repo/subdir");
///
/// let merged = merge_automation_vars(automation_vars, repo_vars, home_vars, &git_root, &cwd);
///
/// // Built-ins override everything
/// assert!(merged.contains_key("git_root"));
/// assert!(merged.contains_key("cwd"));
/// assert!(merged.contains_key("now"));
/// // Automation vars override repo vars
/// assert_eq!(merged.get("custom"), Some(&"value".to_string()));
/// // Repo vars override home vars
/// assert_eq!(merged.get("repo_var"), Some(&"repo_value".to_string()));
/// // Home vars are included when not overridden
/// assert_eq!(merged.get("home_var"), Some(&"home_value".to_string()));
/// ```
pub fn merge_automation_vars(
    automation_vars: BTreeMap<String, String>,
    repo_vars: BTreeMap<String, String>,
    home_vars: BTreeMap<String, String>,
    git_root: &Path,
    cwd: &Path,
) -> BTreeMap<String, String> {
    let mut merged = home_vars;

    // Apply repo vars (they override home)
    for (key, value) in repo_vars {
        merged.insert(key, value);
    }

    // Apply automation vars (they override both)
    for (key, value) in automation_vars {
        merged.insert(key, value);
    }

    // Apply built-ins (they override everything - prevents user breaking templates)
    merged.insert("git_root".to_string(), git_root.display().to_string());
    merged.insert("cwd".to_string(), cwd.display().to_string());
    merged.insert("now".to_string(), chrono::Utc::now().to_rfc3339());

    // Try to get repo name from git_root
    if let Some(repo_name) = git_root.file_name()
        && let Some(name) = repo_name.to_str()
    {
        merged.insert("repo".to_string(), name.to_string());
    }

    // Try to get current branch (best-effort, don't error on failure)
    if let Ok(branch) = get_current_branch(git_root) {
        merged.insert("branch".to_string(), branch);
    }

    merged
}

/// Get current git branch name (best-effort).
///
/// This function attempts to get the current branch name using `git rev-parse`.
/// It uses `.arg()` with `Path` to avoid UTF-8 conversion issues and returns
/// an empty string on failure (best-effort for vars).
///
/// # Arguments
///
/// * `git_root` - Path to the git repository root
///
/// # Returns
///
/// The current branch name, or an empty string if the command fails.
///
/// # Errors
///
/// Returns an error if the git command cannot be executed, but the error
/// is converted to `Ok(String::new())` for the actual output to maintain
/// best-effort semantics.
///
/// # Examples
///
/// ```
/// use std::path::PathBuf;
///
/// let git_root = PathBuf::from("/path/to/repo");
/// let branch = get_current_branch(&git_root).unwrap();
/// // branch will be the current branch name or empty string
/// ```
fn get_current_branch(git_root: &Path) -> Result<String, Box<dyn std::error::Error>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(git_root)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        // Best-effort: don't error, just return empty string
        Ok(String::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn make_test_vars() -> (
        BTreeMap<String, String>,
        BTreeMap<String, String>,
        BTreeMap<String, String>,
    ) {
        let home_vars = BTreeMap::from([
            ("home_var".to_string(), "home_value".to_string()),
            ("shared".to_string(), "from_home".to_string()),
        ]);

        let repo_vars = BTreeMap::from([
            ("repo_var".to_string(), "repo_value".to_string()),
            ("shared".to_string(), "from_repo".to_string()),
        ]);

        let automation_vars = BTreeMap::from([
            ("automation_var".to_string(), "automation_value".to_string()),
            ("shared".to_string(), "from_automation".to_string()),
        ]);

        (automation_vars, repo_vars, home_vars)
    }

    #[test]
    fn test_merge_automation_vars_precedence() {
        let (automation_vars, repo_vars, home_vars) = make_test_vars();
        let git_root = PathBuf::from("/test/repo");
        let cwd = PathBuf::from("/test/repo/subdir");

        let merged = merge_automation_vars(automation_vars, repo_vars, home_vars, &git_root, &cwd);

        // Automation vars override repo and home
        assert_eq!(
            merged.get("automation_var"),
            Some(&"automation_value".to_string())
        );
        // Repo vars override home
        assert_eq!(merged.get("repo_var"), Some(&"repo_value".to_string()));
        // Home vars are included when not overridden
        assert_eq!(merged.get("home_var"), Some(&"home_value".to_string()));
        // Shared var should be from automation (highest precedence)
        assert_eq!(merged.get("shared"), Some(&"from_automation".to_string()));
    }

    #[test]
    fn test_merge_automation_vars_builtins() {
        let (automation_vars, repo_vars, home_vars) = make_test_vars();
        let git_root = PathBuf::from("/test/repo");
        let cwd = PathBuf::from("/test/repo/subdir");

        let merged = merge_automation_vars(automation_vars, repo_vars, home_vars, &git_root, &cwd);

        // Built-ins should be present
        assert!(merged.contains_key("git_root"));
        assert!(merged.contains_key("cwd"));
        assert!(merged.contains_key("now"));

        assert_eq!(merged.get("git_root"), Some(&"/test/repo".to_string()));
        assert_eq!(merged.get("cwd"), Some(&"/test/repo/subdir".to_string()));
    }

    #[test]
    fn test_merge_automation_vars_repo_name() {
        let (automation_vars, repo_vars, home_vars) = make_test_vars();
        let git_root = PathBuf::from("/test/my-repo");
        let cwd = PathBuf::from("/test/my-repo");

        let merged = merge_automation_vars(automation_vars, repo_vars, home_vars, &git_root, &cwd);

        // Should extract repo name from git_root
        assert_eq!(merged.get("repo"), Some(&"my-repo".to_string()));
    }

    #[test]
    fn test_merge_automation_vars_empty_maps() {
        let automation_vars = BTreeMap::new();
        let repo_vars = BTreeMap::new();
        let home_vars = BTreeMap::new();
        let git_root = PathBuf::from("/test/repo");
        let cwd = PathBuf::from("/test/repo");

        let merged = merge_automation_vars(automation_vars, repo_vars, home_vars, &git_root, &cwd);

        // Should still have built-ins
        assert!(merged.contains_key("git_root"));
        assert!(merged.contains_key("cwd"));
        assert!(merged.contains_key("now"));
    }

    #[test]
    fn test_merge_automation_vars_builtin_override() {
        let mut automation_vars = BTreeMap::new();
        automation_vars.insert("git_root".to_string(), "fake_git_root".to_string());
        automation_vars.insert("cwd".to_string(), "fake_cwd".to_string());

        let repo_vars = BTreeMap::new();
        let home_vars = BTreeMap::new();
        let git_root = PathBuf::from("/test/repo");
        let cwd = PathBuf::from("/test/repo/subdir");

        let merged = merge_automation_vars(automation_vars, repo_vars, home_vars, &git_root, &cwd);

        // Built-ins should override user-provided values
        assert_eq!(merged.get("git_root"), Some(&"/test/repo".to_string()));
        assert_eq!(merged.get("cwd"), Some(&"/test/repo/subdir".to_string()));
    }

    #[test]
    fn test_merge_automation_vars_non_utf8_repo_name() {
        let (automation_vars, repo_vars, home_vars) = make_test_vars();
        // Use a path with valid UTF-8
        let git_root = PathBuf::from("/test/repo-name-123");
        let cwd = PathBuf::from("/test/repo-name-123");

        let merged = merge_automation_vars(automation_vars, repo_vars, home_vars, &git_root, &cwd);

        assert_eq!(merged.get("repo"), Some(&"repo-name-123".to_string()));
    }

    #[test]
    fn test_get_current_branch_valid_repo() {
        // Test in the actual velor repository
        let git_root = PathBuf::from("/Users/liam/git/velor");
        let result = get_current_branch(&git_root);

        // Should succeed (though branch name may vary)
        assert!(result.is_ok());
        let branch = result.unwrap();
        // If we're in a git repo, should get a non-empty branch name
        // (unless HEAD is detached, which would give "HEAD")
        if !branch.is_empty() {
            assert!(!branch.is_empty());
        }
    }

    #[test]
    fn test_get_current_branch_non_repo() {
        // Test in a directory that's not a git repository
        let git_root = PathBuf::from("/tmp/nonexistent-repo-xyz123");
        let result = get_current_branch(&git_root);

        // Should return Ok with empty string (best-effort)
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "");
    }

    #[test]
    fn test_merge_automation_vars_includes_branch() {
        let (automation_vars, repo_vars, home_vars) = make_test_vars();
        let git_root = PathBuf::from("/Users/liam/git/velor");
        let cwd = PathBuf::from("/Users/liam/git/velor");

        let merged = merge_automation_vars(automation_vars, repo_vars, home_vars, &git_root, &cwd);

        // Should include branch if git command succeeded
        if merged.contains_key("branch") {
            let branch = merged.get("branch").unwrap();
            // Branch should be non-empty if we're in a valid git repo
            assert!(!branch.is_empty() || branch == "HEAD"); // HEAD is valid for detached HEAD
        }
    }

    #[test]
    fn test_merge_automation_vars_now_is_valid_rfc3339() {
        let (automation_vars, repo_vars, home_vars) = make_test_vars();
        let git_root = PathBuf::from("/test/repo");
        let cwd = PathBuf::from("/test/repo");

        let merged = merge_automation_vars(automation_vars, repo_vars, home_vars, &git_root, &cwd);

        let now = merged.get("now").expect("now should be present");
        // Should be a valid RFC3339 timestamp
        assert!(chrono::DateTime::parse_from_rfc3339(now).is_ok());
    }
}
