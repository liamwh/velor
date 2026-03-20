# File-Based Prompt Support for Velor

## Context

Currently, velor only supports prompts defined inline in TOML configuration files. This limits prompt maintainability and makes it difficult to version control or edit complex prompts alongside project documentation.

This plan adds support for defining prompts as markdown files in `.velor/prompts/` directories, enabling:
- Better version control for prompts
- Easier editing with proper markdown syntax highlighting
- Coexistence with inline TOML prompts
- Home and repo-level prompt organization (repo overrides home)

## Implementation Approach

### 1. Create Prompts Module

**New file:** `crates/velor-core/src/prompts.rs`

Mirrors the existing rules system pattern from `crates/velor-core/src/rules.rs`:

```rust
// Core structures
pub struct Prompt {
    pub name: String,
    pub description: String,
    pub complete_token: Option<String>,
    pub content: String,
}

pub struct PromptFrontmatter {
    pub description: String,
    #[serde(default)]
    pub complete_token: Option<String>,
}

pub struct PromptCache {
    home_dir: PathBuf,
    repo_dir: Option<PathBuf>,
    home_cache: Mutex<Option<BTreeMap<String, Prompt>>>,
    repo_cache: Mutex<Option<BTreeMap<String, Prompt>>>,
}
```

**Key functions:**
- `parse_prompt_file()` - Parse `.md` file with YAML frontmatter (reuse `split_frontmatter()` from rules.rs)
- `discover_prompts()` - Load prompts from a directory (async, size limits, security validation)
- `PromptCache::get()` - Load-once caching with merged home + repo prompts (repo takes precedence)

**Constants:**
- `MAX_PROMPT_FILE_SIZE: 100 * 1024` (100 KB)
- `MAX_TOTAL_PROMPTS: 50`

### 2. Update PromptDef Enum

**File:** `crates/velor-core/src/config.rs` (lines 402-438)

Add new variant for file-based prompts:

```rust
pub enum PromptDef {
    Inline(String),
    Table { template: String, complete_token: Option<String> },
    File { path: String, complete_token: Option<String> },  // NEW
}
```

Update `template()` and `complete_token()` methods to handle the new variant.

### 3. Add PromptsConfig to FileConfig

**File:** `crates/velor-core/src/config.rs`

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PromptsConfig {
    pub enabled: bool,
    pub directory: String,
}

impl Default for PromptsConfig {
    fn default() -> Self {
        Self { enabled: true, directory: ".velor/prompts".to_string() }
    }
}
```

Add `prompts_config: PromptsConfig` field to `FileConfig` struct.

### 4. Update Prompt Resolution

**File:** `apps/velor-cli/src/main.rs` (lines 1116-1139)

Modify `resolve_prompt_template()` to:
1. Check CLI `--prompt-text` override (existing)
2. Check inline/config prompts (existing)
3. For `PromptDef::File` or missing prompts, load from `PromptCache`
4. Return error if prompt not found in any source

Add `PromptCache` parameter to the function signature.

### 5. Initialize PromptCache in CLI

**File:** `apps/velor-cli/src/main.rs`

After loading configs (around line 700-850), initialize `PromptCache`:
```rust
let prompt_cache = PromptCache::new(home_dir, repo_dir)?;
```

Pass cache to `run_once()` and `run_auto()` functions.

### 6. Update Module Exports

**File:** `crates/velor-core/src/lib.rs`

Add:
```rust
pub mod prompts;
pub use prompts::{Prompt, PromptCache, PromptFrontmatter};
```

## Directory Structure

```
~/.velor/
├── velor.toml
└── prompts/              # Home prompts (base)
    ├── common.md
    └── debug.md

{git_root}/.velor/
├── velor.toml
└── prompts/              # Repo prompts (override)
    ├── project-task.md
    └── common.md         # Overrides home/common.md
```

## Prompt File Format

```markdown
---
description: "A human-readable description of this prompt"
complete_token: "<promise>DONE</promise>"
---

# Optional markdown heading

This is the prompt template content. Variables work as usual:
- Use {{git_root}} for the repository root
- Use {{iteration}} for the current iteration
- Any custom variables from [vars] section
```

## Critical Files to Modify

| File | Changes |
|------|---------|
| `crates/velor-core/src/prompts.rs` | **NEW** - Prompt loading module |
| `crates/velor-core/src/config.rs` | Add `PromptDef::File` variant, `PromptsConfig` struct |
| `crates/velor-core/src/lib.rs` | Export new prompts module |
| `apps/velor-cli/src/main.rs` | Initialize `PromptCache`, update `resolve_prompt_template()` |
| `crates/velor-core/src/rules.rs` | Reference for `split_frontmatter()` pattern to reuse |

## Backward Compatibility

- Existing inline prompts continue to work unchanged
- Existing TOML table prompts continue to work
- File-based prompts are opt-in (requires creating `.md` files)
- Mixed configuration (inline + file) supported

## Verification

1. **Unit tests** in `prompts.rs`:
   - Parse prompt with/without frontmatter
   - Merge home + repo prompts (repo takes precedence)
   - Cache behavior

2. **Manual testing**:
   ```bash
   # Create test prompts
   mkdir -p ~/.velor/prompts
   echo -e "---\ndescription: Test prompt\n---\nHello {{name}}" > ~/.velor/prompts/test.md

   # Run velor with file-based prompt
   velor once --prompt test --set name=World

   # Verify repo prompt overrides home
   mkdir -p .velor/prompts
   echo -e "---\ndescription: Repo override\n---\nRepo: {{name}}" > .velor/prompts/test.md
   velor once --prompt test --set name=World
   ```

3. **Integration test** - Verify prompts load from both locations with correct precedence
