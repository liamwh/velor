// Copyright (c) 2024 Liam S. (velor)
//
// This software is licensed under the terms of the UNLICENSE.
// You should have received a copy of the UNLICENSE with this program.
// If not, see https://unlicense.org/

//! Integration tests for prompt name discovery.
//!
//! These tests verify the `discover_prompt_names()` function correctly
//! discovers prompt names from all sources with proper precedence handling.

use std::path::PathBuf;
use tempfile::TempDir;
use velor_core::config::FileConfig;
use velor_core::prompts::discovery::discover_prompt_names;

/// Serial tests that manipulate environment variables must run sequentially.
mod serial {
    use super::*;
    use serial_test::serial;

    /// Test that repo prompts override home prompts with the same name.
    #[tokio::test]
    #[serial]
    async fn test_repo_prompts_override_home() {
        let temp = TempDir::new().expect("failed to create temp dir");

        // Home has "common"
        let home = temp.path().join("home/.velor/prompts");
        tokio::fs::create_dir_all(&home)
            .await
            .expect("failed to create home prompts dir");
        tokio::fs::write(home.join("common.md"), "content")
            .await
            .expect("failed to write home prompt");

        // Repo has "common" (should win) and "repo-only"
        let repo = temp.path().join("repo/.velor/prompts");
        tokio::fs::create_dir_all(&repo)
            .await
            .expect("failed to create repo prompts dir");
        tokio::fs::write(repo.join("common.md"), "content")
            .await
            .expect("failed to write repo common prompt");
        tokio::fs::write(repo.join("repo-only.md"), "content")
            .await
            .expect("failed to write repo-only prompt");

        let cfg = FileConfig::default();
        let git_root = temp.path().join("repo");

        // Mock home_dir by setting the HOME environment variable
        let original_home = std::env::var("HOME").ok();
        unsafe { std::env::set_var("HOME", temp.path().join("home")) };

        let names = discover_prompt_names(Some(&git_root), &cfg)
            .await
            .expect("discovery should succeed");

        // Restore original HOME
        if let Some(home) = original_home {
            unsafe { std::env::set_var("HOME", home) };
        } else {
            unsafe { std::env::remove_var("HOME") };
        }

        // Should have common (from repo), repo-only, but no duplicate
        assert!(
            names.contains(&"common".to_string()),
            "expected common prompt"
        );
        assert!(
            names.contains(&"repo-only".to_string()),
            "expected repo-only prompt"
        );
        assert_eq!(names.len(), 2, "expected exactly 2 prompts");
    }

    /// Test shadowing semantics across all three sources.
    ///
    /// config: foo, bar
    /// home: bar.md, baz.md
    /// repo: baz.md, qux.md
    /// Expected: foo (config), bar (home), baz (repo), qux (repo)
    #[tokio::test]
    #[serial]
    async fn test_shadowing_semantics() {
        let temp = TempDir::new().expect("failed to create temp dir");

        let mut cfg = FileConfig::default();
        cfg.prompts.insert(
            "foo".to_string(),
            velor_core::config::PromptDef::Inline("test".to_string()),
        );
        cfg.prompts.insert(
            "bar".to_string(),
            velor_core::config::PromptDef::Inline("test".to_string()),
        );

        let home = temp.path().join("home/.velor/prompts");
        tokio::fs::create_dir_all(&home)
            .await
            .expect("failed to create home prompts dir");
        tokio::fs::write(home.join("bar.md"), "content")
            .await
            .expect("failed to write bar.md");
        tokio::fs::write(home.join("baz.md"), "content")
            .await
            .expect("failed to write baz.md");

        let repo = temp.path().join("repo/.velor/prompts");
        tokio::fs::create_dir_all(&repo)
            .await
            .expect("failed to create repo prompts dir");
        tokio::fs::write(repo.join("baz.md"), "content")
            .await
            .expect("failed to write baz.md");
        tokio::fs::write(repo.join("qux.md"), "content")
            .await
            .expect("failed to write qux.md");

        let git_root = temp.path().join("repo");

        // Mock home_dir by setting the HOME environment variable
        let original_home = std::env::var("HOME").ok();
        unsafe { std::env::set_var("HOME", temp.path().join("home")) };

        let names = discover_prompt_names(Some(&git_root), &cfg)
            .await
            .expect("discovery should succeed");

        // Restore original HOME
        if let Some(home) = original_home {
            unsafe { std::env::set_var("HOME", home) };
        } else {
            unsafe { std::env::remove_var("HOME") };
        }

        assert_eq!(
            names,
            vec!["bar", "baz", "foo", "qux"],
            "expected correct precedence and sorting"
        );
    }

    /// Test behavior when git_root is None (outside of a git repository).
    #[tokio::test]
    #[serial]
    async fn test_no_git_root_uses_home_and_config_only() {
        let temp = TempDir::new().expect("failed to create temp dir");

        let mut cfg = FileConfig::default();
        cfg.prompts.insert(
            "config-prompt".to_string(),
            velor_core::config::PromptDef::Inline("test".to_string()),
        );

        let home = temp.path().join("home/.velor/prompts");
        tokio::fs::create_dir_all(&home)
            .await
            .expect("failed to create home prompts dir");
        tokio::fs::write(home.join("home-prompt.md"), "content")
            .await
            .expect("failed to write home prompt");

        // Mock home_dir by setting the HOME environment variable
        let original_home = std::env::var("HOME").ok();
        unsafe { std::env::set_var("HOME", temp.path().join("home")) };

        let names = discover_prompt_names(None, &cfg)
            .await
            .expect("discovery should succeed");

        // Restore original HOME
        if let Some(home) = original_home {
            unsafe { std::env::set_var("HOME", home) };
        } else {
            unsafe { std::env::remove_var("HOME") };
        }

        assert_eq!(
            names,
            vec!["config-prompt", "home-prompt"],
            "expected home and config prompts only"
        );
    }
}

/// Test that an empty result is returned when no prompt sources exist.
#[tokio::test]
async fn test_empty_when_no_sources() {
    let cfg = FileConfig::default();
    let names = discover_prompt_names(None, &cfg)
        .await
        .expect("discovery should succeed with no sources");
    assert!(names.is_empty(), "expected no prompt names");
}

/// Test that only config prompts are returned when no file sources exist.
#[tokio::test]
async fn test_config_prompts_only() {
    let mut cfg = FileConfig::default();
    cfg.prompts.insert(
        "alpha".to_string(),
        velor_core::config::PromptDef::Inline("test".to_string()),
    );
    cfg.prompts.insert(
        "zebra".to_string(),
        velor_core::config::PromptDef::Inline("test".to_string()),
    );

    let names = discover_prompt_names(None, &cfg)
        .await
        .expect("discovery should succeed");
    assert_eq!(
        names,
        vec!["alpha", "zebra"],
        "expected config prompts in alphabetical order"
    );
}

/// Test that results are alphabetically sorted regardless of source precedence.
#[tokio::test]
async fn test_alphabetic_sorting() {
    let temp = TempDir::new().expect("failed to create temp dir");

    let git_root = temp.path().join("repo");
    let repo_prompts = git_root.join(".velor/prompts");
    tokio::fs::create_dir_all(&repo_prompts)
        .await
        .expect("failed to create repo prompts dir");

    // Create in non-alphabetic order
    for name in ["zebra", "alpha", "beta"] {
        tokio::fs::write(repo_prompts.join(format!("{name}.md")), "content")
            .await
            .expect("failed to write prompt");
    }

    let cfg = FileConfig::default();

    let names = discover_prompt_names(Some(&git_root), &cfg)
        .await
        .expect("discovery should succeed");

    assert_eq!(
        names,
        vec!["alpha", "beta", "zebra"],
        "expected alphabetical sorting"
    );
}

/// Test that missing directories return empty results (not errors).
#[tokio::test]
async fn test_missing_directory_returns_empty() {
    let cfg = FileConfig::default();
    let non_existent = PathBuf::from("/tmp/velor-test-nonexistent-12345");

    let names = discover_prompt_names(Some(&non_existent), &cfg)
        .await
        .expect("discovery should succeed with missing directory");
    assert!(
        names.is_empty(),
        "expected no prompts from non-existent directory"
    );
}

/// Test that non-.md files are ignored.
#[tokio::test]
async fn test_non_md_files_ignored() {
    let temp = TempDir::new().expect("failed to create temp dir");

    let git_root = temp.path().join("repo");
    let repo_prompts = git_root.join(".velor/prompts");
    tokio::fs::create_dir_all(&repo_prompts)
        .await
        .expect("failed to create repo prompts dir");

    // Should be ignored
    tokio::fs::write(repo_prompts.join("readme.txt"), "content")
        .await
        .expect("failed to write readme.txt");
    tokio::fs::write(repo_prompts.join(".hidden"), "content")
        .await
        .expect("failed to write .hidden");

    // Should be included
    tokio::fs::write(repo_prompts.join("valid.md"), "content")
        .await
        .expect("failed to write valid.md");
    tokio::fs::write(repo_prompts.join("UPPERCASE.MD"), "content")
        .await
        .expect("failed to write UPPERCASE.MD");

    let cfg = FileConfig::default();

    let names = discover_prompt_names(Some(&git_root), &cfg)
        .await
        .expect("discovery should succeed");

    // Case-insensitive extension matching: .MD and .md are both valid
    assert_eq!(
        names,
        vec!["UPPERCASE", "valid"],
        "expected only .md files with case-insensitive matching"
    );
}

/// Test that .MD and .Md extensions are recognized (case-insensitive).
#[tokio::test]
async fn test_case_insensitive_extension() {
    let temp = TempDir::new().expect("failed to create temp dir");

    let git_root = temp.path().join("repo");
    let repo_prompts = git_root.join(".velor/prompts");
    tokio::fs::create_dir_all(&repo_prompts)
        .await
        .expect("failed to create repo prompts dir");

    // Test various case combinations
    tokio::fs::write(repo_prompts.join("lower.md"), "content")
        .await
        .expect("failed to write lower.md");
    tokio::fs::write(repo_prompts.join("upper.MD"), "content")
        .await
        .expect("failed to write upper.MD");
    tokio::fs::write(repo_prompts.join("mixed.Md"), "content")
        .await
        .expect("failed to write mixed.Md");

    let cfg = FileConfig::default();

    let names = discover_prompt_names(Some(&git_root), &cfg)
        .await
        .expect("discovery should succeed");

    assert_eq!(
        names,
        vec!["lower", "mixed", "upper"],
        "expected case-insensitive extension matching"
    );
}

/// Test that only file stems (names without extension) are returned.
#[tokio::test]
async fn test_only_file_stems_returned() {
    let temp = TempDir::new().expect("failed to create temp dir");

    let git_root = temp.path().join("repo");
    let repo_prompts = git_root.join(".velor/prompts");
    tokio::fs::create_dir_all(&repo_prompts)
        .await
        .expect("failed to create repo prompts dir");

    tokio::fs::write(repo_prompts.join("my-prompt.md"), "content")
        .await
        .expect("failed to write prompt");
    tokio::fs::write(repo_prompts.join("another.prompt.md"), "content")
        .await
        .expect("failed to write prompt");

    let cfg = FileConfig::default();

    let names = discover_prompt_names(Some(&git_root), &cfg)
        .await
        .expect("discovery should succeed");

    assert_eq!(
        names,
        vec!["another.prompt", "my-prompt"],
        "expected file stems without .md extension"
    );
}
