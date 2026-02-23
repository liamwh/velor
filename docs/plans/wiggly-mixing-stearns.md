# .agents/rules Implementation Plan

## Status: COMPLETE ✅

All phases have been implemented and tested:
- **Phase 1**: Core Rules Module ✅
- **Phase 2**: Intelligent Rule Selection (ACP) ✅
- **Phase 3**: Glob-Based Rule Activation and Mid-Iteration Injection ✅
- **Phase 4**: Prompt Injection ✅

### Verification (2026-02-22)

1. **All 225 tests pass** - Unit, integration, and property tests cover all phases
2. **Dry-run test works** - `velor once --dry-run --prompt acp-test` correctly injects always-apply rules
3. **Test rules created**:
   - `always-test.mdc` - always-apply rule for testing
   - `toml-test.mdc` - glob-based rule for `.toml` files
   - `intelligent-test.mdc` - intelligent selection rule for planning
4. **Config updated** - Rules system enabled in `.velor/velor.toml`

The rules system is fully functional and ready for use.

---

## Context

Velor needs an intelligent rules system similar to Cursor's `.cursor/rules` feature. Rules are `.mdc` files in `.agents/rules/` at the git root that contain project-specific instructions for the AI agent. Each rule has YAML frontmatter with metadata (`description`, `globs`, `alwaysApply`) controlling when it should be applied.

### User Requirements

Based on user input, the system should support three rule application modes:

1. **Always Apply** (`alwaysApply: true`) → Included in every iteration's first prompt
2. **Apply Intelligently** → Agent decides relevance based on rule description (via ACP)
3. **Apply to Specific Files** → **When agent reads a file matching the glob pattern, inject that rule IMMEDIATELY within the same iteration** (via multi-turn ACP)

**Critical requirement**: Glob-based rules must be injected as soon as the agent reads a matching file, not in the next iteration.

---

## Architecture Overview

### Multi-Turn Per Iteration Design

Each velor iteration becomes a **mini state machine** with multiple ACP prompts (turns) within the same session:

```
┌─────────────────────────────────────────────────────────────────────┐
│                      velor auto-mode iteration                      │
├─────────────────────────────────────────────────────────────────────┤
│                                                                      │
│  ┌────────────────────────────────────────────────────────────┐    │
│  │ TURN A: Initial Prompt                                      │    │
│  │ ─────────────────────────────────────────────────────────── │    │
│  │ 1. Load Rules:                                              │    │
│  │    - alwaysApply rules                                      │    │
│  │    - glob-based rules from previous iterations              │    │
│  │    - (optional) intelligent selection via ACP                │    │
│  │ ─────────────────────────────────────────────────────────── │    │
│  │ 2. Send prompt via conn.prompt()                            │    │
│  └────────────────────────────────────────────────────────────┘    │
│                           │                                         │
│                           ▼                                         │
│  ┌────────────────────────────────────────────────────────────┐    │
│  │ Tool Handling Loop                                          │    │
│  │ ─────────────────────────────────────────────────────────── │    │
│  │ Agent makes requests (read_text_file, etc.)                 │    │
│  │                                                              │    │
│  │ For each read_text_file(path):                              │    │
│  │   1. Normalize path to repo-relative                        │    │
│  │   2. Record in RulesState.files_read                        │    │
│  │   3. Check if path matches any glob-based rules             │    │
│  │   4. If NEW match: add to pending_rules                     │    │
│  │ ─────────────────────────────────────────────────────────── │    │
│  │ Turn completes (stop_reason: end_turn)                      │    │
│  └────────────────────────────────────────────────────────────┘    │
│                           │                                         │
│                           ▼                                         │
│  ┌────────────────────────────────────────────────────────────┐    │
│  │ Decision Point                                             │    │
│  │ ─────────────────────────────────────────────────────────── │    │
│  │ pending_rules.isEmpty()?                                   │    │
│  │     │ YES → End iteration normally                          │    │
│  │     │ NO  → Continue to TURN B                             │    │
│  └────────────────────────────────────────────────────────────┘    │
│                           │ (if pending_rules)                    │
│                           ▼                                         │
│  ┌────────────────────────────────────────────────────────────┐    │
│  │ TURN B: Follow-up Prompt (within same session)             │    │
│  │ ─────────────────────────────────────────────────────────── │    │
│  │ "New project rules became applicable because you opened    │    │
│  │  [files]. Incorporate them and continue from your current  │    │
│  │  plan. Do not restart."                                    │    │
│  │ ─────────────────────────────────────────────────────────── │    │
│  │ Inject only the NEW rules (marked clearly)                 │    │
│  │ Send prompt via conn.prompt() again                        │    │
│  │                                                              │    │
│  │ ← Loop back to Tool Handling, repeat Decision Point        │    │
│  │ (capped at max_mid_iteration_injections, default: 2)       │    │
│  └────────────────────────────────────────────────────────────┘    │
│                                                                      │
└─────────────────────────────────────────────────────────────────────┘
```

### Key Design Decisions

1. **Same ACP Session**: All turns use the same `session_id` from `conn.new_session()`
2. **Minimal Follow-up Prompt**: Short directive that doesn't disrupt the agent's flow
3. **Deduplication**: `RulesState.injected_rules` prevents re-injecting same rules
4. **Cap on Injections**: `max_mid_iteration_injections` prevents ping-pong (default: 2)
5. **User Transparency**: Multi-turn happens invisibly; user sees one logical iteration

### "Immediate" Injection Clarification

**Important**: "Immediate injection" means rules are injected before the agent continues work in the **next turn within the same iteration**. ACP doesn't support mid-turn interruption - we can't inject context while the agent is actively processing.

The flow is:
1. Turn A: Agent works, reads files
2. Turn A completes
3. We detect glob matches from files read
4. Turn B: We inject new rules, agent continues
5. (capped at max_mid_iteration_injections)

This still satisfies "same iteration" because all turns share the same ACP session and the user sees one logical iteration.

---

## Phase 1: Core Rules Module (MVP)

### Files to Create

**`src/rules.rs`** - New module for rules discovery and loading

```rust
use globset::{GlobSet, GlobSetBuilder};
use std::collections::{HashSet, HashMap};
use std::path::Path;
use std::sync::OnceCell;

/// A single rule loaded from a .mdc file
#[derive(Debug, Clone)]
pub struct Rule {
    pub name: String,              // filename without .mdc
    pub description: String,       // from frontmatter
    pub globs: Vec<String>,        // from frontmatter (original patterns)
    pub always_apply: bool,        // from frontmatter
    pub content: String,           // markdown content (after ---)
    pub glob_set: Option<globset::GlobSet>,  // compiled globset for this rule
}

/// Frontmatter metadata (parsed from YAML between --- markers)
#[derive(Debug, serde::Deserialize)]
pub struct RuleFrontmatter {
    pub description: String,
    #[serde(default)]
    pub globs: Vec<String>,
    #[serde(default)]
    pub always_apply: bool,
}

/// All discovered rules, categorized by application mode
#[derive(Debug)]
pub struct RulesSet {
    pub always_apply: Vec<Rule>,
    pub glob_based: Vec<Rule>,
    pub intelligent: Vec<Rule>,
    /// Index for O(1) lookup by name
    pub by_name: HashMap<String, Rule>,
}

/// Selected rules for injection (deduplicated, ordered)
#[derive(Debug)]
pub struct SelectedRules {
    pub rules: Vec<Rule>,
    /// Names of rules already injected (to prevent duplication)
    pub injected: HashSet<String>,
}

/// State tracking across iterations
///
/// All paths are stored as repo-relative strings (forward slashes) for
/// consistent matching across platforms.
#[derive(Debug, Clone, Default)]
pub struct RulesState {
    /// All files ever read across iterations (repo-relative, e.g., "src/main.rs")
    files_read: HashSet<String>,
    /// Rules already injected (by name) - persists across iterations
    injected_rules: HashSet<String>,
}

// Key functions
pub fn split_frontmatter(content: &str) -> Result<(String, String)>  // (yaml, markdown)
pub fn parse_rule_file(path: &Path, git_root: &Path) -> Result<Rule>
pub async fn discover_rules(git_root: &Path) -> Result<RulesSet>
pub fn select_rules(
    rules_set: &RulesSet,
    state: &RulesState,
    intelligent_rules: Option<&[Rule]>,
) -> Result<SelectedRules>
pub fn check_new_glob_matches(
    rules_set: &RulesSet,
    files_read: &[String],
    injected_rules: &HashSet<String>,
) -> Vec<String>  // Returns rule names only (sync, pure)
pub fn get_rules_by_names(rules_set: &RulesSet, names: &[String]) -> Vec<Rule>
pub fn normalize_file_path_if_safe(git_root: &Path, absolute: &Path) -> Option<String>
pub fn path_relative_to(git_root: &Path, absolute: &Path) -> Result<String>

/// Cache for rules loaded once per run
pub struct RulesCache {
    git_root: PathBuf,
    rules: OnceCell<RulesSet>,
}

impl RulesCache {
    pub fn new(git_root: PathBuf) -> Self {
        Self { git_root, rules: OnceCell::new() }
    }

    pub async fn get(&self) -> Result<&RulesSet> {
        self.rules.get_or_try_init(|| async {
            discover_rules(&self.git_root).await
        }).await
    }
}

/// Check for new glob matches based on files read (SYNC - pure function)
///
/// Returns rule NAMES (not full Rule structs) to avoid unnecessary cloning.
/// Rule contents are fetched only when needed for formatting.
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

/// Fetch rules by name from RulesSet (O(1) per lookup via HashMap)
pub fn get_rules_by_names(rules_set: &RulesSet, names: &[String]) -> Vec<Rule> {
    names.iter()
        .filter_map(|name| rules_set.by_name.get(name))
        .cloned()
        .collect()
}

### File Modifications

**`src/main.rs`** - Integrate rules into auto-mode loop with multi-turn support

```rust
/// Core primitive: Run a single ACP turn and collect results
///
/// After conn.prompt() returns, the turn is complete. Tool calls were
/// serviced during execution via client handlers (read_text_file, etc.).
async fn run_turn(
    conn: &acp::ClientSideConnection<VelorClient>,
    session_id: acp::SessionId,
    prompt_text: &str,
) -> Result<(String, Vec<String>)> {
    conn.prompt(acp::PromptRequest::new(
        session_id,
        vec![acp::ContentBlock::Text(acp::TextContent::new(prompt_text.to_string()))],
    )).await?;

    // After turn completes, collect side effects
    let client = conn.get_client();
    let output = client.take_output().await;
    let files_read = client.take_files_read().await;

    Ok((output, files_read))
}

/// Run one iteration with potential multi-turn rule injection
///
/// "Immediate injection" = before the agent continues work in the next turn
/// within the same iteration. ACP doesn't support mid-turn interruption.
///
/// # Usage Pattern
/// ```rust
/// // At startup: load rules once
/// let rules_cache = RulesCache::new(git_root);
/// let rules_set = rules_cache.get().await?.clone();
///
/// // Each iteration: pass rules_set by reference
/// run_auto_iteration(conn, session_id, prompt, &rules_set, rules_state, &config).await
/// ```
async fn run_auto_iteration(
    conn: &acp::ClientSideConnection<VelorClient>,
    session_id: acp::SessionId,
    prompt: &str,
    rules_set: &RulesSet,  // Loaded once at startup, passed by ref
    rules_state: &Arc<tokio::sync::Mutex<RulesState>>,
    config: &RulesConfig,
) -> Result<String> {
    let mut injections = 0;
    let max = config.max_mid_iteration_injections;
    let mut all_output = String::new();
    let mut all_files_read = Vec::new();
    let mut files_delta = Vec::new();  // Rolling delta for chaining activations

    // TURN A: Initial prompt with known rules
    let (initial_rules, injected_so_far) = {
        let state = rules_state.lock().await;
        let rules = select_rules_for_initial_prompt(rules_set, &state)?;
        let injected = state.injected_rules.clone();
        (rules, injected)
        // Lock dropped here
    };

    let prompt_with_rules = inject_rules(prompt, &initial_rules);
    let (turn_output, files_delta) = run_turn(conn, session_id, &prompt_with_rules).await?;
    all_output.push_str(&turn_output);
    all_files_read.extend(files_delta.clone());

    // Multi-turn loop for mid-iteration rule injection
    loop {
        // Check for new glob matches using current delta
        let new_rule_names = {
            let state = rules_state.lock().await;
            check_new_glob_matches(rules_set, &files_delta, &state.injected_rules)?
            // Returns Vec<String> (rule names, not full Rule structs)
            // Lock dropped
        };

        if new_rule_names.is_empty() || injections >= max {
            // Update state and exit iteration
            let mut state = rules_state.lock().await;
            state.files_read.extend(all_files_read);
            break Ok(all_output);
        }

        // Record newly injected rules
        {
            let mut state = rules_state.lock().await;
            for name in &new_rule_names {
                state.injected_rules.insert(name.clone());
            }
            // Lock dropped
        }

        // Fetch rule contents for formatting (only what we need)
        let new_rules: Vec<_> = get_rules_by_names(rules_set, &new_rule_names);

        // TURN B, C, etc.: Follow-up prompt with new rules
        let follow_up = build_follow_up_prompt_delta(&files_delta, &new_rules);
        let (turn_output, more_files) = run_turn(conn, session_id, &follow_up).await?;

        all_output.push_str(&turn_output);
        all_files_read.extend(more_files.clone());
        files_delta = more_files;  // Update delta for next iteration

        injections += 1;
    }
}
```

**`src/acp.rs`** - Track file reads and expose them for rule matching

```rust
struct VelorClient {
    permission_mode: PermissionMode,
    output: Arc<tokio::sync::Mutex<String>>,
    files_read_this_turn: Arc<tokio::sync::Mutex<Vec<String>>>,  // Repo-relative paths
    git_root: PathBuf,  // For path normalization
}

impl VelorClient {
    async fn read_text_file(
        &self,
        request: acp::ReadTextFileRequest,
    ) -> acp::Result<acp::ReadTextFileResponse> {
        // ... existing validation ...

        // Record file read (normalize to repo-relative)
        if let Some(relative) = normalize_file_path_if_safe(&self.git_root, &request.path) {
            self.files_read_this_turn.lock().await.push(relative);
        }

        // ... read and return file ...
    }

    /// Get and clear files read this turn
    pub async fn take_files_read(&self) -> Vec<String> {
        std::mem::take(&mut *self.files_read_this_turn.lock().await)
    }

    /// Get and clear output accumulated this turn
    pub async fn take_output(&self) -> String {
        std::mem::take(&mut *self.output.lock().await)
    }
}
```

**`src/config.rs`** - Add rules configuration

```toml
[rules]
enabled = true
directory = ".agents/rules"
intelligent_selection = true  # Phase 2 feature
```

---

## Phase 2: Intelligent Rule Selection (ACP)

### Design

For rules without `alwaysApply: true`, use ACP to determine relevance:

1. **Pre-selection Prompt**: Send a lightweight prompt asking which rules are relevant
2. **Extract Rule Descriptions**: Send all non-alwaysApply rule descriptions
3. **Agent Decision**: Agent returns **strict JSON** with rule names
4. **Parse and Filter**: Extract relevant rules by name

### Strict JSON Output (Critical)

**Enforce JSON schema to prevent drift:**

```rust
#[derive(Debug, serde::Deserialize)]
struct IntelligentSelectionResponse {
    rules: Vec<String>,
    #[serde(default)]
    reasoning: String,
}

const SELECTION_PROMPT: &str = r#"
You are selecting relevant project rules for the current task.

Available rules:
{rules_descriptions}

Task:
{task_preview}

Respond ONLY with valid JSON in this exact format:
{{"rules":["rule_name_1","rule_name_2"],"reasoning":"Brief explanation..."}}

Respond with {{"rules":[],"reasoning":"none"}} if no rules apply.
"#;
```

### Implementation Flow

```rust
async fn select_intelligent_rules(
    conn: &acp::ClientSideConnection<VelorClient>,
    session_id: acp::SessionId,
    rules: &[Rule],
    task_preview: &str,
    config: &RulesConfig,
) -> Result<Vec<Rule>> {
    if rules.is_empty() {
        return Ok(Vec::new());
    }

    // Build allowed rule names set for validation
    let allowed_names: HashSet<_> = rules.iter().map(|r| r.name.as_str()).collect();

    // Build descriptions list
    let descriptions: String = rules
        .iter()
        .map(|r| format!("- {}: {}", r.name, r.description))
        .collect::<Vec<_>>()
        .join("\n");

    // Render prompt (capped task preview)
    let prompt = SELECTION_PROMPT
        .replace("{rules_descriptions}", &descriptions)
        .replace("{task_preview}", &task_preview.chars().take(500).collect::<String>());

    // Send selection prompt (separate turn, lightweight)
    let response = conn.prompt(acp::PromptRequest::new(
        session_id,
        vec![acp::ContentBlock::Text(acp::TextContent::new(prompt))],
    )).await?;

    // Collect output (capped to prevent abuse)
    let output = {
        let client = conn.get_client();
        let raw = client.take_output().await;
        let max = 4 * 1024;  // 4KB cap
        if raw.len() > max {
            tracing::warn!("Selection output exceeded {} bytes, truncating", max);
            raw.chars().take(max).collect()
        } else {
            raw
        }
    };

    // Parse strict JSON response with fallback
    let selection: IntelligentSelectionResponse = serde_json::from_str(&output)
        .or_else(|_| extract_json_from_markdown(&output))?;

    // VALIDATE: reject any rule name not in the offered set
    let rule_names: Vec<_> = selection.rules
        .into_iter()
        .filter(|name| allowed_names.contains(name.as_str()))
        .collect();

    // Cap the number of intelligent rules
    let max = config.intelligent_selection_max_rules;
    let rule_names: Vec<_> = rule_names.into_iter().take(max).collect();

    // Map rule names to Rule objects (clone only what we need)
    let selected: Vec<_> = rules
        .iter()
        .filter(|r| rule_names.contains(&r.name))
        .cloned()
        .collect();

    tracing::debug!("Intelligent selection: {} rules selected (capped at {})",
        selected.len(), max);

    Ok(selected)
}

/// Fallback: extract JSON from markdown code blocks
fn extract_json_from_markdown(text: &str) -> Result<IntelligentSelectionResponse> {
    // Look for ```json ... ``` blocks
    let re = regex::Regex::new(r"```json\s*(\{.*?\})\s*```")?;
    if let Some(caps) = re.captures(text) {
        serde_json::from_str(&caps[1]).map_err(Into::into)
    } else {
        // Try parsing entire text as JSON
        serde_json::from_str(text).map_err(Into::into)
    }
}
```

### ACP Session Flow

**Critical**: Use a separate short-lived session for intelligent selection to avoid contaminating the main conversation history with "meta" turns.

```
1. conn_selection.new_session() → selection_session_id
2. conn_selection.prompt( intelligent_selection_prompt ) → JSON response
3. Close selection session
4. Parse JSON to get rule names
5. conn_main.new_session() → main_session_id
6. conn_main.prompt( main_prompt + injected_rules ) → agent work
7. Agent reads files → tracked in read_text_file
```

If using the same session is necessary, keep the selection prompt extremely short and strongly delimited (e.g., `=== SELECTION TURN ===` markers).

---

## Phase 3: Glob-Based Rule Activation and Mid-Iteration Injection

### Design

Track which files the agent reads and match against glob patterns to **immediately** inject matching rules within the same iteration (via multi-turn). Once activated, rules persist across future iterations.

**Note**: Uses the canonical `RulesState` defined in Phase 1 (with `HashSet<String>` for repo-relative paths).

    pub fn match_rules_for_files(&self, rules: &[Rule]) -> Vec<Rule> {
        rules.iter()
            .filter(|r| !self.injected_rules.contains(&r.name))
            .filter(|r| self.globs_match_any_file(r))
            .cloned()
            .collect()
    }
}
```

### Frontmatter Parsing

**Critical**: Use line-by-line parsing for robust delimiter detection:

```rust
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
/// 1. First non-empty line must be exactly "---"
/// 2. Scan subsequent lines until a line that trims to "---"
/// 3. Everything between is YAML; after is markdown
///
/// # Error Handling
/// - If opening delimiter exists but no closing delimiter is found, this
///   returns an error (for rule files). This catches malformed files early.
/// - If there is no opening delimiter, treats entire file as markdown.
pub fn split_frontmatter(content: &str) -> Result<(String, String)> {
    let mut lines = content.lines().peekable();

    // Skip leading empty lines
    while lines.peek().map(|l| l.trim().is_empty()).unwrap_or(false) {
        lines.next();
    }

    // First non-empty line must be opening delimiter
    let first = lines.next().ok_or_else(|| eyre!("Empty file"))?;
    if first.trim() != "---" {
        // No frontmatter, entire file is markdown
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

    // Opening delimiter found but no closing delimiter - ERROR
    // (This catches malformed rule files)
    Err(eyre!("Found opening --- but no closing --- delimiter in frontmatter"))
}
```

### Glob Matching with `globset`

**Critical**: Use `globset` instead of `glob` for:
- Better `**` behavior
- Compiled `GlobSet` for fast matching (compile once, match many)
- Cross-platform path handling

```rust
impl Rule {
    /// Creates a new Rule with compiled globset
    pub fn new(
        name: String,
        description: String,
        globs: Vec<String>,
        always_apply: bool,
        content: String,
    ) -> Result<Self> {
        let glob_set = if globs.is_empty() {
            None
        } else {
            let mut builder = GlobSetBuilder::new();
            for pattern in &globs {
                // Normalize pattern: convert to forward slashes
                let normalized = pattern.replace('\\', "/");
                builder.add(globset::Glob::new(&normalized)?);
            }
            Some(builder.build()?)  // GlobSetBuilder::build() returns GlobSet
        };

        Ok(Self { name, description, globs, always_apply, content, glob_set })
    }

    /// Check if this rule matches a repo-relative path
    ///
    /// # Arguments
    /// * `path_relative` - Path relative to git root, e.g., "src/main.rs"
    pub fn matches_path(&self, path_relative: &str) -> bool {
        self.glob_set.as_ref()
            .is_some_and(|gs| gs.is_match(path_relative))
    }
}
```

### Path Normalization

**Critical**: Always store paths as repo-relative, forward-slash strings:

```rust
/// Convert absolute path to repo-relative string
///
/// # Example
/// ```
/// git_root = "/home/user/project"
/// absolute = "/home/user/project/src/main.rs"
/// // Returns: "src/main.rs"
/// ```
pub fn path_relative_to(git_root: &Path, absolute: &Path) -> Result<String> {
    let relative = absolute
        .strip_prefix(git_root)
        .wrap_err_with(|| format!("Path {:?} not under git root {:?}", absolute, git_root))?;

    // Convert to forward slashes for cross-platform consistency
    Ok(relative
        .iter()
        .map(|s| s.to_string_lossy())
        .collect::<Vec<_>>()
        .join("/"))
}
```

### RulesState with Normalized Paths

```rust
impl RulesState {
    /// Record a file read (path will be normalized to repo-relative)
    pub fn record_file_read(&mut self, path_relative: String) {
        self.files_read.insert(path_relative);
    }

    /// Find rules whose globs match any previously-read file
    pub fn match_globs_for_files(&self, rules: &[Rule]) -> Vec<&Rule> {
        rules.iter()
            .filter(|r| !self.injected_rules.contains(&r.name))
            .filter(|r| {
                self.files_read.iter().any(|path| r.matches_path(path))
            })
            .collect()
    }
}
```

### Deterministic Rule Selection (with Deduplication)

**Critical**: Use a Set keyed by rule name, with deterministic ordering:

```rust
impl SelectedRules {
    /// Add a rule if not already injected
    pub fn add(&mut self, rule: Rule) {
        if !self.injected.contains(&rule.name) {
            self.rules.push(rule);
            self.injected.insert(rule.name);
        }
    }
}

/// Select rules for injection with deterministic ordering
///
/// Order (deterministic for reproducibility):
/// 1. alwaysApply rules (sorted by filename)
/// 2. glob-matched rules (sorted by filename)
/// 3. intelligent rules (sorted by filename, capped)
pub fn select_rules(
    rules_set: &RulesSet,
    state: &RulesState,
    intelligent_rules: Option<&[Rule]>,  // From Phase 2
    config: &RulesConfig,
) -> Result<SelectedRules> {
    let mut selected = SelectedRules {
        rules: Vec::new(),
        injected: state.injected_rules.clone(),  // Carry forward already-injected rules
    };

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

        // Cap the number of intelligent rules
        let max = config.intelligent_selection_max_rules;
        for rule in sorted.into_iter().take(max) {
            selected.add(rule.clone());
        }
    }

    Ok(selected)
}
```

---

## Phase 4: Prompt Injection

### Rule Formatting

Format rules consistently for the agent:

```rust
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

pub fn inject_rules(prompt: &str, rules: &[Rule]) -> String {
    let rules_text = format_rules_for_injection(rules);
    format!("{}\n\n{}", rules_text, prompt)
}
```

### Example Output

```
# Project Rules

The following rules from `.agents/rules/` apply to this task:

## rust

# Rust Instructions

- Be aggressive in creating new crates...
- For types exposed via API endpoints...

---

[main rendered prompt continues here...]
```

---

## Critical Files to Modify

| File | Changes |
|------|---------|
| `src/main.rs` | Add multi-turn iteration logic in `run_auto_loop()`; `RulesCache`; `RulesState` |
| `src/acp.rs` | Add `files_read_this_turn` tracking to `VelorClient`; `take_files_read()` getter |
| `src/config.rs` | Add `[rules]` config section with `max_mid_iteration_injections` |
| `src/rules.rs` | **NEW FILE**: All rule logic: discovery, parsing, globset matching, selection |
| `src/lib.rs` | Export `rules` module |
| `Cargo.toml` | Add dependencies: `globset`, `regex`, `serde_yaml` |

---

## Dependencies to Add

```toml
[dependencies]
# Existing...
globset = "0.4"      # Compiled glob matching (better than glob crate)
regex = "1.10"       # For JSON extraction fallback
serde_yaml = "0.9"   # For frontmatter parsing

[dev-dependencies]
# For testing
tempfile = "3"
```

---

## Configuration Options

```toml
# In ~/.velor/velor.toml or .velor/velor.toml

[rules]
enabled = true
directory = ".agents/rules"

# Intelligent selection (Phase 2)
intelligent_selection = true
intelligent_selection_max_rules = 5

# Multi-turn per iteration (for immediate glob-based injection)
max_mid_iteration_injections = 2  # Cap follow-up prompts per iteration

# Security limits
max_rule_file_size_kb = 100
max_total_rules = 50
```

### Follow-Up Prompt Template (Delta-Only)

```rust
/// Build follow-up prompt with ONLY new rules (delta formatting)
///
/// Key requirements:
/// - Short and directive (don't disrupt agent's flow)
/// - List actual file paths opened
/// - Mark clearly as "NEW RULES ONLY"
/// - Instruct agent to continue, not restart
fn build_follow_up_prompt_delta(files_read: &[String], new_rules: &[Rule]) -> String {
    let rules_text = new_rules.iter()
        .map(|r| format!("## {}\n\n{}\n", r.name, r.content))
        .collect::<Vec<_>>()
        .join("\n---\n\n");

    format!(
        r#"# NEW Project Rules (delta)

You opened these files:
{}

These NEW rules now apply:

{}

**Incorporate these new rules and continue from your current plan. Do not restart.**"#,
        files_read.iter().map(|f| format!("- {}", f)).collect::<Vec<_>>().join("\n"),
        rules_text
    )
}
```

---

## Testing Strategy

### Unit Tests (`src/rules.rs` tests)

1. **Frontmatter Splitting**: Parse `.mdc` files with valid/invalid frontmatter, missing closing `---`
2. **Glob Matching**: Test various glob patterns (`**/*.rs`, `src/**/*.ts`, etc.)
3. **Path Normalization**: Windows vs Unix separators, absolute to relative conversion
4. **Rule Formatting**: Verify injected prompt format
5. **Deduplication**: Ensure same rule not injected twice
6. **Deterministic Ordering**: Verify rules always in same order for same inputs

### Integration Tests

1. **End-to-end**: Create temp git repo with `.agents/rules/`, run velor, verify rules injected
2. **File Read Tracking**: Verify `read_text_file` captures file paths
3. **Multi-iteration**: Verify glob-based rules appear after matching files read

### Manual Verification

```bash
# 1. Create test repo
mkdir /tmp/test-velor-rules && cd /tmp/test-velor-rules
git init
mkdir -p .agents/rules

# 2. Create test rule
cat > .agents/rules/test.mdc << 'EOF'
---
description: Test rule for Rust files
globs: "*.rs"
alwaysApply: false
---
# Test Rule

This is a test rule for Rust files.
EOF

# 3. Run velor with dry-run to see injected context
velor once --dry-run --prompt "Help me with main.rs"
```

---

## Verification Steps

1. **Always-apply rules appear**: Run `velor once --dry-run`, verify `alwaysApply: true` rules in output
2. **Mid-iteration glob injection**: When agent reads `main.rs`, verify glob-matching rule is injected in follow-up prompt (same iteration)
3. **No duplication**: Same rule never injected twice (tracked by `injected_rules` set)
4. **Multi-turn cap**: Verify `max_mid_iteration_injections` prevents infinite loops
5. **Configurable**: `rules.enabled = false` disables the feature
6. **Intelligent selection**: Verify JSON schema enforcement and fallback parsing
7. **Path normalization**: Test with Windows paths, absolute paths, symlinks

---

## Security and Safety Considerations

1. **Path Traversal Prevention**: Never allow rules to escape repo root via `..` symlinks or relative paths. Only load files under `${git_root}/.agents/rules/`.

```rust
pub fn validate_rules_directory(git_root: &Path, rules_dir: &Path) -> Result<PathBuf> {
    // Canonicalize both paths to resolve symlinks
    let canonical_git_root = git_root.canonicalize()
        .wrap_err("Failed to canonicalize git root")?;
    let canonical_rules_dir = rules_dir.canonicalize()
        .wrap_err("Rules directory does not exist")?;

    // Ensure rules_dir is under git_root
    if !canonical_rules_dir.starts_with(&canonical_git_root) {
        return Err(eyre!("Rules directory must be under git root"));
    }

    Ok(canonical_rules_dir)
}

/// Validate that a rule file is under the rules directory
pub fn validate_rule_file(rules_dir: &Path, rule_file: &Path) -> Result<PathBuf> {
    let canonical_rules_dir = rules_dir.canonicalize()?;
    let canonical_rule_file = rule_file.canonicalize()
        .wrap_err_with(|| format!("Rule file does not exist: {:?}", rule_file))?;

    if !canonical_rule_file.starts_with(&canonical_rules_dir) {
        return Err(eyre!("Rule file must be under rules directory"));
    }

    Ok(canonical_rule_file)
}
```

2. **File Read Security**: For files read by the agent, only normalize to repo-relative if the absolute path starts with the canonicalized git_root.

```rust
pub fn normalize_file_path_if_safe(git_root: &Path, absolute: &Path) -> Option<String> {
    let canonical_git_root = git_root.canonicalize().ok()?;
    let canonical_absolute = absolute.canonicalize().ok()?;

    // Only process files under git root
    if !canonical_absolute.starts_with(&canonical_git_root) {
        return None;  // Outside repo, don't record
    }

    // Strip git root and normalize to forward slashes
    let relative = canonical_absolute.strip_prefix(&canonical_git_root).ok()?;
    Some(relative.iter()
        .map(|s| s.to_string_lossy())
        .collect::<Vec<_>>()
        .join("/"))
}
```

3. **Size Limits**: Cap rule file size (e.g., 100KB) and total rules count to prevent resource exhaustion.

```rust
const MAX_RULE_FILE_SIZE: usize = 100 * 1024;  // 100KB
const MAX_TOTAL_RULES: usize = 50;
```

---

## Performance Considerations

1. **Caching**: Parse all rule files once per run and cache in memory. Rules don't change during execution.

```rust
pub struct RulesCache {
    git_root: PathBuf,
    rules: OnceCell<RulesSet>,
}

impl RulesCache {
    pub async fn get(&self) -> Result<&RulesSet> {
        self.rules.get_or_try_init(|| async {
            discover_rules(&self.git_root).await
        }).await
    }
}
```

2. **GlobSet Compilation**: Compile glob patterns once when rules are loaded, not on every match.

3. **Disk I/O**: Only read rules directory once at startup; avoid repeated disk reads per iteration.

---

## Open Questions / Future Enhancements

### Resolved: Mid-Iteration Injection

**Solution**: Multi-turn per iteration using ACP's session support. When glob-based rules match files read during the current turn, a follow-up prompt is sent within the same ACP session to inject those rules immediately. Capped at `max_mid_iteration_injections` (default: 2) to prevent ping-pong.

### Future Enhancements

1. **Rule Dependencies**: Should rules be able to reference other rules?
2. **Remote Rules**: Support importing rules from GitHub (like Cursor)
3. **Rule Conflicts**: Explicit conflict resolution / precedence syntax
4. **Rule Testing**: Validate rules against test cases
5. **Adaptive Injection Cap**: Dynamically adjust `max_mid_iteration_injections` based on agent behavior
