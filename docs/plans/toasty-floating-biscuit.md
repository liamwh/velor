# Multi-Repo Velor Automations with Binary-Managed Launchd

## Context

The current velor automation system has a limitation: launchd services are tied to a single git repository via a hardcoded `WorkingDirectory`. This makes it cumbersome to set up automations for multiple repositories - each would require its own launchd service and justfile command.

This plan implements a clean, multi-repo automation system where:
- **Single launchd service** that never needs to change when repos are added/removed
- **Binary-managed launchd** - `vel automations install/uninstall/status` commands
- **Registry-based repo discovery** - `vel project add/remove/enable/disable` commands
- **Stable plist** - no repo paths embedded, just runs `vel automations tick` on a schedule
- **Path-explicit execution** - no `set_current_dir`, all paths passed explicitly
- **Single-instance tick** - lock file prevents overlapping runs
- **Robust git detection** - handles submodules, worktrees, symlinks

## Architecture

### Mental Model

```
launchd (stable, one-time setup)
  ├─ Runs: vel automations tick
  ├─ Every 60 seconds
  ├─ WorkingDirectory: $HOME (or omitted)
  └─ Logs to: ~/Library/Logs/velor/

vel automations tick (on each wake)
  ├─ Acquire lock: ~/.local/state/velor/automations.lock
  ├─ If locked: exit cleanly (already running)
  ├─ Load registry: XDG_CONFIG_HOME/velor/projects.toml
  ├─ Load global config once: XDG_CONFIG_HOME/velor/velor.toml
  ├─ For each enabled project:
  │   ├─ Resolve canonical git root (handles worktrees/submodules)
  │   ├─ Load repo config: {git_root}/.velor/velor.toml
  │   ├─ Load global automations: XDG_CONFIG_HOME/velor/automations/*.toml
  │   ├─ Load repo automations: {git_root}/.velor/automations/*.toml
  │   └─ Execute due automations (all Command calls use explicit cwd)
  └─ Release lock

Config precedence (documented, deterministic):
  1. Global config values
  2. Repo config values (override global)
  3. Automation file fields (override both configs)

Two patterns for defining automations:
  Pattern A: Global repo-aware automations
    └─ ~/.config/velor/automations/my-task.toml
       └─ Contains: project_path = "/Users/liam/git/repoA"

  Pattern B: Repo-local automations
    └─ /Users/liam/git/repoA/.velor/automations/*.toml
       └─ Discovered via registry

Security: All Command calls use argument arrays (never shell interpolation)
```

### User Experience

```bash
# One-time setup
vel automations install --interval 60

# Add repos to registry
vel project add ~/git/velor
vel project add ~/git/dotfiles
vel project add ~/git/other-project --id other

# List registered projects
vel project list

# Disable/enable without removing
vel project disable dotfiles
vel project enable dotfiles

# Remove a project
vel project remove dotfiles

# Check service status
vel automations status

# Uninstall service
vel automations uninstall
```

## Critical Design Decisions

### 1. No `set_current_dir` - Path-Explicit Execution

Changing CWD in a long-running tick is dangerous. All paths are passed explicitly:

```rust
// ❌ DON'T
std::env::set_current_dir(&project.path)?;

// ✅ DO - pass explicit paths
Command::new("git")
    .current_dir(&git_root)  // Per-command cwd
    .args(["status"])
    .output()?;

// Load config with explicit path
let config_path = git_root.join(".velor/velor.toml");
```

### 2. Robust Git Repository Detection

Uses `git rev-parse --show-toplevel` and canonicalizes paths to handle:
- Submodules (`.git` is a file, not directory)
- Worktrees (`.git` is a file pointing to gitdir)
- Symlinked paths (prevents duplicate registrations)

### 3. Registry Schema: Versioned BTreeMap

```toml
# ~/.config/velor/projects.toml
version = 1

[projects.velor]
path = "/Users/liam/git/velor"
enabled = true
added_at = "2026-03-05T20:00:00Z"

[projects.dotfiles]
path = "/Users/liam/git/dotfiles"
enabled = true
```

Uses `BTreeMap<String, ProjectEntry>` for O(1) lookups and stable ordering.

### 4. Single-Instance Tick with Lock File

Uses `fs2` crate for cross-process locking at `~/.local/state/velor/automations.lock`. Returns immediately if lock is held.

### 5. Idempotent launchctl Operations

```rust
// Idempotent install pattern:
launchctl bootout gui/$UID <plist>  // Try first, ignore failure
write_plist(...)
launchctl bootstrap gui/$UID <plist>
launchctl enable gui/$UID/com.liamwh.velor
launchctl kickstart -k gui/$UID/com.liamwh.velor  // Run immediately

// Uninstall:
launchctl bootout gui/$UID <plist>  // Using plist path
remove plist
```

### 6. Launchd Plist Best Practices

```xml
<key>StartInterval</key><integer>60</integer>
<key>RunAtLoad</key><true/>
<key>ThrottleInterval</key><integer>10</integer>
<!-- No ProcessType - let system decide -->
<!-- No WorkingDirectory - or set to $HOME -->
```

### 7. Backwards Compatibility Migration

If registry is empty, falls back to legacy single-repo behavior with helpful message to run `vel project add .`

### 8. Enable/Disable Commands

Users can temporarily disable projects without removing them from the registry.

## Implementation Plan

### Phase 1: Project Registry Module

**File: `crates/automations/src/registry.rs`** (NEW)

```rust
//! Project registry for multi-repo automation discovery.

use color_eyre::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProjectRegistry {
    pub version: u32,
    #[serde(default)]
    pub projects: BTreeMap<String, ProjectEntry>,
}

impl Default for ProjectRegistry {
    fn default() -> Self {
        Self { version: 1, projects: BTreeMap::new() }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProjectEntry {
    pub path: PathBuf,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub added_at: Option<String>,
}

fn default_true() -> bool { true }

impl ProjectRegistry {
    pub fn registry_path() -> Result<PathBuf> {
        Ok(dirs::config_dir()
            .ok_or_else(|| color_eyre::eyre!("Cannot determine XDG config directory"))?
            .join("velor/projects.toml"))
    }

    pub async fn load() -> Result<Self> {
        let path = Self::registry_path()?;
        if !path.exists() { return Ok(Self::default()); }
        let content = tokio::fs::read_to_string(&path).await?;
        toml::from_str(&content).wrap_err_with(|| "Failed to parse projects.toml")
    }

    pub async fn save(&self) -> Result<()> {
        let path = Self::registry_path()?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let content = toml::to_string_pretty(self)?;
        tokio::fs::write(&path, content).await
            .wrap_err_with(|| format!("Failed to write {}", path.display()))
    }

    /// Add project (SYNC - no async I/O)
    pub fn add(&mut self, path: PathBuf, id: Option<String>) -> Result<()> {
        let path = if path.is_absolute() { path } else { std::env::current_dir()?.join(path) };
        let path = dunce::canonicalize(&path)?;  // Resolve symlinks
        let git_root = velor_core::git::discover_git_root(&path)?;

        let id = id.unwrap_or_else(|| {
            path.file_name().and_then(|n| n.to_str()).unwrap_or("unknown").to_string()
        });
        let id = self.unique_id(id);

        self.projects.insert(id, ProjectEntry {
            path,
            enabled: true,
            added_at: Some(chrono::Utc::now().to_rfc3339()),
        });
        Ok(())
    }

    fn unique_id(&self, base_id: String) -> String {
        let mut candidate = base_id.clone();
        let mut suffix = 2;
        while self.projects.contains_key(&candidate) {
            candidate = format!("{}-{}", base_id, suffix);
            suffix += 1;
        }
        candidate
    }

    pub fn remove(&mut self, id: &str) -> Result<()> {
        self.projects.remove(id)
            .ok_or_else(|| color_eyre::eyre!("Project '{}' not found", id))?;
        Ok(())
    }

    pub fn enable(&mut self, id: &str) -> Result<()> {
        self.projects.get_mut(id)
            .ok_or_else(|| color_eyre::eyre!("Project '{}' not found", id))?
            .enabled = true;
        Ok(())
    }

    pub fn disable(&mut self, id: &str) -> Result<()> {
        self.projects.get_mut(id)
            .ok_or_else(|| color_eyre::eyre!("Project '{}' not found", id))?
            .enabled = false;
        Ok(())
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &ProjectEntry)> {
        self.projects.iter()
    }

    pub fn enabled_iter(&self) -> impl Iterator<Item = (&String, &ProjectEntry)> {
        self.projects.iter().filter(|(_, p)| p.enabled)
    }
}
```

**File: `crates/automations/src/lib.rs`** (MODIFY)
```rust
pub mod registry;
pub use registry::{ProjectEntry, ProjectRegistry};
```

### Phase 2: Project Management Commands

**File: `apps/velor-cli/src/main.rs`** (MODIFY)
```rust
Commands::Project(args) => run_project(args).await?,
```

Add before main:
```rust
/// Project management commands
#[derive(Debug, Args)]
struct ProjectArgs {
    #[command(subcommand)]
    command: ProjectCommand,
}

#[derive(Debug, clap::Subcommand)]
enum ProjectCommand {
    /// Register a project for automations
    Add {
        /// Path to the project (defaults to current directory)
        path: Option<String>,
        /// Unique identifier for this project
        #[arg(long)]
        id: Option<String>,
    },
    /// Remove a project from the registry
    Remove { id: String },
    /// List all registered projects
    List,
    /// Enable a disabled project
    Enable { id: String },
    /// Disable a project temporarily
    Disable { id: String },
}
```

**File: `apps/velor-cli/src/projects.rs`** (NEW)
```rust
//! Project management command handlers.

use color_eyre::Result;
use velor_automations::ProjectRegistry;

pub async fn run_project(command: ProjectCommand) -> Result<()> {
    match command {
        ProjectCommand::Add { path, id } => {
            let path = path.unwrap_or_else(|| ".".to_string());
            let mut registry = ProjectRegistry::load().await?;
            registry.add(path.into(), id)?;
            registry.save().await?;
            println!("✅ Project registered");
        }
        ProjectCommand::Remove { id } => {
            let mut registry = ProjectRegistry::load().await?;
            registry.remove(&id)?;
            registry.save().await?;
            println!("✅ Project '{}' removed", id);
        }
        ProjectCommand::List => {
            let registry = ProjectRegistry::load().await?;

            println!("════════════════════════════════════════");
            println!("📁 Registered Projects");
            println!("════════════════════════════════════════");

            let mut count = 0;
            for (id, p) in registry.iter() {
                let status = if p.enabled { "✅" } else { "❌" };
                println!("{} {} ({})", status, id, p.path.display());
                count += 1;
            }

            if count == 0 {
                println!("\nNo projects registered.");
                println!("Add one with: vel project add <path>");
            }
        }
        ProjectCommand::Enable { id } => {
            let mut registry = ProjectRegistry::load().await?;
            registry.enable(&id)?;
            registry.save().await?;
            println!("✅ Project '{}' enabled", id);
        }
        ProjectCommand::Disable { id } => {
            let mut registry = ProjectRegistry::load().await?;
            registry.disable(&id)?;
            registry.save().await?;
            println!("✅ Project '{}' disabled", id);
        }
    }
    Ok(())
}
```

### Phase 3: Multi-Repo Tick with Lock

**File: `apps/velor-cli/src/automations.rs`** (MODIFY `run_tick`)

```rust
pub async fn run_tick(home_cfg: FileConfig, _git_root: PathBuf) -> Result<()> {
    use fs2::FileExt;
    use std::fs::OpenOptions;

    // Acquire single-instance lock
    let state_dir = dirs::state_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap().join(".local/state"))
        .join("velor");
    std::fs::create_dir_all(&state_dir)?;

    let lock_path = state_dir.join("automations.lock");
    let lock_file = OpenOptions::new().write(true).create(true).open(&lock_path)?;

    if lock_file.try_lock_exclusive().is_err() {
        tracing::info!("Tick already running, exiting");
        return Ok(());
    }

    // Load registry
    let registry = ProjectRegistry::load().await?;

    // Backwards compatibility: if empty, use current directory
    let projects: Vec<_> = if registry.enabled_iter().count() == 0 {
        let git_root = velor_core::git::discover_git_root(&std::env::current_dir()?)?;
        println!("⚠️  No projects registered. Running in legacy mode.");
        println!("   Run 'vel project add .' to enable multi-repo support.");
        vec![("current".to_string(), ProjectEntry {
            path: git_root,
            enabled: true,
            added_at: None,
        })]
    } else {
        registry.enabled_iter().map(|(id, p)| (id.clone(), p.clone())).collect()
    };

    let now = chrono::Utc::now();
    println!("🕐 Tick at {} ({} projects)", now.format("%Y-%m-%d %H:%M:%S UTC"), projects.len());

    // Load global config once
    let global_cfg_path = FileConfig::home_config_path()?;
    let global_cfg = FileConfig::load_if_exists(&global_cfg_path)?.unwrap_or_default();

    // Process each project (PATH-EXPLICIT, no set_current_dir)
    for (id, project) in projects {
        println!("\n📁 Project: {}", id);

        let git_root = velor_core::git::discover_git_root(&project.path)?;

        // Load repo config
        let repo_cfg_path = git_root.join(".velor/velor.toml");
        let repo_cfg = FileConfig::load_if_exists(&repo_cfg_path)?.unwrap_or_default();
        let merged_cfg = FileConfig::merge(global_cfg.clone(), repo_cfg);

        // Load and run automations for this project
        // ... (reuse existing automation logic with explicit git_root)
    }

    println!("\n✅ Tick complete.");
    Ok(())
}
```

### Phase 4: Launchd Management Commands

**File: `apps/velor-cli/src/automations.rs`** (MODIFY `AutomationsCommand`)
```rust
enum AutomationsCommand {
    // ... existing commands ...
    /// Install launchd service
    Install {
        /// Tick interval in seconds
        #[arg(long)]
        interval: Option<u64>,
    },
    /// Uninstall launchd service
    Uninstall,
    /// Show launchd service status
    Status,
}
```

**File: `apps/velor-cli/src/automations/launchd.rs`** (NEW)
```rust
//! launchd service management for macOS.

use color_eyre::Result;
use std::path::PathBuf;

fn plist_path() -> Result<PathBuf> {
    Ok(dirs::home_dir()
        .ok_or_else(|| color_eyre::eyre!("Cannot determine home directory"))?
        .join("Library/LaunchAgents/com.liamwh.velor.plist"))
}

fn log_directory_path() -> Result<PathBuf> {
    Ok(dirs::home_dir()
        .ok_or_else(|| color_eyre::eyre!("Cannot determine home directory"))?
        .join("Library/Logs/velor"))
}

pub async fn run_install(interval: Option<u64>) -> Result<()> {
    let plist = plist_path()?;
    let bin_path = std::env::current_exe()?;
    let log_dir = log_directory_path()?;
    tokio::fs::create_dir_all(&log_dir).await?;

    let interval_sec = interval.unwrap_or(60);
    let plist_content = format!(r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.liamwh.velor</string>
    <key>ProgramArguments</key>
    <array>
        <string>{}</string>
        <string>automations</string>
        <string>tick</string>
    </array>
    <key>StartInterval</key>
    <integer>{}</integer>
    <key>RunAtLoad</key>
    <true/>
    <key>ThrottleInterval</key>
    <integer>10</integer>
    <key>StandardOutPath</key>
    <string>{}/automations.log</string>
    <key>StandardErrorPath</key>
    <string>{}/automations.error.log</string>
    <key>EnvironmentVariables</key>
    <dict>
        <key>PATH</key>
        <string>/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin</string>
    </dict>
</dict>
</plist>
"#, bin_path.display(), interval_sec, log_dir.display(), log_dir.display());

    // Idempotent: try bootout first (ignore failure)
    let uid = String::from_utf8(std::process::Command::new("id").arg("-u").output()?.stdout)?.trim().to_string();
    let domain = format!("gui/{}", uid);
    let _ = std::process::Command::new("launchctl")
        .args(["bootout", &domain, &plist.to_string_lossy()])
        .output();

    tokio::fs::write(&plist, plist_content).await?;

    // Bootstrap
    let output = std::process::Command::new("launchctl")
        .args(["bootstrap", &domain, &plist.to_string_lossy()])
        .output()?;
    if !output.status.success() {
        return Err(color_eyre::eyre!("Failed to bootstrap: {}", String::from_utf8_lossy(&output.stderr)));
    }

    // Enable and kickstart
    let _ = std::process::Command::new("launchctl")
        .args(["enable", &format!("{}/com.liamwh.velor", domain)]).output();
    let _ = std::process::Command::new("launchctl")
        .args(["kickstart", "-k", &format!("{}/com.liamwh.velor", domain)]).output();

    println!("✅ Velor automations service installed");
    println!("   Interval: {}s | Logs: {}/automations.log", interval_sec, log_dir.display());
    println!("\nNext steps:");
    println!("  vel project add <path>     Register a project");
    println!("  vel automations status      Check service status");
    Ok(())
}

pub async fn run_uninstall() -> Result<()> {
    let plist = plist_path()?;
    if !plist.exists() {
        println!("ℹ️  Service not installed");
        return Ok(());
    }

    let uid = String::from_utf8(std::process::Command::new("id").arg("-u").output()?.stdout)?.trim().to_string();
    let domain = format!("gui/{}", uid);

    // Bootout using plist path
    let _ = std::process::Command::new("launchctl")
        .args(["bootout", &domain, &plist.to_string_lossy()])
        .output();

    tokio::fs::remove_file(&plist).await?;
    println!("✅ Velor automations service uninstalled");
    Ok(())
}

pub async fn run_status() -> Result<()> {
    let uid = String::from_utf8(std::process::Command::new("id").arg("-u").output()?.stdout)?.trim().to_string();
    let domain = format!("gui/{}", uid);

    let output = std::process::Command::new("launchctl")
        .args(["list", &format!("{}/com.liamwh.velor", domain)])
        .output()?;

    let log_path = log_directory_path()?.join("automations.log");

    if output.status.success() {
        println!("✅ Service is running");
        if log_path.exists() {
            println!("\nRecent logs:");
            let tail = std::process::Command::new("tail").arg("-10").arg(&log_path).output()?;
            print!("{}", String::from_utf8_lossy(&tail.stdout));
        }
    } else {
        println!("❌ Service is not running");
        println!("   Run 'vel automations install' to install");
    }
    Ok(())
}
```

### Phase 5: Dependencies

**File: `Cargo.toml`** (for workspace members)
```toml
# In velor-cli and/or automations Cargo.toml
[dependencies]
dirs = "5"
dunce = "1"
fs2 = "0.4"
```

### Phase 6: Cleanup

**DELETE**: `scripts/install-launchd.sh`

**MODIFY**: `justfile`
```justfile
install-launchd:
    @cargo build --release -p velor-cli -q
    @cp target/release/vel ~/bin/vel
    ~/bin/vel automations install

uninstall-launchd:
    ~/bin/vel automations uninstall

launchd-status:
    ~/bin/vel automations status
```

## Verification

### Manual Testing

```bash
# 1. Install service
vel automations install --interval 60
launchctl list | grep velor

# 2. Register projects
cd ~/git/velor && vel project add .
cd ~/git/dotfiles && vel project add .
vel project list

# 3. Create test automation
mkdir -p .velor/automations
cat > .velor/automations/test.toml << 'EOF'
description = "Test"
schedule = "0 * * * * *"
prompt = "Write 'Tick at {{timestamp}}' to /tmp/velor-tick-test.txt"
EOF

# 4. Run tick manually
vel automations tick
cat /tmp/velor-tick-test.txt

# 5. Check logs
tail ~/Library/Logs/velor/automations.log

# 6. Test enable/disable
vel project disable dotfiles
vel project list
vel project enable dotfiles

# 7. Uninstall
vel automations uninstall
```

## Critical Files

1. **`crates/automations/src/registry.rs`** - NEW: Project registry with BTreeMap
2. **`crates/automations/src/lib.rs`** - MODIFY: Export registry module
3. **`apps/velor-cli/src/main.rs`** - MODIFY: Add Project command
4. **`apps/velor-cli/src/projects.rs`** - NEW: Project command handlers
5. **`apps/velor-cli/src/automations.rs`** - MODIFY: Add install/uninstall/status, update tick with lock
6. **`apps/velor-cli/src/automations/launchd.rs`** - NEW: launchd management
7. **`justfile`** - MODIFY: Update recipes
8. **`scripts/install-launchd.sh`** - DELETE

## Migration Notes

- Existing users: run `vel project add <current-repo>` after upgrade
- Old launchd service replaced by new `vel automations install`
- `projects.toml` is the single source of truth
