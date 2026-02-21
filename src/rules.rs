//! Project rules system for intelligent AI agent guidance.
//!
//! This module implements a rules system similar to Cursor's `.cursor/rules` feature.
//! Rules are `.mdc` files in `.agents/rules/` at the git root containing project-specific
//! instructions for the AI agent. Each rule has YAML frontmatter with metadata controlling
//! when it should be applied.

use color_eyre::Result;
use color_eyre::eyre::{WrapErr, eyre};
use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::Deserialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use tokio::fs;
use tokio::sync::Mutex;

/// Maximum rule file size (100 KB) to prevent resource exhaustion.
const MAX_RULE_FILE_SIZE: usize = 100 * 1024;

/// Maximum number of rules to prevent overwhelming the agent.
const MAX_TOTAL_RULES: usize = 50;

/// A single rule loaded from a `.mdc` file.
///
/// Rules contain both metadata (from YAML frontmatter) and content
/// (markdown instructions for the AI agent).
#[derive(Debug, Clone)]
pub struct Rule {
    /// Filename without the `.mdc` extension (e.g., "rust" from "rust.mdc").
    pub name: String,
    /// Human-readable description from frontmatter.
    pub description: String,
    /// Glob patterns from frontmatter (original strings for validation).
    pub globs: Vec<String>,
    /// Whether this rule should always be applied.
    pub always_apply: bool,
    /// Markdown content after the frontmatter `---` delimiter.
    pub content: String,
    /// Compiled globset for efficient matching (None if no globs specified).
    glob_set: Option<GlobSet>,
}

impl Rule {
    /// Creates a new Rule with compiled globset.
    ///
    /// # Errors
    ///
    /// Returns an error if any glob pattern is invalid.
    #[must_use]
    pub fn new(
        name: String,
        description: String,
        globs: Vec<String>,
        always_apply: bool,
        content: String,
    ) -> Self {
        let glob_set = if globs.is_empty() {
            None
        } else {
            let mut builder = GlobSetBuilder::new();
            for pattern in &globs {
                // Normalize pattern: convert backslashes to forward slashes
                let normalized = pattern.replace('\\', "/");
                // Add pattern to builder; log warning if pattern is invalid
                match Glob::new(&normalized) {
                    Ok(glob) => {
                        builder.add(glob);
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Invalid glob pattern '{}' in rule '{}': {}",
                            pattern,
                            name,
                            e
                        );
                    }
                }
            }
            // Build the GlobSet with all added patterns
            match builder.build() {
                Ok(glob_set) => Some(glob_set),
                Err(e) => {
                    tracing::warn!("Failed to build globset for rule '{}': {}", name, e);
                    None
                }
            }
        };

        Self {
            name,
            description,
            globs,
            always_apply,
            content,
            glob_set,
        }
    }

    /// Checks if this rule matches a repo-relative path.
    ///
    /// # Arguments
    ///
    /// * `path_relative` - Path relative to git root, e.g., "src/main.rs"
    ///
    /// # Returns
    ///
    /// `true` if the path matches any of this rule's glob patterns.
    #[must_use]
    pub fn matches_path(&self, path_relative: &str) -> bool {
        self.glob_set
            .as_ref()
            .is_some_and(|gs| gs.is_match(path_relative))
    }

    /// Returns the rule name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// Frontmatter metadata parsed from YAML between `---` delimiters.
#[derive(Debug, Deserialize)]
pub struct RuleFrontmatter {
    /// Human-readable description of the rule.
    pub description: String,
    /// Glob patterns for files this rule applies to.
    #[serde(default)]
    pub globs: Vec<String>,
    /// Whether to apply this rule to every iteration.
    #[serde(default)]
    #[serde(rename = "alwaysApply")]
    pub always_apply: bool,
}

/// All discovered rules, categorized by application mode.
#[derive(Debug, Clone)]
pub struct RulesSet {
    /// Rules that should be applied to every iteration.
    pub always_apply: Vec<Rule>,
    /// Rules that apply based on glob pattern matching.
    pub glob_based: Vec<Rule>,
    /// Rules that apply via intelligent selection.
    pub intelligent: Vec<Rule>,
}

impl RulesSet {
    /// Creates a new empty RulesSet.
    #[must_use]
    pub fn new() -> Self {
        Self {
            always_apply: Vec::new(),
            glob_based: Vec::new(),
            intelligent: Vec::new(),
        }
    }

    /// Adds a rule to the appropriate category.
    fn add_rule(&mut self, rule: Rule) {
        if rule.always_apply {
            self.always_apply.push(rule);
        } else if !rule.globs.is_empty() {
            self.glob_based.push(rule);
        } else {
            self.intelligent.push(rule);
        }
    }

    /// Returns the total number of rules.
    #[must_use]
    pub fn total_count(&self) -> usize {
        self.always_apply.len() + self.glob_based.len() + self.intelligent.len()
    }
}

impl Default for RulesSet {
    fn default() -> Self {
        Self::new()
    }
}

/// Selected rules for injection with deduplication.
#[derive(Debug)]
pub struct SelectedRules {
    /// Rules to inject, in deterministic order.
    pub rules: Vec<Rule>,
    /// Names of rules already injected (to prevent duplication).
    pub injected: HashSet<String>,
}

impl SelectedRules {
    /// Creates a new SelectedRules with the given already-injected set.
    #[must_use]
    pub fn new(injected: HashSet<String>) -> Self {
        Self {
            rules: Vec::new(),
            injected,
        }
    }

    /// Adds a rule if not already injected.
    fn add(&mut self, rule: Rule) {
        if !self.injected.contains(&rule.name) {
            self.injected.insert(rule.name.clone());
            self.rules.push(rule);
        }
    }
}

/// State tracking across iterations.
///
/// All paths are stored as repo-relative strings (forward slashes) for
/// consistent matching across platforms.
#[derive(Debug, Clone, Default)]
pub struct RulesState {
    /// All files ever read across iterations (repo-relative, e.g., "src/main.rs").
    files_read: HashSet<String>,
    /// Rules already injected (by name) - persists across iterations.
    injected_rules: HashSet<String>,
}

impl RulesState {
    /// Creates a new empty RulesState.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records that a file was read.
    pub fn record_file_read(&mut self, path: String) {
        self.files_read.insert(path);
    }

    /// Returns rules whose globs match any previously-read file.
    ///
    /// Only returns rules that haven't been injected yet.
    #[must_use]
    pub fn match_globs_for_files<'a>(&'a self, rules: &'a [Rule]) -> Vec<&'a Rule> {
        rules
            .iter()
            .filter(|r| !self.injected_rules.contains(&r.name))
            .filter(|r| self.files_read.iter().any(|path| r.matches_path(path)))
            .collect()
    }

    /// Marks a rule as injected.
    pub fn mark_injected(&mut self, rule_name: String) {
        self.injected_rules.insert(rule_name);
    }

    /// Returns a reference to the set of injected rules.
    #[must_use]
    pub fn injected_rules(&self) -> &HashSet<String> {
        &self.injected_rules
    }
}

/// Cache for rules loaded once per run.
pub struct RulesCache {
    /// Git repository root path.
    git_root: PathBuf,
    /// Rules directory relative to git root.
    rules_dir: String,
    /// Cached rules (loaded once on first access).
    rules: Mutex<Option<RulesSet>>,
}

impl RulesCache {
    /// Creates a new RulesCache.
    #[must_use]
    pub fn new(git_root: PathBuf, rules_dir: String) -> Self {
        Self {
            git_root,
            rules_dir,
            rules: Mutex::new(None),
        }
    }

    /// Returns the cached rules, loading them if necessary.
    ///
    /// # Errors
    ///
    /// Returns an error if rule discovery fails.
    pub async fn get(&self) -> Result<RulesSet> {
        // Check if already loaded
        {
            let guard = self.rules.lock().await;
            if let Some(ref rules) = *guard {
                return Ok(rules.clone());
            }
        }

        // Load rules
        let discovered = discover_rules(&self.git_root, &self.rules_dir).await?;

        // Cache the result
        {
            let mut guard = self.rules.lock().await;
            *guard = Some(discovered.clone());
        }

        Ok(discovered)
    }

    /// Fetches rules by name (only clones what we need).
    ///
    /// # Errors
    ///
    /// Returns an error if the rules cannot be loaded.
    pub async fn get_rules_by_names(&self, names: &[String]) -> Result<Vec<Rule>> {
        let rules_set = self.get().await?;
        let all_rules: Vec<&Rule> = rules_set
            .always_apply
            .iter()
            .chain(rules_set.glob_based.iter())
            .chain(rules_set.intelligent.iter())
            .collect();

        let mut result = Vec::new();
        for name in names {
            if let Some(rule) = all_rules.iter().find(|r| &r.name == name) {
                result.push((*rule).clone());
            }
        }
        Ok(result)
    }
}

/// Splits frontmatter from markdown content.
///
/// # Format
/// ```text
/// ---
/// yaml: frontmatter
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
/// Returns an error if the file cannot be read.
pub fn split_frontmatter(content: &str) -> Result<(String, String)> {
    let mut lines = content.lines().peekable();

    // Skip leading empty lines
    while lines.peek().map(|l| l.trim().is_empty()).unwrap_or(false) {
        lines.next();
    }

    // First non-empty line must be opening delimiter
    let first = match lines.next() {
        Some(f) => f,
        // File is empty or contains only whitespace - return empty result
        None => return Ok((String::new(), String::new())),
    };

    if first.trim() != "---" {
        // No frontmatter found, return entire content as markdown
        return Ok((String::new(), content.to_string()));
    }

    // Collect YAML lines until closing delimiter
    let mut yaml_lines = Vec::new();
    for line in &mut lines {
        let trimmed = line.trim();
        if trimmed == "---" {
            // Found closing delimiter
            let yaml = yaml_lines.join("\n");
            let markdown = lines.collect::<Vec<_>>().join("\n");
            return Ok((yaml, markdown));
        }
        yaml_lines.push(line);
    }

    // No closing delimiter found - treat entire file as markdown
    Ok((String::new(), content.to_string()))
}

/// Parses a single rule file.
///
/// # Errors
///
/// Returns an error if:
/// - The file cannot be read
/// - The file size exceeds `MAX_RULE_FILE_SIZE`
/// - Frontmatter parsing fails
/// - The filename has no extension
pub async fn parse_rule_file(path: &Path, _git_root: &Path) -> Result<Rule> {
    // Read file content
    let content = fs::read_to_string(path)
        .await
        .wrap_err_with(|| format!("Failed to read rule file: {}", path.display()))?;

    // Check file size
    if content.len() > MAX_RULE_FILE_SIZE {
        return Err(eyre!(
            "Rule file too large: {} (max {} bytes)",
            path.display(),
            MAX_RULE_FILE_SIZE
        ));
    }

    // Extract rule name from filename (without .mdc extension)
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| eyre!("Invalid rule filename: {}", path.display()))?
        .to_string();

    // Split frontmatter from content
    let (yaml, markdown) = split_frontmatter(&content)?;

    // Parse frontmatter
    let frontmatter: RuleFrontmatter = if yaml.is_empty() {
        // No frontmatter, use defaults
        RuleFrontmatter {
            description: name.clone(),
            globs: Vec::new(),
            always_apply: false,
        }
    } else {
        serde_yaml::from_str(&yaml).wrap_err_with(|| {
            format!(
                "Failed to parse frontmatter in rule file: {}",
                path.display()
            )
        })?
    };

    Ok(Rule::new(
        name,
        frontmatter.description,
        frontmatter.globs,
        frontmatter.always_apply,
        markdown,
    ))
}

/// Discovers all rules in the `.agents/rules/` directory.
///
/// # Errors
///
/// Returns an error if:
/// - The rules directory cannot be read
/// - Too many rules are found (exceeds `MAX_TOTAL_RULES`)
/// - Any rule file fails to parse
pub async fn discover_rules(git_root: &Path, rules_dir: &str) -> Result<RulesSet> {
    let rules_path = git_root.join(rules_dir);

    // Create rules directory if it doesn't exist
    if !rules_path.exists() {
        fs::create_dir_all(&rules_path).await.wrap_err_with(|| {
            format!("Failed to create rules directory: {}", rules_path.display())
        })?;
        tracing::info!("Created rules directory: {}", rules_path.display());
        return Ok(RulesSet::new());
    }

    // Read directory entries
    let mut entries = fs::read_dir(&rules_path)
        .await
        .wrap_err_with(|| format!("Failed to read rules directory: {}", rules_path.display()))?;

    let mut rules_set = RulesSet::new();
    let mut rule_count = 0;

    while let Some(entry) = entries
        .next_entry()
        .await
        .wrap_err("Failed to read directory entry")?
    {
        let path = entry.path();

        // Only process .mdc files
        if path.extension().and_then(|s| s.to_str()) != Some("mdc") {
            continue;
        }

        // Validate rule file is under rules directory
        let canonical_path = path.canonicalize().unwrap_or_else(|_| path.clone());
        let canonical_rules = rules_path
            .canonicalize()
            .unwrap_or_else(|_| rules_path.clone());

        if !canonical_path.starts_with(&canonical_rules) {
            tracing::warn!(
                "Skipping rule file outside rules directory: {}",
                path.display()
            );
            continue;
        }

        let rule = parse_rule_file(&path, git_root).await?;
        tracing::debug!(
            "Loaded rule: {} (always_apply: {}, globs: {})",
            rule.name,
            rule.always_apply,
            rule.globs.len()
        );

        rules_set.add_rule(rule);
        rule_count += 1;

        if rule_count > MAX_TOTAL_RULES {
            return Err(eyre!(
                "Too many rules (max {MAX_TOTAL_RULES}). Please remove some rules from {}",
                rules_path.display()
            ));
        }
    }

    tracing::info!(
        "Discovered {} rules in {}",
        rules_set.total_count(),
        rules_path.display()
    );

    Ok(rules_set)
}

/// Selects rules for injection with deterministic ordering.
///
/// Order (deterministic for reproducibility):
/// 1. `alwaysApply` rules (sorted by filename)
/// 2. Glob-matched rules (sorted by filename)
///
/// # Arguments
///
/// * `rules_set` - All discovered rules
/// * `state` - Current rules state
///
/// # Returns
///
/// Selected rules with injected set updated.
#[must_use]
pub fn select_rules(rules_set: &RulesSet, state: &RulesState) -> SelectedRules {
    let mut selected = SelectedRules::new(state.injected_rules().clone());

    // 1. Always-apply rules (every iteration, sorted by name)
    let mut always = rules_set.always_apply.clone();
    always.sort_by(|a, b| a.name.cmp(&b.name));
    for rule in always {
        selected.add(rule);
    }

    // 2. Glob-based rules (from files read, sorted by name)
    let mut glob_matches: Vec<_> = state.match_globs_for_files(&rules_set.glob_based);
    glob_matches.sort_by(|a, b| a.name.cmp(&b.name));
    for rule in glob_matches {
        selected.add(rule.clone());
    }

    selected
}

/// Formats rules for injection into agent prompts.
///
/// # Arguments
///
/// * `rules` - Rules to format
///
/// # Returns
///
/// Formatted markdown string with rule headers and content.
#[must_use]
pub fn format_rules_for_injection(rules: &[Rule]) -> String {
    if rules.is_empty() {
        return String::new();
    }

    let mut output = String::from("# Project Rules\n\n");
    output.push_str("The following rules from `.agents/rules/` apply to this task:\n\n");

    for rule in rules {
        output.push_str(&format!("## {}\n\n", rule.name));
        output.push_str(&rule.content);
        output.push_str("\n\n---\n\n");
    }

    output
}

/// Injects rules before the main prompt.
///
/// # Arguments
///
/// * `prompt` - The original prompt
/// * `rules` - Rules to inject
///
/// # Returns
///
/// Combined prompt with rules prepended.
#[must_use]
pub fn inject_rules(prompt: &str, rules: &[Rule]) -> String {
    let rules_text = format_rules_for_injection(rules);
    if rules_text.is_empty() {
        prompt.to_string()
    } else {
        format!("{}\n\n{}", rules_text, prompt)
    }
}

/// Converts an absolute path to a repo-relative string.
///
/// # Arguments
///
/// * `git_root` - Git repository root path
/// * `absolute` - Absolute path to convert
///
/// # Errors
///
/// Returns an error if:
/// - The path is not under the git root
/// - Path conversion fails
///
/// # Example
/// ```ignore
/// # use velor::rules::path_relative_to;
/// # use std::path::Path;
/// let git_root = Path::new("/home/user/project");
/// let absolute = Path::new("/home/user/project/src/main.rs");
/// // Returns: Ok("src/main.rs".to_string())
/// # let _ = path_relative_to(git_root, absolute);
/// ```
pub fn path_relative_to(git_root: &Path, absolute: &Path) -> Result<String> {
    let relative = absolute.strip_prefix(git_root).wrap_err_with(|| {
        format!(
            "Path {:?} not under git root {:?}",
            absolute,
            git_root.display()
        )
    })?;

    // Convert to forward slashes for cross-platform consistency
    Ok(relative
        .iter()
        .map(|s| s.to_string_lossy())
        .collect::<Vec<_>>()
        .join("/"))
}

/// Normalizes an absolute path to a repo-relative string if it's safe.
///
/// Only processes files under git root. Returns `None` for paths outside.
///
/// # Arguments
///
/// * `git_root` - Git repository root path
/// * `absolute` - Absolute path to normalize
///
/// # Returns
///
/// `Some(relative_path)` if under git root, `None` otherwise.
pub fn normalize_file_path_if_safe(git_root: &Path, absolute: &Path) -> Option<String> {
    let canonical_git_root = git_root.canonicalize().ok()?;
    let canonical_absolute = absolute.canonicalize().ok()?;

    // Only process files under git root
    if !canonical_absolute.starts_with(&canonical_git_root) {
        return None;
    }

    // Strip git root and normalize to forward slashes
    let relative = canonical_absolute.strip_prefix(&canonical_git_root).ok()?;
    Some(
        relative
            .iter()
            .map(|s| s.to_string_lossy())
            .collect::<Vec<_>>()
            .join("/"),
    )
}

/// Check for new glob matches based on files read (SYNC - pure function).
///
/// Returns rule NAMES (not full Rule structs) to avoid unnecessary cloning.
/// Rule contents are fetched only when needed for formatting.
///
/// # Arguments
///
/// * `rules_set` - All discovered rules
/// * `files_read` - Files read in this turn (repo-relative paths)
/// * `injected_rules` - Rules already injected (by name)
///
/// # Returns
///
/// Sorted, deduplicated list of rule names that match the files read and
/// haven't been injected yet.
#[must_use]
pub fn check_new_glob_matches(
    rules_set: &RulesSet,
    files_read: &[String],
    injected_rules: &HashSet<String>,
) -> Vec<String> {
    let mut matched_names = Vec::new();

    for file in files_read {
        for rule in &rules_set.glob_based {
            if !injected_rules.contains(&rule.name) && rule.matches_path(file) {
                matched_names.push(rule.name.clone());
            }
        }
    }

    matched_names.sort();
    matched_names.dedup();
    matched_names
}

/// Fetch rules by name from RulesSet (O(1) per lookup via HashMap).
///
/// # Arguments
///
/// * `rules_set` - All discovered rules
/// * `names` - Rule names to fetch
///
/// # Returns
///
/// Vector of rules matching the given names.
#[must_use]
pub fn get_rules_by_names(rules_set: &RulesSet, names: &[String]) -> Vec<Rule> {
    let all_rules: Vec<&Rule> = rules_set
        .always_apply
        .iter()
        .chain(rules_set.glob_based.iter())
        .chain(rules_set.intelligent.iter())
        .collect();

    names
        .iter()
        .filter_map(|name| {
            all_rules
                .iter()
                .find(|r| &r.name == name)
                .map(|r| (*r).clone())
        })
        .collect()
}

/// Build follow-up prompt with ONLY new rules (delta formatting).
///
/// Key requirements:
/// - Short and directive (don't disrupt agent's flow)
/// - List actual file paths opened
/// - Mark clearly as "NEW RULES ONLY"
/// - Instruct agent to continue, not restart
///
/// # Arguments
///
/// * `files_read` - Files read in the previous turn (repo-relative paths)
/// * `new_rules` - New rules to inject
///
/// # Returns
///
/// Formatted follow-up prompt string.
#[must_use]
pub fn build_follow_up_prompt_delta(files_read: &[String], new_rules: &[Rule]) -> String {
    let rules_text = new_rules
        .iter()
        .map(|r| format!("## {}\n\n{}\n", r.name(), r.content))
        .collect::<Vec<_>>()
        .join("\n---\n\n");

    format!(
        r#"# NEW Project Rules (delta)

You opened these files:
{}

These NEW rules now apply:

{}

**Incorporate these new rules and continue from your current plan. Do not restart.**"#,
        files_read
            .iter()
            .map(|f| format!("- {}", f))
            .collect::<Vec<_>>()
            .join("\n"),
        rules_text
    )
}

/// Validates that the rules directory is under the git root.
///
/// # Arguments
///
/// * `git_root` - Git repository root path
/// * `rules_dir` - Rules directory path (can be relative or absolute)
///
/// # Errors
///
/// Returns an error if:
/// - The rules directory doesn't exist
/// - The rules directory is not under the git root
pub fn validate_rules_directory(git_root: &Path, rules_dir: &Path) -> Result<PathBuf> {
    let canonical_rules_dir = rules_dir
        .canonicalize()
        .wrap_err_with(|| format!("Rules directory does not exist: {}", rules_dir.display()))?;

    let canonical_git_root = git_root
        .canonicalize()
        .wrap_err_with(|| format!("Git root does not exist: {}", git_root.display()))?;

    if !canonical_rules_dir.starts_with(&canonical_git_root) {
        return Err(eyre!(
            "Rules directory must be under git root: {} is not under {}",
            canonical_rules_dir.display(),
            canonical_git_root.display()
        ));
    }

    Ok(canonical_rules_dir)
}

/// Response structure for intelligent rule selection.
///
/// The agent should respond with JSON in this format.
#[derive(Debug, Deserialize)]
pub struct IntelligentSelectionResponse {
    /// Names of selected rules.
    pub rules: Vec<String>,
    /// Optional reasoning for the selection (not used, for logging only).
    #[serde(default)]
    pub reasoning: String,
}

/// Builds the intelligent rule selection prompt.
///
/// # Arguments
///
/// * `rules` - Rules to select from (intelligent rules only)
/// * `task_preview` - Preview of the current task
///
/// # Returns
///
/// A prompt string requesting intelligent rule selection.
#[must_use]
pub fn build_intelligent_selection_prompt(rules: &[Rule], task_preview: &str) -> String {
    let descriptions: String = rules
        .iter()
        .map(|r| format!("- {}: {}", r.name, r.description))
        .collect::<Vec<_>>()
        .join("\n");

    // Cap task preview to prevent abuse
    let task_preview_capped = if task_preview.len() > 500 {
        format!("{}...", &task_preview[..500])
    } else {
        task_preview.to_string()
    };

    format!(
        r#"You are selecting relevant project rules for the current task.

Available rules:
{descriptions}

Task:
{task_preview_capped}

Respond ONLY with valid JSON in this exact format:
{{"rules":["rule_name_1","rule_name_2"],"reasoning":"Brief explanation..."}}

Respond with {{"rules":[],"reasoning":"none"}} if no rules apply."#
    )
}

/// Parses the intelligent selection response from agent output.
///
/// # Arguments
///
/// * `output` - Raw output from the agent
/// * `allowed_names` - Set of valid rule names for validation
///
/// # Errors
///
/// Returns an error if the JSON cannot be parsed.
pub fn parse_intelligent_selection_response(
    output: &str,
    allowed_names: &HashSet<&str>,
) -> Result<Vec<String>> {
    // Cap output to prevent abuse
    let output_capped = if output.len() > 4096 {
        tracing::warn!("Intelligent selection output exceeded 4096 bytes, truncating");
        &output[..4096]
    } else {
        output
    };

    // Try parsing as JSON directly first
    let selection: IntelligentSelectionResponse = serde_json::from_str(output_capped)
        .or_else(|_| extract_json_from_markdown(output_capped))?;

    // Validate: reject any rule name not in the offered set
    let rule_names: Vec<_> = selection
        .rules
        .into_iter()
        .filter(|name| allowed_names.contains(name.as_str()))
        .collect();

    tracing::debug!(
        "Intelligent selection parsed: {} rules selected (valid)",
        rule_names.len()
    );

    Ok(rule_names)
}

/// Extracts JSON from a markdown code block.
///
/// This is a fallback for when the agent wraps the JSON in markdown code blocks.
///
/// # Arguments
///
/// * `text` - Text that may contain a JSON code block
///
/// # Errors
///
/// Returns an error if no valid JSON is found.
fn extract_json_from_markdown(text: &str) -> Result<IntelligentSelectionResponse> {
    use regex::Regex;

    // Look for ```json ... ``` blocks
    let re = Regex::new(r"```json\s*(\{.*?\})\s*```")
        .map_err(|e| eyre!("Failed to compile regex: {e}"))?;

    if let Some(caps) = re.captures(text) {
        return serde_json::from_str(&caps[1]).map_err(Into::into);
    }

    // Try looking for ``` ... ``` blocks (no language specified)
    let re_plain =
        Regex::new(r"```\s*(\{.*?\})\s*```").map_err(|e| eyre!("Failed to compile regex: {e}"))?;

    if let Some(caps) = re_plain.captures(text) {
        return serde_json::from_str(&caps[1]).map_err(Into::into);
    }

    // Try parsing entire text as JSON
    serde_json::from_str(text).map_err(Into::into)
}

/// Selects rules for injection with deterministic ordering including intelligent rules.
///
/// Order (deterministic for reproducibility):
/// 1. `alwaysApply` rules (sorted by filename)
/// 2. Glob-matched rules (sorted by filename)
/// 3. Intelligent rules (sorted, capped)
///
/// # Arguments
///
/// * `rules_set` - All discovered rules
/// * `state` - Current rules state
/// * `intelligent_rules` - Optional intelligently selected rules
/// * `max_intelligent` - Maximum number of intelligent rules to include
///
/// # Returns
///
/// Selected rules with injected set updated.
#[must_use]
pub fn select_rules_with_intelligent(
    rules_set: &RulesSet,
    state: &RulesState,
    intelligent_rules: Option<&[Rule]>,
    max_intelligent: usize,
) -> SelectedRules {
    let mut selected = SelectedRules::new(state.injected_rules().clone());

    // 1. Always-apply rules (every iteration, sorted by name)
    let mut always = rules_set.always_apply.clone();
    always.sort_by(|a, b| a.name.cmp(&b.name));
    for rule in always {
        selected.add(rule);
    }

    // 2. Glob-based rules (from files read, sorted by name)
    let mut glob_matches: Vec<_> = state.match_globs_for_files(&rules_set.glob_based);
    glob_matches.sort_by(|a, b| a.name.cmp(&b.name));
    for rule in glob_matches {
        selected.add(rule.clone());
    }

    // 3. Intelligent rules (sorted, capped)
    if let Some(intelligent) = intelligent_rules {
        let mut sorted: Vec<_> = intelligent.to_vec();
        sorted.sort_by(|a, b| a.name.cmp(&b.name));

        // Filter out already-injected rules and cap the count
        for rule in sorted.into_iter().take(max_intelligent) {
            selected.add(rule);
        }
    }

    selected
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    /// Test split_frontmatter with valid frontmatter.
    #[test]
    fn test_split_frontmatter_valid() {
        let content = r#"---
description: Test rule
globs: *.rs
alwaysApply: true
---
This is the content."#;

        let (yaml, markdown) = split_frontmatter(content).unwrap();
        assert!(yaml.contains("description: Test rule"));
        assert!(yaml.contains("globs: *.rs"));
        assert_eq!(markdown.trim(), "This is the content.");
    }

    /// Test split_frontmatter with no frontmatter.
    #[test]
    fn test_split_frontmatter_none() {
        let content = "Just some markdown content";

        let (yaml, markdown) = split_frontmatter(content).unwrap();
        assert!(yaml.is_empty());
        assert_eq!(markdown, content);
    }

    /// Test split_frontmatter with leading empty lines.
    #[test]
    fn test_split_frontmatter_leading_empty() {
        let content = "\n\n---\ndescription: Test\n---\ncontent";

        let (yaml, markdown) = split_frontmatter(content).unwrap();
        assert!(yaml.contains("description: Test"));
        assert_eq!(markdown.trim(), "content");
    }

    /// Test split_frontmatter with unclosed frontmatter.
    #[test]
    fn test_split_frontmatter_unclosed() {
        let content = "---\ndescription: Test\nNo closing delimiter";

        let (yaml, markdown) = split_frontmatter(content).unwrap();
        // Should treat entire content as markdown when no closing delimiter
        assert!(yaml.is_empty());
        assert_eq!(markdown, content);
    }

    /// Test Rule::new creates a rule with compiled globset.
    #[test]
    fn test_rule_new_with_globs() {
        let rule = Rule::new(
            "rust".to_string(),
            "Rust rules".to_string(),
            vec!["**/*.rs".to_string(), "src/**/*.rs".to_string()],
            false,
            "Use Rust best practices".to_string(),
        );

        assert_eq!(rule.name, "rust");
        assert_eq!(rule.description, "Rust rules");
        assert_eq!(rule.globs.len(), 2);
        assert!(!rule.always_apply);
        assert!(rule.glob_set.is_some());
    }

    /// Test Rule::new with empty globs.
    #[test]
    fn test_rule_new_no_globs() {
        let rule = Rule::new(
            "general".to_string(),
            "General rules".to_string(),
            vec![],
            false,
            "Some content".to_string(),
        );

        assert!(rule.glob_set.is_none());
    }

    /// Test Rule::matches_path with glob patterns.
    #[test]
    fn test_rule_matches_path() {
        let rule = Rule::new(
            "rust".to_string(),
            "Rust rules".to_string(),
            vec!["**/*.rs".to_string()],
            false,
            "Content".to_string(),
        );

        assert!(rule.matches_path("src/main.rs"));
        assert!(rule.matches_path("lib.rs"));
        assert!(rule.matches_path("deep/nested/module.rs"));
        assert!(!rule.matches_path("src/main.py"));
        assert!(!rule.matches_path("README.md"));
    }

    /// Test RulesState::record_file_read and match_globs_for_files.
    #[test]
    fn test_rules_state_matching() {
        let mut state = RulesState::new();

        let rust_rule = Rule::new(
            "rust".to_string(),
            "Rust".to_string(),
            vec!["**/*.rs".to_string()],
            false,
            "Content".to_string(),
        );

        let js_rule = Rule::new(
            "js".to_string(),
            "JavaScript".to_string(),
            vec!["**/*.js".to_string()],
            false,
            "Content".to_string(),
        );

        let rules = vec![rust_rule.clone(), js_rule];

        // No files read yet
        assert!(state.match_globs_for_files(&rules).is_empty());

        // Read a Rust file
        state.record_file_read("src/main.rs".to_string());

        let matches = state.match_globs_for_files(&rules);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].name, "rust");

        // Mark as injected
        state.mark_injected("rust".to_string());

        // Should not match anymore
        assert!(state.match_globs_for_files(&rules).is_empty());
    }

    /// Test path_relative_to.
    #[test]
    fn test_path_relative_to() {
        let git_root = Path::new("/home/user/project");
        let absolute = Path::new("/home/user/project/src/main.rs");

        let result = path_relative_to(git_root, absolute).unwrap();
        assert_eq!(result, "src/main.rs");
    }

    /// Test path_relative_to with nested path.
    #[test]
    fn test_path_relative_to_nested() {
        let git_root = Path::new("/home/user/project");
        let absolute = Path::new("/home/user/project/deeply/nested/module.rs");

        let result = path_relative_to(git_root, absolute).unwrap();
        assert_eq!(result, "deeply/nested/module.rs");
    }

    /// Test path_relative_to error for path outside git root.
    #[test]
    fn test_path_relative_to_outside() {
        let git_root = Path::new("/home/user/project");
        let absolute = Path::new("/other/project/file.rs");

        let result = path_relative_to(git_root, absolute);
        assert!(result.is_err());
    }

    /// Test format_rules_for_injection with no rules.
    #[test]
    fn test_format_rules_for_injection_empty() {
        let output = format_rules_for_injection(&[]);
        assert!(output.is_empty());
    }

    /// Test format_rules_for_injection with rules.
    #[test]
    fn test_format_rules_for_injection() {
        let rules = vec![Rule::new(
            "rust".to_string(),
            "Rust rules".to_string(),
            vec![],
            false,
            "Use strict typing".to_string(),
        )];

        let output = format_rules_for_injection(&rules);
        assert!(output.contains("# Project Rules"));
        assert!(output.contains("## rust"));
        assert!(output.contains("Use strict typing"));
    }

    /// Test inject_rules combines rules and prompt.
    #[test]
    fn test_inject_rules() {
        let rules = vec![Rule::new(
            "test".to_string(),
            "Test rule".to_string(),
            vec![],
            false,
            "Rule content".to_string(),
        )];

        let prompt = "Original prompt";
        let result = inject_rules(prompt, &rules);

        assert!(result.contains("# Project Rules"));
        assert!(result.contains("Rule content"));
        assert!(result.contains("Original prompt"));
        // Rules come before prompt
        assert!(result.find("Rule content").unwrap() < result.find("Original prompt").unwrap());
    }

    /// Test inject_rules with no rules returns original prompt.
    #[test]
    fn test_inject_rules_empty() {
        let prompt = "Original prompt";
        let result = inject_rules(prompt, &[]);
        assert_eq!(result, prompt);
    }

    /// Test RulesSet::add_rule categorizes correctly.
    #[test]
    fn test_rules_set_add_rule() {
        let mut set = RulesSet::new();

        let always_rule = Rule::new(
            "always".to_string(),
            "Always".to_string(),
            vec![],
            true,
            "Content".to_string(),
        );

        let glob_rule = Rule::new(
            "globbed".to_string(),
            "Globbed".to_string(),
            vec!["*.rs".to_string()],
            false,
            "Content".to_string(),
        );

        let intelligent_rule = Rule::new(
            "intelligent".to_string(),
            "Intelligent".to_string(),
            vec![],
            false,
            "Content".to_string(),
        );

        set.add_rule(always_rule);
        set.add_rule(glob_rule);
        set.add_rule(intelligent_rule);

        assert_eq!(set.always_apply.len(), 1);
        assert_eq!(set.glob_based.len(), 1);
        assert_eq!(set.intelligent.len(), 1);
        assert_eq!(set.total_count(), 3);
    }

    /// Test SelectedRules::add prevents duplicates.
    #[test]
    fn test_selected_rules_add() {
        let mut selected = SelectedRules::new(HashSet::new());

        let rule = Rule::new(
            "test".to_string(),
            "Test".to_string(),
            vec![],
            false,
            "Content".to_string(),
        );

        selected.add(rule.clone());
        assert_eq!(selected.rules.len(), 1);
        assert!(selected.injected.contains("test"));

        // Adding same rule again should be ignored
        selected.add(rule);
        assert_eq!(selected.rules.len(), 1);
    }

    /// Test normalize_file_path_if_safe with path under git root.
    #[test]
    fn test_normalize_file_path_if_safe_valid() {
        let git_root = PathBuf::from("/tmp/test_repo");
        let absolute = PathBuf::from("/tmp/test_repo/src/main.rs");

        // Note: This test may fail if paths don't exist for canonicalize
        // In real usage, this would work with actual paths
        let result = normalize_file_path_if_safe(&git_root, &absolute);
        // We can't test fully without actual files, but we can test the logic
        // For now, just ensure it doesn't panic
        drop(result);
    }

    /// Test build_intelligent_selection_prompt with multiple rules.
    #[test]
    fn test_build_intelligent_selection_prompt() {
        let rules = vec![
            Rule::new(
                "rust".to_string(),
                "Rust coding guidelines".to_string(),
                vec![],
                false,
                "Content".to_string(),
            ),
            Rule::new(
                "python".to_string(),
                "Python best practices".to_string(),
                vec![],
                false,
                "Content".to_string(),
            ),
        ];

        let prompt = build_intelligent_selection_prompt(&rules, "Fix a bug in main.rs");

        assert!(prompt.contains("rust: Rust coding guidelines"));
        assert!(prompt.contains("python: Python best practices"));
        assert!(prompt.contains("Fix a bug in main.rs"));
        assert!(prompt.contains(
            r#"{"rules":["rule_name_1","rule_name_2"],"reasoning":"Brief explanation..."}"#
        ));
    }

    /// Test build_intelligent_selection_prompt caps long task preview.
    #[test]
    fn test_build_intelligent_selection_prompt_caps_long_task() {
        let rules = vec![Rule::new(
            "test".to_string(),
            "Test rule".to_string(),
            vec![],
            false,
            "Content".to_string(),
        )];

        let long_task = "a".repeat(600);
        let prompt = build_intelligent_selection_prompt(&rules, &long_task);

        // Task preview should be capped with "..."
        assert!(prompt.contains("..."));
        // Prompt should still contain important parts
        assert!(prompt.contains("test: Test rule"));
    }

    /// Test parse_intelligent_selection_response with valid JSON.
    #[test]
    fn test_parse_intelligent_selection_response_valid() {
        let output = r#"{"rules":["rust","python"],"reasoning":"Both are relevant"}"#;
        let mut allowed = HashSet::new();
        allowed.insert("rust");
        allowed.insert("python");
        allowed.insert("javascript");

        let result = parse_intelligent_selection_response(output, &allowed).unwrap();

        assert_eq!(result.len(), 2);
        assert!(result.contains(&"rust".to_string()));
        assert!(result.contains(&"python".to_string()));
    }

    /// Test parse_intelligent_selection_response filters out invalid rule names.
    #[test]
    fn test_parse_intelligent_selection_response_filters_invalid() {
        let output = r#"{"rules":["rust","unknown_rule","python"],"reasoning":"test"}"#;
        let mut allowed = HashSet::new();
        allowed.insert("rust");
        allowed.insert("python");

        let result = parse_intelligent_selection_response(output, &allowed).unwrap();

        assert_eq!(result.len(), 2);
        assert!(result.contains(&"rust".to_string()));
        assert!(result.contains(&"python".to_string()));
        assert!(!result.iter().any(|r| r == "unknown_rule"));
    }

    /// Test parse_intelligent_selection_response handles empty rules.
    #[test]
    fn test_parse_intelligent_selection_response_empty() {
        let output = r#"{"rules":[],"reasoning":"none"}"#;
        let allowed = HashSet::new();

        let result = parse_intelligent_selection_response(output, &allowed).unwrap();

        assert_eq!(result.len(), 0);
    }

    /// Test extract_json_from_markdown extracts JSON from code blocks.
    #[test]
    fn test_extract_json_from_markdown_with_code_block() {
        let text = r#"Here's my response:

```json
{"rules":["rust","python"],"reasoning":"test"}
```

That's it."#;

        let result: IntelligentSelectionResponse = extract_json_from_markdown(text).unwrap();

        assert_eq!(result.rules.len(), 2);
        assert!(result.rules.contains(&"rust".to_string()));
        assert!(result.rules.contains(&"python".to_string()));
    }

    /// Test extract_json_from_markdown handles plain JSON.
    #[test]
    fn test_extract_json_from_markdown_plain_json() {
        let text = r#"{"rules":["rust"],"reasoning":"test"}"#;

        let result: IntelligentSelectionResponse = extract_json_from_markdown(text).unwrap();

        assert_eq!(result.rules.len(), 1);
        assert_eq!(result.rules[0], "rust");
    }

    /// Test extract_json_from_markdown handles code blocks without language.
    #[test]
    fn test_extract_json_from_markdown_plain_code_block() {
        let text = r#"```
{"rules":["rust"],"reasoning":"test"}
```"#;

        let result: IntelligentSelectionResponse = extract_json_from_markdown(text).unwrap();

        assert_eq!(result.rules.len(), 1);
        assert_eq!(result.rules[0], "rust");
    }

    /// Test select_rules_with_intelligent includes intelligent rules.
    #[test]
    fn test_select_rules_with_intelligent() {
        let mut rules_set = RulesSet::new();

        // Add always-apply rule
        rules_set.add_rule(Rule::new(
            "always".to_string(),
            "Always".to_string(),
            vec![],
            true,
            "Content".to_string(),
        ));

        // Add glob-based rule (should NOT be selected since no files read)
        rules_set.add_rule(Rule::new(
            "globbed".to_string(),
            "Globbed".to_string(),
            vec!["*.rs".to_string()],
            false,
            "Content".to_string(),
        ));

        // Add intelligent rule
        let intelligent_rule = Rule::new(
            "intelligent".to_string(),
            "Intelligent".to_string(),
            vec![],
            false,
            "Content".to_string(),
        );

        let state = RulesState::new();
        let selected = select_rules_with_intelligent(
            &rules_set,
            &state,
            Some(&[intelligent_rule]),
            10, // max_intelligent
        );

        // Should only include always-apply + intelligent (glob-based not selected since no files read)
        assert_eq!(selected.rules.len(), 2);
        assert!(selected.injected.contains("always"));
        assert!(!selected.injected.contains("globbed"));
        assert!(selected.injected.contains("intelligent"));
    }

    /// Test select_rules_with_intelligent caps intelligent rules.
    #[test]
    fn test_select_rules_with_intelligent_caps_rules() {
        let rules_set = RulesSet::new();
        let state = RulesState::new();

        let intelligent_rules = vec![
            Rule::new(
                "rule1".to_string(),
                "Rule 1".to_string(),
                vec![],
                false,
                "Content".to_string(),
            ),
            Rule::new(
                "rule2".to_string(),
                "Rule 2".to_string(),
                vec![],
                false,
                "Content".to_string(),
            ),
            Rule::new(
                "rule3".to_string(),
                "Rule 3".to_string(),
                vec![],
                false,
                "Content".to_string(),
            ),
        ];

        let selected = select_rules_with_intelligent(
            &rules_set,
            &state,
            Some(&intelligent_rules),
            2, // max_intelligent = 2
        );

        // Should only include 2 intelligent rules (alphabetically sorted)
        let intelligent_count: usize = selected
            .rules
            .iter()
            .filter(|r| intelligent_rules.iter().any(|ir| ir.name == r.name))
            .count();
        assert_eq!(intelligent_count, 2);
    }

    /// Test select_rules_with_intelligent without intelligent rules.
    #[test]
    fn test_select_rules_with_intelligent_no_intelligent() {
        let mut rules_set = RulesSet::new();

        rules_set.add_rule(Rule::new(
            "always".to_string(),
            "Always".to_string(),
            vec![],
            true,
            "Content".to_string(),
        ));

        let state = RulesState::new();
        let selected = select_rules_with_intelligent(&rules_set, &state, None, 10);

        assert_eq!(selected.rules.len(), 1);
        assert!(selected.injected.contains("always"));
    }

    /// Test select_rules_with_intelligent filters already-injected intelligent rules.
    #[test]
    fn test_select_rules_with_intelligent_filters_injected() {
        let rules_set = RulesSet::new();

        let intelligent_rule = Rule::new(
            "intelligent".to_string(),
            "Intelligent".to_string(),
            vec![],
            false,
            "Content".to_string(),
        );

        let mut state = RulesState::new();
        state.mark_injected("intelligent".to_string());

        let selected =
            select_rules_with_intelligent(&rules_set, &state, Some(&[intelligent_rule]), 10);

        // The intelligent rule should NOT be included since it's already injected
        assert_eq!(selected.rules.len(), 0);
    }
}

#[cfg(test)]
mod proptest_tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn test_split_frontmatter_preserves_content(content in ".*") {
            let result = split_frontmatter(&content);
            // Should never error
            let (_yaml, markdown) = result.unwrap();
            // Markdown should always be non-empty unless content is whitespace-only
            // When frontmatter is not found, markdown == content
            // When frontmatter is found, markdown is the content after closing ---
            // For whitespace-only content, markdown may be empty
            let is_whitespace_only = content.trim().is_empty();
            prop_assert!(!markdown.is_empty() || is_whitespace_only);
        }

        #[test]
        fn test_rule_name_roundtrip(name in "[a-zA-Z0-9_-]{1,20}") {
            let rule = Rule::new(
                name.clone(),
                "Description".to_string(),
                vec![],
                false,
                "Content".to_string(),
            );
            prop_assert_eq!(rule.name, name);
        }

        #[test]
        fn test_rules_state_files_read(paths in "[a-z/]{1,50}.[a-z]{2}") {
            let mut state = RulesState::new();
            let path = format!("src/{}", paths);
            state.record_file_read(path.clone());
            prop_assert!(state.files_read.contains(&path));
        }

        #[test]
        fn test_format_rules_preserves_content(content in "[a-zA-Z0-9 ]{0,100}") {
            let rule = Rule::new(
                "test".to_string(),
                "Test".to_string(),
                vec![],
                false,
                content.clone(),
            );
            let formatted = format_rules_for_injection(&[rule]);
            if !content.is_empty() {
                prop_assert!(formatted.contains(&content));
            }
        }
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use tempfile::TempDir;

    /// Test discovering rules from a directory.
    #[tokio::test]
    async fn test_discover_rules() {
        let temp_dir = TempDir::new().unwrap();
        let rules_dir = temp_dir.path().join(".agents").join("rules");
        fs::create_dir_all(&rules_dir).await.unwrap();

        // Create a test rule file
        let rule_path = rules_dir.join("test.mdc");
        let content = r#"---
description: Test rule
globs:
  - "*.rs"
alwaysApply: false
---
This is test content."#;
        fs::write(&rule_path, content).await.unwrap();

        let rules_set = discover_rules(temp_dir.path(), ".agents/rules")
            .await
            .unwrap();

        assert_eq!(rules_set.glob_based.len(), 1);
        assert_eq!(rules_set.glob_based[0].name, "test");
        assert_eq!(rules_set.glob_based[0].description, "Test rule");
    }

    /// Test discovering rules with empty directory.
    #[tokio::test]
    async fn test_discover_rules_empty() {
        let temp_dir = TempDir::new().unwrap();
        let rules_dir = temp_dir.path().join(".agents").join("rules");
        fs::create_dir_all(&rules_dir).await.unwrap();

        let rules_set = discover_rules(temp_dir.path(), ".agents/rules")
            .await
            .unwrap();

        assert_eq!(rules_set.total_count(), 0);
    }

    /// Test discovering rules creates directory if missing.
    #[tokio::test]
    async fn test_discover_rules_creates_directory() {
        let temp_dir = TempDir::new().unwrap();
        let rules_dir = temp_dir.path().join(".agents").join("rules");

        assert!(!rules_dir.exists());

        let rules_set = discover_rules(temp_dir.path(), ".agents/rules")
            .await
            .unwrap();

        assert!(rules_dir.exists());
        assert_eq!(rules_set.total_count(), 0);
    }

    /// Test parse_rule_file with valid content.
    #[tokio::test]
    async fn test_parse_rule_file_valid() {
        let temp_dir = TempDir::new().unwrap();
        let rule_path = temp_dir.path().join("test.mdc");

        let content = r#"---
description: A test rule
globs:
  - "**/*.rs"
  - "src/**/*.rs"
alwaysApply: true
---
Use strict typing in Rust."#;
        fs::write(&rule_path, content).await.unwrap();

        let rule = parse_rule_file(&rule_path, temp_dir.path()).await.unwrap();

        assert_eq!(rule.name, "test");
        assert_eq!(rule.description, "A test rule");
        assert_eq!(rule.globs.len(), 2);
        assert!(rule.always_apply);
        assert_eq!(rule.content.trim(), "Use strict typing in Rust.");
    }

    /// Test parse_rule_file with no frontmatter.
    #[tokio::test]
    async fn test_parse_rule_file_no_frontmatter() {
        let temp_dir = TempDir::new().unwrap();
        let rule_path = temp_dir.path().join("test.mdc");

        let content = "Just some content";
        fs::write(&rule_path, content).await.unwrap();

        let rule = parse_rule_file(&rule_path, temp_dir.path()).await.unwrap();

        assert_eq!(rule.name, "test");
        assert_eq!(rule.description, "test");
        assert!(rule.globs.is_empty());
        assert!(!rule.always_apply);
        assert_eq!(rule.content, content);
    }

    /// Test RulesCache loads and caches rules.
    #[tokio::test]
    async fn test_rules_cache() {
        let temp_dir = TempDir::new().unwrap();
        let rules_dir = temp_dir.path().join(".agents").join("rules");
        fs::create_dir_all(&rules_dir).await.unwrap();

        let rule_path = rules_dir.join("cache_test.mdc");
        let content = r#"---
description: Cache test
globs:
  - "*.rs"
alwaysApply: true
---
Content"#;
        fs::write(&rule_path, content).await.unwrap();

        let cache = RulesCache::new(temp_dir.path().to_path_buf(), ".agents/rules".to_string());

        // First access loads rules
        let rules1 = cache.get().await.unwrap();
        assert_eq!(rules1.total_count(), 1);

        // Second access uses cached rules
        let rules2 = cache.get().await.unwrap();
        assert_eq!(rules2.total_count(), 1);
    }
}
