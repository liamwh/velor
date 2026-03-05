# File-Based Automations for Velor CLI

## Context

Currently, automations are defined as TOML files in a single directory (`.velor/automations.d/`). This plan adds support for:

1. **Dual-location discovery**: Global automations in `XDG_CONFIG_HOME/velor/automations/` and project-specific in `{repo}/.velor/automations/`
2. **Per-automation configuration**: Each automation as its own TOML file with name, schedule, prompt source, vars, worktree setting, and project path
3. **Flexible prompt sources**: Inline prompts, file references, or prompt names from the prompts directory
4. **Worktree control**: Option to run in dedicated worktree or on current branch (default)

## Implementation Plan

### Phase 1: Core Types

**File: `crates/automations/src/file_config.rs`** (new)

Create raw and validated types for strong typing:

```rust
/// Source of an automation definition
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutomationSource {
    Global,   // ~/.config/velor/automations/
    Project,  // {repo}/.velor/automations/
    Legacy,   // .velor/automations.d/ (deprecated)
}

/// Raw TOML representation (before validation)
#[derive(Debug, Clone, Deserialize)]
pub struct AutomationFileRaw {
    pub description: String,
    pub schedule: String,
    /// Timezone: None means system local timezone (not UTC)
    pub timezone: Option<String>,
    #[serde(flatten)]
    pub prompt_source: PromptSourceRaw,
    #[serde(default)]
    pub worktree: bool,
    /// Working directory for execution (can be inside a repo)
    pub project: Option<PathBuf>,
    #[serde(default)]
    pub vars: BTreeMap<String, String>,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub catch_up: CatchUpPolicy,
    #[serde(default)]
    pub max_catch_up: u32,
    pub timeout_seconds: Option<u64>,
    #[serde(default = "default_true")]
    pub notify_on_success: bool,
    #[serde(default = "default_true")]
    pub notify_on_failure: bool,
}

/// Validated automation (pure config, no provenance)
#[derive(Debug, Clone)]
pub struct AutomationFile {
    pub name: String,
    pub description: String,
    /// Raw cron expression (for display)
    pub schedule_raw: String,
    /// Parsed cron schedule for next-run calculation
    pub schedule: cron::Schedule,
    /// Timezone for schedule interpretation
    pub timezone: chrono_tz::Tz,
    pub prompt_source: PromptSource,
    pub worktree: bool,
    /// Working directory (resolved, validated)
    pub project: Option<PathBuf>,
    pub vars: BTreeMap<String, String>,
    pub enabled: bool,
    pub catch_up: CatchUpPolicy,
    pub max_catch_up: u32,
    pub timeout_seconds: Option<u64>,
    pub notify_on_success: bool,
    pub notify_on_failure: bool,
}

/// Cache entry with provenance metadata
#[derive(Debug, Clone)]
pub struct AutomationEntry {
    pub automation: AutomationFile,
    pub source: AutomationSource,
    pub path: PathBuf,
}

/// Raw prompt source (Option B: explicit struct for better validation)
#[derive(Debug, Clone, Deserialize, Default)]
pub struct PromptSourceRaw {
    pub prompt: Option<String>,
    pub prompt_file: Option<String>,  // Looks in {repo}/.velor/prompts/ or ~/.velor/prompts/
    pub prompt_name: Option<String>,  // Looks up named prompt from PromptCache
}

/// Validated prompt source (exactly one field set)
#[derive(Debug, Clone)]
pub enum PromptSource {
    Inline(String),
    /// Reference to a file in the prompts/ directory (renamed from File for clarity)
    PromptsDirFile(String),
    /// Reference to a named prompt from velor.toml [prompts]
    Name(String),
}

fn default_enabled() -> bool {
    true
}

fn default_true() -> bool {
    true
}

/// Get system local timezone
fn get_local_timezone() -> chrono_tz::Tz {
    iana_time_zone::get_timezone()
        .ok()
        .and_then(|tz| tz.parse().ok())
        .unwrap_or(chrono_tz::UTC)
}

/// Normalize cron expression to 6-field format (seconds minutes hours day month weekday)
fn normalize_cron_schedule(expr: &str) -> Result<String> {
    let parts: Vec<&str> = expr.split_whitespace().collect();

    let normalized = match parts.len() {
        5 => format!("0 {}", expr),  // Prefix with 0 seconds
        6 => expr.to_string(),
        _ => {
            return Err(eyre!(
                "Invalid cron expression '{}': expected 5 or 6 fields (seconds minutes hours day month weekday), got {}",
                expr, parts.len()
            ));
        }
    };

    Ok(normalized)
}
```

**Dependencies** (add to `Cargo.toml`):

```toml
# For cron parsing with timezone support
cron = "0.12"
# For detecting system local timezone
iana-time-zone = "0.1"
```

**Important**: The `cron` crate's timezone semantics must be validated. The schedule is evaluated in the target timezone, then converted to UTC for storage. This ensures DST transitions are handled correctly by the crate's timezone-aware scheduling.

Validation function:

```rust
impl PromptSource {
    pub fn from_raw(raw: PromptSourceRaw) -> Result<Self> {
        let fields_set = [
            raw.prompt.is_some(),
            raw.prompt_file.is_some(),
            raw.prompt_name.is_some(),
        ].iter().filter(|&&x| x).count();

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
                prompt_file: Some(f), ..
            } => {
                let normalized = f.strip_suffix(".md").unwrap_or(&f);
                PromptSource::PromptsDirFile(normalized.to_string())
            }
            PromptSourceRaw {
                prompt_name: Some(n), ..
            } => PromptSource::Name(n),
            _ => unreachable!(),
        })
    }
}

impl AutomationFile {
    pub fn from_raw(
        name: String,
        raw: AutomationFileRaw,
    ) -> Result<Self> {
        // Parse timezone (None means local timezone)
        let timezone = match raw.timezone {
            None => get_local_timezone(),
            Some(ref tz) if tz.is_empty() => {
                return Err(eyre!("timezone cannot be an empty string; omit the field or use a valid IANA timezone"));
            }
            Some(tz) => {
                tz.parse::<chrono_tz::Tz>()
                    .map_err(|_| eyre!("Invalid timezone: '{}'", tz))?
            }
        };

        // Normalize and parse cron expression
        let normalized_schedule = normalize_cron_schedule(&raw.schedule)?;
        let schedule = cron::Schedule::from_str(&normalized_schedule)
            .map_err(|e| eyre!("Invalid cron expression '{}': {}", raw.schedule, e))?;

        // Validate prompt source
        let prompt_source = PromptSource::from_raw(raw.prompt_source)?;

        // Validate project path if specified
        let project = raw.project.clone();  // Keep for async validation later

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

    /// Get next scheduled occurrence after a given time
    ///
    /// IMPORTANT: The cron crate evaluates schedules in the target timezone.
    /// DST transitions are handled by the crate's timezone-aware scheduling.
    pub fn next_after(&self, after: chrono::DateTime<chrono::Utc>) -> chrono::DateTime<chrono::Utc> {
        // Convert to target timezone, evaluate schedule, convert back to UTC
        let after_tz = after.with_timezone(&self.timezone);
        let next = self.schedule.after(&after_tz).next();
        next.unwrap().with_timezone(&chrono::Utc)
    }

    /// Get missed runs since a given time (up to max_count)
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

    /// Get stale-run timeout based on automation's configured timeout
    pub fn stale_run_timeout(&self) -> chrono::Duration {
        let timeout_secs = self.timeout_seconds.unwrap_or(3600);
        // Use 2x the automation timeout, minimum 15 minutes
        let secs = (timeout_secs * 2).max(900);
        chrono::Duration::seconds(secs as i64)
    }
}
```

**DST Tests** (add to `tests/`):

The cron crate must be validated for DST behavior. Write tests for:

```rust
#[cfg(test)]
mod dst_tests {
    use super::*;
    use chrono_tz::Europe::Amsterdam;

    #[test]
    fn test_spring_forward_no_double_run() {
        // 2024-03-31 02:30 CET doesn't exist (spring forward)
        // Schedule "0 30 2 * * *" should not panic or double-run
        let raw = AutomationFileRaw {
            description: "test".to_string(),
            schedule: "0 30 2 * * *".to_string(),
            timezone: Some("Europe/Amsterdam".to_string()),
            // ... rest of fields ...
        };
        let automation = AutomationFile::from_raw("test".to_string(), raw).unwrap();
        // Verify: next_after(2024-03-30 02:30 CET) gives 2024-03-31 03:00 CEST
    }

    #[test]
    fn test_fall_back_runs_once() {
        // 2024-10-27 02:30 CEST happens twice (fall back)
        // Schedule "0 30 2 * * *" should run at first occurrence
        let raw = AutomationFileRaw {
            description: "test".to_string(),
            schedule: "0 30 2 * * *".to_string(),
            timezone: Some("Europe/Amsterdam".to_string()),
            // ... rest of fields ...
        };
        let automation = AutomationFile::from_raw("test".to_string(), raw).unwrap();
        // Verify: next_after respects the chosen DST rule
    }

    #[test]
    fn test_weekly_schedule_stable_across_dst() {
        // "Mon-Fri at 09:00" should stay stable around DST
        let raw = AutomationFileRaw {
            description: "test".to_string(),
            schedule: "0 0 9 * * Mon-Fri".to_string(),
            timezone: Some("America/New_York".to_string()),
            // ... rest of fields ...
        };
        let automation = AutomationFile::from_raw("test".to_string(), raw).unwrap();
        // Verify: 09:00 local time is consistent across DST transition
    }
}
```

### Phase 2: Automation Cache (Discovery)

**File: `crates/automations/src/cache.rs`** (new)

Mirror the `PromptCache` pattern - load fresh each time (fast enough):

```rust
pub struct AutomationCache {
    home_dir: PathBuf,          // XDG_CONFIG_HOME/velor
    repo_dir: Option<PathBuf>,  // {git_root}/.velor
}

impl AutomationCache {
    pub fn new(home_dir: PathBuf, repo_dir: Option<PathBuf>) -> Self {
        Self { home_dir, repo_dir }
    }

    /// Returns all automations with project overriding global by name
    pub async fn get(&self) -> Result<BTreeMap<String, AutomationEntry>> {
        let mut home_automations = self.discover_automations(
            &self.home_dir,
            AutomationSource::Global,
        ).await?;

        let repo_automations = if let Some(ref repo_dir) = self.repo_dir {
            self.discover_automations(repo_dir, AutomationSource::Project).await?
        } else {
            BTreeMap::new()
        };

        // Merge: project overrides global by name
        for (name, entry) in repo_automations {
            home_automations.insert(name, entry);
        }

        Ok(home_automations)
    }

    /// Fetches a single automation (respects override precedence)
    pub async fn get_by_name(&self, name: &str) -> Result<AutomationEntry> {
        let all = self.get().await?;
        all.get(name)
            .cloned()
            .ok_or_else(|| eyre!("automation '{}' not found", name))
    }

    /// Lists all automations including duplicates (shows source of each)
    pub async fn list_all(&self) -> Result<Vec<AutomationEntry>> {
        let mut result = Vec::new();

        // Load home automations
        let home = self.discover_automations(
            &self.home_dir,
            AutomationSource::Global,
        ).await?;
        result.extend(home.into_values());

        // Load repo automations
        if let Some(ref repo_dir) = self.repo_dir {
            let repo = self.discover_automations(
                repo_dir,
                AutomationSource::Project,
            ).await?;
            result.extend(repo.into_values());
        }

        // Sort by name for consistent output
        result.sort_by(|a, b| a.automation.name.cmp(&b.automation.name));
        Ok(result)
    }

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
                return Err(e).wrap_err_with(|| format!(
                    "Failed to read automations directory: {}",
                    automations_dir.display()
                ))
            }
        };

        let mut automations = BTreeMap::new();

        while let Some(entry) = entries.next_entry().await
            .wrap_err("Failed to read directory entry")?
        {
            let path = entry.path();

            if path.is_dir() {
                continue;
            }

            if path.extension().and_then(|s| s.to_str()) != Some("toml") {
                continue;
            }

            let (name, automation_file) = parse_automation_file(&path, source).await?;

            automations.insert(name, AutomationEntry {
                automation: automation_file,
                source,
                path,
            });
        }

        Ok(automations)
    }
}

/// Parse a single automation TOML file
async fn parse_automation_file(
    path: &Path,
    source: AutomationSource,
) -> Result<(String, AutomationFile)> {
    let content = fs::read_to_string(path).await
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
    let automation = validate_and_convert(name.clone(), raw, path).await?;

    Ok((name, automation))
}

/// Validate project path using async metadata check
async fn validate_and_convert(
    name: String,
    mut raw: AutomationFileRaw,
    path: &Path,
) -> Result<AutomationFile> {
    // Validate project path exists using async metadata
    if let Some(ref proj) = raw.project {
        match fs::metadata(proj).await {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(eyre!("project path {} does not exist", proj.display()));
            }
            Err(e) => {
                return Err(e).wrap_err_with(|| format!(
                    "Failed to access project path: {}", proj.display()
                ));
            }
        }
    }

    AutomationFile::from_raw(name, raw)
}
```

**Helper in `apps/velor-cli/src/automations.rs`**:

```rust
fn get_xdg_config_home() -> PathBuf {
    std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .map(|h| h.join(".config"))
                .expect("Unable to determine home directory")
        })
}
```

### Phase 2b: Prompt Source Resolution

**File: `crates/automations/src/file_config.rs`** (add)

```rust
impl PromptSource {
    /// Resolve to actual prompt content
    ///
    /// For PromptsDirFile references: Try repo prompts first, then global (override pattern).
    /// Uses try-read approach instead of exists() to avoid sync filesystem calls.
    pub async fn resolve(
        &self,
        prompt_cache: &PromptCache,
        home_dir: &Path,
        repo_dir: Option<&Path>,
    ) -> Result<String> {
        match self {
            PromptSource::Inline(content) => Ok(content.clone()),
            PromptSource::PromptsDirFile(name) => {
                let filename = format!("{name}.md");

                // Try repo prompts first
                if let Some(repo) = repo_dir {
                    let repo_path = repo.join("prompts").join(&filename);
                    match tokio::fs::read_to_string(&repo_path).await {
                        Ok(content) => return Ok(content),
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                            // Fall through to global
                        }
                        Err(e) => {
                            return Err(e).wrap_err_with(||
                                format!("Failed to read prompt file: {}", repo_path.display())
                            );
                        }
                    }
                }

                // Try global prompts
                let global_path = home_dir.join("prompts").join(&filename);
                tokio::fs::read_to_string(&global_path).await
                    .wrap_err_with(|| {
                        format!("Prompt file '{name}' not found in repo or global prompts directories (tried: {})",
                            if repo_dir.is_some() {
                                format!("{}/prompts/{} and {}/prompts/{}",
                                    repo_dir.unwrap().display(), filename,
                                    home_dir.display(), filename)
                            } else {
                                format!("{}/prompts/{}", home_dir.display(), filename)
                            }
                        )
                    })
            }
            PromptSource::Name(name) => {
                prompt_cache.get_by_name(name).await
                    .map(|p| p.content)
            }
        }
    }
}
```

### Phase 3: Variable Merging

**File: `crates/automations/src/vars.rs`** (new)

```rust
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

/// Merge variables from multiple sources with built-ins
///
/// Precedence (highest to lowest):
/// 1. Built-in variables (git_root, cwd, now, repo, branch, etc.)
/// 2. Automation-specific vars (from automation file)
/// 3. Repo config vars (from .velor/velor.toml)
/// 4. Home config vars (from ~/.velor/velor.toml)
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
    if let Some(repo_name) = git_root.file_name() {
        if let Some(name) = repo_name.to_str() {
            merged.insert("repo".to_string(), name.to_string());
        }
    }

    // Try to get current branch (best-effort, don't error on failure)
    if let Ok(branch) = get_current_branch(git_root) {
        merged.insert("branch".to_string(), branch);
    }

    merged
}

/// Get current git branch name (best-effort)
///
/// Uses .arg() with Path to avoid UTF-8 conversion issues.
/// Returns empty string on failure (best-effort for vars).
fn get_current_branch(git_root: &Path) -> Result<String, Box<dyn std::error::Error>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(git_root)  // Pass Path directly, no display().to_string()
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
```

### Phase 4: Update AutomationRunner

**File: `crates/automations/src/runner.rs`** (modify)

1. Add git root resolution helper (truly handles non-UTF8 paths)
2. Add `worktree` and `project` handling with clear semantics
3. Sanitize worktree names with collision resistance
4. Create worktrees base directory at runner init (not in hot path)

**Project semantics**:
- `project` is the working directory (can be inside a repo)
- Git root is derived from `project` via `git rev-parse --show-toplevel`
- If `worktree=true` and `project` is outside a git repo → config error

```rust
impl AutomationRunner {
    /// Resolve git root from a given path (handles non-UTF8 paths)
    async fn resolve_git_root(&self, path: &Path) -> Result<PathBuf> {
        // Use .arg() with OsStr to handle non-UTF8 paths
        let output = Command::new("git")
            .arg("-C")
            .arg(path)  // OsStr passed directly
            .args(["rev-parse", "--show-toplevel"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await?;

        if !output.status.success() {
            return Err(eyre!(
                "Failed to resolve git root for {}: git exited with {:?}",
                path.display(),
                output.status.code()
            ));
        }

        let git_root = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Ok(PathBuf::from(git_root))
    }

    /// Sanitize automation name for use in worktree paths
    fn sanitize_worktree_name(name: &str) -> String {
        name.chars()
            .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
            .collect::<String>()
            .split('-')
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("-")
    }

    /// Generate unique worktree path with collision resistance
    fn generate_worktree_path(
        git_root: &Path,
        automation_name: &str,
    ) -> PathBuf {
        let sanitized = Self::sanitize_worktree_name(automation_name);

        // Use parent directory or a dedicated .velor-worktrees directory
        let worktrees_base = git_root
            .parent()
            .unwrap_or(git_root)
            .join(".velor-worktrees");

        // Add ULID for collision resistance
        let ulid = ulid::Ulid::new().to_string();
        let wt_name = format!("automation-{}-{}", sanitized, &ulid[..8]);
        worktrees_base.join(wt_name)
    }

    /// In new() or init: create worktrees base directory once (sync OK here)
    async fn init_worktrees_base(&self) -> Result<()> {
        let worktrees_base = self.git_root
            .parent()
            .unwrap_or(&self.git_root)
            .join(".velor-worktrees");

        tokio::fs::create_dir_all(&worktrees_base).await
            .wrap_err_with(|| {
                format!("Failed to create worktrees base directory: {}",
                    worktrees_base.display())
            })?;

        // Optional: Clean up orphaned worktrees on init
        self.prune_orphaned_worktrees().await?;

        Ok(())
    }

    /// Clean up orphaned worktrees (no automation currently using them)
    async fn prune_orphaned_worktrees(&self) -> Result<()> {
        let worktrees_base = self.git_root
            .parent()
            .unwrap_or(&self.git_root)
            .join(".velor-worktrees");

        if !worktrees_base.exists() {
            return Ok(());
        }

        let mut entries = match fs::read_dir(&worktrees_base).await {
            Ok(e) => e,
            Err(_) => return Ok(()),
        };

        while let Some(entry) = entries.next_entry().await.ok() {
            let path = entry.path();
            if path.is_dir() {
                // Check if it's a valid worktree
                let output = Command::new("git")
                    .args(["worktree", "list", "--porcelain"])
                    .current_dir(&self.git_root)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .output()
                    .await;

                if let Ok(out) = output {
                    let list = String::from_utf8_lossy(&out.stdout);
                    if !list.contains(&path.display().to_string()) {
                        // Orphaned worktree - remove it
                        tracing::debug!("Removing orphaned worktree: {}", path.display());
                        let _ = tokio::fs::remove_dir_all(&path).await;
                    }
                }
            }
        }

        Ok(())
    }

    /// In run_automation():
    async fn run_automation(
        &self,
        automation: &AutomationFile,
        scheduled_for: chrono::DateTime<Utc>,
        cancel_token: &CancellationToken,
    ) -> color_eyre::Result<AutomationResult> {
        // ... existing setup with state.try_start_run() ...

        // Determine base repository and working directory
        let (base_repo, work_dir) = if automation.worktree {
            // Resolve git root from project path (or git_root)
            let proj_path = automation.project.as_ref()
                .unwrap_or(&self.git_root);
            let base_repo = Self::resolve_git_root(proj_path).await?;

            // Create worktree
            let cleanup = self.setup_worktree_for_repo(automation, &base_repo).await?;
            let work_dir = cleanup.as_ref().map(|wc| &wc.path).unwrap_or(&base_repo);
            (base_repo, work_dir.clone())
        } else {
            // No worktree: use project path directly (or git_root)
            if let Some(ref proj) = automation.project {
                // proj already validated to exist in from_raw()
                (Self::resolve_git_root(proj).await?, proj.clone())
            } else {
                (self.git_root.clone(), self.git_root.clone())
            }
        };

        tracing::debug!("Running automation in {:?}", work_dir);

        // ... rest of execution with work_dir ...
    }

    async fn setup_worktree_for_repo(
        &self,
        automation: &AutomationFile,
        git_root: &Path,
    ) -> color_eyre::Result<Option<WorktreeCleanup>> {
        // Generate collision-resistant worktree path
        let wt_path = Self::generate_worktree_path(git_root, &automation.name);

        tracing::debug!("Creating worktree '{}' at {:?}", automation.name, wt_path);

        // Create worktree using git
        let output = Command::new("git")
            .args(["worktree", "add", "-d"])
            .arg(&wt_path)
            .current_dir(git_root)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(eyre!(
                "Failed to create worktree: git exited with {:?}: {}",
                output.status.code(),
                stderr
            ));
        }

        Ok(Some(WorktreeCleanup::new(wt_path, git_root.to_path_buf())))
    }
}
```

**Dependencies** (add to `Cargo.toml` for ULID):

```toml
ulid = "1.1"
```
        let output = Command::new("git")
            .args(["-C", path.to_str().unwrap(), "rev-parse", "--show-toplevel"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await?;

        if !output.status.success() {
            return Err(eyre!(
                "Failed to resolve git root for {}: git exited with {:?}",
                path.display(),
                output.status.code()
            ));
        }

        let git_root = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Ok(PathBuf::from(git_root))
    }

    /// In run_automation():
    async fn run_automation(
        &self,
        automation: &AutomationFile,
        scheduled_for: chrono::DateTime<Utc>,
        cancel_token: &CancellationToken,
    ) -> color_eyre::Result<AutomationResult> {
        // ... existing setup ...

        // Determine base repository and working directory
        let (base_repo, work_dir) = if automation.worktree {
            // Resolve git root from project path (or git_root)
            let proj_path = automation.project.as_ref()
                .unwrap_or(&self.git_root);
            let base_repo = Self::resolve_git_root(proj_path).await?;

            // Create worktree
            let cleanup = self.setup_worktree_for_repo(automation, &base_repo).await?;
            let work_dir = cleanup.as_ref().map(|wc| &wc.path).unwrap_or(&base_repo);
            (base_repo, work_dir.clone())
        } else {
            // No worktree: use project path directly, or git_root
            if let Some(ref proj) = automation.project {
                if !proj.exists() {
                    return Err(eyre!(
                        "project path {} does not exist",
                        proj.display()
                    ));
                }
                (Self::resolve_git_root(proj).await?, proj.clone())
            } else {
                (self.git_root.clone(), self.git_root.clone())
            }
        };

        tracing::debug!("Running automation in {:?}", work_dir);

        // ... rest of execution with work_dir ...
    }

    async fn setup_worktree_for_repo(
        &self,
        automation: &AutomationFile,
        git_root: &Path,
    ) -> color_eyre::Result<Option<WorktreeCleanup>> {
        let wt_name = format!(
            "automation-{}-{}",
            automation.name,
            Utc::now().format("%Y%m%d-%H%M%S")
        );

        // Put worktree in sibling directory to git_root
        let wt_path = git_root
            .parent()
            .unwrap_or(git_root)
            .join(&wt_name);

        tracing::debug!("Creating worktree '{}' at {:?}", wt_name, wt_path);

        // Create worktree using git
        let output = Command::new("git")
            .args(["worktree", "add", "-d"])
            .arg(&wt_path)
            .current_dir(git_root)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(eyre!(
                "Failed to create worktree: git exited with {:?}: {}",
                output.status.code(),
                stderr
            ));
        }

        Ok(Some(WorktreeCleanup::new(wt_path, git_root.to_path_buf())))
    }
}
```

### Phase 4b: State Tracking for Scheduled Runs

**File: `crates/automations/src/state.rs`** (new)

Add state tracking with UNIQUE constraint pattern for idempotency using sqlx:

```rust
use sqlx::{SqlitePool, sqlite::SqliteConnectOptions, ConnectOptions};
use std::path::Path;
use std::str::FromStr;

/// Run status with string constants for consistency
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    Pending,
    Running,
    Completed,
    Failed,
}

impl RunStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "running" => Some(Self::Running),
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

/// State database for tracking automation runs with idempotency
pub struct AutomationState {
    pool: SqlitePool,
}

impl AutomationState {
    /// Open or create state database at given path
    pub async fn open(path: &Path) -> Result<Self> {
        let options = SqliteConnectOptions::from_str(path.to_str().unwrap())?
            .create_if_missing(true);

        let pool = SqlitePool::connect_with(options).await?;

        // Create runs table with UNIQUE constraint for idempotency
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS runs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                automation_name TEXT NOT NULL,
                scheduled_for TEXT NOT NULL,
                status TEXT NOT NULL,
                started_at TEXT,
                finished_at TEXT,
                error_message TEXT,
                UNIQUE(automation_name, scheduled_for)
            )
            "#,
        )
        .execute(&pool)
        .await?;

        // Create index for faster queries
        sqlx::query(
            r#"
            CREATE INDEX IF NOT EXISTS idx_runs_automation
            ON runs(automation_name, scheduled_for DESC)
            "#,
        )
        .execute(&pool)
        .await?;

        Ok(Self { pool })
    }

    /// Attempt to start a run for an automation at a scheduled time.
    /// Returns Ok(id) if the run was started, Err if already running/completed.
    ///
    /// Stale runs (exceeded stale_timeout) are allowed to retry.
    pub async fn try_start_run(
        &self,
        name: &str,
        scheduled_for: chrono::DateTime<chrono::Utc>,
        stale_timeout: chrono::Duration,
    ) -> Result<i64> {
        let scheduled_str = scheduled_for.to_rfc3339();
        let started_at = chrono::Utc::now().to_rfc3339();
        let now = chrono::Utc::now();
        let stale_cutoff = now - stale_timeout;

        // Try to insert - UNIQUE constraint prevents duplicates
        match sqlx::query(
            r#"
            INSERT INTO runs (automation_name, scheduled_for, status, started_at)
            VALUES (?1, ?2, ?3, ?4)
            "#,
        )
        .bind(name)
        .bind(&scheduled_str)
        .bind(RunStatus::Running.as_str())
        .bind(&started_at)
        .execute(&self.pool)
        .await
        {
            Ok(result) => Ok(result.last_insert_rowid()),
            Err(sqlx::Error::Database(db_err)) if db_err.is_unique_violation() => {
                // Check if there's a stale running run
                if let Some((id, started_at_str)) =
                    self.get_run_info(name, scheduled_for).await?
                {
                    if let Ok(started) = chrono::DateTime::parse_from_rfc3339(&started_at_str) {
                        let started = started.with_timezone(&chrono::Utc);
                        if started < stale_cutoff {
                            // Stale run - allow retry by updating it
                            sqlx::query(
                                r#"
                                UPDATE runs SET status = ?1, started_at = ?2, error_message = NULL
                                WHERE id = ?3
                                "#,
                            )
                            .bind(RunStatus::Running.as_str())
                            .bind(&started_at)
                            .bind(id)
                            .execute(&self.pool)
                            .await?;
                            return Ok(id);
                        }
                    }
                }
                Err(eyre!("Run already exists for {} at {}", name, scheduled_for))
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Mark a run as completed
    pub async fn complete_run(&self, id: i64) -> Result<()> {
        let finished_at = chrono::Utc::now().to_rfc3339();
        sqlx::query(
            "UPDATE runs SET status = ?, finished_at = ? WHERE id = ?",
        )
        .bind(RunStatus::Completed.as_str())
        .bind(&finished_at)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Mark a run as failed with an error message
    pub async fn fail_run(&self, id: i64, error: &str) -> Result<()> {
        let finished_at = chrono::Utc::now().to_rfc3339();
        sqlx::query(
            "UPDATE runs SET status = ?, finished_at = ?, error_message = ? WHERE id = ?",
        )
        .bind(RunStatus::Failed.as_str())
        .bind(&finished_at)
        .bind(error)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Get the most recent completed run for an automation
    pub async fn get_last_completed_run(
        &self,
        name: &str,
    ) -> Result<Option<chrono::DateTime<chrono::Utc>>> {
        let row = sqlx::query_as::<_, (String,)>(
            "SELECT scheduled_for FROM runs
             WHERE automation_name = ? AND status = ?
             ORDER BY scheduled_for DESC
             LIMIT 1"
        )
        .bind(name)
        .bind(RunStatus::Completed.as_str())
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some((s,)) => {
                let dt = chrono::DateTime::parse_from_rfc3339(&s)?
                    .with_timezone(&chrono::Utc);
                Ok(Some(dt))
            }
            None => Ok(None),
        }
    }

    /// Get run info for idempotency check
    async fn get_run_info(
        &self,
        name: &str,
        scheduled_for: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<(i64, String)>> {
        let scheduled_str = scheduled_for.to_rfc3339();

        let row = sqlx::query_as::<_, (i64, String)>(
            "SELECT id, started_at FROM runs
             WHERE automation_name = ? AND scheduled_for = ?"
        )
        .bind(name)
        .bind(&scheduled_str)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row)
    }
}
```

**State database location**: `~/.config/velor/state.db` (global)

**Idempotency**: The `UNIQUE(automation_name, scheduled_for)` constraint ensures that the same scheduled run can never be executed twice, even if `tick` is invoked concurrently or the process crashes mid-execution. Stale runs are allowed to retry based on the automation's `stale_run_timeout()` (2x the automation's timeout, minimum 15 minutes).

**RunStatus**: Uses consistent string constants and `FromStr` impl to avoid typos that could cause runs to get stuck.

**IMPORTANT**: All `scheduled_for` values are stored in UTC (RFC3339) to ensure the UNIQUE constraint is stable across DST transitions. The conversion from timezone-aware cron to UTC happens at evaluation time, before storage.

**Dependencies** (add to `Cargo.toml`):

```toml
# For async SQLite with compile-time checked queries
sqlx = { version = "0.8", features = ["runtime-tokio", "sqlite"] }
```

### Phase 5: CLI Flags

**Update command definitions** in `apps/velor-cli/src/automations.rs`:

```rust
#[derive(Debug, clap::Subcommand)]
pub enum AutomationsCommand {
    /// List all configured automations
    List {
        /// Show disabled automations
        #[arg(long)]
        all: bool,
    },

    /// Validate automation definitions
    Validate {},

    /// Run an automation immediately (bypassing schedule)
    Run {
        /// Name of the automation to run
        name: String,
        /// Force run even if disabled
        #[arg(long)]
        force: bool,
    },

    /// Show automation status and recent runs
    Status {
        /// Optional automation name to filter by
        name: Option<String>,
    },

    /// Run one tick of the scheduler (for use with launchd/cron)
    Tick {},
}
```

**Note**: Prefer `velor automations tick` over a long-running daemon. Use launchd (macOS) or cron (Linux) to schedule periodic ticks. Example launchd plist:

```xml
<!-- ~/Library/LaunchAgents/com.velor.automations.plist -->
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.velor.automations</string>
    <key>ProgramArguments</key>
    <array>
        <string>/usr/local/bin/velor</string>
        <string>automations</string>
        <string>tick</string>
    </array>
    <key>StartInterval</key>
    <integer>60</integer>
    <key>RunAtLoad</key>
    <true/>
</dict>
</plist>
```

### Phase 5: Update CLI Commands

**File: `apps/velor-cli/src/automations.rs`** (modify)

Update command handlers (simplified without --no-cache):

```rust
pub async fn run_list(
    all: bool,
    home_cfg: FileConfig,
    git_root: PathBuf,
) -> Result<()> {
    let xdg_config = get_xdg_config_home();
    let home_dir = xdg_config.join("velor");
    let repo_dir = Some(git_root.join(".velor"));

    let cache = AutomationCache::new(home_dir, repo_dir);
    let automations = cache.list_all().await?;

    // Filter by enabled unless --all
    let automations: Vec<_> = automations
        .into_iter()
        .filter(|e| all || e.automation.enabled)
        .collect();

    if automations.is_empty() {
        println!("No {}automations configured.", if all { "" } else { "enabled " });
        return Ok(());
    }

    println!("════════════════════════════════════════");
    println!("📋 Configured Automations");
    println!("════════════════════════════════════════\n");

    for entry in automations {
        let (source_icon, source_label) = match entry.source {
            AutomationSource::Global => ("🌍", "global"),
            AutomationSource::Project => ("📁", "project"),
            AutomationSource::Legacy => ("⚠️ ", "legacy"),
        };
        println!("{} {} ({})", source_icon, entry.automation.name, source_label);
        println!("  Description: {}", entry.automation.description);
        println!("  Schedule: {}", entry.automation.schedule_raw);
        println!("  Timezone: {}", entry.automation.timezone);
        println!("  Worktree: {}", if entry.automation.worktree { "yes" } else { "no" });
        if let Some(ref proj) = entry.automation.project {
            println!("  Project: {}", proj.display());
        }
        println!("  Status: {}", if entry.automation.enabled { "✅ Enabled" } else { "❌ Disabled" });
        if !entry.automation.enabled {
            println!("  Hint: Use --all to show disabled automations");
        }
        println!();
    }
}

pub async fn run_run(
    name: String,
    force: bool,
    home_cfg: FileConfig,
    git_root: PathBuf,
) -> Result<()> {
    let xdg_config = get_xdg_config_home();
    let home_dir = xdg_config.join("velor");
    let repo_dir = Some(git_root.join(".velor"));

    let cache = AutomationCache::new(home_dir, repo_dir);
    let entry = cache.get_by_name(&name).await?;

    if !entry.automation.enabled && !force {
        println!("⚠️  Automation '{}' is disabled. Use --force to run anyway.", name);
        return Ok(());
    }

    // Merge variables with built-ins (home -> repo -> automation -> built-ins)
    let config_path = FileConfig::default_config_path(&git_root);
    let repo_cfg = FileConfig::load_if_exists(&config_path)?.unwrap_or_default();
    let cwd = std::env::current_dir()?;
    let merged_vars = merge_automation_vars(
        entry.automation.vars.clone(),
        repo_cfg.vars.clone(),
        home_cfg.vars.clone(),
        &git_root,
        &cwd,
    );

    // Resolve prompt
    let prompt_cache = PromptCache::new(home_dir, repo_dir);
    let prompt = entry.automation.prompt_source.resolve(
        &prompt_cache, &home_dir, repo_dir.as_deref()
    ).await?;

    // Convert to legacy Automation for runner compatibility
    let automation = entry.automation.to_automation(prompt)?;

    // Run with worktree flag respected
    // ... existing runner logic ...
}

pub async fn run_validate(
    home_cfg: FileConfig,
    git_root: PathBuf,
) -> Result<()> {
    let xdg_config = get_xdg_config_home();
    let home_dir = xdg_config.join("velor");
    let repo_dir = Some(git_root.join(".velor"));

    let cache = AutomationCache::new(home_dir, repo_dir);
    let automations = cache.list_all().await?;

    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    // Create prompt cache once
    let prompt_cache = PromptCache::new(home_dir, repo_dir);

    for entry in automations {
        let source_label = match entry.source {
            AutomationSource::Global => "global",
            AutomationSource::Project => "project",
            AutomationSource::Legacy => "legacy",
        };
        println!("Checking: {} ({})", entry.path.display(), source_label);

        // Check catch_up consistency
        if entry.automation.catch_up != CatchUpPolicy::Skip
            && entry.automation.max_catch_up == 0 {
            warnings.push((
                "catch_up enabled but max_catch_up is 0".to_string(),
                entry.path.clone(),
            ));
        }

        // Resolve prompt to check it exists
        match entry.automation.prompt_source
            .resolve(&prompt_cache, &home_dir, repo_dir.as_deref()).await
        {
            Ok(_) => {}
            Err(e) => errors.push((e.to_string(), entry.path.clone())),
        }

        // Warn about legacy format
        if entry.source == AutomationSource::Legacy {
            warnings.push((
                "Legacy .velor/automations.d/ format detected. Migrate to .velor/automations/".to_string(),
                entry.path.clone(),
            ));
        }
    }

    // Report results
    if errors.is_empty() && warnings.is_empty() {
        println!("✅ All {} automation(s) are valid!", automations.len());
    } else {
        for (msg, path) in &warnings {
            println!("⚠️  Warning: {} ({})", msg, path.display());
        }
        for (msg, path) in &errors {
            println!("❌ Error: {} ({})", msg, path.display());
        }
        if !errors.is_empty() {
            return Err(eyre!("Validation failed with {} error(s)", errors.len()));
        }
    }
}

pub async fn run_tick(
    home_cfg: FileConfig,
    git_root: PathBuf,
) -> Result<()> {
    let xdg_config = get_xdg_config_home();
    let home_dir = xdg_config.join("velor");
    let repo_dir = Some(git_root.join(".velor"));

    let cache = AutomationCache::new(home_dir, repo_dir);
    let automations = cache.get().await?;

    let enabled_automations: Vec<_> = automations
        .into_values()
        .filter(|e| e.automation.enabled)
        .collect();

    if enabled_automations.is_empty() {
        return Ok(());
    }

    // Open state database
    let state_path = xdg_config.join("velor/state.db");
    let state = AutomationState::open(&state_path)?;

    let now = chrono::Utc::now();

    for entry in enabled_automations {
        let automation = &entry.automation;

        // Get last completed run
        let last_run = state.get_last_completed_run(&automation.name)?
            .unwrap_or_else(|| chrono::Utc::now() - chrono::Duration::days(1));

        // Calculate runs to execute based on catch-up policy
        let runs_to_execute = match automation.catch_up {
            CatchUpPolicy::Skip => {
                let next_run = automation.next_after(last_run);
                if next_run <= now { vec![next_run] } else { vec![] }
            }
            CatchUpPolicy::RunOnce => {
                let missed = automation.missed_runs_since(last_run, now, u32::MAX);
                if missed.is_empty() { vec![] } else { vec![missed[0]] }
            }
            CatchUpPolicy::RunAll => {
                automation.missed_runs_since(last_run, now, automation.max_catch_up)
            }
        };

        if runs_to_execute.is_empty() {
            continue;
        }

        // Get stale-run timeout based on automation's configured timeout
        let stale_timeout = automation.stale_run_timeout();

        // Run each scheduled execution
        for scheduled_for in runs_to_execute {
            // Try to start the run (idempotent due to UNIQUE constraint)
            let run_id = match state.try_start_run(&automation.name, scheduled_for, stale_timeout) {
                Ok(id) => id,
                Err(_) => {
                    // Run already exists (already running or completed)
                    continue;
                }
            };

            // ... run automation with worktree flag respected ...

            // Update state after completion
            match result.status {
                AutomationRunStatus::Completed => {
                    state.complete_run(run_id)?;
                }
                AutomationRunStatus::Failed => {
                    state.fail_run(run_id, &result.error.unwrap_or_default())?;
                }
                _ => {}
            }
        }
    }

    Ok(())
}
```

### Phase 6: Exports

**File: `crates/automations/src/lib.rs`** (modify)

Add new module exports:

```rust
pub mod file_config;
pub mod cache;
pub mod vars;

pub use file_config::{AutomationFile, PromptSource};
pub use cache::AutomationCache;
pub use vars::merge_automation_vars;
```

## Critical Files

| File | Action |
|------|--------|
| `crates/automations/src/file_config.rs` | New: `AutomationFileRaw`, `AutomationFile`, `AutomationEntry`, `PromptSourceRaw`, `PromptSource`, `AutomationSource`, `normalize_cron_schedule()`, DST tests |
| `crates/automations/src/cache.rs` | New: `AutomationCache` (loads fresh, no TTL), `parse_automation_file()`, async metadata validation |
| `crates/automations/src/vars.rs` | New: `merge_automation_vars()` with built-ins, best-effort branch lookup |
| `crates/automations/src/runner.rs` | Modify: `resolve_git_root()` with OsStr, `generate_worktree_path()`, `init_worktrees_base()`, `prune_orphaned_worktrees()` |
| `crates/automations/src/state.rs` | New: `AutomationState` with sqlx async SQLite, UNIQUE constraint, `RunStatus` constants, per-automation stale timeout |
| `crates/automations/src/lib.rs` | Modify: Export new modules |
| `apps/velor-cli/src/automations.rs` | Modify: Use `AutomationCache`, `get_xdg_config_home()`, `--all`, `--force`, `tick` command |
| `Cargo.toml` (workspace) | Add dependencies: `cron = "0.12"`, `iana-time-zone = "0.1"`, `ulid = "1.1"`, `sqlx = { version = "0.8", features = ["runtime-tokio", "sqlite"] }` |

## Example Automation Files

**Global automation** (`~/.config/velor/automations/daily-summary.toml`):

```toml
description = "Generate daily standup summary"
schedule = "0 0 9 * * Mon-Fri"
timezone = "America/New_York"

# Inline prompt
prompt = '''
Generate standup summary for {{ team_name }}:
- Git root: {{ git_root }}
- Date: {{ now }}
'''

worktree = false  # Run on current branch

[vars]
team_name = "Engineering"
format = "markdown"
```

**Project automation** (`{repo}/.velor/automations/nightly-tests.toml`):

```toml
description = "Run full test suite in isolated worktree"
schedule = "0 0 2 * * *"

# Reference to prompt file (resolves to run-tests.md in prompts/)
prompt_file = "run-tests"

worktree = true  # Create isolated worktree
# project = "/custom/path"  # Optional: override working directory

[vars]
test_type = "full"
coverage_threshold = "80"
```

**Using named prompt** (`{repo}/.velor/automations/pr-reminder.toml`):

```toml
description = "Send PR reminder notifications"
schedule = "0 0 10 * * Mon-Fri"
# timezone omitted = system local timezone

# Reference to named prompt from velor.toml [prompts]
prompt_name = "pr-reminder"

worktree = false
```

## Verification

1. **Create test automation files** in both `~/.config/velor/automations/` and `{repo}/.velor/automations/`
2. **`velor automations list`** - verify source icons (🌍 global, 📁 project), enabled filtering
3. **`velor automations list --all`** - verify disabled automations appear
4. **`velor automations run <name>`** - verify correct directory and worktree behavior
5. **`velor automations run disabled-name --force`** - verify force flag works
6. **`velor automations validate`** - verify actionable error/warning output
7. **Override test**: Create same-named automation in both global and project, verify project wins
8. **Variable merging**: Test built-ins win, then automation > repo > home precedence
9. **Worktree only when requested**: Verify worktree creation only when `worktree = true`
10. **State tracking**: Run `velor automations tick` twice, verify no double-runs (UNIQUE constraint)
11. **Git root resolution**: Set project to a subdirectory, verify git root is found correctly
12. **Timezone defaults**: Verify omitted timezone uses system local timezone
13. **Cron format**: Verify both 5-field and 6-field expressions work (auto-normalized)
14. **Cron parsing**: Verify invalid cron expressions are rejected
15. **Worktree naming**: Verify sanitized names and ULID suffix prevents collisions
16. **DST behavior**: Run DST tests to verify spring-forward/fall-back handling
17. **Non-UTF8 paths**: Test git commands with paths containing non-UTF8 characters
18. **Stale-run timeout**: Verify stale runs retry based on automation's timeout_seconds
19. **Orphaned worktrees**: Verify periodic cleanup of orphaned worktrees

## Backward Compatibility

- Keep existing `load_automations()` for legacy `.velor/automations.d/` format
- Try new format first via `AutomationCache`, fall back to legacy if no files found
- Deprecation warning when legacy format is detected in `validate` output

## Launchd Setup (macOS)

Create `~/Library/LaunchAgents/com.velor.automations.plist`:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.velor.automations</string>
    <key>ProgramArguments</key>
    <array>
        <string>/usr/local/bin/velor</string>
        <string>automations</string>
        <string>tick</string>
    </array>
    <key>StartInterval</key>
    <integer>60</integer>
    <key>RunAtLoad</key>
    <true/>
</dict>
</plist>
```

Load with: `launchctl load ~/Library/LaunchAgents/com.velor.automations.plist`
