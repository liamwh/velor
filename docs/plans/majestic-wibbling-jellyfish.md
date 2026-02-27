# Velor Tauri GUI Implementation Plan

## Context

This plan implements a Tauri-based GUI version of the Velor Agent CLI, providing all CLI features through a modern dark-themed interface. The user wants:
- **Architecture**: Shared library crate that both CLI and Tauri use
- **Config**: Show both project-level and global configs with ability to edit either
- **Automations**: Background daemon mode with system tray (runs even when app closed)
- **Execution UI**: Live streaming chat interface (not terminal-style)
- **UI Library**: shadcn-svelte with dark theme matching the reference designs

## Architecture Overview

```
velor/
├── crates/
│   ├── automations/          # EXISTING: Cron scheduling, store, runner
│   └── velor-core/           # NEW: Shared business logic
│       ├── src/
│       │   ├── lib.rs
│       │   ├── config.rs     # Extracted from CLI
│       │   ├── template.rs   # Extracted from CLI
│       │   ├── git.rs        # Extracted from CLI
│       │   ├── retry.rs      # Extracted from CLI
│       │   ├── notification.rs # Extracted from CLI
│       │   ├── rules.rs      # Extracted from CLI
│       │   ├── agent.rs      # NEW: Unified agent interface
│       │   └── execution.rs  # NEW: Execution state machine
│       └── Cargo.toml
├── apps/
│   ├── velor-cli/            # MODIFIED: Uses velor-core
│   └── velor/                # Tauri GUI App
│       ├── src/              # SvelteKit frontend
│       │   ├── lib/
│       │   │   ├── components/
│       │   │   ├── stores/
│       │   │   └── types/
│       │   └── routes/
│       └── src-tauri/        # Rust backend
│           └── src/
│               ├── lib.rs    # Tauri commands
│               ├── daemon.rs # Background daemon
│               └── tray.rs   # System tray
```

---

## Phase 1: Core Library Extraction (velor-core crate)

### 1.1 Create velor-core Crate

**File: `crates/velor-core/Cargo.toml`**
- Dependencies: minijinja, serde, tokio, toml, chrono, color-eyre
- Optional ACP support (feature-gated)
- Integration with velor-automations

### 1.2 Modules to Extract from CLI

| Source | Target | Notes |
|--------|--------|-------|
| `config.rs` | `config.rs` | Add serde for JSON serialization |
| `template.rs` | `template.rs` | No changes |
| `git.rs` | `git.rs` | No changes |
| `retry.rs` | `retry.rs` | No changes |
| `notification.rs` | `notification.rs` | Add async variants |
| `rules.rs` | `rules.rs` | No changes |
| `claude.rs` | `agent.rs` | Generalize to `AgentRunner` trait |

### 1.3 Execution State Machine

Create `execution.rs` with:
- `ExecutionId` - unique identifier
- `ExecutionState` - Pending, Rendering, Running, Retrying, Completed, Failed, Cancelled
- `ExecutionEvent` - StateChanged, OutputChunk, Error, IterationCompleted, MetricsUpdated
- `ExecutionConfig` - prompt_name, template_vars, max_iterations, etc.

---

## Phase 2: Tauri Backend

### 2.1 Tauri Commands

**Config Commands:**
- `get_config` - Returns merged config + home/repo configs separately
- `save_config` - Save to home or repo config file
- `get_home_config` / `get_repo_config`

**Execution Commands:**
- `start_execution` - Start agent run, returns execution ID
- `cancel_execution` - Cancel running execution
- `get_execution_status` - Get current state
- `get_execution_history` - List past executions

**Automation Commands:**
- `list_automations` - Get all automations
- `get_automation` - Get single automation
- `toggle_automation` - Enable/disable
- `run_automation_now` - Manual trigger
- `get_automation_runs` - Run history
- `start_daemon` / `stop_daemon`

**Notification Commands:**
- `test_notification` - Send test notification

**System Commands:**
- `discover_git_root` - Find git root for current directory
- `check_binary_available` - Verify claude binary exists

### 2.2 App State

Global state in `state.rs`:
- Currently loaded config
- Git root directory
- Active executions (with cancel tokens)
- Automation store
- Daemon running flag

### 2.3 Background Daemon

`daemon.rs`:
- Start on app launch if configured
- Run tick loop at configurable interval
- Check automations due
- Execute via AutomationRunner
- Emit events to frontend

### 2.4 System Tray

`tray.rs`:
- Menu items: Open, Start/Stop Daemon, Quit
- Show daemon status with colored indicator
- Click to show/hide main window

---

## Phase 3: Frontend (SvelteKit + shadcn-svelte)

### 3.1 Setup Commands

```bash
cd apps/velor
bunx sv add tailwindcss
bunx shadcn-svelte@latest init
```

### 3.2 Directory Structure

```
src/lib/
├── components/
│   ├── ui/              # shadcn-svelte components
│   ├── layout/
│   │   ├── Sidebar.svelte
│   │   ├── Header.svelte
│   │   └── MainLayout.svelte
│   ├── chat/
│   │   ├── ChatMessage.svelte
│   │   ├── ChatStream.svelte
│   │   └── ChatInput.svelte
│   ├── automations/
│   │   ├── AutomationList.svelte
│   │   ├── AutomationCard.svelte
│   │   ├── AutomationEditor.svelte
│   │   └── AutomationRuns.svelte
│   ├── settings/
│   │   ├── ConfigEditor.svelte
│   │   ├── PromptEditor.svelte
│   │   └── NotificationSettings.svelte
│   └── execution/
│       ├── ExecutionStatus.svelte
│       └── ExecutionControls.svelte
├── stores/
│   ├── config.ts
│   ├── execution.ts
│   └── automations.ts
├── types/
│   ├── config.ts
│   ├── execution.ts
│   └── automation.ts
└── services/
    ├── tauri.ts
    └── events.ts
```

### 3.3 Key Components

**Sidebar** (matching reference design):
- Top: Automations button, New Prompt button
- Middle: Recent prompts list
- Bottom: Daemon status toggle, Settings button

**ChatStream**:
- Live streaming output from agent
- Auto-scroll to bottom
- Chat bubble style (not terminal)
- Status bar with state and iteration count

**ConfigEditor**:
- Tabs: Effective (merged), Global, Project
- TOML textarea editor
- Save button per config type

**AutomationEditor**:
- Form fields: name, description, schedule, timezone
- Prompt selector (from configured prompts)
- Vars editor (key-value pairs)
- Enable/disable toggle
- Schedule visualization

### 3.4 Dark Theme

Colors matching reference:
- Background primary: `#121212`
- Background sidebar: `#1E1E1E`
- Text primary: `#FFFFFF`
- Text secondary: `#B0B0B0`
- Accent primary: `#3B82F6`
- Border: `#2A2A2A`

---

## Phase 4: Update CLI to Use Core Crate

### 4.1 Modify CLI Cargo.toml

Replace direct dependencies with:
```toml
velor-core = { path = "../../crates/velor-core" }
```

### 4.2 Update CLI main.rs

Import from velor-core instead of local modules.

---

## Implementation Order

1. **Create velor-core crate** - Extract shared logic
2. **Add execution state machine** - For GUI event streaming
3. **Update CLI to use velor-core** - Ensure no regression
4. **Add Tauri dependencies** - shell, tray plugins
5. **Implement Tauri commands** - Config, execution, automations
6. **Add system tray** - Menu and daemon toggle
7. **Setup shadcn-svelte** - Install and configure
8. **Create dark theme** - CSS variables
9. **Build layout components** - Sidebar, main layout
10. **Implement chat interface** - Streaming output view
11. **Build automation UI** - List, editor, runs
12. **Build settings UI** - Config editor with tabs
13. **Add daemon mode** - Background execution
14. **Testing and polish**

---

## Critical Files

| File | Purpose |
|------|---------|
| `apps/velor-cli/src/config.rs` | Core config types to extract |
| `apps/velor-cli/src/main.rs` | Execution loop logic to generalize |
| `apps/velor-cli/src/claude.rs` | Agent runner to abstract for streaming |
| `crates/automations/src/lib.rs` | Automation crate to reference |
| `apps/velor/src-tauri/src/lib.rs` | Tauri scaffold to extend |

---

## Verification

1. **CLI still works**: Run `cargo test` in velor-cli after extraction
2. **Tauri commands work**: Test each invoke from frontend
3. **Execution streaming**: Start execution, see live output in chat
4. **Config editing**: Edit and save, verify TOML written correctly
5. **Automations**: Create, edit, enable/disable, run manually
6. **Daemon mode**: Close window, verify automations still run
7. **Tray icon**: Click shows/hides window, daemon toggle works
