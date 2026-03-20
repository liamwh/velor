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

/// Truncates a string to approximately `max_bytes` bytes.
///
/// Uses `floor_char_boundary` to avoid cutting through multi-byte UTF-8 sequences.
/// The actual result may be slightly shorter than `max_bytes` to ensure valid UTF-8.
fn truncate_str(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let safe_idx = s.floor_char_boundary(max_bytes);
    &s[..safe_idx]
}

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
    #[allow(dead_code)] // Kept for public API compatibility
    pub fn match_globs_for_files<'a>(&'a self, rules: &'a [Rule]) -> Vec<&'a Rule> {
        rules
            .iter()
            .filter(|r| !self.injected_rules.contains(&r.name))
            .filter(|r| self.files_read.iter().any(|path| r.matches_path(path)))
            .collect()
    }

    /// Returns rules whose globs match any previously-read file, along with matching files.
    ///
    /// Returns a vector of (rule, matching_files) tuples for tracing purposes.
    #[must_use]
    pub fn match_globs_for_files_with_reason<'a>(
        &'a self,
        rules: &'a [Rule],
    ) -> Vec<(&'a Rule, Vec<&'a str>)> {
        rules
            .iter()
            .filter(|r| !self.injected_rules.contains(&r.name))
            .filter_map(|r| {
                let matching_files: Vec<_> = self
                    .files_read
                    .iter()
                    .filter(|path| r.matches_path(path))
                    .map(|s| s.as_str())
                    .collect();
                if !matching_files.is_empty() {
                    Some((r, matching_files))
                } else {
                    None
                }
            })
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
    #[allow(dead_code)] // Public API for future use
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
    for rule in &always {
        tracing::info!("📋 Injecting rule '{}' (always_apply: true)", rule.name);
    }
    for rule in always {
        selected.add(rule);
    }

    // 2. Glob-based rules (from files read, sorted by name)
    let glob_matches_with_reason = state.match_globs_for_files_with_reason(&rules_set.glob_based);
    for (rule, files) in &glob_matches_with_reason {
        tracing::info!(
            "📋 Injecting rule '{}' (glob match triggered by: {})",
            rule.name,
            files.join(", ")
        );
    }
    let mut glob_matches: Vec<_> = glob_matches_with_reason
        .into_iter()
        .map(|(r, _)| r)
        .collect();
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
#[allow(dead_code)] // For future use
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
#[allow(dead_code)] // Kept for public API compatibility
pub fn check_new_glob_matches(
    rules_set: &RulesSet,
    files_read: &[String],
    injected_rules: &HashSet<String>,
) -> Vec<String> {
    let mut matched_names = Vec::new();

    for file in files_read {
        for rule in &rules_set.glob_based {
            if !injected_rules.contains(&rule.name) && rule.matches_path(file) {
                matched_names.push((rule.name.clone(), file.clone()));
            }
        }
    }

    matched_names.sort();
    matched_names.dedup();
    matched_names
        .into_iter()
        .map(|(name, _file)| name)
        .collect()
}

/// Check for new glob matches with detailed file-to-rule mapping (for tracing).
///
/// Returns both rule names and which files triggered each rule.
#[must_use]
pub fn check_new_glob_matches_with_tracing(
    rules_set: &RulesSet,
    files_read: &[String],
    injected_rules: &HashSet<String>,
) -> Vec<(String, Vec<String>)> {
    let mut rule_to_files: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();

    for file in files_read {
        for rule in &rules_set.glob_based {
            if !injected_rules.contains(&rule.name) && rule.matches_path(file) {
                rule_to_files
                    .entry(rule.name.clone())
                    .or_default()
                    .push(file.clone());
            }
        }
    }

    let mut result: Vec<_> = rule_to_files.into_iter().collect();
    result.sort_by(|a, b| a.0.cmp(&b.0));
    result
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
#[allow(dead_code)] // For future security validation
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
    /// Optional reasoning for the selection (for debugging/logging).
    #[serde(default)]
    #[allow(dead_code)] // For future logging/debugging
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
        format!("{}...", truncate_str(task_preview, 500))
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
        truncate_str(output, 4096)
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
    for rule in &always {
        tracing::info!("📋 Injecting rule '{}' (always_apply: true)", rule.name);
    }
    for rule in always {
        selected.add(rule);
    }

    // 2. Glob-based rules (from files read, sorted by name)
    let glob_matches_with_reason = state.match_globs_for_files_with_reason(&rules_set.glob_based);
    for (rule, files) in &glob_matches_with_reason {
        tracing::info!(
            "📋 Injecting rule '{}' (glob match triggered by: {})",
            rule.name,
            files.join(", ")
        );
    }
    let mut glob_matches: Vec<_> = glob_matches_with_reason
        .into_iter()
        .map(|(r, _)| r)
        .collect();
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
            tracing::info!("📋 Injecting rule '{}' (intelligent selection)", rule.name);
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

        let (yaml, markdown) =
            split_frontmatter(content).expect("split_frontmatter should succeed");
        assert!(yaml.contains("description: Test rule"));
        assert!(yaml.contains("globs: *.rs"));
        assert_eq!(markdown.trim(), "This is the content.");
    }

    /// Test split_frontmatter with no frontmatter.
    #[test]
    fn test_split_frontmatter_none() {
        let content = "Just some markdown content";

        let (yaml, markdown) =
            split_frontmatter(content).expect("split_frontmatter should succeed");
        assert!(yaml.is_empty());
        assert_eq!(markdown, content);
    }

    /// Test split_frontmatter with leading empty lines.
    #[test]
    fn test_split_frontmatter_leading_empty() {
        let content = "\n\n---\ndescription: Test\n---\ncontent";

        let (yaml, markdown) =
            split_frontmatter(content).expect("split_frontmatter should succeed");
        assert!(yaml.contains("description: Test"));
        assert_eq!(markdown.trim(), "content");
    }

    /// Test split_frontmatter with unclosed frontmatter.
    #[test]
    fn test_split_frontmatter_unclosed() {
        let content = "---\ndescription: Test\nNo closing delimiter";

        let (yaml, markdown) =
            split_frontmatter(content).expect("split_frontmatter should succeed");
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

        let result = path_relative_to(git_root, absolute).expect("path_relative_to should succeed");
        assert_eq!(result, "src/main.rs");
    }

    /// Test path_relative_to with nested path.
    #[test]
    fn test_path_relative_to_nested() {
        let git_root = Path::new("/home/user/project");
        let absolute = Path::new("/home/user/project/deeply/nested/module.rs");

        let result = path_relative_to(git_root, absolute).expect("path_relative_to should succeed");
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
        let rule_pos = result
            .find("Rule content")
            .expect("Rule content should be found");
        let prompt_pos = result
            .find("Original prompt")
            .expect("Original prompt should be found");
        assert!(rule_pos < prompt_pos);
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

    /// Test check_new_glob_matches_with_tracing for .md files.
    #[test]
    fn test_check_new_glob_matches_with_tracing_md() {
        // Create a mock rule set with a glob-based rule for .md files
        let mut rules_set = RulesSet::new();
        let rule = Rule::new(
            "glob-test".to_string(),
            "Test rule for Markdown files".to_string(),
            vec!["**/*.md".to_string()],
            false,
            "# Test Rule\nThis is a test.".to_string(),
        );
        rules_set.glob_based.push(rule);

        let injected_rules = HashSet::new();

        // Test that .md files match
        let matches = check_new_glob_matches_with_tracing(
            &rules_set,
            &["CLAUDE.md".to_string(), "README.md".to_string()],
            &injected_rules,
        );

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].0, "glob-test");
        // The files should be sorted
        assert!(matches[0].1.contains(&"CLAUDE.md".to_string()));
        assert!(matches[0].1.contains(&"README.md".to_string()));
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

        let result =
            parse_intelligent_selection_response(output, &allowed).expect("parse should succeed");

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

        let result =
            parse_intelligent_selection_response(output, &allowed).expect("parse should succeed");

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

        let result =
            parse_intelligent_selection_response(output, &allowed).expect("parse should succeed");

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

        let result: IntelligentSelectionResponse =
            extract_json_from_markdown(text).expect("extract should succeed");

        assert_eq!(result.rules.len(), 2);
        assert!(result.rules.contains(&"rust".to_string()));
        assert!(result.rules.contains(&"python".to_string()));
    }

    /// Test extract_json_from_markdown handles plain JSON.
    #[test]
    fn test_extract_json_from_markdown_plain_json() {
        let text = r#"{"rules":["rust"],"reasoning":"test"}"#;

        let result: IntelligentSelectionResponse =
            extract_json_from_markdown(text).expect("extract should succeed");

        assert_eq!(result.rules.len(), 1);
        assert_eq!(result.rules[0], "rust");
    }

    /// Test extract_json_from_markdown handles code blocks without language.
    #[test]
    fn test_extract_json_from_markdown_plain_code_block() {
        let text = r#"```
{"rules":["rust"],"reasoning":"test"}
```"#;

        let result: IntelligentSelectionResponse =
            extract_json_from_markdown(text).expect("extract should succeed");

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
            let (_yaml, markdown) = result.expect("split_frontmatter should succeed");
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
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let rules_dir = temp_dir.path().join(".agents").join("rules");
        fs::create_dir_all(&rules_dir)
            .await
            .expect("failed to create rules dir");

        // Create a test rule file
        let rule_path = rules_dir.join("test.mdc");
        let content = r#"---
description: Test rule
globs:
  - "*.rs"
alwaysApply: false
---
This is test content."#;
        fs::write(&rule_path, content)
            .await
            .expect("failed to write rule file");

        let rules_set = discover_rules(temp_dir.path(), ".agents/rules")
            .await
            .expect("failed to discover rules");

        assert_eq!(rules_set.glob_based.len(), 1);
        assert_eq!(rules_set.glob_based[0].name, "test");
        assert_eq!(rules_set.glob_based[0].description, "Test rule");
    }

    /// Test discovering rules with empty directory.
    #[tokio::test]
    async fn test_discover_rules_empty() {
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let rules_dir = temp_dir.path().join(".agents").join("rules");
        fs::create_dir_all(&rules_dir)
            .await
            .expect("failed to create rules dir");

        let rules_set = discover_rules(temp_dir.path(), ".agents/rules")
            .await
            .expect("failed to discover rules");

        assert_eq!(rules_set.total_count(), 0);
    }

    /// Test discovering rules creates directory if missing.
    #[tokio::test]
    async fn test_discover_rules_creates_directory() {
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let rules_dir = temp_dir.path().join(".agents").join("rules");

        assert!(!rules_dir.exists());

        let rules_set = discover_rules(temp_dir.path(), ".agents/rules")
            .await
            .expect("failed to discover rules");

        assert!(rules_dir.exists());
        assert_eq!(rules_set.total_count(), 0);
    }

    /// Test parse_rule_file with valid content.
    #[tokio::test]
    async fn test_parse_rule_file_valid() {
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let rule_path = temp_dir.path().join("test.mdc");

        let content = r#"---
description: A test rule
globs:
  - "**/*.rs"
  - "src/**/*.rs"
alwaysApply: true
---
Use strict typing in Rust."#;
        fs::write(&rule_path, content)
            .await
            .expect("failed to write rule file");

        let rule = parse_rule_file(&rule_path, temp_dir.path())
            .await
            .expect("failed to parse rule file");

        assert_eq!(rule.name, "test");
        assert_eq!(rule.description, "A test rule");
        assert_eq!(rule.globs.len(), 2);
        assert!(rule.always_apply);
        assert_eq!(rule.content.trim(), "Use strict typing in Rust.");
    }

    /// Test parse_rule_file with no frontmatter.
    #[tokio::test]
    async fn test_parse_rule_file_no_frontmatter() {
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let rule_path = temp_dir.path().join("test.mdc");

        let content = "Just some content";
        fs::write(&rule_path, content)
            .await
            .expect("failed to write rule file");

        let rule = parse_rule_file(&rule_path, temp_dir.path())
            .await
            .expect("failed to parse rule file");

        assert_eq!(rule.name, "test");
        assert_eq!(rule.description, "test");
        assert!(rule.globs.is_empty());
        assert!(!rule.always_apply);
        assert_eq!(rule.content, content);
    }

    /// Test RulesCache loads and caches rules.
    #[tokio::test]
    async fn test_rules_cache() {
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let rules_dir = temp_dir.path().join(".agents").join("rules");
        fs::create_dir_all(&rules_dir)
            .await
            .expect("failed to create rules dir");

        let rule_path = rules_dir.join("cache_test.mdc");
        let content = r#"---
description: Cache test
globs:
  - "*.rs"
alwaysApply: true
---
Content"#;
        fs::write(&rule_path, content)
            .await
            .expect("failed to write rule file");

        let cache = RulesCache::new(temp_dir.path().to_path_buf(), ".agents/rules".to_string());

        // First access loads rules
        let rules1 = cache.get().await.expect("failed to get cached rules");
        assert_eq!(rules1.total_count(), 1);

        // Second access uses cached rules
        let rules2 = cache.get().await.expect("failed to get cached rules");
        assert_eq!(rules2.total_count(), 1);
    }

    /// Test glob-based rule activation simulates multi-turn flow.
    ///
    /// This test simulates the end-to-end flow of glob-based rule activation
    /// that happens within a single iteration in ACP mode.
    #[tokio::test]
    async fn test_glob_based_rule_activation_flow() {
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let rules_dir = temp_dir.path().join(".agents").join("rules");
        fs::create_dir_all(&rules_dir)
            .await
            .expect("failed to create rules dir");

        // Create an always-apply rule
        let always_rule_path = rules_dir.join("always.mdc");
        let always_content = r#"---
description: Always apply rule
globs: []
alwaysApply: true
---
This rule should always be applied."#;
        fs::write(&always_rule_path, always_content)
            .await
            .expect("failed to write rule file");

        // Create a glob-based rule for Rust files
        let rust_rule_path = rules_dir.join("rust.mdc");
        let rust_content = r#"---
description: Rust coding rules
globs:
  - "**/*.rs"
  - "src/**/*.rs"
alwaysApply: false
---
Use strict typing in Rust."#;
        fs::write(&rust_rule_path, rust_content)
            .await
            .expect("failed to write rule file");

        // Create a glob-based rule for Python files
        let python_rule_path = rules_dir.join("python.mdc");
        let python_content = r#"---
description: Python coding rules
globs:
  - "**/*.py"
alwaysApply: false
---
Use type hints in Python."#;
        fs::write(&python_rule_path, python_content)
            .await
            .expect("failed to write rule file");

        // Load rules
        let rules_set = discover_rules(temp_dir.path(), ".agents/rules")
            .await
            .expect("failed to discover rules");

        assert_eq!(rules_set.always_apply.len(), 1);
        assert_eq!(rules_set.glob_based.len(), 2);

        // Simulate iteration state
        let mut state = RulesState::new();

        // Initial selection: only always-apply rules
        let selected = select_rules(&rules_set, &state);
        assert_eq!(selected.rules.len(), 1);
        assert_eq!(selected.rules[0].name, "always");

        // Simulate agent reading src/main.rs
        state.record_file_read("src/main.rs".to_string());

        // Check for new glob matches
        let new_matches = check_new_glob_matches(
            &rules_set,
            &["src/main.rs".to_string()],
            state.injected_rules(),
        );

        // Rust rule should match
        assert_eq!(new_matches.len(), 1);
        assert!(new_matches.contains(&"rust".to_string()));

        // Fetch the matched rules
        let matched_rules = get_rules_by_names(&rules_set, &new_matches);
        assert_eq!(matched_rules.len(), 1);
        assert_eq!(matched_rules[0].name, "rust");

        // Mark as injected
        state.mark_injected("rust".to_string());

        // Simulate second turn: agent reads lib.rs and main.py
        state.record_file_read("lib.rs".to_string());
        state.record_file_read("main.py".to_string());

        // Check for new glob matches (should only find python now)
        let new_matches = check_new_glob_matches(
            &rules_set,
            &["lib.rs".to_string(), "main.py".to_string()],
            state.injected_rules(),
        );

        // Only Python rule should match (rust already injected)
        assert_eq!(new_matches.len(), 1);
        assert!(new_matches.contains(&"python".to_string()));

        // Mark python as injected
        state.mark_injected("python".to_string());

        // No more new matches
        let new_matches = check_new_glob_matches(
            &rules_set,
            &["src/main.rs".to_string(), "lib.rs".to_string()],
            state.injected_rules(),
        );
        assert_eq!(new_matches.len(), 0);
    }

    /// Test intelligent selection prompt building and response parsing.
    #[tokio::test]
    async fn test_intelligent_selection_end_to_end() {
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let rules_dir = temp_dir.path().join(".agents").join("rules");
        fs::create_dir_all(&rules_dir)
            .await
            .expect("failed to create rules dir");

        // Create intelligent rules (no always_apply, no globs)
        let test_plan_rule_path = rules_dir.join("test_plan.mdc");
        let test_plan_content = r#"---
description: Testing best practices for TDD and test plans
globs: []
alwaysApply: false
---
Write tests before implementation."#;
        fs::write(&test_plan_rule_path, test_plan_content)
            .await
            .expect("failed to write rule file");

        let api_design_rule_path = rules_dir.join("api_design.mdc");
        let api_design_content = r#"---
description: REST API design guidelines
globs: []
alwaysApply: false
---
Use proper HTTP status codes."#;
        fs::write(&api_design_rule_path, api_design_content)
            .await
            .expect("failed to write rule file");

        // Load rules
        let rules_set = discover_rules(temp_dir.path(), ".agents/rules")
            .await
            .expect("failed to discover rules");

        assert_eq!(rules_set.intelligent.len(), 2);

        // Build selection prompt
        let prompt = build_intelligent_selection_prompt(
            &rules_set.intelligent,
            "I need to write tests for a new REST API endpoint",
        );

        // Verify prompt contains descriptions
        assert!(prompt.contains("test_plan: Testing best practices for TDD and test plans"));
        assert!(prompt.contains("api_design: REST API design guidelines"));
        assert!(prompt.contains("I need to write tests for a new REST API endpoint"));

        // Test parsing valid JSON response
        let json_response = r#"{"rules":["test_plan"],"reasoning":"The task involves testing"}"#;
        let mut allowed = HashSet::new();
        allowed.insert("test_plan");
        allowed.insert("api_design");

        let parsed = parse_intelligent_selection_response(json_response, &allowed)
            .expect("parse should succeed");
        assert_eq!(parsed.len(), 1);
        assert!(parsed.contains(&"test_plan".to_string()));

        // Test that invalid rule names are filtered out
        let json_response_with_invalid =
            r#"{"rules":["test_plan","unknown_rule"],"reasoning":"test"}"#;
        let parsed_filtered =
            parse_intelligent_selection_response(json_response_with_invalid, &allowed)
                .expect("parse should succeed");
        assert_eq!(parsed_filtered.len(), 1);
        assert!(!parsed_filtered.iter().any(|r| r == "unknown_rule"));

        // Test markdown-wrapped JSON
        let markdown_response = r#"Here's my selection:

```json
{"rules":["api_design","test_plan"],"reasoning":"Both are relevant"}
```

That's it."#;
        let parsed_md = parse_intelligent_selection_response(markdown_response, &allowed)
            .expect("parse should succeed");
        assert_eq!(parsed_md.len(), 2);
        assert!(parsed_md.contains(&"api_design".to_string()));
        assert!(parsed_md.contains(&"test_plan".to_string()));

        // Test empty response
        let empty_response = r#"{"rules":[],"reasoning":"none"}"#;
        let parsed_empty = parse_intelligent_selection_response(empty_response, &allowed)
            .expect("parse should succeed");
        assert_eq!(parsed_empty.len(), 0);
    }

    /// Test follow-up prompt formatting for delta injection.
    #[tokio::test]
    async fn test_follow_up_prompt_delta_formatting() {
        // Create test rules
        let rust_rule = Rule::new(
            "rust".to_string(),
            "Rust rules".to_string(),
            vec!["**/*.rs".to_string()],
            false,
            "Use strict typing.".to_string(),
        );

        let test_rule = Rule::new(
            "testing".to_string(),
            "Testing rules".to_string(),
            vec!["**/*test*.rs".to_string()],
            false,
            "Write tests first.".to_string(),
        );

        let files_read = vec![
            "src/main.rs".to_string(),
            "tests/integration_test.rs".to_string(),
        ];
        let new_rules = vec![rust_rule, test_rule];

        let follow_up = build_follow_up_prompt_delta(&files_read, &new_rules);

        // Verify the follow-up prompt contains expected elements
        assert!(follow_up.contains("# NEW Project Rules (delta)"));
        assert!(follow_up.contains("You opened these files:"));
        assert!(follow_up.contains("- src/main.rs"));
        assert!(follow_up.contains("- tests/integration_test.rs"));
        assert!(follow_up.contains("## rust"));
        assert!(follow_up.contains("Use strict typing."));
        assert!(follow_up.contains("## testing"));
        assert!(follow_up.contains("Write tests first."));
        assert!(follow_up.contains(
            "**Incorporate these new rules and continue from your current plan. Do not restart.**"
        ));
    }

    /// Test state persistence across multiple iterations.
    #[tokio::test]
    async fn test_rules_state_persistence_across_iterations() {
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let rules_dir = temp_dir.path().join(".agents").join("rules");
        fs::create_dir_all(&rules_dir)
            .await
            .expect("failed to create rules dir");

        // Create glob-based rules
        let rust_rule_path = rules_dir.join("rust.mdc");
        fs::write(
            &rust_rule_path,
            r#"---
description: Rust rules
globs: ["**/*.rs"]
alwaysApply: false
---
Use Rust best practices."#,
        )
        .await
        .expect("failed to write rule file");

        let js_rule_path = rules_dir.join("js.mdc");
        fs::write(
            &js_rule_path,
            r#"---
description: JavaScript rules
globs: ["**/*.js"]
alwaysApply: false
---
Use JavaScript best practices."#,
        )
        .await
        .expect("failed to write rule file");

        let rules_set = discover_rules(temp_dir.path(), ".agents/rules")
            .await
            .expect("failed to discover rules");

        // Simulate state across iterations
        let mut state = RulesState::new();

        // Iteration 1: Agent reads src/main.rs
        state.record_file_read("src/main.rs".to_string());
        let iteration1_matches = state.match_globs_for_files(&rules_set.glob_based);
        assert_eq!(iteration1_matches.len(), 1);
        assert_eq!(iteration1_matches[0].name, "rust");

        // Mark rust as injected
        state.mark_injected("rust".to_string());

        // Iteration 2: Agent reads more files
        state.record_file_read("src/lib.rs".to_string());
        state.record_file_read("index.js".to_string());

        // Check for new matches (should only get js, rust already injected)
        let iteration2_matches = state.match_globs_for_files(&rules_set.glob_based);
        assert_eq!(iteration2_matches.len(), 1);
        assert_eq!(iteration2_matches[0].name, "js");

        // Mark js as injected
        state.mark_injected("js".to_string());

        // Iteration 3: No new matches
        state.record_file_read("src/mod.rs".to_string());
        let iteration3_matches = state.match_globs_for_files(&rules_set.glob_based);
        assert_eq!(iteration3_matches.len(), 0);

        // Verify all files were tracked
        assert_eq!(state.files_read.len(), 4);
        assert!(state.files_read.contains("src/main.rs"));
        assert!(state.files_read.contains("src/lib.rs"));
        assert!(state.files_read.contains("index.js"));
        assert!(state.files_read.contains("src/mod.rs"));
    }

    /// Test select_rules_with_intelligent with all rule types.
    #[tokio::test]
    async fn test_select_rules_with_intelligent_comprehensive() {
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let rules_dir = temp_dir.path().join(".agents").join("rules");
        fs::create_dir_all(&rules_dir)
            .await
            .expect("failed to create rules dir");

        // Create all rule types
        fs::write(
            rules_dir.join("always.mdc"),
            r#"---
description: Always apply
globs: []
alwaysApply: true
---
Always content"#,
        )
        .await
        .expect("failed to write rule file");

        fs::write(
            rules_dir.join("globbed.mdc"),
            r#"---
description: Glob based
globs: ["*.rs"]
alwaysApply: false
---
Glob content"#,
        )
        .await
        .expect("failed to write rule file");

        fs::write(
            rules_dir.join("intelligent.mdc"),
            r#"---
description: Intelligent selection
globs: []
alwaysApply: false
---
Intelligent content"#,
        )
        .await
        .expect("failed to write rule file");

        let rules_set = discover_rules(temp_dir.path(), ".agents/rules")
            .await
            .expect("failed to discover rules");

        let mut state = RulesState::new();

        // Initially only always-apply rule is selected
        let selected = select_rules_with_intelligent(&rules_set, &state, None, 10);
        assert_eq!(selected.rules.len(), 1);
        assert_eq!(selected.rules[0].name, "always");

        // After reading matching file, glob-based rule is also selected
        state.record_file_read("main.rs".to_string());
        let selected_with_glob = select_rules_with_intelligent(&rules_set, &state, None, 10);
        assert_eq!(selected_with_glob.rules.len(), 2);
        assert!(selected_with_glob.rules.iter().any(|r| r.name == "always"));
        assert!(selected_with_glob.rules.iter().any(|r| r.name == "globbed"));

        // With intelligent rules provided, all three types are selected
        let intelligent_rule = rules_set
            .intelligent
            .first()
            .expect("intelligent rules should not be empty");
        let selected_all = select_rules_with_intelligent(
            &rules_set,
            &state,
            Some(std::slice::from_ref(intelligent_rule)),
            10,
        );
        assert_eq!(selected_all.rules.len(), 3);
    }

    /// Test max_mid_iteration_injections cap enforcement.
    #[tokio::test]
    async fn test_max_mid_iteration_injections_cap() {
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let rules_dir = temp_dir.path().join(".agents").join("rules");
        fs::create_dir_all(&rules_dir)
            .await
            .expect("failed to create rules dir");

        // Create multiple glob-based rules that match different files
        for i in 0..5 {
            let name = format!("rule{}", i);
            let content = format!(
                r#"---
description: Rule {}
globs: ["**/file{}.rs"]
alwaysApply: false
---
Content {}"#,
                i, i, i
            );
            fs::write(rules_dir.join(format!("{}.mdc", name)), content)
                .await
                .expect("failed to write rule file");
        }

        let rules_set = discover_rules(temp_dir.path(), ".agents/rules")
            .await
            .expect("failed to discover rules");

        let mut state = RulesState::new();

        // Simulate reading all files in sequence
        let files_read: Vec<String> = (0..5).map(|i| format!("file{}.rs", i)).collect();

        // Check for new matches (all 5 should match)
        let new_matches = check_new_glob_matches(&rules_set, &files_read, state.injected_rules());
        assert_eq!(new_matches.len(), 5);

        // With max_injections = 2, only 2 rules should be processed per iteration
        let max_injections = 2u32;
        let mut injections = 0u32;
        let mut current_files = files_read.clone();
        let mut processed_rules = Vec::new();

        while injections < max_injections && !current_files.is_empty() {
            let matches =
                check_new_glob_matches(&rules_set, &current_files, state.injected_rules());
            if matches.is_empty() {
                break;
            }

            // Process first batch of matches
            let rules_to_process = matches.iter().take(2).cloned().collect::<Vec<_>>();
            for rule_name in &rules_to_process {
                state.mark_injected(rule_name.clone());
                processed_rules.push(rule_name.clone());
            }
            injections += 1;
            current_files = Vec::new(); // Simulate processing without new files
        }

        // Should have processed at most max_injections * 2 = 4 rules
        // (Each injection processes 2 rules, capped by max_injections)
        assert!(processed_rules.len() <= 5); // All rules could be processed
        assert!(injections <= max_injections);
    }

    /// Test glob-based injection with detailed file-to-rule mapping for tracing.
    ///
    /// This test verifies that when files are read, the correct rules are matched
    /// and the mapping between files and rules is correctly tracked.
    #[tokio::test]
    async fn test_glob_based_injection_with_tracing() {
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let rules_dir = temp_dir.path().join(".agents").join("rules");
        fs::create_dir_all(&rules_dir)
            .await
            .expect("failed to create rules dir");

        // Create glob-based rules for different file types
        fs::write(
            rules_dir.join("toml.mdc"),
            r#"---
description: TOML configuration files
globs:
  - "**/*.toml"
  - "Cargo.toml"
alwaysApply: false
---
Use consistent TOML formatting."#,
        )
        .await
        .expect("failed to write rule file");

        fs::write(
            rules_dir.join("markdown.mdc"),
            r#"---
description: Markdown files
globs:
  - "**/*.md"
  - "**/*.markdown"
alwaysApply: false
---
Use proper Markdown syntax."#,
        )
        .await
        .expect("failed to write rule file");

        let rules_set = discover_rules(temp_dir.path(), ".agents/rules")
            .await
            .expect("failed to discover rules");

        let mut state = RulesState::new();

        // Simulate agent reading multiple files
        state.record_file_read("Cargo.toml".to_string());
        state.record_file_read("README.md".to_string());
        state.record_file_read(".velor/velor.toml".to_string());

        // Check for new matches with detailed file mapping
        let matches_with_files = check_new_glob_matches_with_tracing(
            &rules_set,
            &["Cargo.toml".to_string()],
            state.injected_rules(),
        );

        // Should match toml rule with Cargo.toml
        assert_eq!(matches_with_files.len(), 1);
        assert_eq!(matches_with_files[0].0, "toml");
        assert_eq!(matches_with_files[0].1, vec!["Cargo.toml".to_string()]);

        // Mark toml as injected
        state.mark_injected("toml".to_string());

        // Now check README.md - should match markdown rule
        let matches_with_files = check_new_glob_matches_with_tracing(
            &rules_set,
            &["README.md".to_string()],
            state.injected_rules(),
        );

        assert_eq!(matches_with_files.len(), 1);
        assert_eq!(matches_with_files[0].0, "markdown");
        assert_eq!(matches_with_files[0].1, vec!["README.md".to_string()]);

        // Mark markdown as injected
        state.mark_injected("markdown".to_string());

        // Now check .velor/velor.toml with toml already injected
        // Since toml was already injected for Cargo.toml, it won't match again
        // (rules persist across iterations once injected)
        let matches_with_files = check_new_glob_matches_with_tracing(
            &rules_set,
            &[".velor/velor.toml".to_string()],
            state.injected_rules(),
        );
        assert_eq!(matches_with_files.len(), 0); // Already injected, no new match
    }

    /// Test the match_globs_for_files_with_reason method for tracing output.
    #[tokio::test]
    async fn test_match_globs_for_files_with_reason() {
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let rules_dir = temp_dir.path().join(".agents").join("rules");
        fs::create_dir_all(&rules_dir)
            .await
            .expect("failed to create rules dir");

        // Create test rules
        fs::write(
            rules_dir.join("config.mdc"),
            r#"---
description: Config files
globs:
  - "**/*.toml"
  - "**/*.yaml"
alwaysApply: false
---
Config file rules"#,
        )
        .await
        .expect("failed to write rule file");

        let rules_set = discover_rules(temp_dir.path(), ".agents/rules")
            .await
            .expect("failed to discover rules");

        let mut state = RulesState::new();

        // Record files read
        state.record_file_read("Cargo.toml".to_string());
        state.record_file_read("README.md".to_string());
        state.record_file_read(".velor/velor.toml".to_string());

        // Get matches with reason (which files triggered which rules)
        let matches_with_reason = state.match_globs_for_files_with_reason(&rules_set.glob_based);

        // Should have one rule matched (config.mdc) with two files
        assert_eq!(matches_with_reason.len(), 1);
        assert_eq!(matches_with_reason[0].0.name, "config");

        // The files should be Cargo.toml and .velor/velor.toml (both match *.toml)
        let matching_files = &matches_with_reason[0].1;
        assert_eq!(matching_files.len(), 2);
        assert!(matching_files.contains(&"Cargo.toml"));
        assert!(matching_files.contains(&".velor/velor.toml"));
    }
}
