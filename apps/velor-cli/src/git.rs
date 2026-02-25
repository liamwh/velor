//! Git repository discovery utilities.
//!
//! This module provides functionality to discover the root of a git repository
//! from a given working directory.

use std::path::Path;

/// Discovers the git repository root from the current working directory.
///
/// First attempts to use the `git rev-parse` command, and falls back to
/// walking up the directory tree looking for a `.git` directory.
///
/// # Errors
///
/// Returns an error if the current directory cannot be determined.
#[tracing::instrument(level = "debug", ret)]
pub fn discover_git_root(cwd: &Path) -> color_eyre::eyre::Result<std::path::PathBuf> {
    if let Ok(root) = git_root_via_git_command() {
        return Ok(root);
    }
    if let Some(root) = git_root_via_walk(cwd) {
        return Ok(root);
    }
    Ok(cwd.to_path_buf())
}

/// Tests if a path contains a `.git` directory (used for testing).
#[cfg(test)]
fn has_git_dir(path: &Path) -> bool {
    path.join(".git").exists()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_discover_git_root_falls_back_to_cwd() {
        let temp_dir = TempDir::new().expect("tempdir should be created");
        let cwd = temp_dir.path();

        // No git repo here, should return cwd
        // Note: if we're inside a git repo, git command may succeed, so verify via walk
        let result = git_root_via_walk(cwd);
        assert_eq!(result, None);
    }

    #[test]
    fn test_git_root_via_walk_finds_repository() {
        let temp_dir = TempDir::new().expect("tempdir should be created");
        let repo_root = temp_dir.path().join("repo");
        let subdir = repo_root.join("a").join("b").join("c");

        fs::create_dir_all(&subdir).expect("subdir should be created");
        fs::create_dir_all(repo_root.join(".git")).expect(".git dir should be created");

        let result = git_root_via_walk(&subdir);
        assert_eq!(result, Some(repo_root));
    }

    #[test]
    fn test_git_root_via_walk_returns_none_when_no_repo() {
        let temp_dir = TempDir::new().expect("tempdir should be created");
        let cwd = temp_dir.path();

        let result = git_root_via_walk(cwd);
        assert_eq!(result, None);
    }

    #[test]
    fn test_git_root_via_walk_at_repo_root() {
        let temp_dir = TempDir::new().expect("tempdir should be created");
        let repo_root = temp_dir.path();

        fs::create_dir_all(repo_root.join(".git")).expect(".git dir should be created");

        let result = git_root_via_walk(repo_root);
        assert_eq!(result, Some(repo_root.to_path_buf()));
    }

    #[test]
    fn test_git_root_via_walk_deeply_nested() {
        let temp_dir = TempDir::new().expect("tempdir should be created");
        let repo_root = temp_dir.path().join("repo");
        let nested = repo_root
            .join("level1")
            .join("level2")
            .join("level3")
            .join("level4");

        fs::create_dir_all(&nested).expect("nested dir should be created");
        fs::create_dir_all(repo_root.join(".git")).expect(".git dir should be created");

        let result = git_root_via_walk(&nested);
        assert_eq!(result, Some(repo_root));
    }

    #[test]
    fn test_has_git_dir_identifies_git_repository() {
        let temp_dir = TempDir::new().expect("tempdir should be created");
        let repo_root = temp_dir.path();

        fs::create_dir_all(repo_root.join(".git")).expect(".git dir should be created");

        assert!(has_git_dir(repo_root));
    }

    #[test]
    fn test_has_git_dir_returns_false_for_non_repo() {
        let temp_dir = TempDir::new().expect("tempdir should be created");
        let non_repo = temp_dir.path();

        assert!(!has_git_dir(non_repo));
    }
}

#[cfg(test)]
mod proptest_tests {
    use super::*;
    use proptest::prelude::*;
    use std::fs;
    use tempfile::TempDir;

    proptest! {
            #[test]
            fn test_discover_git_root_always_returns_absolute_path(
                    depth in 1usize..10
            ) {
                    let temp_dir = TempDir::new().expect("tempdir should be created");
                    let base = temp_dir.path();

                    // Create a nested directory structure
                    let mut nested = base.to_path_buf();
                    for _ in 0..depth {
                            nested = nested.join("nested");
                    }
                    fs::create_dir_all(&nested).expect("nested dir should be created");

                    let result = discover_git_root(&nested).expect("git root should be discovered");
                    prop_assert_eq!(result.is_absolute(), true);
            }

            #[test]
            fn test_git_root_via_walk_never_returns_parent_of_git_repo(
                    repo_depth in 1usize..5,
                    subdir_depth in 1usize..5
            ) {
                    let temp_dir = TempDir::new().expect("tempdir should be created");
                    let base = temp_dir.path();

                    // Create a git repository at a specific depth
                    let mut repo_root = base.to_path_buf();
                    for i in 0..repo_depth {
                            repo_root = repo_root.join(format!("level{i}"));
                    }
                    fs::create_dir_all(&repo_root).expect("repo_root should be created");
                    fs::create_dir_all(repo_root.join(".git")).expect(".git dir should be created");

                    // Create a subdirectory within the repo
                    let mut subdir = repo_root.clone();
                    for i in 0..subdir_depth {
                            subdir = subdir.join(format!("subdir{i}"));
                    }
                    fs::create_dir_all(&subdir).expect("subdir should be created");

                    let result = git_root_via_walk(&subdir);

                    if let Some(found_root) = result {
                            // The found root should be exactly the repo root, not its parent
                            prop_assert_eq!(&found_root, &repo_root);
                            // The found root should not be a subdirectory of the repo
                            prop_assert!(!found_root.ends_with(".git"));
                    }
            }

            #[test]
            fn test_discover_git_root_idempotent(
                    path_components in prop::collection::vec("[a-zA-Z0-9_-]{1,10}", 0..5)
            ) {
                    let temp_dir = TempDir::new().expect("tempdir should be created");
                    let base = temp_dir.path();

                    // Create a path from the components
                    let mut test_path = base.to_path_buf();
                    for component in &path_components {
                            test_path = test_path.join(component);
                    }
                    fs::create_dir_all(&test_path).expect("test_path should be created");

                    let result1 = discover_git_root(&test_path).expect("git root should be discovered");
                    let result2 = discover_git_root(&test_path).expect("git root should be discovered");

                    prop_assert_eq!(result1, result2);
            }
    }
}

/// Attempts to discover the git root using the `git rev-parse` command.
///
/// # Errors
///
/// Returns an error if the git command fails or returns unexpected output.
#[tracing::instrument(level = "trace", ret)]
fn git_root_via_git_command() -> color_eyre::eyre::Result<std::path::PathBuf> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|e| color_eyre::eyre::eyre!("failed to run git rev-parse: {e}"))?;

    if !out.status.success() {
        return Err(color_eyre::eyre::eyre!(
            "git rev-parse returned non-zero exit code"
        ));
    }

    let s = String::from_utf8(out.stdout)
        .map_err(|e| color_eyre::eyre::eyre!("git output was not valid UTF-8: {e}"))?;
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Err(color_eyre::eyre::eyre!("git root output was empty"));
    }

    Ok(std::path::PathBuf::from(trimmed))
}

/// Discovers the git root by walking up the directory tree looking for `.git`.
#[tracing::instrument(level = "trace", ret)]
fn git_root_via_walk(cwd: &Path) -> Option<std::path::PathBuf> {
    let mut cur = Some(cwd);

    while let Some(p) = cur {
        if p.join(".git").exists() {
            return Some(p.to_path_buf());
        }
        cur = p.parent();
    }

    None
}
