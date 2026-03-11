# Plan: Add Tab Autocomplete for --prompt Argument

## Context

Users want tab autocomplete support for the `--prompt` CLI argument. Prompts come from two dynamic sources:
- **Config prompts**: `[prompts.<name>]` in `velor.toml`
- **File-based prompts**: `.velor/prompts/*.md` files (home and repo locations)

The completion must be **dynamic** - reading current prompts at runtime - to always be accurate without manual regeneration.

## Architecture

### Design Principles

1. **Completion plumbing is orthogonal to runtime semantics** - internal hidden subcommands only
2. **Shared prompt discovery infrastructure** - one source of truth in velor-core
3. **Module separation** - completion code in its own module
4. **Zsh first-class** - custom Zsh completion with dynamic prompt completion
5. **Graceful degradation** - completion never fails loudly; empty output on errors

### Precedence Rules

Prompt resolution follows this order (highest to lowest):
1. **Repo file prompts** - `{git_root}/.velor/prompts/*.md`
2. **Home file prompts** - `~/.velor/prompts/*.md`
3. **Config prompts** - `[prompts.<name>]` in velor.toml

**Display order**: Alphabetically sorted (independent of precedence)
**Duplicate names**: Highest-precedence source wins
**Outside git repo**: Only home file prompts and config prompts are available

## Implementation Plan

### Phase 1: Shared Prompt Discovery in velor-core

**File**: `crates/velor-core/src/prompts.rs`

Add a public function for prompt name discovery with internal layer tracking:

```rust
use std::{collections::BTreeMap, path::Path};
use std::io::ErrorKind;
use thiserror::Error;

/// Return all prompt names visible from the current execution context.
///
/// Includes config-defined prompts and file-based prompts from supported scopes.
/// Returned names are sorted alphabetically. Duplicate names are resolved by
/// precedence: repo files > home files > config.
///
/// # Performance
/// This function is called during shell completion, so it must be:
/// - Fast (< 50ms typically)
/// - Side-effect free (no writes, no network calls)
/// - Robust to missing directories or malformed files
pub async fn discover_prompt_names(
    git_root: Option<&Path>,
    cfg: &FileConfig,
) -> Result<Vec<String>, PromptDiscoveryError> {
    let mut prompts = BTreeMap::<String, PromptSource>::new();

    // Layer 3: Config prompts (lowest precedence)
    for name in cfg.prompts.keys() {
        prompts.insert(name.clone(), PromptSource::Config);
    }

    // Layer 2: Home file prompts
    if let Some(home) = dirs::home_dir() {
        let home_prompts = home.join(".velor/prompts");
        match scan_prompt_dir(&home_prompts).await {
            Ok(names) => {
                for name in names {
                    prompts.insert(name, PromptSource::HomeFile);
                }
            }
            Err(PromptDiscoveryError::NotFound) => {
                // Directory missing is expected; skip silently
            }
            Err(e) => {
                // Log but continue; other sources may still be available
                tracing::debug!("Failed to scan home prompts: {e}");
            }
        }
    }

    // Layer 1: Repo file prompts (highest precedence)
    if let Some(root) = git_root {
        let repo_prompts = root.join(".velor/prompts");
        match scan_prompt_dir(&repo_prompts).await {
            Ok(names) => {
                for name in names {
                    prompts.insert(name, PromptSource::RepoFile);
                }
            }
            Err(PromptDiscoveryError::NotFound) => {
                // Directory missing is expected; skip silently
            }
            Err(e) => {
                tracing::debug!("Failed to scan repo prompts: {e}");
            }
        }
    }

    // Extract just the names, sorted alphabetically by BTreeMap
    Ok(prompts.into_keys().collect())
}

/// Scan a directory for .md prompt files, returning names without extension.
///
/// Returns NotFound if the directory does not exist.
/// Returns empty Vec if the directory exists but contains no .md files.
async fn scan_prompt_dir(dir: &Path) -> Result<Vec<String>, PromptDiscoveryError> {
    let mut names = Vec::new();

    let mut entries = match tokio::fs::read_dir(dir).await {
        Ok(entries) => entries,
        Err(e) if e.kind() == ErrorKind::NotFound => {
            return Err(PromptDiscoveryError::NotFound);
        }
        Err(e) => return Err(PromptDiscoveryError::Io(e)),
    };

    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();

        // Only process regular files with .md extension
        if path.extension().map_or(false, |ext| ext == "md") {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                names.push(stem.to_string());
            }
        }
        // Skip non-.md files, directories, symlinks, etc.
    }

    Ok(names)
}

#[derive(Debug, Clone, Copy)]
enum PromptSource {
    Config,
    HomeFile,
    RepoFile,
}

#[derive(Debug, Error)]
pub enum PromptDiscoveryError {
    #[error("Directory not found")]
    NotFound,

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}
```

**Key design decisions**:
- `PromptSource` is **private** - not exposed in public API
- All layers use `insert()` (not `or_insert()`) so higher precedence overwrites
- `NotFound` is distinguished from other errors for graceful handling
- Tracing used for debug logging, not visible in normal completion usage

### Phase 2: Internal Hidden Subcommand

**File**: `apps/velor-cli/src/main.rs`

Add internal commands namespace:

```rust
#[derive(Debug, Subcommand)]
enum Commands {
    Once(OnceArgs),
    Auto(AutoArgs),
    Init,
    Plan(PlanArgs),
    TestNotification,
    Automations(AutomationsArgs),
    Project(ProjectArgs),
    Vault(vault::VaultArgs),
    Completion(CompletionArgs),

    /// Hidden internal commands for developer tooling
    #[command(hide = true)]
    Internal(InternalCommands),
}

#[derive(Debug, Subcommand)]
enum InternalCommands {
    /// Output available prompt names for shell completion (newline-delimited)
    ///
    /// Prints one prompt name per line, sorted alphabetically.
    /// Outputs nothing on failure (graceful degradation for shell completion).
    CompletePrompts,
}
```

**Handler implementation** - degrades to empty output on failure:

```rust
async fn handle_internal_complete_prompts() -> color_eyre::eyre::Result<()> {
    // Attempt to load config and discover prompts
    let result: color_eyre::eyre::Result<Vec<String>> = async {
        let git_root = git::discover_git_root().await.ok();
        let cfg = config::load_config(git_root.as_ref()).await?;
        velor_core::prompts::discover_prompt_names(
            git_root.as_deref(),
            &cfg,
        ).await
    }.await;

    // Output: one name per line, or nothing on failure
    // Shell completion expects quiet degradation
    match result {
        Ok(names) => {
            for name in names {
                println!("{name}");
            }
        }
        Err(e) => {
            // Silently exit with no output
            // Shell completion will simply show no options
            tracing::debug!("Completion failed: {e}");
        }
    }

    Ok(())
}
```

**Completion output contract**:
- Newline-delimited names only
- Sorted alphabetically
- No duplicates
- No logging, no colors, no stderr noise
- Always exits 0 (success)
- Empty output on any failure (graceful degradation)

### Phase 3: Custom Zsh Completion

**New file**: `apps/velor-cli/src/completion.rs`

Custom Zsh completion script with dynamic prompt completion:

```rust
use clap::Command;
use std::io::Write;

/// Generate shell completion script and print to stdout.
///
/// For Zsh: fully custom script with dynamic prompt completion.
/// For other shells: uses clap_complete for static scaffolding.
pub fn generate_completion(shell: Shell) -> color_eyre::eyre::Result<()> {
    match shell {
        Shell::Zsh => {
            print_zsh_completion();
            Ok(())
        }
        _ => {
            // For other shells, use clap_complete to generate static completion
            let mut cmd = Cli::command();
            let mut buf = Vec::new();
            clap_complete::generate(shell, &mut cmd, "velor", &mut buf);
            std::io::stdout().write_all(&buf)?;
            Ok(())
        }
    }
}

#[derive(Clone, Copy)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
    Elvish,
    PowerShell,
    Nushell,
}

fn print_zsh_completion() {
    // Custom Zsh completion with dynamic --prompt completion
    print!(r#"#compdef velor

_velor() {{
    local -a commands
    commands=(
        'once:Run a single agent iteration'
        'auto:Run agent in auto mode with retries'
        'init:Initialize velor configuration'
        'plan:Execute a plan from a markdown file'
        'test-notification:Test notification configuration'
        'automations:Manage automations'
        'project:Project-specific commands'
        'vault:Vault and secret management'
    )

    local -a common_args
    common_args=(
        '(--config -c)'{--config,-c}'[Override config path]:path:_files'
        '(--prompt --prompt-text)--prompt[Prompt name from TOML]:prompt:_velor_prompts'
        '(--prompt --prompt-text)--prompt-text[Inline template string]:template:'
        '(-p --pin)'{--pin,-p}'[Context identifier]'
        '(-v --vars-file)'{--vars-file,-v}'[Variables file]:path:_files'
        '(-n --dry-run)'{--dry-run,-n}'[Dry run mode]'
    )

    case $words[1] in
        once)
            _arguments -C $common_args
            ;;
        auto)
            _arguments -C $common_args \
                '--max-retries[Maximum retry attempts]:number:' \
                '--backoff-base[Backoff base in seconds]:number:'
            ;;
        completion)
            _arguments '--shell[Shell type]:shell:(bash zsh fish elvish powershell nushell)'
            ;;
        *)
            _describe 'command' commands
            ;;
    esac
}}

_velor_prompts() {{
    local -a prompt_names
    # Call velor to get available prompts at runtime
    prompt_names=("${{(@f)$(velor internal complete-prompts 2>/dev/null)}}")
    _describe 'prompt' prompt_names
}}
"#);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_zsh_completion_is_valid() {
        // Ensure the completion script is syntactically valid
        let script = get_zsh_completion_script();
        // Basic sanity checks
        assert!(script.contains("#compdef velor"));
        assert!(script.contains("_velor_prompts"));
        assert!(script.contains("velor internal complete-prompts"));
    }

    fn get_zsh_completion_script() -> String {
        // Helper to capture the script for testing
        // In actual implementation, this would be refactored
        // to return a String instead of printing directly
        unimplemented!()
    }
}
```

### Phase 4: Completion Command

**File**: `apps/velor-cli/src/main.rs`

Add completion subcommand:

```rust
#[derive(Debug, Args)]
struct CompletionArgs {
    /// Shell type for completion script
    #[arg(short, long, value_name = "SHELL")]
    shell: completion::Shell,
}
```

Command handler:

```rust
Commands::Completion(args) => {
    completion::generate_completion(args.shell)?;
    Ok(())
}

Commands::Internal(internal) => {
    match internal {
        InternalCommands::CompletePrompts => {
            handle_internal_complete_prompts().await?;
            Ok(())
        }
    }
}
```

### Phase 5: Zsh Installation

**Add to project README or docs**:

```zsh
# Add to ~/.zshrc

# Option 1: Eval completion (simplest)
eval "$(velor completion --shell zsh)"

# Option 2: Source from file (more robust)
mkdir -p ~/.zsh/completion
velor completion --shell zsh > ~/.zsh/completion/_velor
fpath=(~/.zsh/completion $fpath)
autoload -U compinit && compinit
```

### Phase 6: Comprehensive Tests

**New file**: `crates/velor-core/tests/prompt_discovery.rs`

```rust
use velor_core::prompts::discover_prompt_names;
use tempfile::TempDir;
use std::path::PathBuf;

#[tokio::test]
async fn test_empty_when_no_sources() {
    let cfg = FileConfig::default();
    let names = discover_prompt_names(None, &cfg).await.unwrap();
    assert!(names.is_empty());
}

#[tokio::test]
async fn test_config_prompts_only() {
    let mut cfg = FileConfig::default();
    cfg.prompts.insert("alpha".to_string(), PromptDef::Inline("test".to_string()));
    cfg.prompts.insert("zebra".to_string(), PromptDef::Inline("test".to_string()));

    let names = discover_prompt_names(None, &cfg).await.unwrap();
    assert_eq!(names, vec!["alpha", "zebra"]);
}

#[tokio::test]
async fn test_repo_prompts_override_home() {
    let temp = TempDir::new().unwrap();

    // Home has "common"
    let home = temp.path().join("home/.velor/prompts");
    tokio::fs::create_dir_all(&home).await.unwrap();
    tokio::fs::write(home.join("common.md"), "content").await.unwrap();

    // Repo has "common" (should win) and "repo-only"
    let repo = temp.path().join("repo/.velor/prompts");
    tokio::fs::create_dir_all(&repo).await.unwrap();
    tokio::fs::write(repo.join("common.md"), "content").await.unwrap();
    tokio::fs::write(repo.join("repo-only.md"), "content").await.unwrap();

    let cfg = FileConfig::default();
    let git_root = temp.path().join("repo");

    // Mock home_dir to point to temp/home
    // ... (would need a test helper for this)

    let names = discover_prompt_names(Some(&git_root), &cfg).await.unwrap();
    // Should have common (from repo), repo-only, but no duplicate
    assert!(names.contains(&"common".to_string()));
    assert!(names.contains(&"repo-only".to_string()));
    assert_eq!(names.len(), 2);
}

#[tokio::test]
async fn test_shadowing_semantics() {
    // config: foo, bar
    // home: bar.md, baz.md
    // repo: baz.md, qux.md
    // Expected: foo (config), bar (home), baz (repo), qux (repo)

    let temp = TempDir::new().unwrap();

    let mut cfg = FileConfig::default();
    cfg.prompts.insert("foo".to_string(), PromptDef::Inline("test".to_string()));
    cfg.prompts.insert("bar".to_string(), PromptDef::Inline("test".to_string()));

    let home = temp.path().join("home/.velor/prompts");
    tokio::fs::create_dir_all(&home).await.unwrap();
    tokio::fs::write(home.join("bar.md"), "content").await.unwrap();
    tokio::fs::write(home.join("baz.md"), "content").await.unwrap();

    let repo = temp.path().join("repo/.velor/prompts");
    tokio::fs::create_dir_all(&repo).await.unwrap();
    tokio::fs::write(repo.join("baz.md"), "content").await.unwrap();
    tokio::fs::write(repo.join("qux.md"), "content").await.unwrap();

    let git_root = temp.path().join("repo");
    let names = discover_prompt_names(Some(&git_root), &cfg).await.unwrap();

    assert_eq!(names, vec!["bar", "baz", "foo", "qux"]);
}

#[tokio::test]
async fn test_alphabetic_sorting() {
    let temp = TempDir::new().unwrap();

    let repo = temp.path().join("repo/.velor/prompts");
    tokio::fs::create_dir_all(&repo).await.unwrap();

    // Create in non-alphabetic order
    for name in ["zebra", "alpha", "beta"] {
        tokio::fs::write(repo.join(format!("{name}.md")), "content").await.unwrap();
    }

    let cfg = FileConfig::default();
    let names = discover_prompt_names(Some(&repo.parent().unwrap()), &cfg).await.unwrap();

    assert_eq!(names, vec!["alpha", "beta", "zebra"]);
}

#[tokio::test]
async fn test_missing_directory_returns_empty() {
    let cfg = FileConfig::default();
    let non_existent = PathBuf::from("/tmp/velor-test-nonexistent-12345");

    let names = discover_prompt_names(Some(&non_existent), &cfg).await.unwrap();
    assert!(names.is_empty());
}

#[tokio::test]
async fn test_non_md_files_ignored() {
    let temp = TempDir::new().unwrap();

    let repo = temp.path().join("repo/.velor/prompts");
    tokio::fs::create_dir_all(&repo).await.unwrap();

    // Should be ignored
    tokio::fs::write(repo.join("readme.txt"), "content").await.unwrap();
    tokio::fs::write(repo.join(".hidden"), "content").await.unwrap();

    // Should be included
    tokio::fs::write(repo.join("valid.md"), "content").await.unwrap();
    tokio::fs::write(repo.join("UPPERCASE.MD"), "content").await.unwrap(); // Case-sensitive?

    let cfg = FileConfig::default();
    let names = discover_prompt_names(Some(&repo.parent().unwrap()), &cfg).await.unwrap();

    // Decide: should UPPERCASE.MD be included? Probably yes (case-sensitive filesystem)
    // Or normalize to lowercase? Need to decide.
    assert_eq!(names, vec!["UPPERCASE", "valid"]);
}
```

**Open question**: Should `.MD` / `.Md` extensions be recognized?
- Decision: Yes, case-insensitive extension matching for `.md`
- Implementation: Use `path.extension().map(|e| e.eq_ignore_ascii_case("md"))`

## Critical Files to Modify

| File | Purpose | Change Type |
|------|---------|-------------|
| `crates/velor-core/src/prompts.rs` | Add `discover_prompt_names()` | New function |
| `crates/velor-core/src/config.rs` | Ensure `PromptDef` is accessible | None (verify access) |
| `apps/velor-cli/src/main.rs` | Add `InternalCommands`, handler | New enum + handler |
| `apps/velor-cli/src/completion.rs` | **NEW** - Completion generation | New file |
| `apps/velor-cli/Cargo.toml` | Add `clap_complete` dependency | New dep |
| `crates/velor-core/tests/prompt_discovery.rs` | **NEW** - Tests | New file |

## Dependencies

**apps/velor-cli/Cargo.toml**:
```toml
[dependencies]
clap_complete = "4"
```

## Verification

1. `velor internal complete-prompts` - should output newline-delimited prompt names
2. `velor completion --shell zsh > ~/.zsh/completion/_velor` - should generate custom script
3. Source completion in zsh, type `velor once --prompt <TAB>` - should show prompts
4. Create new prompt in `.velor/prompts/` - TAB again, should show new prompt
5. Delete `.velor/prompts/` directory - TAB should show only config prompts
6. Run tests - `cargo test --package velor-core prompt_discovery`

## Shell Support Status

| Shell | Status | Notes |
|-------|--------|-------|
| Zsh | **First-class** | Fully custom script with dynamic `--prompt` completion |
| Bash | Static | clap_complete generated (no dynamic `--prompt` yet) |
| Fish | Static | clap_complete generated (no dynamic `--prompt` yet) |
| Elvish | Static | clap_complete generated |
| PowerShell | Static | clap_complete generated |
| Nushell | Static | clap_complete generated |

## Fuzzy Finding (Documentation Only)

For fuzzy finding, users configure their shell. Add to user docs:

```zsh
# ~/.zshrc: fzf-based fuzzy prompt selector (requires fzf)
_velor_fzf_prompt() {
    local prompt=$(velor internal complete-prompts | fzf --prompt="Select prompt: ")
    if [[ -n "$prompt" ]]; then
        LBUFFER+="$prompt"
        zle reset-prompt
    fi
}
zle -N _velor_fzf_prompt
bindkey '^g' _velor_fzf_prompt  # Ctrl+G to open fzf selector
```

This is **user configuration**, not part of the core implementation.

## Performance Requirements

Completion must be fast:
- **Target**: < 50ms for typical config
- No network calls
- No expensive initialization
- Quiet on stderr (no noisy logs during completion)
- Always exit 0 (success)
- Empty output on failures (graceful degradation)
