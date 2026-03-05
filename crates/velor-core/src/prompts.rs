// Copyright (c) 2024 Liam S. (velor)
//
// This software is licensed under the terms of the UNLICENSE.
// You should have received a copy of the UNLICENSE with this program.
// If not, see https://unlicense.org/

//! File-based prompt system for Velor.
//!
//! This module implements support for defining prompts as markdown files in
//! `.velor/prompts/` directories. Each prompt file contains YAML frontmatter
//! with metadata and markdown template content. Prompts are loaded from both
//! home (`~/.velor/prompts/`) and repository (`{git_root}/.velor/prompts/`)
//! locations, with repository prompts taking precedence.

use color_eyre::Result;
use color_eyre::eyre::{WrapErr, eyre};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use tokio::fs;
use tokio::sync::Mutex;

/// Maximum prompt file size (100 KB) to prevent resource exhaustion.
const MAX_PROMPT_FILE_SIZE: usize = 100 * 1024;

/// Maximum number of prompts to prevent overwhelming the system.
const MAX_TOTAL_PROMPTS: usize = 50;

/// A single prompt loaded from a `.md` file.
///
/// Prompts contain both metadata (from YAML frontmatter) and content
/// (markdown template with MiniJinja variables).
#[derive(Debug, Clone)]
pub struct Prompt {
    /// Filename without the `.md` extension (e.g., "debug" from "debug.md").
    pub name: String,
    /// Human-readable description from frontmatter.
    pub description: String,
    /// Optional override of the completion token for this prompt.
    pub complete_token: Option<String>,
    /// Markdown content after the frontmatter `---` delimiter.
    pub content: String,
}

/// YAML frontmatter from a prompt file.
#[derive(Debug, Clone, Deserialize)]
struct PromptFrontmatter {
    /// Human-readable description of what this prompt does.
    #[serde(default = "default_description")]
    description: String,

    /// Optional override of the completion token for this prompt.
    #[serde(default)]
    complete_token: Option<String>,
}

/// Default value for description field (uses the prompt name).
fn default_description() -> String {
    "Unnamed prompt".to_string()
}

/// Cache for prompts loaded once per run.
///
/// Prompts are loaded from both home and repository directories,
/// with repository prompts taking precedence over home prompts.
#[derive(Debug)]
pub struct PromptCache {
    /// Home directory path (`~/.velor`).
    home_dir: PathBuf,
    /// Optional git repository root (for repo-level prompts).
    repo_dir: Option<PathBuf>,
    /// Cached home prompts (loaded once on first access).
    home_cache: Mutex<Option<BTreeMap<String, Prompt>>>,
    /// Cached repo prompts (loaded once on first access).
    repo_cache: Mutex<Option<BTreeMap<String, Prompt>>>,
}

impl PromptCache {
    /// Creates a new PromptCache.
    ///
    /// # Arguments
    ///
    /// * `home_dir` - Path to the home directory (`~/.velor`).
    /// * `repo_dir` - Optional path to the git repository root.
    #[must_use]
    pub fn new(home_dir: PathBuf, repo_dir: Option<PathBuf>) -> Self {
        Self {
            home_dir,
            repo_dir,
            home_cache: Mutex::new(None),
            repo_cache: Mutex::new(None),
        }
    }

    /// Returns all cached prompts, loading them if necessary.
    ///
    /// Home and repository prompts are merged, with repository prompts
    /// taking precedence over home prompts with the same name.
    ///
    /// # Errors
    ///
    /// Returns an error if prompt discovery fails.
    #[tracing::instrument(level = "debug", ret, err, skip(self))]
    pub async fn get(&self) -> Result<BTreeMap<String, Prompt>> {
        // Load home prompts
        if self.home_cache.lock().await.is_none() {
            let home_prompts = self.discover_prompts(&self.home_dir).await?;
            *self.home_cache.lock().await = Some(home_prompts);
        }

        // Load repo prompts (if repo directory exists)
        let repo_prompts = if let Some(ref repo_dir) = self.repo_dir {
            if self.repo_cache.lock().await.is_none() {
                let discovered = self.discover_prompts(repo_dir).await?;
                *self.repo_cache.lock().await = Some(discovered);
            }
            self.repo_cache
                .lock()
                .await
                .as_ref()
                .cloned()
                .unwrap_or_default()
        } else {
            BTreeMap::new()
        };

        // Merge prompts (repo takes precedence)
        let home_prompts = self.home_cache.lock().await;
        let home = home_prompts.as_ref().cloned().unwrap_or_default();
        let mut result = home;
        for (name, prompt) in repo_prompts {
            result.insert(name, prompt);
        }

        Ok(result)
    }

    /// Fetches a single prompt by name.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The prompts cannot be loaded
    /// - The prompt name is not found
    #[tracing::instrument(level = "debug", ret, err, skip(self))]
    pub async fn get_by_name(&self, name: &str) -> Result<Prompt> {
        let all_prompts = self.get().await?;
        all_prompts
            .get(name)
            .cloned()
            .ok_or_else(|| eyre!("prompt '{name}' not found in home or repo directories"))
    }

    /// Discovers all prompts in a directory.
    ///
    /// # Arguments
    ///
    /// * `base_dir` - Base directory containing the `prompts/` subdirectory.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The directory cannot be read
    /// - A prompt file exceeds size limits
    /// - Frontmatter parsing fails
    #[tracing::instrument(level = "debug", ret, err, fields(base_dir = %base_dir.display()))]
    async fn discover_prompts(&self, base_dir: &Path) -> Result<BTreeMap<String, Prompt>> {
        let prompts_dir = base_dir.join("prompts");

        // If the prompts directory doesn't exist, return an empty map
        if !prompts_dir.exists() {
            tracing::debug!(
                "prompts directory does not exist: {}",
                prompts_dir.display()
            );
            return Ok(BTreeMap::new());
        }

        // Read directory entries
        let mut entries = fs::read_dir(&prompts_dir).await.wrap_err_with(|| {
            format!(
                "Failed to read prompts directory: {}",
                prompts_dir.display()
            )
        })?;

        let mut prompts = BTreeMap::new();

        // Process each entry
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

            // Only process .md files
            if path.extension().and_then(|s| s.to_str()) != Some("md") {
                continue;
            }

            // Parse the prompt file
            let prompt = parse_prompt_file(&path).await?;

            // Check if we've exceeded the limit
            if prompts.len() >= MAX_TOTAL_PROMPTS {
                tracing::warn!(
                    "Reached maximum prompt limit ({MAX_TOTAL_PROMPTS}), skipping: {}",
                    path.display()
                );
                break;
            }

            prompts.insert(prompt.name.clone(), prompt);
        }

        tracing::debug!(
            "Discovered {} prompts in {}",
            prompts.len(),
            prompts_dir.display()
        );
        Ok(prompts)
    }
}

/// Parses a single prompt file.
///
/// # Arguments
///
/// * `path` - Path to the `.md` file.
///
/// # Errors
///
/// Returns an error if:
/// - The file cannot be read
/// - The file size exceeds `MAX_PROMPT_FILE_SIZE`
/// - Frontmatter parsing fails
/// - The filename has no extension
#[tracing::instrument(level = "debug", ret, err, fields(path = %path.display()))]
async fn parse_prompt_file(path: &Path) -> Result<Prompt> {
    // Read file content
    let content = fs::read_to_string(path)
        .await
        .wrap_err_with(|| format!("Failed to read prompt file: {}", path.display()))?;

    // Check file size
    if content.len() > MAX_PROMPT_FILE_SIZE {
        return Err(eyre!(
            "Prompt file too large: {} (max {} bytes)",
            path.display(),
            MAX_PROMPT_FILE_SIZE
        ));
    }

    // Extract prompt name from filename (without .md extension)
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| eyre!("Invalid prompt filename: {}", path.display()))?
        .to_string();

    // Split frontmatter from content
    let (yaml, markdown) = split_frontmatter(&content);

    // Parse frontmatter
    let frontmatter: PromptFrontmatter = if yaml.is_empty() {
        // No frontmatter, use defaults
        PromptFrontmatter {
            description: name.clone(),
            complete_token: None,
        }
    } else {
        serde_yaml::from_str(&yaml).wrap_err_with(|| {
            format!(
                "Failed to parse frontmatter in prompt file: {}",
                path.display()
            )
        })?
    };

    Ok(Prompt {
        name,
        description: frontmatter.description,
        complete_token: frontmatter.complete_token,
        content: markdown,
    })
}

/// Splits frontmatter from markdown content.
///
/// # Format
/// ```text
/// ---
/// # yaml: frontmatter
/// ---
/// markdown content
/// ```
///
/// # Algorithm
/// 1. Skip leading empty lines
/// 2. First non-empty line must be exactly "---"
/// 3. Scan subsequent lines until a line that trims to "---"
/// 4. Everything between is YAML; after is markdown
///
/// This prevents false matches from YAML values or markdown containing "---".
///
/// # Errors
///
/// This function never returns an error; it always returns a valid result.
/// If no frontmatter is found, the entire content is returned as markdown.
#[must_use]
pub fn split_frontmatter(content: &str) -> (String, String) {
    let mut lines = content.lines().peekable();

    // Skip leading empty lines
    while lines.peek().map(|l| l.trim().is_empty()).unwrap_or(false) {
        lines.next();
    }

    // First non-empty line must be opening delimiter
    let first = match lines.next() {
        Some(f) => f,
        // File is empty or contains only whitespace - return empty result
        None => return (String::new(), String::new()),
    };

    if first.trim() != "---" {
        // No frontmatter found, return entire content as markdown
        return (String::new(), content.to_string());
    }

    // Collect YAML lines until closing delimiter
    let mut yaml_lines = Vec::new();
    for line in &mut lines {
        let trimmed = line.trim();
        if trimmed == "---" {
            // Found closing delimiter
            let yaml = yaml_lines.join("\n");
            let markdown = lines.collect::<Vec<_>>().join("\n");
            return (yaml, markdown);
        }
        yaml_lines.push(line);
    }

    // No closing delimiter found, treat entire content as markdown
    (String::new(), content.to_string())
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    /// Test split_frontmatter with valid frontmatter.
    #[test]
    fn test_split_frontmatter_valid() {
        let content = r#"---
description: Test prompt
complete_token: "<promise>DONE</promise>"
---
# Test Prompt

This is the content."#;

        let (yaml, markdown) = split_frontmatter(content);
        assert!(yaml.contains("description: Test prompt"));
        assert!(yaml.contains("complete_token"));
        assert!(markdown.contains("# Test Prompt"));
        assert!(markdown.contains("This is the content."));
    }

    /// Test split_frontmatter with no frontmatter.
    #[test]
    fn test_split_frontmatter_none() {
        let content = "Just some markdown content";

        let (yaml, markdown) = split_frontmatter(content);
        assert!(yaml.is_empty());
        assert_eq!(markdown, content);
    }

    /// Test split_frontmatter with leading empty lines.
    #[test]
    fn test_split_frontmatter_leading_empty() {
        let content = "\n\n---\ndescription: Test\n---\ncontent";

        let (yaml, markdown) = split_frontmatter(content);
        assert!(yaml.contains("description: Test"));
        assert_eq!(markdown.trim(), "content");
    }

    /// Test split_frontmatter with unclosed frontmatter.
    #[test]
    fn test_split_frontmatter_unclosed() {
        let content = "---\ndescription: Test\nNo closing delimiter";

        let (yaml, markdown) = split_frontmatter(content);
        // Should treat entire content as markdown when no closing delimiter
        assert!(yaml.is_empty());
        assert_eq!(markdown, content);
    }

    /// Test split_frontmatter with empty content.
    #[test]
    fn test_split_frontmatter_empty() {
        let content = "";

        let (yaml, markdown) = split_frontmatter(content);
        assert!(yaml.is_empty());
        assert!(markdown.is_empty());
    }

    /// Test split_frontmatter with only whitespace.
    #[test]
    fn test_split_frontmatter_whitespace_only() {
        let content = "   \n\n  \n  ";

        let (yaml, markdown) = split_frontmatter(content);
        assert!(yaml.is_empty());
        assert!(markdown.is_empty());
    }

    /// Test Prompt creation with all fields.
    #[test]
    fn test_prompt_creation() {
        let prompt = Prompt {
            name: "test".to_string(),
            description: "Test prompt".to_string(),
            complete_token: Some("<promise>DONE</promise>".to_string()),
            content: "Hello {{name}}".to_string(),
        };

        assert_eq!(prompt.name, "test");
        assert_eq!(prompt.description, "Test prompt");
        assert_eq!(
            prompt.complete_token,
            Some("<promise>DONE</promise>".to_string())
        );
        assert_eq!(prompt.content, "Hello {{name}}");
    }

    /// Test Prompt creation with optional complete_token as None.
    #[test]
    fn test_prompt_no_complete_token() {
        let prompt = Prompt {
            name: "test".to_string(),
            description: "Test prompt".to_string(),
            complete_token: None,
            content: "Hello {{name}}".to_string(),
        };

        assert_eq!(prompt.name, "test");
        assert_eq!(prompt.description, "Test prompt");
        assert!(prompt.complete_token.is_none());
        assert_eq!(prompt.content, "Hello {{name}}");
    }

    /// Test PromptFrontmatter default values.
    #[test]
    fn test_prompt_frontmatter_defaults() {
        let yaml = r#"description: Test prompt"#;
        let frontmatter: PromptFrontmatter =
            serde_yaml::from_str(yaml).expect("valid YAML frontmatter should parse successfully");

        assert_eq!(frontmatter.description, "Test prompt");
        assert!(frontmatter.complete_token.is_none());
    }

    /// Test PromptFrontmatter with all fields.
    #[test]
    fn test_prompt_frontmatter_all_fields() {
        let yaml = r#"
description: Test prompt
complete_token: "<promise>DONE</promise>"
"#;
        let frontmatter: PromptFrontmatter =
            serde_yaml::from_str(yaml).expect("valid YAML frontmatter should parse successfully");

        assert_eq!(frontmatter.description, "Test prompt");
        assert_eq!(
            frontmatter.complete_token,
            Some("<promise>DONE</promise>".to_string())
        );
    }

    /// Test PromptFrontmatter with empty frontmatter (should use defaults).
    #[test]
    fn test_prompt_frontmatter_empty() {
        let yaml = "";
        let frontmatter: PromptFrontmatter =
            serde_yaml::from_str(yaml).expect("valid YAML frontmatter should parse successfully");

        assert_eq!(frontmatter.description, "Unnamed prompt");
        assert!(frontmatter.complete_token.is_none());
    }
}

#[cfg(test)]
mod proptest_tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn test_split_frontmatter_preserves_content(content in ".*") {
            let (_yaml, markdown) = split_frontmatter(&content);
            // Should never error
            // Markdown should always be non-empty unless content is whitespace-only
            // When frontmatter is not found, markdown == content
            // When frontmatter is found, markdown is the content after closing ---
            // For whitespace-only content, markdown may be empty
            let is_whitespace_only = content.trim().is_empty();
            prop_assert!(!markdown.is_empty() || is_whitespace_only);
        }

        #[test]
        fn test_prompt_name_roundtrip(name in "[a-zA-Z0-9_-]{1,20}") {
            let prompt = Prompt {
                name: name.clone(),
                description: "Description".to_string(),
                complete_token: None,
                content: "Content".to_string(),
            };
            prop_assert_eq!(prompt.name, name);
        }

        #[test]
        fn test_prompt_description_roundtrip(desc in "[a-zA-Z0-9 ]{0,100}") {
            let prompt = Prompt {
                name: "test".to_string(),
                description: desc.clone(),
                complete_token: None,
                content: "Content".to_string(),
            };
            prop_assert_eq!(prompt.description, desc);
        }

        #[test]
        fn test_prompt_content_preserves_template(template in "[a-zA-Z0-9 {{}}]{0,200}") {
            let prompt = Prompt {
                name: "test".to_string(),
                description: "Description".to_string(),
                complete_token: None,
                content: template.clone(),
            };
            prop_assert_eq!(prompt.content, template);
        }

        #[test]
        fn test_complete_token_optional(token in "[a-zA-Z0-9<>]{0,50}") {
            let complete_token = if token.is_empty() {
                None
            } else {
                Some(token.clone())
            };
            let prompt = Prompt {
                name: "test".to_string(),
                description: "Description".to_string(),
                complete_token: complete_token.clone(),
                content: "Content".to_string(),
            };
            prop_assert_eq!(prompt.complete_token, complete_token);
        }
    }
}
