//! Plan subcommand implementation.
//!
//! This module provides AI-powered plan generation and review using OpenAI's API.
//! It reads spec files and generates detailed implementation plans.

use color_eyre::eyre::WrapErr;
use color_eyre::eyre::{self, Result};
use std::path::{Path, PathBuf};
use tracing::{debug, instrument};

/// Configuration for running the plan subcommand.
#[derive(Debug, Clone)]
pub struct PlanRunConfig {
    /// Path to the specs directory.
    pub specs_dir: PathBuf,
    /// Maximum number of refinement iterations (for future iterative refinement feature).
    #[allow(dead_code)]
    pub max_iterations: u32,
    /// OpenAI API key.
    pub api_key: String,
    /// OpenAI model to use.
    pub model: String,
    /// Optional custom OpenAI base URL.
    pub base_url: Option<String>,
    /// Whether to use a dry run (no API calls).
    pub dry_run: bool,
}

/// A spec file discovered in the specs directory.
#[derive(Debug, Clone)]
pub struct SpecFile {
    /// The file path.
    pub path: PathBuf,
    /// The file name (without extension).
    pub name: String,
    /// The file content.
    pub content: String,
}

/// Result from a plan generation or review.
#[derive(Debug, Clone)]
pub struct PlanResult {
    /// The generated or refined plan content.
    pub content: String,
    /// Number of iterations used (for future iterative refinement feature).
    #[allow(dead_code)]
    pub iterations: u32,
}

/// Discovers and reads all spec files from the specs directory.
///
/// # Errors
///
/// Returns an error if the specs directory doesn't exist or cannot be read.
#[instrument(level = "debug", ret, err, fields(specs_dir = %specs_dir.display()))]
pub fn discover_specs(specs_dir: &Path) -> Result<Vec<SpecFile>> {
    if !specs_dir.exists() {
        return Err(eyre::eyre!(
            "specs directory not found: {}",
            specs_dir.display()
        ));
    }

    let mut specs = Vec::new();

    for entry in std::fs::read_dir(specs_dir)
        .wrap_err_with(|| format!("failed to read specs directory: {}", specs_dir.display()))?
    {
        let entry = entry.wrap_err("failed to read directory entry")?;
        let path = entry.path();

        // Only process .md files
        if path.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }

        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| eyre::eyre!("invalid spec file name: {}", path.display()))?
            .to_string();

        let content = std::fs::read_to_string(&path)
            .wrap_err_with(|| format!("failed to read spec file: {}", path.display()))?;

        debug!(
            ?name,
            ?path,
            content_len = content.len(),
            "discovered spec file"
        );

        specs.push(SpecFile {
            path,
            name,
            content,
        });
    }

    // Sort by name for deterministic ordering
    specs.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(specs)
}

/// Generates a plan prompt from the discovered spec files.
#[must_use]
pub fn build_plan_prompt(specs: &[SpecFile]) -> String {
    let mut prompt = String::from(
        "# Plan Generation Request\n\n\
        You are an expert software architect. Please review the following specification(s) \
        and generate a detailed implementation plan.\n\n\
        The plan should:\n\
        1. Break down the work into clear, actionable tasks\n\
        2. Identify dependencies between tasks\n\
        3. Suggest an optimal execution order\n\
        4. Note any potential risks or technical challenges\n\
        5. Reference the specific spec files being addressed\n\n",
    );

    if specs.is_empty() {
        prompt
            .push_str("WARNING: No spec files were found. Please verify the specs directory.\n\n");
    } else {
        prompt.push_str("## Specifications\n\n");
        for spec in specs {
            prompt.push_str(&format!("### {} ({})\n\n", spec.name, spec.path.display()));
            prompt.push_str(&spec.content);
            prompt.push_str("\n\n");
        }
    }

    prompt.push_str(
        "## Output Format\n\n\
        Please output the implementation plan in markdown format with:\n\
        - Clear task headings with task numbers\n\
        - Dependencies between tasks clearly marked\n\
            - Estimated complexity for each task (Low/Medium/High)\n\
        - Risk assessment where applicable\n\n\
        Begin your response with the plan directly.",
    );

    prompt
}

/// Runs the plan generation with OpenAI.
///
/// # Errors
///
/// Returns an error if API key is missing, API call fails, or response is invalid.
#[instrument(level = "debug", ret, err, skip(config))]
pub fn run_plan_generation(config: &PlanRunConfig) -> Result<PlanResult> {
    // Validate API key at startup
    if config.api_key.is_empty() {
        return Err(eyre::eyre!(
            "OpenAI API key is empty. Set the {} environment variable.",
            config.openai_api_key_env_placeholder()
        ));
    }

    let specs = discover_specs(&config.specs_dir)?;

    if specs.is_empty() {
        return Err(eyre::eyre!(
            "no spec files found in {}. Please create .md spec files first.",
            config.specs_dir.display()
        ));
    }

    let prompt = build_plan_prompt(&specs);

    debug!(
        specs_count = specs.len(),
        prompt_len = prompt.len(),
        "prepared plan generation prompt"
    );

    if config.dry_run {
        println!("📋 Plan Generation Prompt (Dry Run):\n");
        println!("{}\n", prompt);
        println!("✅ Dry run complete. No API call was made.");
        return Ok(PlanResult {
            content: String::from("[Dry run - no content generated]"),
            iterations: 0,
        });
    }

    // Build the API request
    let client = reqwest::blocking::Client::builder()
        .build()
        .wrap_err("failed to build HTTP client")?;

    let request = client
        .post(
            config
                .base_url
                .as_deref()
                .unwrap_or("https://api.openai.com/v1/chat/completions"),
        )
        .header("Authorization", format!("Bearer {}", config.api_key))
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({
            "model": config.model,
            "messages": [
                {
                    "role": "user",
                    "content": prompt
                }
            ],
            "temperature": 0.7,
        }));

    let response = request
        .send()
        .wrap_err("failed to send request to OpenAI API")?;

    if !response.status().is_success() {
        let status = response.status();
        let error_body = response
            .text()
            .unwrap_or_else(|_| "unable to read error body".to_string());
        return Err(eyre::eyre!(
            "OpenAI API request failed with status {}: {}",
            status,
            error_body
        ));
    }

    let response_json: serde_json::Value = response
        .json()
        .wrap_err("failed to parse OpenAI API response as JSON")?;

    let content = response_json["choices"][0]["message"]["content"]
        .as_str()
        .ok_or_else(|| eyre::eyre!("OpenAI API response missing content field"))?
        .to_string();

    Ok(PlanResult {
        content,
        iterations: 1,
    })
}

impl PlanRunConfig {
    /// Placeholder text for API key env var (for error messages).
    #[must_use]
    fn openai_api_key_env_placeholder(&self) -> &str {
        "OPENAI_API_KEY"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_discover_specs_empty_dir() {
        let temp_dir = TempDir::new().expect("tempdir should be created");
        let specs = discover_specs(temp_dir.path()).unwrap();
        assert!(specs.is_empty());
    }

    #[test]
    fn test_discover_specs_with_md_files() {
        let temp_dir = TempDir::new().expect("tempdir should be created");

        // Create some spec files
        std::fs::write(
            temp_dir.path().join("auth.md"),
            "# Auth Spec\n\nImplement auth.",
        )
        .expect("auth.md should be written");
        std::fs::write(
            temp_dir.path().join("database.md"),
            "# Database Spec\n\nImplement database.",
        )
        .expect("database.md should be written");

        // Create a non-md file (should be ignored)
        std::fs::write(temp_dir.path().join("readme.txt"), "This should be ignored")
            .expect("readme.txt should be written");

        let specs = discover_specs(temp_dir.path()).unwrap();

        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].name, "auth");
        assert_eq!(specs[1].name, "database");
        assert!(specs[0].content.contains("Implement auth"));
        assert!(specs[1].content.contains("Implement database"));
    }

    #[test]
    fn test_discover_specs_nonexistent_dir() {
        let path = PathBuf::from("/nonexistent/path/that/does/not/exist");
        let result = discover_specs(&path);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("specs directory not found")
        );
    }

    #[test]
    fn test_build_plan_prompt_empty_specs() {
        let prompt = build_plan_prompt(&[]);
        assert!(prompt.contains("WARNING: No spec files were found"));
    }

    #[test]
    fn test_build_plan_prompt_with_specs() {
        let specs = vec![
            SpecFile {
                path: PathBuf::from("/specs/auth.md"),
                name: "auth".to_string(),
                content: "# Auth\n\nImplement OAuth2.".to_string(),
            },
            SpecFile {
                path: PathBuf::from("/specs/db.md"),
                name: "database".to_string(),
                content: "# Database\n\nUse PostgreSQL.".to_string(),
            },
        ];

        let prompt = build_plan_prompt(&specs);

        assert!(prompt.contains("auth.md"));
        assert!(prompt.contains("Implement OAuth2"));
        assert!(prompt.contains("db.md"));
        assert!(prompt.contains("Use PostgreSQL"));
        assert!(prompt.contains("## Specifications"));
        assert!(prompt.contains("## Output Format"));
    }

    #[test]
    fn test_build_plan_prompt_includes_instructions() {
        let prompt = build_plan_prompt(&[]);
        assert!(prompt.contains("Plan Generation Request"));
        assert!(prompt.contains("expert software architect"));
        assert!(prompt.contains("actionable tasks"));
        assert!(prompt.contains("Dependencies between tasks"));
        assert!(prompt.contains("Risk assessment"));
    }

    #[test]
    fn test_spec_file_fields() {
        let spec = SpecFile {
            path: PathBuf::from("/test/spec.md"),
            name: "spec".to_string(),
            content: "# Content\n\nTest content.".to_string(),
        };

        assert_eq!(spec.path, PathBuf::from("/test/spec.md"));
        assert_eq!(spec.name, "spec");
        assert_eq!(spec.content, "# Content\n\nTest content.");
    }

    #[test]
    fn test_plan_result_fields() {
        let result = PlanResult {
            content: "# Plan\n\nTest plan.".to_string(),
            iterations: 5,
        };

        assert_eq!(result.content, "# Plan\n\nTest plan.");
        assert_eq!(result.iterations, 5);
    }

    #[test]
    fn test_discover_specs_sorted_alphabetically() {
        let temp_dir = TempDir::new().expect("tempdir should be created");

        // Create files in non-alphabetical order
        std::fs::write(temp_dir.path().join("zebra.md"), "# Zebra")
            .expect("zebra.md should be written");
        std::fs::write(temp_dir.path().join("apple.md"), "# Apple")
            .expect("apple.md should be written");
        std::fs::write(temp_dir.path().join("middle.md"), "# Middle")
            .expect("middle.md should be written");

        let specs = discover_specs(temp_dir.path()).unwrap();

        assert_eq!(specs.len(), 3);
        assert_eq!(specs[0].name, "apple");
        assert_eq!(specs[1].name, "middle");
        assert_eq!(specs[2].name, "zebra");
    }

    // Property tests
    #[test]
    fn test_discover_specs_idempotent() {
        let temp_dir = TempDir::new().expect("tempdir should be created");

        std::fs::write(temp_dir.path().join("test.md"), "# Test")
            .expect("test.md should be written");

        let specs1 = discover_specs(temp_dir.path()).unwrap();
        let specs2 = discover_specs(temp_dir.path()).unwrap();

        assert_eq!(specs1.len(), specs2.len());
        if specs1.len() == specs2.len() && !specs1.is_empty() {
            assert_eq!(specs1[0].name, specs2[0].name);
            assert_eq!(specs1[0].content, specs2[0].content);
        }
    }
}

#[cfg(test)]
mod proptest_tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn test_spec_file_roundtrip(
            ref name in "[a-zA-Z0-9_-]+",
            content in "[a-zA-Z0-9\n\r\t .,!?@#$%^&*()_-]+"
        ) {
            let spec = SpecFile {
                path: PathBuf::from(format!("/specs/{}.md", name)),
                name: name.clone(),
                content: content.clone(),
            };

            prop_assert_eq!(&spec.name, name);
            prop_assert_eq!(spec.content, content);
            // Check path ends with the expected filename
            let expected_suffix = format!("{}.md", name);
            prop_assert!(spec.path.ends_with(&expected_suffix));
        }

        #[test]
        fn test_plan_result_roundtrip(
            content in "[a-zA-Z0-9\n\r\t .,!?@#$%^&*()_-]+",
            iterations in 1u32..100
        ) {
            let result = PlanResult {
                content: content.clone(),
                iterations,
            };

            prop_assert_eq!(result.content, content);
            prop_assert_eq!(result.iterations, iterations);
        }

        #[test]
        fn test_build_plan_prompt_always_contains_headers(specs in prop::collection::vec(
            "[a-z]+".prop_map(|name| {
                SpecFile {
                    path: PathBuf::from(format!("/specs/{}.md", name)),
                    name: name.clone(),
                    content: format!("# {}", name),
                }
            }),
            0..10
        )) {
            let prompt = build_plan_prompt(&specs);

            // Always contains these sections regardless of specs
            prop_assert!(prompt.contains("Plan Generation Request"));
            prop_assert!(prompt.contains("## Output Format"));

            // If specs exist, all should be mentioned and "## Specifications" should be present
            if !specs.is_empty() {
                prop_assert!(prompt.contains("## Specifications"));
                for spec in &specs {
                    prop_assert!(prompt.contains(&spec.name));
                }
            } else {
                prop_assert!(prompt.contains("WARNING: No spec files were found"));
            }
        }
    }
}
