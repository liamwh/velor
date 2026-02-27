# Velor UI Implementation Plan

## Context

The Velor GUI currently has placeholder UI components that need to be connected to real functionality. The goal is to make the UI useful for:
1. **Automations** - Full CRUD management and viewing
2. **Config editing** - Edit TOML configuration files
3. **Sessions/Executions** - View and manage execution history with full input/output capture
4. **SQLite persistence** - All data (except TOML config) stored in SQLite

The backend already has excellent foundations - SQLite is integrated with automation_runs/locks tables, and most commands exist but some aren't registered.

---

## Phase 1: Register Missing Commands (Backend Quick Wins)

**Goal**: Enable existing automation CRUD functionality

### File: `src-tauri/src/lib.rs`

Add to imports (line 22-27):
```rust
use commands::{
    // ... existing ...
    create_automation,
    update_automation,
};
```

Add to `invoke_handler` (line 81-105):
```rust
create_automation,
update_automation,
```

**Impact**: Unlocks create/update automation functionality already written in `commands.rs:584-729`

---

## Phase 2: SQLite Schema Extensions

**Goal**: Persist execution sessions with full input/output

### New Tables in `automations.db`

```sql
-- Sessions: Complete execution records with I/O
CREATE TABLE IF NOT EXISTS sessions (
    id TEXT PRIMARY KEY,                    -- UUID from ExecutionId
    prompt_name TEXT NOT NULL,
    state TEXT NOT NULL,                    -- ExecutionState enum
    started_at TEXT NOT NULL,
    ended_at TEXT,
    config_json TEXT NOT NULL,              -- ExecutionConfig as JSON
    metrics_json TEXT,                      -- ExecutionMetrics as JSON
    output TEXT,                            -- Accumulated output
    error TEXT,                             -- Error message if failed
    automation_name TEXT,                   -- FK if triggered by automation
    automation_run_id INTEGER,              -- FK to automation_runs
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Session events: Granular event log
CREATE TABLE IF NOT EXISTS session_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL,
    event_type TEXT NOT NULL,
    event_data TEXT NOT NULL,               -- JSON payload
    timestamp TEXT NOT NULL,
    FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
);

-- Indexes
CREATE INDEX IF NOT EXISTS idx_sessions_started_at ON sessions(started_at DESC);
CREATE INDEX IF NOT EXISTS idx_sessions_state ON sessions(state);
CREATE INDEX IF NOT EXISTS idx_sessions_automation_name ON sessions(automation_name);
```

### New File: `src-tauri/src/session_store.rs`

```rust
pub struct SessionStore { pool: SqlitePool }

impl SessionStore {
    pub async fn open(path: &Path) -> Result<Self>;
    pub async fn init_schema(&self) -> Result<()>;
    pub async fn insert_session(&self, record: &ExecutionRecord) -> Result<()>;
    pub async fn update_session(&self, record: &ExecutionRecord) -> Result<()>;
    pub async fn get_session(&self, id: &str) -> Result<Option<ExecutionRecord>>;
    pub async fn list_sessions(&self, limit: u32, offset: u32) -> Result<Vec<ExecutionRecord>>;
    pub async fn delete_session(&self, id: &str) -> Result<()>;
    pub async fn get_session_count(&self) -> Result<u64>;
    pub async fn append_event(&self, session_id: &str, event: &ExecutionEvent) -> Result<()>;
}
```

### Update: `src-tauri/src/state.rs`

Add `session_store: Arc<RwLock<Option<SessionStore>>>` and methods:
- `init_session_store(db_path)` - called alongside automation store init
- `session_store()` - getter
- `persist_session(record)` - persist on state changes

---

## Phase 3: New Backend Commands

**Goal**: Expose session management and delete automation

### File: `src-tauri/src/commands.rs`

```rust
// Session management
#[tauri::command]
pub async fn list_sessions(state: State<'_, Arc<AppState>>, limit: Option<u32>, offset: Option<u32>) -> CommandResult<Vec<ExecutionRecord>>;

#[tauri::command]
pub async fn get_session(state: State<'_, Arc<AppState>>, id: String) -> CommandResult<Option<ExecutionRecord>>;

#[tauri::command]
pub async fn delete_session(state: State<'_, Arc<AppState>>, id: String) -> CommandResult<()>;

#[tauri::command]
pub async fn get_session_stats(state: State<'_, Arc<AppState>>) -> CommandResult<SessionStats>;

// Automation management
#[tauri::command]
pub async fn delete_automation(state: State<'_, Arc<AppState>>, name: String) -> CommandResult<()>;
```

### Register in `lib.rs`

```rust
list_sessions,
get_session,
delete_session,
get_session_stats,
delete_automation,
```

---

## Phase 4: Frontend Service Layer

**Goal**: Connect TypeScript to new backend commands

### File: `src/lib/services/tauri.ts`

Add functions:
```typescript
// Sessions
export async function listSessions(limit?: number, offset?: number): Promise<ExecutionRecord[]>;
export async function getSession(id: string): Promise<ExecutionRecord | null>;
export async function deleteSession(id: string): Promise<void>;
export async function getSessionStats(): Promise<SessionStats>;

// Automations
export async function createAutomation(request: CreateAutomationRequest): Promise<void>;
export async function updateAutomation(request: UpdateAutomationRequest): Promise<void>;
export async function deleteAutomation(name: string): Promise<void>;
```

---

## Phase 5: Frontend Stores

**Goal**: Reactive state management for sessions

### New File: `src/lib/stores/sessions.ts`

```typescript
interface SessionsState {
  sessions: ExecutionRecord[];
  selectedSession: ExecutionRecord | null;
  stats: SessionStats | null;
  loading: boolean;
  error: string | null;
  hasMore: boolean;
}

function createSessionsStore() {
  return {
    subscribe,
    load,
    loadMore,
    get,
    delete,
    refresh,
    select,
  };
}
```

### Update: `src/lib/stores/automations.ts`

Add methods:
```typescript
async function create(request: CreateAutomationRequest): Promise<void>;
async function update(request: UpdateAutomationRequest): Promise<void>;
async function delete(name: string): Promise<void>;
```

---

## Phase 6: UI Components

**Goal**: Functional pages for all features

### Update: `/automations` Page
- Connect AutomationEditor to create/update stores
- Add delete button with confirmation dialog
- Show real run history from SQLite

### Update: `/executions` Page
- Replace placeholder with SessionsList component
- Show historical sessions from database
- Click to view session detail

### New: `src/lib/components/sessions/SessionsList.svelte`
- Table of sessions with state badges
- Filter by state (completed, failed, cancelled)
- Search by prompt name
- Pagination/infinite scroll

### New: `src/lib/components/sessions/SessionDetail.svelte`
- Full output display
- Event timeline
- Metrics display
- Retry button (new session with same config)
- Delete button

### Update: `/settings` Page
- Verify ConfigEditor save functionality
- Add syntax validation for TOML
- Show config source (home vs repo)

---

## Implementation Order

| Phase | Priority | Effort | Description |
|-------|----------|--------|-------------|
| 1 | HIGH | 5 min | Register missing commands |
| 2 | HIGH | 2 hr | SQLite schema + session_store.rs |
| 3 | HIGH | 1 hr | New commands + registration |
| 4 | MED | 30 min | Frontend tauri service |
| 5 | MED | 1 hr | Frontend stores |
| 6 | MED | 3 hr | UI components |

**Recommended sequence**: 1 → 4 → 5 (partial) → 2 → 3 → 5 (complete) → 6

---

## Critical Files

| File | Purpose |
|------|---------|
| `src-tauri/src/lib.rs` | Register commands in invoke_handler |
| `src-tauri/src/commands.rs` | Add session and delete commands |
| `src-tauri/src/state.rs` | Add session_store to AppState |
| `src-tauri/src/session_store.rs` | **NEW** - SQLite session persistence |
| `crates/automations/src/store.rs` | Reference pattern for SQLite |
| `src/lib/services/tauri.ts` | Frontend API functions |
| `src/lib/stores/automations.ts` | Add CRUD methods |
| `src/lib/stores/sessions.ts` | **NEW** - Session state |
| `src/routes/automations/+page.svelte` | Connect to real data |
| `src/routes/executions/+page.svelte` | Show real sessions |

---

## Verification

1. **Backend tests**: `cargo test -p velor-gui`
2. **Type check**: `bun run check`
3. **Manual testing**:
   - Create/edit/delete automations from UI
   - Run automation, verify session in database
   - View session output in UI
   - Edit and save config
   - Restart app, verify history persists
