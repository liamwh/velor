//! Streaming TUI for `vel auto` — multi-line, per-type event rendering with
//! syntax-highlighted word-level diffs, token usage, animated spinner, and
//! terminal title integration.
//!
//! Performance design: the live transcript is a **bounded semantic buffer**
//! (see [`crate::tui_transcript`]); the complete run transcript remains in the
//! durable JSONL run log. Rendering is viewport-only — each frame lays out only
//! the visible rows plus a small overscan, never the whole history — and a
//! bounded layout cache avoids re-wrapping/re-highlighting entries between
//! frames. Streamed text/thinking coalesces into one entry per segment.

use std::collections::{HashMap, VecDeque};
use std::io;
use std::time::{Duration, Instant};

use chrono::{DateTime, Local};
use color_eyre::eyre::WrapErr;
use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    terminal::SetTitle,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};
use tokio_util::sync::CancellationToken;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::highlight::theme::token_style;
use crate::highlight::token::{SemanticSpan, SemanticToken};
use crate::highlight::{HighlightEngine, HighlightRequest};
use crate::theme;
use crate::tui_transcript::{
    self, LiveEntry, RowMeter, ScrollState, Transcript, TuiLimits, Viewport,
};
use velor_core::file_edit::{DiffLine, FileEdit, FileEditKind, FileHunk, LineKind};

// The entry model lives in `tui_transcript`; re-exported for callers (main.rs).
pub use crate::tui_transcript::{EntryKind, TuiEntry};

// ── Live-steering command/outcome model ─────────────────────────────────────

use velor_core::agent::SteeringDelivery;
use velor_core::execution_service::adapters::claude_stream::{PersistentAppend, SteeringText};
use velor_core::execution_service::capabilities::LiveSteeringStatus;

/// A steering command sent from the TUI to the run-loop controller, each
/// carrying an acknowledgement the controller resolves with an explicit outcome.
#[derive(Debug)]
pub enum TuiSteeringCommand {
    /// One-shot live steering (the `c` key): send `text` to the active session.
    SendOnce {
        /// The steering text (validated non-empty by the TUI before submission).
        text: SteeringText,
        /// Resolves with the delivery outcome.
        acknowledgement: tokio::sync::oneshot::Sender<Result<SteeringOutcome, TuiSteeringError>>,
    },
    /// Replace the persistent append (the `a` key). An empty `append` clears it.
    ReplacePersistent {
        /// The new append (`None` clears).
        append: Option<PersistentAppend>,
        /// Whether to also send the new append live when a session is available.
        send_live_when_available: bool,
        /// Resolves with the append outcome.
        acknowledgement: tokio::sync::oneshot::Sender<Result<AppendOutcome, TuiSteeringError>>,
    },
}

/// The outcome of a one-shot `c` steering submission.
#[derive(Debug, Clone)]
pub enum SteeringOutcome {
    /// The message was delivered (to the degree reported).
    Sent {
        /// The delivery state observed.
        delivery: SteeringDelivery,
    },
    /// No active, steerable session was available.
    Unavailable {
        /// Why it was unavailable.
        status: LiveSteeringStatus,
    },
}

/// The outcome of an `a` persistent-append edit.
#[derive(Debug, Clone)]
pub enum AppendOutcome {
    /// Append updated and sent live to the active session.
    UpdatedAndSent {
        /// The delivery state observed for the live send.
        delivery: SteeringDelivery,
    },
    /// Append updated; no live session was available to steer.
    UpdatedOnly {
        /// The session status at the time.
        status: LiveSteeringStatus,
    },
    /// Append cleared and the clear sent live.
    ClearedAndSent {
        /// The delivery state observed for the live send.
        delivery: SteeringDelivery,
    },
    /// Append cleared; no live session was available.
    ClearedOnly {
        /// The session status at the time.
        status: LiveSteeringStatus,
    },
}

/// Error returned when a steering command could not be processed (e.g. the
/// controller has gone away).
#[derive(Debug, Clone)]
pub struct TuiSteeringError(pub String);

impl std::fmt::Display for TuiSteeringError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "steering command was not processed: {}", self.0)
    }
}

impl std::error::Error for TuiSteeringError {}

const TAB_WIDTH: usize = 4;
const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
/// Extra rows laid out above/below the viewport for smooth scrolling. The
/// per-frame cost is proportional to viewport + this, never to total history.
const OVERSCAN_ROWS: u32 = 24;
/// Maximum rendered entries held in the layout cache. Bounded so the cache
/// cannot replace one unbounded problem with another.
const CACHE_CAPACITY: usize = 512;

/// Maximum gap between the `g` prefix and its second key before a pending `g`
/// is cancelled.
const G_CHORD_TIMEOUT: Duration = Duration::from_millis(500);

/// The provider/binary/model this run was launched with, shown by the `m`
/// modal. Resolved once at startup from config — nothing here changes over
/// the course of a run.
#[derive(Debug, Clone)]
pub struct ProviderInfo {
    /// Human-readable provider name (e.g. "Claude", "Codex", "Omp").
    pub provider: String,
    /// The actual executable invoked (e.g. "claude", "claude-glm", "omp").
    pub binary: String,
    /// The configured model override, when the provider supports one and it
    /// was set; `None` means the binary's own default is in effect.
    pub model: Option<String>,
}

// ── Messages ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum TuiMessage {
    Entry(TuiEntry),
    SetPrompt(String),
    SetLogPath(String),
    /// Sets the provider/binary/model shown by the `m` modal. Sent once at
    /// startup.
    SetProviderInfo(ProviderInfo),
    /// Updates the current/total iteration counter shown in the status line.
    SetIteration {
        current: u32,
        total: u32,
    },
    /// Updates the live-steering availability shown in the status line.
    SetLiveSteeringStatus(LiveSteeringStatus),
    /// Updates the controller-owned persistent append (for the indicator and the
    /// `a` editor's pre-fill).
    SetPersistentAppend(Option<PersistentAppend>),
    /// Reports the delivery state of the most recent live steering send.
    SteeringDeliveryUpdated(SteeringDelivery),
    /// Signals the entire auto loop is done — the TUI should exit.
    RunComplete,
}

pub fn agent_event_to_tui(event: &velor_core::agent::AgentEvent) -> Option<TuiEntry> {
    use velor_core::agent::AgentEvent;
    match event {
        AgentEvent::TextDelta { text } if text.is_empty() => None,
        AgentEvent::TextDelta { text } => Some(TuiEntry::now(EntryKind::Text(text.clone()))),
        AgentEvent::Thinking { text } if text.is_empty() => None,
        AgentEvent::Thinking { text } => Some(TuiEntry::now(EntryKind::Thinking(text.clone()))),
        AgentEvent::ToolCall {
            tool,
            detail,
            input,
        } => Some(TuiEntry::now(EntryKind::ToolCall {
            tool: tool.clone(),
            detail: detail.clone(),
            input: input.clone(),
        })),
        AgentEvent::ToolResult {
            tool,
            detail,
            success,
            command,
        } => Some(TuiEntry::now(EntryKind::ToolResult {
            tool: tool.clone(),
            detail: detail.clone(),
            success: *success,
            command: command.clone(),
        })),
        AgentEvent::FileEdit { edit } => {
            Some(TuiEntry::now(EntryKind::FileEdit(Box::new(edit.clone()))))
        }
        AgentEvent::Error { message } => Some(TuiEntry::now(EntryKind::Error(message.clone()))),
        AgentEvent::Status { message } if message.starts_with("session: ") => None,
        AgentEvent::Status { message } if message.starts_with("thread started: ") => None,
        AgentEvent::Status { message } => Some(TuiEntry::now(EntryKind::Info(message.clone()))),
        AgentEvent::Usage {
            input_tokens,
            output_tokens,
            cached_input_tokens,
        } => Some(TuiEntry::now(EntryKind::Usage {
            input_tokens: *input_tokens,
            output_tokens: *output_tokens,
            cached_input_tokens: *cached_input_tokens,
        })),
    }
}

// ── State ───────────────────────────────────────────────────────────────────

/// One explicit input mode drives the steering/append editors (rather than a
/// tangle of booleans). The data model holds the buffer so a later multiline
/// editor can grow from the same shape.
#[derive(Debug, Clone)]
enum InputMode {
    /// Normal browsing/scrolling.
    Normal,
    /// The `c` one-shot steering editor.
    Steering {
        /// The text being composed.
        buffer: String,
        /// Current submission state.
        submission: SubmissionState,
    },
    /// The `a` persistent-append editor, pre-filled with the current append.
    EditingPersistentAppend {
        /// The text being composed.
        buffer: String,
        /// Current submission state.
        submission: SubmissionState,
    },
}

impl Default for InputMode {
    fn default() -> Self {
        Self::Normal
    }
}

impl InputMode {
    /// Returns `true` when an editor modal is open (and so should capture keys).
    const fn is_modal(&self) -> bool {
        matches!(
            self,
            Self::Steering { .. } | Self::EditingPersistentAppend { .. }
        )
    }
}

/// The submission lifecycle of an editor modal.
#[derive(Debug, Clone)]
enum SubmissionState {
    /// The user is still typing.
    Editing,
    /// A command has been submitted and is awaiting its acknowledgement.
    Submitting,
    /// The last submission failed; the editor retains the text and shows this.
    Failed {
        /// Why the submission failed.
        message: String,
    },
}

/// One recorded iteration boundary: the divider's stable entry id and its 1-based
/// iteration number. The number is stored alongside the id so the iteration a
/// reader is *viewing* can be reported even after the divider entry itself is
/// trimmed from the live transcript.
#[derive(Debug, Clone, Copy)]
struct IterationBoundary {
    /// The divider entry id marking where this iteration begins.
    id: crate::tui_transcript::EntryId,
    /// The 1-based iteration number this divider introduces.
    number: u32,
}

struct TuiState {
    transcript: Transcript,
    scroll: ScrollState,
    cache: LayoutCache,
    prompt: Option<String>,
    log_path: Option<String>,
    /// Provider/binary/model this run was launched with, shown by the `m`
    /// modal. `None` until `SetProviderInfo` arrives (should be immediate).
    provider_info: Option<ProviderInfo>,
    show_prompt: bool,
    show_help: bool,
    /// Whether the `m` provider/model info modal is open.
    show_provider_info: bool,
    /// Live search query for filtering the `?` help modal's keybinding list
    /// (`/` starts typing, Enter commits the filter, Esc clears it).
    help_search: String,
    /// Whether `/` search is actively capturing keystrokes in the help
    /// modal — while true, printable keys append to `help_search` instead of
    /// the usual "any key closes the modal" behavior.
    help_search_active: bool,
    show_errors: bool,
    prompt_scroll: u16,
    error_scroll: u16,
    spinner_idx: usize,
    spinner_verb: &'static str,
    show_thinking: bool,
    /// Whether tool-result/file-edit content renders inline (toggled by `o`).
    /// The `ToolCall` invocation itself always renders; this hides only what
    /// came back from it, distinct from `Ctrl+O`'s collapsed-vs-full toggle
    /// on the content that *is* shown.
    show_tool_output: bool,
    open_log: bool,
    /// The active input mode (normal vs. a steering/append editor).
    input_mode: InputMode,
    /// Sender for steering commands to the run-loop controller.
    steering_tx: Option<tokio::sync::mpsc::Sender<TuiSteeringCommand>>,
    /// Current live-steering availability (drives whether `c` is offered).
    live_steering_status: LiveSteeringStatus,
    /// The controller-owned persistent append (for the indicator + `a` pre-fill).
    persistent_append: Option<PersistentAppend>,
    /// The delivery state of the most recent live send (shown briefly).
    last_delivery: Option<SteeringDelivery>,
    /// A pending steering acknowledgement being polled by the event loop.
    pending_ack: Option<tokio::sync::oneshot::Receiver<Result<SteeringOutcome, TuiSteeringError>>>,
    /// A pending append acknowledgement (separate type, polled alongside).
    pending_append_ack:
        Option<tokio::sync::oneshot::Receiver<Result<AppendOutcome, TuiSteeringError>>>,
    /// A transient status line (e.g. "live steering unsupported by this provider").
    transient_status: Option<(Instant, String)>,
    /// Last content width seen by render, so scroll keypresses can resolve row
    /// counts without re-deriving the layout area.
    last_width: u16,
    /// Last viewport height seen by render, used for scroll-up-from-tail.
    last_viewport_rows: u32,
    // Token usage (latest).
    input_tokens: u64,
    output_tokens: u64,
    cached_tokens: u64,
    /// Current/total iteration (shown in the status line).
    iteration: Option<(u32, u32)>,
    /// Boundaries marking where each iteration began, in order. Each carries the
    /// divider's stable entry id and its 1-based number, so the iteration being
    /// *viewed* can be reported even after the divider entry is trimmed. Used to
    /// resolve "this iteration" from the viewport for `gg`/`G` navigation and for
    /// the "viewing iteration" indicator.
    iteration_starts: Vec<IterationBoundary>,
    /// A pending iteration boundary awaiting its first entry. `Some((current,
    /// total))` is set when `SetIteration` advances the counter; the next ingested
    /// entry is preceded by a divider, which is indexed as the iteration boundary
    /// for `gg`/`G` navigation.
    pending_iteration: Option<(u32, u32)>,
    /// A pending `g` prefix awaiting its second key (`g`/`T`/`B`). Cleared on
    /// timeout or any non-matching key.
    pending_g: bool,
    /// Deadline at which a pending `g` prefix is cancelled.
    g_deadline: Option<Instant>,
    /// The current todo-list state, rendered as a sticky panel pinned above
    /// the spinner/hints rows so it survives scrolling. Refreshed whenever a
    /// todo-tool call/result carries a new snapshot; never cleared mid-run
    /// (the last known state is more useful than nothing while idle).
    sticky_todo: Option<String>,
    /// Whether the sticky todo panel renders at its expanded height (showing
    /// the whole board) or its small default preview. Toggled by `Ctrl+T`.
    sticky_todo_expanded: bool,
}

/// Whether an entry renders in the live log under the current visibility
/// toggles: `Thinking` gated by `show_thinking` (`t`), tool-result/file-edit
/// content gated by `show_tool_output` (`o`). The `ToolCall` invocation itself
/// is never hidden — only what came back from it.
fn entry_visible(kind: &EntryKind, show_thinking: bool, show_tool_output: bool) -> bool {
    match kind {
        EntryKind::Thinking(_) => show_thinking,
        EntryKind::ToolResult { .. } | EntryKind::FileEdit(_) => show_tool_output,
        _ => true,
    }
}

impl TuiState {
    fn new(limits: TuiLimits) -> Self {
        Self {
            transcript: Transcript::new(limits),
            scroll: ScrollState::Tail,
            cache: LayoutCache::new(CACHE_CAPACITY, limits.max_entry_lines),
            prompt: None,
            log_path: None,
            provider_info: None,
            show_prompt: false,
            show_help: false,
            show_provider_info: false,
            help_search: String::new(),
            help_search_active: false,
            show_errors: false,
            prompt_scroll: 0,
            error_scroll: 0,
            spinner_idx: 0,
            spinner_verb: "starting",
            show_thinking: true,
            show_tool_output: true,
            open_log: false,
            last_width: 80,
            last_viewport_rows: 10,
            input_tokens: 0,
            output_tokens: 0,
            cached_tokens: 0,
            iteration: None,
            iteration_starts: Vec::new(),
            pending_iteration: None,
            pending_g: false,
            g_deadline: None,
            input_mode: InputMode::default(),
            steering_tx: None,
            live_steering_status: LiveSteeringStatus::Unsupported,
            persistent_append: None,
            last_delivery: None,
            pending_ack: None,
            pending_append_ack: None,
            transient_status: None,
            sticky_todo: None,
            sticky_todo_expanded: false,
        }
    }

    /// Scrolls toward older content by `n` rows; stable under streaming/trimming.
    fn scroll_up_by(&mut self, n: u32) {
        let entries = self.transcript.entries();
        let (show_thinking, show_tool_output) = (self.show_thinking, self.show_tool_output);
        let pred = move |e: &LiveEntry| entry_visible(&e.kind, show_thinking, show_tool_output);
        self.scroll = tui_transcript::scroll_up(
            entries,
            &mut self.cache,
            self.last_width,
            self.last_viewport_rows,
            self.scroll,
            n,
            pred,
        );
    }

    /// Scrolls toward newer content by `n` rows; returns to follow-tail at bottom.
    fn scroll_down_by(&mut self, n: u32) {
        let entries = self.transcript.entries();
        let (show_thinking, show_tool_output) = (self.show_thinking, self.show_tool_output);
        let pred = move |e: &LiveEntry| entry_visible(&e.kind, show_thinking, show_tool_output);
        self.scroll = tui_transcript::scroll_down(
            entries,
            &mut self.cache,
            self.last_width,
            self.scroll,
            n,
            pred,
        );
    }

    /// Jump to the bottom of the transcript, re-enabling live follow-tail.
    fn jump_to_bottom(&mut self) {
        self.scroll = ScrollState::Tail;
    }

    /// Jump to the first row of the entry whose id is `id`, anchoring there. If
    /// the entry was trimmed from the live view, pins to the oldest retained
    /// entry instead (clamped by viewport selection).
    fn jump_to_entry(&mut self, id: crate::tui_transcript::EntryId) {
        let entries = self.transcript.entries();
        if entries.is_empty() {
            self.scroll = ScrollState::Tail;
            return;
        }
        let anchor = entries
            .iter()
            .find(|e| e.id == id)
            .map(|e| e.id)
            .unwrap_or_else(|| entries[0].id);
        self.scroll = ScrollState::Anchored {
            entry_id: anchor,
            hidden_rows: 0,
        };
    }

    /// Jump to the absolute top of the retained transcript.
    fn jump_to_absolute_top(&mut self) {
        let entries = self.transcript.entries();
        if let Some(first) = entries.first() {
            self.scroll = ScrollState::Anchored {
                entry_id: first.id,
                hidden_rows: 0,
            };
        } else {
            self.scroll = ScrollState::Tail;
        }
    }

    /// Returns the entry id anchoring the current viewport top, resolving a
    /// `Tail` state to the newest entry. Used to locate "this iteration" from
    /// wherever the reader is browsing.
    fn viewport_top_id(&self) -> Option<crate::tui_transcript::EntryId> {
        let entries = self.transcript.entries();
        if entries.is_empty() {
            return None;
        }
        match self.scroll {
            ScrollState::Tail => entries.last().map(|e| e.id),
            ScrollState::Anchored { entry_id, .. } => Some(entry_id),
        }
    }

    /// Finds the iteration that contains the viewport's top entry, returning the
    /// `(start_id, end_id, number)` boundary. `end_id` is the entry just before
    /// the next iteration begins (or the newest entry for the last iteration);
    /// `number` is the 1-based iteration the viewport is currently in.
    fn current_iteration_bounds(
        &self,
    ) -> Option<(
        crate::tui_transcript::EntryId,
        crate::tui_transcript::EntryId,
        u32,
    )> {
        let entries = self.transcript.entries();
        if self.iteration_starts.is_empty() || entries.is_empty() {
            return None;
        }
        let view_id = self.viewport_top_id()?;
        // Position of the viewport entry; fall back to the last retained entry
        // if the anchor was trimmed.
        let view_idx = entries
            .iter()
            .position(|e| e.id == view_id)
            .unwrap_or(entries.len() - 1);

        // Find the latest iteration start at or before the viewport entry.
        let mut start_idx = 0;
        let mut start_id = entries[0].id;
        let mut number = self.iteration_starts[0].number;
        for boundary in &self.iteration_starts {
            if let Some(i) = entries.iter().position(|e| e.id == boundary.id) {
                if i <= view_idx {
                    start_idx = i;
                    start_id = entries[i].id;
                    number = boundary.number;
                } else {
                    break;
                }
            }
        }
        // The iteration ends just before the next recorded iteration start that
        // comes after `start_idx`, or at the newest entry.
        let end_id = self
            .iteration_starts
            .iter()
            .filter_map(|b| entries.iter().position(|e| e.id == b.id))
            .find(|&i| i > start_idx)
            .map(|i| entries[i - 1].id)
            .unwrap_or_else(|| entries.last().unwrap().id);
        // If the viewport sits before the first iteration start we still know
        // the end (the entry before that start); use the absolute start as begin.
        let begin = if view_idx < start_idx {
            entries.first().unwrap().id
        } else {
            start_id
        };
        let _ = start_idx;
        Some((begin, end_id, number))
    }

    /// The 1-based number of the iteration the viewport's top entry currently
    /// sits in, or `None` when there are no iteration boundaries. Used for the
    /// "viewing iteration" indicator, which differs from the *running* iteration
    /// ([`TuiState::iteration`]) when the reader has scrolled into history.
    fn viewing_iteration_number(&self) -> Option<u32> {
        self.current_iteration_bounds().map(|(_, _, number)| number)
    }

    /// `gg` — jump to the top of the iteration currently in view.
    fn jump_to_iteration_top(&mut self) {
        match self.current_iteration_bounds() {
            Some((start_id, _, _)) => self.jump_to_entry(start_id),
            None => self.jump_to_absolute_top(),
        }
    }

    /// `G` — jump to the bottom of the iteration currently in view. For the
    /// latest iteration this is the live tail, so it re-enables streaming.
    fn jump_to_iteration_bottom(&mut self) {
        let Some((_, end_id, _)) = self.current_iteration_bounds() else {
            self.jump_to_bottom();
            return;
        };
        let entries = self.transcript.entries();
        if entries.last().is_some_and(|e| e.id == end_id) {
            // The iteration's last entry is also the newest → live tail.
            self.scroll = ScrollState::Tail;
        } else {
            self.jump_to_entry(end_id);
        }
    }

    /// `gT` — jump to the absolute top of the whole chat.
    fn jump_to_absolute_top_cmd(&mut self) {
        self.jump_to_absolute_top();
    }

    /// `gB` — jump to the absolute bottom (re-enable live tail).
    fn jump_to_absolute_bottom(&mut self) {
        self.jump_to_bottom();
    }

    // ── `g` prefix chord ──────────────────────────────────────────────────────

    /// Begins a `g` prefix; the next key (`g`/`T`/`B`) resolves it.
    fn begin_g(&mut self) {
        self.pending_g = true;
        self.g_deadline = Some(Instant::now() + G_CHORD_TIMEOUT);
    }

    /// Cancels any pending `g` prefix. Called on a non-matching key or timeout.
    fn cancel_g(&mut self) {
        self.pending_g = false;
        self.g_deadline = None;
    }

    /// Flushes a pending `g` prefix if its deadline has passed.
    fn tick_g_chord(&mut self) {
        if self.pending_g && self.g_deadline.map_or(true, |d| Instant::now() >= d) {
            self.cancel_g();
        }
    }

    // ── Live-steering editors ───────────────────────────────────────────────

    /// Opens the `c` steering editor (only when a session is Ready), or sets a
    /// transient status explaining why it is unavailable.
    fn open_steering(&mut self) {
        if self.pending_ack.is_some() {
            return; // already submitting
        }
        match self.live_steering_status {
            LiveSteeringStatus::Ready => {
                self.input_mode = InputMode::Steering {
                    buffer: String::new(),
                    submission: SubmissionState::Editing,
                };
            }
            LiveSteeringStatus::Unsupported => {
                self.set_transient("Live steering is not supported by this provider.");
            }
            LiveSteeringStatus::Inactive => {
                self.set_transient("No active agent session is available to steer.");
            }
            LiveSteeringStatus::Closing => {
                self.set_transient("The active agent session is closing.");
            }
        }
    }

    /// Builds an AI-friendly plain-text transcript export honoring the
    /// current thinking/tool-output visibility toggles and current todo
    /// state. `since_last_prompt` scopes the export to the current iteration
    /// (from the most recent iteration divider onward — i.e. since the
    /// model's last rendered prompt); falls back to the whole retained
    /// transcript when no iteration boundary has been recorded yet.
    fn transcript_export(&self, since_last_prompt: bool) -> String {
        let entries = self.transcript.entries();
        let start = if since_last_prompt {
            self.iteration_starts
                .last()
                .and_then(|b| entries.iter().position(|e| e.id == b.id))
                .unwrap_or(0)
        } else {
            0
        };
        tui_transcript::render_plain_text(
            &entries[start..],
            self.sticky_todo.as_deref(),
            tui_transcript::PlainTextOptions {
                show_thinking: self.show_thinking,
                show_tool_output: self.show_tool_output,
            },
        )
    }

    /// Copies the transcript export to the system clipboard (native access
    /// via `arboard`) and reports the outcome as a transient status line.
    fn copy_transcript_to_clipboard(&mut self, since_last_prompt: bool) {
        let text = self.transcript_export(since_last_prompt);
        if text.trim().is_empty() {
            self.set_transient("Nothing to copy yet.");
            return;
        }
        let scope = if since_last_prompt {
            "since last prompt"
        } else {
            "entire transcript"
        };
        let bytes = text.len();
        match arboard::Clipboard::new().and_then(|mut c| c.set_text(text)) {
            Ok(()) => self.set_transient(&format!("Copied {scope} ({bytes} bytes) to clipboard.")),
            Err(e) => self.set_transient(&format!("Clipboard copy failed: {e}")),
        }
    }

    /// Opens the `a` persistent-append editor, pre-filled with the current append.
    fn open_append_editor(&mut self) {
        if self.pending_append_ack.is_some() {
            return; // already submitting
        }
        let buffer = self
            .persistent_append
            .as_ref()
            .map(|a| a.as_str().to_string())
            .unwrap_or_default();
        self.input_mode = InputMode::EditingPersistentAppend {
            buffer,
            submission: SubmissionState::Editing,
        };
    }

    fn set_transient(&mut self, msg: &str) {
        self.transient_status = Some((Instant::now(), msg.to_string()));
    }

    /// Handles a key while a steering/append editor modal is open. Returns true
    /// if the key was consumed by the modal.
    fn handle_modal_key(&mut self, key: event::KeyEvent) -> bool {
        if !self.input_mode.is_modal() {
            return false;
        }
        // Esc always cancels, restoring prior state.
        if key.code == KeyCode::Esc {
            self.cancel_modal();
            return true;
        }
        // Collect any submit action (Enter) without calling `self` methods inside
        // the `&mut self.input_mode` borrow; dispatch afterwards.
        // `(is_append, text)`.
        let mut submit: Option<(bool, String)> = None;
        match &mut self.input_mode {
            InputMode::Steering { buffer, submission } => match submission {
                SubmissionState::Submitting => return true, // ignore keys while awaiting ack
                SubmissionState::Editing | SubmissionState::Failed { .. } => match key.code {
                    KeyCode::Enter => submit = Some((false, buffer.clone())),
                    KeyCode::Char(c) if c != '\n' => {
                        buffer.push(c);
                        *submission = SubmissionState::Editing;
                    }
                    KeyCode::Backspace => {
                        buffer.pop();
                        *submission = SubmissionState::Editing;
                    }
                    _ => {}
                },
            },
            InputMode::EditingPersistentAppend { buffer, submission } => match submission {
                SubmissionState::Submitting => return true,
                SubmissionState::Editing | SubmissionState::Failed { .. } => match key.code {
                    KeyCode::Enter => submit = Some((true, buffer.clone())),
                    KeyCode::Char(c) if c != '\n' => {
                        buffer.push(c);
                        *submission = SubmissionState::Editing;
                    }
                    KeyCode::Backspace => {
                        buffer.pop();
                        *submission = SubmissionState::Editing;
                    }
                    _ => {}
                },
            },
            // Guarded out at the top; kept for exhaustiveness.
            InputMode::Normal => {}
        }
        if let Some((is_append, text)) = submit {
            if is_append {
                self.submit_append(text);
            } else {
                self.submit_steering(text);
            }
        }
        true
    }

    /// Submits the `c` steering buffer: validates, sends the command on a spawned
    /// task, and records the pending acknowledgement for polling.
    fn submit_steering(&mut self, buffer: String) {
        let text = match SteeringText::new(buffer.clone()) {
            Ok(t) => t,
            Err(_) => {
                self.set_submission_failed("Steering text must not be empty.");
                return;
            }
        };
        let Some(tx) = self.steering_tx.clone() else {
            self.set_submission_failed("No steering channel is connected.");
            return;
        };
        let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
        let cmd = TuiSteeringCommand::SendOnce {
            text,
            acknowledgement: ack_tx,
        };
        // Send on a task so a full/closed controller channel cannot stall the
        // TUI polling loop; if the send fails the ack sender drops and the poll
        // surfaces the failure.
        tokio::spawn(async move {
            let _ = tx.send(cmd).await;
        });
        self.pending_ack = Some(ack_rx);
        self.set_submission_state(SubmissionState::Submitting);
    }

    /// Submits the `a` append buffer: replaces (or clears) the append, optionally
    /// sending it live, and records the pending acknowledgement.
    fn submit_append(&mut self, buffer: String) {
        let append = PersistentAppend::new(buffer);
        let Some(tx) = self.steering_tx.clone() else {
            self.set_submission_failed("No steering channel is connected.");
            return;
        };
        let send_live = matches!(self.live_steering_status, LiveSteeringStatus::Ready);
        let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
        let cmd = TuiSteeringCommand::ReplacePersistent {
            append,
            send_live_when_available: send_live,
            acknowledgement: ack_tx,
        };
        tokio::spawn(async move {
            let _ = tx.send(cmd).await;
        });
        self.pending_append_ack = Some(ack_rx);
        self.set_submission_state(SubmissionState::Submitting);
    }

    fn set_submission_state(&mut self, state: SubmissionState) {
        match &mut self.input_mode {
            InputMode::Steering { submission, .. } => *submission = state,
            InputMode::EditingPersistentAppend { submission, .. } => *submission = state,
            InputMode::Normal => {}
        }
    }

    fn set_submission_failed(&mut self, message: &str) {
        self.set_submission_state(SubmissionState::Failed {
            message: message.to_string(),
        });
    }

    fn cancel_modal(&mut self) {
        // Esc restores the prior append indicator (the controller owns the value;
        // the editor never mutated it) and returns to Normal.
        self.input_mode = InputMode::Normal;
    }

    /// Polls any pending steering/append acknowledgement (non-blocking) and
    /// applies the outcome: close the modal on success, surface a message on
    /// failure or unavailable. Called once per render cycle.
    fn poll_pending_submission(&mut self) {
        if let Some(mut ack) = self.pending_ack.take() {
            match ack.try_recv() {
                Ok(Ok(SteeringOutcome::Sent { delivery })) => {
                    self.last_delivery = Some(delivery);
                    self.input_mode = InputMode::Normal;
                }
                Ok(Ok(SteeringOutcome::Unavailable { status })) => {
                    let msg = match status {
                        LiveSteeringStatus::Unsupported => {
                            "Live steering is not supported by this provider."
                        }
                        LiveSteeringStatus::Inactive => "No active session is available.",
                        LiveSteeringStatus::Closing => "The active session is closing.",
                        LiveSteeringStatus::Ready => "Steering was unexpectedly unavailable.",
                    };
                    self.set_submission_failed(msg);
                }
                Ok(Err(e)) => self.set_submission_failed(&e.to_string()),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                    self.pending_ack = Some(ack); // still pending
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                    self.set_submission_failed("Steering command was not processed.");
                }
            }
        }
        if let Some(mut ack) = self.pending_append_ack.take() {
            match ack.try_recv() {
                Ok(Ok(outcome)) => {
                    // The controller owns the append value and has already sent
                    // SetPersistentAppend; reflect the live-delivery state if any.
                    let (delivery, msg) = match &outcome {
                        AppendOutcome::UpdatedAndSent { delivery } => (
                            Some(*delivery),
                            "Append updated and sent to the active Claude session.",
                        ),
                        AppendOutcome::ClearedAndSent { delivery } => {
                            (Some(*delivery), "Persistent append cleared.")
                        }
                        AppendOutcome::UpdatedOnly { status }
                        | AppendOutcome::ClearedOnly { status } => {
                            let m = match status {
                                LiveSteeringStatus::Unsupported => {
                                    "Append updated; live steering is unsupported by this provider."
                                }
                                _ => "Append updated; no active session was available to steer.",
                            };
                            (None, m)
                        }
                    };
                    if let Some(d) = delivery {
                        self.last_delivery = Some(d);
                    }
                    self.set_transient(msg);
                    self.input_mode = InputMode::Normal;
                }
                Ok(Err(e)) => self.set_submission_failed(&e.to_string()),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                    self.pending_append_ack = Some(ack); // still pending
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                    self.set_submission_failed("Append update was not processed.");
                }
            }
        }
        // Expire the transient status after a short window.
        if let Some((at, _)) = self.transient_status {
            if at.elapsed() > Duration::from_secs(4) {
                self.transient_status = None;
            }
        }
    }
}

// ── Layout cache ─────────────────────────────────────────────────────────────

#[derive(Hash, PartialEq, Eq, Clone)]
struct CacheKey {
    id: u64,
    rev: u64,
    width: u16,
    /// Whether output was rendered expanded. Part of the key so toggling
    /// `Ctrl+O` naturally invalidates every cached layout that differs
    /// between the two states, without needing to touch entry revisions.
    expand: bool,
}

/// The wrapped, timestamped display rows for one entry at one width.
struct CachedLayout {
    rows: Vec<Line<'static>>,
}

/// A bounded FIFO cache of per-entry rendered layouts. Keyed by entry id +
/// revision + width + expand state, so it naturally misses when an entry
/// streams (rev bumps), the terminal is resized (width changes), or the user
/// toggles `Ctrl+O`. Capacity-bounded by eviction.
struct LayoutCache {
    map: HashMap<CacheKey, CachedLayout>,
    order: VecDeque<CacheKey>,
    capacity: usize,
    engine: HighlightEngine,
    /// Entries rendered since construction (cache misses). Instrumented so tests
    /// can assert per-frame work is bounded by the viewport, not total history.
    renders: usize,
    /// Collapsed-view line cap for a tool result's output body (from
    /// [`TuiLimits::max_entry_lines`]), applied at render time so `Ctrl+O`
    /// can reveal the full retained text instead of data lost at ingest.
    max_entry_lines: usize,
    /// Whether tool-result output currently renders expanded (full text) or
    /// collapsed (head + tail), toggled by `Ctrl+O`.
    expand_output: bool,
}

impl LayoutCache {
    fn new(capacity: usize, max_entry_lines: usize) -> Self {
        Self {
            map: HashMap::new(),
            order: VecDeque::new(),
            capacity,
            engine: HighlightEngine::new(),
            renders: 0,
            max_entry_lines,
            expand_output: false,
        }
    }

    /// Flips the expand-output state. The cache doesn't need clearing: the
    /// new `expand` value simply produces a different [`CacheKey`], so old
    /// entries just age out via normal eviction rather than being reused.
    fn toggle_expand_output(&mut self) {
        self.expand_output = !self.expand_output;
    }

    /// Ensures a layout exists for `entry` at `width`, returning its row count.
    /// Rendering + wrapping happen on cache miss only. This is the [`RowMeter`]
    /// hook the viewport selector uses.
    fn rows_for(&mut self, entry: &LiveEntry, width: u16) -> u32 {
        let key = CacheKey {
            id: entry.id.raw(),
            rev: entry.rev,
            width,
            expand: self.expand_output,
        };
        if !self.map.contains_key(&key) {
            let opts = RenderOpts {
                expand: self.expand_output,
                max_entry_lines: self.max_entry_lines,
            };
            let rows = build_layout(&entry.kind, entry.ts, width, &mut self.engine, opts);
            self.insert(key.clone(), CachedLayout { rows });
            self.renders += 1;
        }
        self.map.get(&key).map_or(1, |c| c.rows.len().max(1) as u32)
    }

    /// Returns the cached wrapped rows for `entry` at `width`, if present.
    fn layout(&self, entry: &LiveEntry, width: u16) -> Option<&[Line<'static>]> {
        let key = CacheKey {
            id: entry.id.raw(),
            rev: entry.rev,
            width,
            expand: self.expand_output,
        };
        self.map.get(&key).map(|c| c.rows.as_slice())
    }

    fn insert(&mut self, key: CacheKey, val: CachedLayout) {
        if self.map.contains_key(&key) {
            return;
        }
        while self.map.len() >= self.capacity {
            let Some(evicted) = self.order.pop_front() else {
                break;
            };
            self.map.remove(&evicted);
        }
        self.order.push_back(key.clone());
        self.map.insert(key, val);
    }
}

impl RowMeter for LayoutCache {
    fn rows(&mut self, entry: &LiveEntry, width: u16) -> u32 {
        self.rows_for(entry, width)
    }
}

/// Renders an entry's content to fully-wrapped display rows, with the timestamp
/// prefixed on the first row. Wrapping uses Unicode display width so the row
/// count is exact and predictable for viewport math.
///
/// Iteration dividers are exempt from the timestamp prefix: they read as a clean
/// full-width rule rather than a content line.
/// Default collapsed-view line count for a tool result's output body — small
/// enough that collapsing is actually visible for typical output (a few
/// hundred lines is common for a file read), independent of how generous
/// [`TuiLimits::max_entry_lines`] is configured as a hard safety cap.
const COLLAPSED_PREVIEW_LINES: usize = 20;

/// Render-time options that affect an entry's rendered content (and so must
/// be part of the layout cache key — see [`CacheKey`]), as opposed to purely
/// structural inputs like width.
#[derive(Debug, Clone, Copy)]
struct RenderOpts {
    /// Whether tool-result output renders full (`Ctrl+O`) or collapsed
    /// (head + tail).
    expand: bool,
    /// Collapsed-view line cap for a tool result's output body.
    max_entry_lines: usize,
}

impl Default for RenderOpts {
    /// Collapsed, with [`TuiLimits::default`]'s line cap — the options a call
    /// site not exercising expand/collapse behaviour reaches for.
    fn default() -> Self {
        Self {
            expand: false,
            max_entry_lines: TuiLimits::default().max_entry_lines,
        }
    }
}

fn build_layout(
    kind: &EntryKind,
    ts: DateTime<Local>,
    width: u16,
    engine: &mut HighlightEngine,
    opts: RenderOpts,
) -> Vec<Line<'static>> {
    let content_lines = render_entry(kind, width as usize, engine, opts);
    // Dividers, file edits, and boxed tool-result cards read as clean
    // full-width rules/gutters/borders from column 0; a timestamp would break
    // the row. ToolCall is never boxed (see its render_entry arm) and
    // edit-tool results stay a plain unboxed label (the real content is the
    // FileEdit card), so both keep their timestamp like prose does.
    let no_timestamp = matches!(
        kind,
        EntryKind::IterationDivider { .. } | EntryKind::FileEdit(_)
    ) || matches!(kind, EntryKind::ToolResult { tool, .. } if !is_file_edit_tool(tool));
    // File-edit lines carry a gutter; their wrapped continuation rows indent
    // to align beneath the source rather than the gutter.
    let hang = entry_hang(kind);
    let ts_span = Span::styled(
        ts.format("%H:%M:%S ").to_string(),
        Style::default().fg(theme::active().dim),
    );
    let w = (width as usize).max(1);
    let mut rows: Vec<Line<'static>> = Vec::new();
    for (i, line) in content_lines.into_iter().enumerate() {
        let mut spans: Vec<Span<'static>> = Vec::with_capacity(line.spans.len() + 1);
        if i == 0 && !no_timestamp {
            spans.push(ts_span.clone());
        }
        spans.extend(line.spans);
        rows.extend(wrap_spans_indented(&spans, w, hang));
    }
    if rows.is_empty() {
        rows.push(Line::default());
    }
    rows
}

/// Greedily wraps styled spans to `width` columns with an optional hanging
/// indent of `hang` columns: the first row of each call is laid out as-is (so a
/// file-edit line's leading gutter sits at column 0), and every wrapped
/// continuation row is prefixed with `hang` spaces so it aligns beneath the
/// source text rather than beneath the gutter. Empty input yields one empty
/// line; `hang` of 0 gives plain wrapping.
fn wrap_spans_indented(spans: &[Span<'static>], width: usize, hang: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let indent_w = hang.min(width.saturating_sub(1));
    let indent = Span::raw(" ".repeat(indent_w));
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut cur: Vec<Span<'static>> = Vec::new();
    let mut cur_w: usize = 0;
    let mut buf = String::new();
    let mut buf_w: usize = 0;

    for span in spans {
        let style = span.style;
        for ch in span.content.chars() {
            let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
            if cur_w + buf_w + cw > width && (!cur.is_empty() || !buf.is_empty()) {
                flush_span_buf(&mut buf, &mut buf_w, &mut cur, &mut cur_w, style);
                out.push(Line::from(std::mem::take(&mut cur)));
                cur_w = 0;
                // Continuation row: reserve the hanging indent up front.
                if indent_w > 0 {
                    cur.push(indent.clone());
                    cur_w = indent_w;
                }
            }
            buf.push(ch);
            buf_w += cw;
        }
        flush_span_buf(&mut buf, &mut buf_w, &mut cur, &mut cur_w, style);
    }
    if !cur.is_empty() || out.is_empty() {
        out.push(Line::from(std::mem::take(&mut cur)));
    }
    out
}

/// Folds the pending per-span character buffer into the current line as a span.
fn flush_span_buf(
    buf: &mut String,
    buf_w: &mut usize,
    cur: &mut Vec<Span<'static>>,
    cur_w: &mut usize,
    style: Style,
) {
    if buf.is_empty() {
        return;
    }
    cur.push(Span::styled(std::mem::take(buf), style));
    *cur_w += *buf_w;
    *buf_w = 0;
}

// ── Syntax highlighting ─────────────────────────────────────────────────────
//
// The highlight engine (`crate::highlight`) classifies source into semantic
// tokens; this module composes those tokens with diff/gutter styling. The engine
// is owned by `LayoutCache` and built once; see `crate::highlight` for the
// provider architecture (syntect for ~14 languages, tree-sitter for Svelte).

fn expand_tabs(s: &str) -> String {
    s.replace('\t', &" ".repeat(TAB_WIDTH))
}

// ── Run loop ────────────────────────────────────────────────────────────────

/// Runs the streaming TUI until the auto loop completes or the user quits.
/// Returns the retained live entries (the full transcript remains in the run log).
///
/// `limits` bound the live working set; see [`TuiLimits`].
pub async fn run_streaming_tui(
    mut rx: tokio::sync::mpsc::Receiver<TuiMessage>,
    steering_tx: Option<tokio::sync::mpsc::Sender<TuiSteeringCommand>>,
    cancel: CancellationToken,
    cancel_handler: crate::cancellation::CancellationHandler,
    limits: TuiLimits,
) -> color_eyre::eyre::Result<()> {
    enable_raw_mode().wrap_err("enable raw mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).wrap_err("enter alt screen")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).wrap_err("create terminal")?;
    terminal.clear()?;

    let mut state = TuiState::new(limits);
    state.steering_tx = steering_tx;
    set_title("vel auto — starting");

    // `RunComplete` ends the run. Tracked as a flag and checked after a final
    // render rather than via a bare `break` in the message-drain loop: that
    // `break` only exited the inner `while let Ok(...)` drain, never the outer
    // run loop, so the TUI hung forever after a run finished (until ^C×2).
    let mut run_complete = false;

    loop {
        while let Ok(msg) = rx.try_recv() {
            match msg {
                TuiMessage::Entry(e) => {
                    // Update spinner verb + token usage from the event kind, then
                    // ingest (coalesces/bounds/trims; transient Usage is dropped).
                    match &e.kind {
                        EntryKind::ToolCall { tool, input, .. } => {
                            state.spinner_verb = match tool.as_str() {
                                "Bash" => "running command",
                                "Read" => "reading file",
                                "Edit" | "Write" => "editing",
                                "Grep" => "searching",
                                "Glob" => "finding files",
                                _ => "working",
                            };
                            // A todo-tool call that carries the full current list
                            // (e.g. Claude's TodoWrite) is the freshest, most
                            // reliable source — always overwrite with it.
                            if is_todo_tool(tool)
                                && let Some(summary) = todo_summary_from_input(input)
                            {
                                state.sticky_todo = Some(summary);
                            }
                        }
                        EntryKind::ToolResult { tool, detail, .. } => {
                            state.spinner_verb = "thinking";
                            // A todo tool's *result* text is the freshest source
                            // for providers that report the full board back
                            // (e.g. omp's "Remaining items… Overall: N/M done…"),
                            // as opposed to a short generic acknowledgement — the
                            // line-count/length heuristic tells those apart
                            // without needing to know the provider.
                            if is_todo_tool(tool) && is_substantial_todo_summary(detail) {
                                state.sticky_todo = Some(detail.clone());
                            }
                        }
                        EntryKind::Text(_) => {
                            state.spinner_verb = "generating";
                        }
                        EntryKind::Thinking(_) => {
                            state.spinner_verb = "reasoning";
                        }
                        EntryKind::Error(_) => {
                            state.spinner_verb = "error";
                        }
                        EntryKind::Usage {
                            input_tokens,
                            output_tokens,
                            cached_input_tokens,
                        } => {
                            if let Some(i) = input_tokens {
                                state.input_tokens = *i;
                            }
                            if let Some(o) = output_tokens {
                                state.output_tokens = *o;
                            }
                            if let Some(c) = cached_input_tokens {
                                state.cached_tokens = *c;
                            }
                        }
                        EntryKind::Info(_) => {}
                        EntryKind::Warning(_) => {
                            // A retry/backoff is in flight — say so on the spinner
                            // so a sustained provider outage doesn't read as "stuck
                            // generating". The inline transcript entry carries the
                            // reason (overload / rate-limit / attempt count).
                            state.spinner_verb = "retrying";
                        }
                        // File edits render their own header/hunks; the surrounding
                        // ToolCall/ToolResult already drove the spinner verb.
                        EntryKind::FileEdit(_) => {}
                        // Dividers are visual separators; they carry no spinner
                        // verb or usage payload.
                        EntryKind::IterationDivider { .. } => {}
                    }
                    // Refresh the terminal's window title after every entry:
                    // spinner_verb and/or sticky_todo may have just changed
                    // above, and this is the single place both are known to be
                    // current (ToolResult updates sticky_todo *after* setting
                    // spinner_verb, so an inline set_title per-arm would race
                    // against its own todo update).
                    set_title(&window_title(&state));
                    // If this is the first entry of a new iteration, emit a
                    // divider immediately before it and index the divider as the
                    // iteration boundary for `gg`/`G` navigation. The divider is a
                    // whole entry that never coalesces, so it lands at the top of
                    // the iteration and trims cleanly.
                    if let Some((current, total)) = state.pending_iteration.take() {
                        state
                            .transcript
                            .ingest(TuiEntry::now(EntryKind::IterationDivider {
                                number: current,
                                maximum: Some(total),
                            }));
                        if let Some(divider) = state.transcript.entries().last() {
                            state.iteration_starts.push(IterationBoundary {
                                id: divider.id,
                                number: current,
                            });
                        }
                    }
                    state.transcript.ingest(e);
                    // Note: scroll is intentionally left untouched here. In
                    // follow-tail mode the newest content stays pinned; in
                    // anchored mode the viewport stays put as new content streams.
                }
                TuiMessage::SetPrompt(p) => {
                    state.prompt = Some(p);
                    state.prompt_scroll = 0;
                }
                TuiMessage::SetLogPath(p) => {
                    state.log_path = Some(p);
                }
                TuiMessage::SetProviderInfo(info) => {
                    state.provider_info = Some(info);
                }
                TuiMessage::SetIteration { current, total } => {
                    // An iteration change arms a divider that will be emitted just
                    // before the iteration's first entry, marking the boundary
                    // `gg`/`G` use to find "this iteration".
                    if state.iteration != Some((current, total)) {
                        state.iteration = Some((current, total));
                        state.pending_iteration = Some((current, total));
                    }
                }
                TuiMessage::SetLiveSteeringStatus(status) => {
                    state.live_steering_status = status;
                }
                TuiMessage::SetPersistentAppend(append) => {
                    state.persistent_append = append;
                }
                TuiMessage::SteeringDeliveryUpdated(delivery) => {
                    state.last_delivery = Some(delivery);
                }
                TuiMessage::RunComplete => {
                    while let Ok(msg) = rx.try_recv() {
                        if let TuiMessage::Entry(e) = msg {
                            state.transcript.ingest(e);
                        }
                    }
                    run_complete = true;
                }
            }
        }

        // Poll any pending steering/append acknowledgement (non-blocking) so
        // submissions resolve within the polling loop.
        state.poll_pending_submission();

        terminal
            .draw(|f| render(f, &mut state, &cancel_handler))
            .wrap_err("draw")?;

        // Run is done: exit after this final render. Skipping the input poll
        // below avoids blocking ~100 ms for a keystroke we will not act on, so
        // the TUI tears down promptly when the auto loop finishes.
        if run_complete {
            break;
        }

        if event::poll(Duration::from_millis(100))
            .map_err(|e| color_eyre::eyre::eyre!("poll: {e}"))?
            && let Event::Key(key) =
                event::read().map_err(|e| color_eyre::eyre::eyre!("read: {e}"))?
        {
            handle_key(key, &mut state, &cancel, &cancel_handler);
        }

        // Flush a pending `g` chord whose timeout has elapsed — a lone `g`
        // should resolve to "jump to bottom" without waiting for another key.
        state.tick_g_chord();

        // Open log file in pager if requested.
        if state.open_log {
            state.open_log = false;
            if let Some(path) = &state.log_path {
                disable_raw_mode().ok();
                execute!(terminal.backend_mut(), LeaveAlternateScreen).ok();
                let pager = std::env::var("PAGER").unwrap_or_else(|_| "less".to_string());
                let _ = std::process::Command::new(&pager).arg(path).status();
                enable_raw_mode().ok();
                execute!(terminal.backend_mut(), EnterAlternateScreen).ok();
                terminal.clear().ok();
            }
        }

        if cancel.is_cancelled() {
            break;
        }
    }

    set_title("vel auto — done");
    disable_raw_mode().wrap_err("disable raw mode")?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen).wrap_err("leave alt screen")?;
    Ok(())
}

fn set_title(title: &str) {
    let _ = execute!(io::stdout(), SetTitle(title));
}

/// Builds the terminal window/tab title, mirroring the on-screen block title
/// built by [`build_title`]: `vel auto — {task} — {verb}` when a todo item is
/// in progress, else just `vel auto — {verb}`.
fn window_title(state: &TuiState) -> String {
    match current_task_label(state) {
        Some(task) => format!("vel auto — {task} — {}", state.spinner_verb),
        None => format!("vel auto — {}", state.spinner_verb),
    }
}

fn handle_key(
    key: event::KeyEvent,
    state: &mut TuiState,
    cancel: &CancellationToken,
    cancel_handler: &crate::cancellation::CancellationHandler,
) {
    // A steering/append editor captures all keys while open.
    if state.handle_modal_key(key) {
        return;
    }
    // While the help modal is open, `/` starts an inline search that filters
    // the keybinding list live as you type (see `render_help_modal`); Enter
    // commits it, leaving the filter applied, and Esc clears it. Only once
    // search isn't being actively typed does an arbitrary key close the
    // modal, preserving the original "any key closes" behavior.
    if state.show_help {
        if state.help_search_active {
            match key.code {
                KeyCode::Esc => {
                    state.help_search.clear();
                    state.help_search_active = false;
                }
                KeyCode::Enter => {
                    state.help_search_active = false;
                }
                KeyCode::Backspace => {
                    state.help_search.pop();
                }
                KeyCode::Char(c) => {
                    state.help_search.push(c);
                }
                _ => {}
            }
        } else if key.code == KeyCode::Char('/') {
            state.help_search_active = true;
        } else {
            state.show_help = false;
            state.help_search.clear();
        }
        return;
    }
    if state.show_prompt {
        match key.code {
            KeyCode::Char('p') | KeyCode::Esc | KeyCode::Enter => state.show_prompt = false,
            KeyCode::Down | KeyCode::Char('j') => {
                state.prompt_scroll = state.prompt_scroll.saturating_add(1)
            }
            KeyCode::Up | KeyCode::Char('k') => {
                state.prompt_scroll = state.prompt_scroll.saturating_sub(1)
            }
            KeyCode::PageDown => state.prompt_scroll = state.prompt_scroll.saturating_add(10),
            KeyCode::PageUp => state.prompt_scroll = state.prompt_scroll.saturating_sub(10),
            _ => {}
        }
        return;
    }
    // The provider/model modal closes on any key, and swallows the keypress.
    if state.show_provider_info {
        state.show_provider_info = false;
        return;
    }
    if state.show_errors {
        match key.code {
            KeyCode::Char('e') | KeyCode::Esc | KeyCode::Enter => state.show_errors = false,
            KeyCode::Down | KeyCode::Char('j') => {
                state.error_scroll = state.error_scroll.saturating_add(1)
            }
            KeyCode::Up | KeyCode::Char('k') => {
                state.error_scroll = state.error_scroll.saturating_sub(1)
            }
            KeyCode::PageDown => state.error_scroll = state.error_scroll.saturating_add(10),
            KeyCode::PageUp => state.error_scroll = state.error_scroll.saturating_sub(10),
            _ => {}
        }
        return;
    }
    // While a `g` prefix is pending, only its second key (`g`/`T`/`B`) resolves
    // it; anything else cancels the prefix. Handle this before the main match so
    // the prefix can't leak into other commands.
    if state.pending_g {
        match key.code {
            KeyCode::Char('g') => {
                state.cancel_g();
                state.jump_to_iteration_top();
            }
            KeyCode::Char('T') => {
                state.cancel_g();
                state.jump_to_absolute_top_cmd();
            }
            KeyCode::Char('B') => {
                state.cancel_g();
                state.jump_to_absolute_bottom();
            }
            // The very keys that open a modal or scroll would, mid-prefix, just
            // cancel it rather than also triggering their action — clearer for
            // the reader. Any other key cancels and falls through to re-handle.
            KeyCode::Char('G') => {
                state.cancel_g();
                state.jump_to_iteration_bottom();
            }
            _ => {
                state.cancel_g();
            }
        }
        return;
    }

    match key.code {
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            cancel.cancel();
        }
        KeyCode::Char('q') => {
            cancel.cancel();
        }
        KeyCode::Char('p') => {
            state.show_prompt = true;
            state.prompt_scroll = 0;
        }
        // Ctrl+T toggles the sticky todo panel between its small default
        // preview and an expanded height that can actually show a long
        // board. Guarded and placed before the plain `t` arm below (toggle
        // thinking display), which would otherwise also match Ctrl+T since
        // crossterm reports the same `Char('t')` regardless of modifiers.
        KeyCode::Char('t' | 'T') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.sticky_todo_expanded = !state.sticky_todo_expanded;
        }
        KeyCode::Char('t') => {
            state.show_thinking = !state.show_thinking;
            // The layout cache key is width/rev-based, not view-setting-based, so
            // no invalidation is needed: filtering happens at viewport selection.
        }
        KeyCode::Char('e') => {
            state.show_errors = true;
            state.error_scroll = 0;
        }
        KeyCode::Char('s') => {
            cancel_handler.toggle_stop_after_iteration();
        }
        // `c` — one-shot live steering (Ctrl+C is handled above with its guard).
        KeyCode::Char('c') => state.open_steering(),
        // `a` — edit the persistent append (always available; cleared when empty).
        KeyCode::Char('a') => state.open_append_editor(),
        KeyCode::Char('?') => {
            state.show_help = true;
        }
        // `m` shows the provider/binary/model this run was launched with.
        KeyCode::Char('m') => {
            state.show_provider_info = true;
        }
        // `l` opens the full run log.
        KeyCode::Char('l') => {
            state.open_log = true;
        }
        // Ctrl+O expands every collapsed tool result in place to its full
        // retained output (toggle — pressing again re-collapses), matching
        // the "(Ctrl+O: Expand)" hint. Distinct from `l`: this reveals
        // content inline rather than leaving the TUI for a pager. Guarded and
        // placed before the plain `o` arm below (toggle tool-output
        // visibility), which would otherwise also match `Char('o')`.
        KeyCode::Char('o' | 'O') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.cache.toggle_expand_output();
        }
        // `o` hides/shows tool-result and file-edit content entirely (the
        // `ToolCall` invocation itself stays visible either way). Distinct
        // from Ctrl+O, which only toggles collapsed vs. full for content that
        // *is* shown.
        KeyCode::Char('o') => {
            state.show_tool_output = !state.show_tool_output;
        }
        // `y` copies the transcript since the start of the current iteration
        // (i.e. since the model's last rendered prompt) to the system
        // clipboard, in a plain-text form suitable for pasting into another
        // AI session. `Y` copies the entire retained transcript. Both honor
        // the current `t`/`o` visibility toggles.
        KeyCode::Char('y') => state.copy_transcript_to_clipboard(true),
        KeyCode::Char('Y') => state.copy_transcript_to_clipboard(false),
        // `g` begins a prefix chord: `gg` → top of this iteration,
        // `gT` → absolute top of the chat, `gB` → absolute bottom (live tail).
        KeyCode::Char('g') => state.begin_g(),
        // `G` → bottom of this iteration (live tail when viewing the latest).
        KeyCode::Char('G') => state.jump_to_iteration_bottom(),
        // `B` → absolute bottom (live tail), same as `gB` but a single key
        // for the common case of "get back to the live feed right now".
        KeyCode::Char('B') => state.jump_to_absolute_bottom(),
        // Scrolling is anchored to entry ids, so it stays stable while output
        // streams and history is trimmed. Up/k → older, Down/j → newer.
        KeyCode::Up | KeyCode::Char('k') => state.scroll_up_by(1),
        KeyCode::Down | KeyCode::Char('j') => state.scroll_down_by(1),
        KeyCode::PageUp => state.scroll_up_by(10),
        KeyCode::PageDown => state.scroll_down_by(10),
        _ => {}
    }
}

// ── Render ──────────────────────────────────────────────────────────────────

fn render(
    f: &mut Frame,
    state: &mut TuiState,
    cancel_handler: &crate::cancellation::CancellationHandler,
) {
    let area = f.area();

    let sticky_height = sticky_todo_height(
        state.sticky_todo.as_deref(),
        area.height,
        state.sticky_todo_expanded,
    );
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),
            Constraint::Length(sticky_height),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(area);
    let log_area = chunks[0];
    let sticky_area = chunks[1];
    let spinner_area = chunks[2];
    let hints_area = chunks[3];

    state.spinner_idx = (state.spinner_idx + 1) % SPINNER.len();

    // Content width excludes the left/right borders. Layouts are pre-wrapped to
    // this width, so the Paragraph needs no wrap pass.
    let content_width = log_area.width.saturating_sub(2);
    state.last_width = content_width;
    let viewport_rows = log_area.height.saturating_sub(2) as u32;
    state.last_viewport_rows = viewport_rows;

    let entries = state.transcript.entries();
    let show_thinking = state.show_thinking;
    let show_tool_output = state.show_tool_output;
    let pred = move |e: &LiveEntry| entry_visible(&e.kind, show_thinking, show_tool_output);

    // Viewport-only selection: cost ∝ viewport + overscan, not history size.
    let vp = if entries.is_empty() || viewport_rows == 0 {
        Viewport::EMPTY
    } else {
        tui_transcript::select_viewport(
            entries,
            &mut state.cache,
            content_width,
            viewport_rows,
            OVERSCAN_ROWS,
            state.scroll,
            pred,
        )
    };

    // Assemble visible rows from cached layouts only (all window entries were
    // rendered during selection, so these are cache hits).
    let mut visible_lines: Vec<Line<'static>> = Vec::new();
    if vp.count > 0 {
        let end = (vp.start + vp.count).min(entries.len());
        for (rel, i) in (vp.start..end).enumerate() {
            let entry = &entries[i];
            if !entry_visible(&entry.kind, show_thinking, show_tool_output) {
                continue;
            }
            let skip = if rel == 0 { vp.top_skip as usize } else { 0 };
            if let Some(rows) = state.cache.layout(entry, content_width) {
                for line in rows.iter().skip(skip) {
                    visible_lines.push(line.clone());
                }
            }
        }
    }

    // One-shot omitted-history marker, shown only at the top of the retained
    // content (never duplicated, never re-appended while scrolling).
    let omitted = state.transcript.omitted();
    if vp.start == 0 && !omitted.is_empty() {
        let marker = Line::from(vec![
            Span::styled("↑ ", Style::default().fg(theme::active().dim)),
            Span::styled(
                format!(
                    "Earlier transcript content omitted from the live view — {} entries, ~{} KiB; full history remains in the run log.",
                    omitted.entries,
                    omitted.bytes / 1024
                ),
                Style::default()
                    .fg(theme::active().dim)
                    .add_modifier(Modifier::DIM),
            ),
        ]);
        visible_lines.insert(0, marker);
    }

    let error_count = entries
        .iter()
        .filter(|e| matches!(e.kind, EntryKind::Error(_)))
        .count();

    let (left_title, right_title) = build_title(state, entries.len(), error_count);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::active().border))
        .title(Line::from(left_title))
        .title(Line::from(right_title).alignment(Alignment::Right));
    let para = Paragraph::new(visible_lines).block(block);
    f.render_widget(para, log_area);

    if sticky_area.height > 0
        && let Some(todo) = &state.sticky_todo
    {
        render_sticky_todo(f, sticky_area, todo, state.sticky_todo_expanded);
    }

    render_spinner(f, state, spinner_area);
    render_hints(f, state, cancel_handler, error_count, hints_area);

    if state.show_prompt
        && let Some(prompt) = &state.prompt
    {
        render_prompt_modal(f, area, prompt, state.prompt_scroll);
    }
    if state.show_errors {
        render_error_modal(f, area, entries, state.error_scroll, omitted);
    }
    match &state.input_mode {
        InputMode::Steering { buffer, submission } => {
            render_steering_modal(f, area, buffer, submission, false)
        }
        InputMode::EditingPersistentAppend {
            buffer, submission, ..
        } => render_steering_modal(f, area, buffer, submission, true),
        InputMode::Normal => {}
    }
    if let Some((_, msg)) = &state.transient_status {
        render_transient_status(f, area, msg);
    }
    if state.show_help {
        render_help_modal(f, area, &state.help_search, state.help_search_active);
    }
    if state.show_provider_info {
        render_provider_info_modal(f, area, state.provider_info.as_ref());
    }
}

/// Renders the `c` steering / `a` append editor modal.
/// Minimum/maximum inner (content) rows for the steer/append popup. The
/// popup grows with the buffer so a long line wraps across visible rows
/// instead of being clipped; past `MAX_MODAL_INNER_ROWS` it scrolls to keep
/// the tail of the input (where the cursor is) and the footer in view.
const MIN_MODAL_INNER_ROWS: u16 = 7;
const MAX_MODAL_INNER_ROWS_PCT: u16 = 70;

fn render_steering_modal(
    f: &mut Frame,
    area: Rect,
    buffer: &str,
    submission: &SubmissionState,
    is_append: bool,
) {
    let (title, prefix) = if is_append {
        (" ✏️  Append (Enter=save · Esc=cancel) ", "append › ")
    } else {
        (" 🎯  Steer (Enter=send · Esc=cancel) ", "steer › ")
    };
    let theme = theme::active();
    let prompt_style = Style::default()
        .fg(theme.accent)
        .add_modifier(Modifier::BOLD);

    // Popup width is a fixed share of the screen; height grows with content
    // (in exact rows, not a percentage, so it works at any terminal size).
    let pop_w = area.width.saturating_mul(80) / 100;
    let content_width = pop_w.saturating_sub(2).max(1) as usize; // account for left/right borders

    // Pre-wrap the input and footer ourselves (rather than relying on the
    // Paragraph widget's own wrap pass) so we know the exact row count up
    // front and can size/scroll the popup to it.
    let input = format!("{prefix}{buffer}");
    let input_spans = [Span::styled(input, prompt_style)];
    let hang = UnicodeWidthStr::width(prefix);
    let mut lines = wrap_spans_indented(&input_spans, content_width, hang);
    let footer_spans: Vec<Span<'static>> = match submission {
        SubmissionState::Editing => vec![Span::styled(
            if is_append {
                "Empty clears the append. It is folded into every later iteration."
            } else {
                "Sends one message to the active session. Not replayed in later iterations."
            },
            Style::default().fg(theme.dim),
        )],
        SubmissionState::Submitting => vec![Span::styled(
            "Submitting…",
            Style::default().fg(theme.warning),
        )],
        SubmissionState::Failed { message } => vec![
            Span::styled("✗ ", Style::default().fg(theme.error)),
            Span::styled(message.clone(), Style::default().fg(theme.error)),
        ],
    };
    lines.push(Line::from(""));
    lines.extend(wrap_spans_indented(&footer_spans, content_width, 0));

    let max_inner = (area.height.saturating_mul(MAX_MODAL_INNER_ROWS_PCT) / 100)
        .saturating_sub(2)
        .max(MIN_MODAL_INNER_ROWS);
    let total_rows = lines.len() as u16;
    let inner_rows = total_rows.clamp(MIN_MODAL_INNER_ROWS, max_inner);
    // If content overflows the visible rows, scroll so the tail (the end of
    // the input, where typing happens, plus the footer) stays in view.
    let scroll_y = total_rows.saturating_sub(inner_rows);

    let popup_h = inner_rows + 2; // + top/bottom borders
    let x = area.x + (area.width.saturating_sub(pop_w)) / 2;
    let y = area.y + (area.height.saturating_sub(popup_h)) / 2;
    let popup = Rect {
        x,
        y,
        width: pop_w,
        height: popup_h,
    };

    f.render_widget(Clear, popup);
    let para = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_style(Style::default().fg(theme.border_accent)),
        )
        .scroll((scroll_y, 0));
    f.render_widget(para, popup);
}

/// Renders a transient one-line status near the bottom of the screen.
fn render_transient_status(f: &mut Frame, area: Rect, msg: &str) {
    let height = area.height.max(3);
    let y = height.saturating_sub(3);
    let rect = Rect::new(area.x, area.y + y, area.width, 1);
    f.render_widget(Clear, rect);
    let theme = theme::active();
    let line = Line::from(vec![
        Span::styled("ⓘ ", Style::default().fg(theme.accent)),
        Span::styled(msg.to_string(), Style::default().fg(theme.muted)),
    ]);
    f.render_widget(Paragraph::new(line), rect);
}

/// Builds the log-area title as a `(left, right)` pair rendered on the same
/// border row: a breadcrumb of current activity on the left, and a compact
/// position/size summary on the right — mirroring a single-row terminal
/// header rather than a separate content line.
fn build_title(
    state: &TuiState,
    n: usize,
    error_count: usize,
) -> (Vec<Span<'static>>, Vec<Span<'static>>) {
    let mut left = vec![Span::styled(
        " vel auto ",
        Style::default().add_modifier(Modifier::BOLD),
    )];
    if let Some(task) = current_task_label(state) {
        left.push(Span::styled(
            format!("› {task} "),
            theme::active().text_style(),
        ));
    }
    left.push(Span::styled(
        format!("› {} ", state.spinner_verb),
        Style::default().fg(theme::active().muted),
    ));

    let scroll_label = match state.scroll {
        ScrollState::Tail => "live",
        ScrollState::Anchored { .. } => "history",
    };
    let kib = state.transcript.retained_bytes() / 1024;
    let mut s = format!("{n} events · {kib} KiB");
    if error_count > 0 {
        s += &format!(" · {error_count} err");
    }
    let omitted = state.transcript.omitted();
    if !omitted.is_empty() {
        s += &format!(" · {} trimmed", omitted.entries);
    }
    s += &format!(" · {scroll_label} ");
    let right_color = if error_count > 0 {
        theme::active().error
    } else {
        theme::active().muted
    };
    let right = vec![Span::styled(s, Style::default().fg(right_color))];
    (left, right)
}

/// Max lines of todo content shown in the sticky panel's default (collapsed)
/// preview before truncating with a "N more (Ctrl+T: Expand)" marker — keeps
/// the panel small most of the time even when the board is long.
const MAX_STICKY_TODO_LINES_COLLAPSED: usize = 6;
/// Max lines shown once the panel is expanded (`Ctrl+T`) — generous enough to
/// read a real board, still bounded so a huge one can't consume the whole
/// terminal (the log area keeps its own guaranteed minimum regardless).
const MAX_STICKY_TODO_LINES_EXPANDED: usize = 30;

/// Row height (including the top/bottom border) for the sticky todo panel:
/// `0` collapses it entirely when there's nothing to show. Bounded so it can
/// never crowd out the log area's guaranteed minimum or the spinner/hints
/// rows on a short terminal.
fn sticky_todo_height(todo: Option<&str>, terminal_height: u16, expanded: bool) -> u16 {
    let Some(todo) = todo else {
        return 0;
    };
    if todo.trim().is_empty() {
        return 0;
    }
    let cap = if expanded {
        MAX_STICKY_TODO_LINES_EXPANDED
    } else {
        MAX_STICKY_TODO_LINES_COLLAPSED
    };
    let content_lines = todo.lines().count().clamp(1, cap);
    let desired = (content_lines + 2) as u16; // + top/bottom border
    let reserved_for_log_and_footer = 3 + 1 + 1; // log's Constraint::Min(3) + spinner + hints
    let budget = terminal_height.saturating_sub(reserved_for_log_and_footer);
    desired.min(budget)
}

/// Renders the sticky todo panel: a small bordered block pinned above the
/// spinner/hints rows so the current task list stays visible regardless of
/// scroll position. Checklist-style lines (`[x]`/`[~]`/`[ ]`, as produced by
/// [`todo_summary_from_input`]) are colour-coded by status; free-text board
/// summaries (as reported directly by some providers) render plain. `expanded`
/// (toggled by `Ctrl+T`) mirrors the taller area [`sticky_todo_height`]
/// already reserved when true, so the truncation marker only offers the
/// expand hint when expanding would actually reveal more.
fn render_sticky_todo(f: &mut Frame, area: Rect, todo: &str, expanded: bool) {
    let visible_budget = area.height.saturating_sub(2) as usize;
    if visible_budget == 0 {
        return;
    }
    let total_lines = todo.lines().count();
    let truncated = total_lines > visible_budget;
    let take = if truncated {
        visible_budget.saturating_sub(1).max(1)
    } else {
        visible_budget
    };
    let mut lines: Vec<Line<'static>> = todo.lines().take(take).map(todo_line_span).collect();
    if truncated {
        let hidden = total_lines - lines.len();
        // Expanded but still truncated means the panel is genuinely
        // height-limited (a very short terminal) — Ctrl+T has nothing left
        // to reveal, so don't claim it does.
        let marker = if expanded {
            format!("… {hidden} more")
        } else {
            format!("… {hidden} more (Ctrl+T: Expand)")
        };
        lines.push(Line::from(Span::styled(
            marker,
            Style::default().fg(theme::active().dim),
        )));
    }
    let title_text = if expanded {
        " Todos (Ctrl+T: Collapse) "
    } else {
        " Todos "
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::active().border))
        .title(Line::from(Span::styled(
            title_text,
            Style::default().add_modifier(Modifier::BOLD),
        )));
    f.render_widget(Paragraph::new(lines).block(block), area);
}

/// Colours one line of sticky todo content by its checklist marker, if any.
fn todo_line_span(line: &str) -> Line<'static> {
    let theme = theme::active();
    let style = match line.trim_start() {
        s if s.starts_with("[x]") => Style::default().fg(theme.success),
        s if s.starts_with("[~]") => Style::default().fg(theme.warning),
        s if s.starts_with("[ ]") => Style::default().fg(theme.muted),
        _ => theme.text_style(),
    };
    Line::from(Span::styled(line.to_string(), style))
}

fn render_spinner(f: &mut Frame, state: &TuiState, area: Rect) {
    let theme = theme::active();
    let spinner = SPINNER[state.spinner_idx];
    let cached_pct = (state.cached_tokens * 100)
        .checked_div(state.input_tokens)
        .unwrap_or(0);
    let mut spans = vec![
        Span::styled(format!("{spinner} "), Style::default().fg(theme.accent)),
        Span::styled(
            state.spinner_verb,
            Style::default().fg(theme.dim).add_modifier(Modifier::DIM),
        ),
        Span::raw("…  "),
        Span::styled(
            format!(
                "↑ {} ↓ {} · {}% cached",
                fmt_tokens(state.input_tokens),
                fmt_tokens(state.output_tokens),
                cached_pct
            ),
            Style::default().fg(theme.dim),
        ),
    ];
    if let Some((current, total)) = state.iteration {
        spans.push(Span::raw("  ·  "));
        spans.push(Span::styled(
            format!("iter {current}/{total}"),
            Style::default().fg(theme.muted),
        ));
    }
    // "Viewing iteration" — distinct from the running iteration above. Shown
    // only when the reader has scrolled into a different iteration than the
    // one currently running, so they can tell e.g. they are reading iteration
    // 2 while the agent works on iteration 3.
    if let Some(viewing) = state.viewing_iteration_number() {
        let running = state.iteration.map(|(c, _)| c);
        if running.is_some_and(|r| r != viewing) {
            spans.push(Span::raw("  ·  "));
            spans.push(Span::styled(
                format!("viewing iter {viewing}"),
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ));
        }
    }
    // Persistent-append indicator (set by the controller via SetPersistentAppend).
    if let Some(append) = &state.persistent_append {
        let preview: String = append.as_str().chars().take(20).collect();
        spans.push(Span::raw("  ·  "));
        spans.push(Span::styled(
            format!("append: {preview}"),
            Style::default().fg(theme.success),
        ));
    }
    // Live-steering availability, shown only when relevant.
    if !matches!(state.live_steering_status, LiveSteeringStatus::Unsupported) {
        let (label, color) = match state.live_steering_status {
            LiveSteeringStatus::Ready => ("steer: ready", theme.success),
            LiveSteeringStatus::Inactive => ("steer: idle", theme.dim),
            LiveSteeringStatus::Closing => ("steer: closing", theme.warning),
            LiveSteeringStatus::Unsupported => ("", theme.dim),
        };
        spans.push(Span::raw("  ·  "));
        spans.push(Span::styled(label, Style::default().fg(color)));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

#[allow(clippy::too_many_arguments)]
fn render_hints(
    f: &mut Frame,
    state: &TuiState,
    cancel_handler: &crate::cancellation::CancellationHandler,
    error_count: usize,
    area: Rect,
) {
    let key = |k: &str| {
        Span::styled(
            k.to_string(),
            Style::default()
                .fg(theme::active().accent)
                .add_modifier(Modifier::BOLD),
        )
    };
    let hints: Vec<Span> = if state.show_prompt {
        vec![
            key("p/Esc"),
            Span::raw(" close  "),
            key("↑↓"),
            Span::raw(" scroll"),
        ]
    } else if state.show_errors {
        vec![
            key("e/Esc"),
            Span::raw(" close  "),
            key("↑↓"),
            Span::raw(" scroll"),
        ]
    } else {
        let (label, color) = if error_count > 0 {
            (format!(" errors:{} ", error_count), theme::active().error)
        } else {
            (" errors ".to_string(), theme::active().dim)
        };
        vec![
            key("p"),
            Span::raw(" prompt  "),
            key("t"),
            Span::raw(if state.show_thinking {
                " thinking✓  "
            } else {
                " thinking✗  "
            }),
            key("o"),
            Span::raw(if state.show_tool_output {
                " output✓  "
            } else {
                " output✗  "
            }),
            key("e"),
            Span::styled(label, Style::default().fg(color)),
            key("s"),
            Span::raw(if cancel_handler.stop_after_iteration_requested() {
                " stop-after✓  "
            } else {
                " stop-after✗  "
            }),
            key("l"),
            Span::raw(" log  "),
            key("c"),
            Span::raw(" steer  "),
            key("a"),
            Span::raw(" append  "),
            key("y/Y"),
            Span::raw(" copy  "),
            key("?"),
            Span::raw(" help  "),
            key("m"),
            Span::raw(" model  "),
            key("↑↓"),
            Span::raw(" scroll  "),
            key("^C×2"),
            Span::raw(" stop"),
        ]
    };
    let hints_para =
        Paragraph::new(Line::from(hints)).style(Style::default().add_modifier(Modifier::DIM));
    f.render_widget(hints_para, area);
}

fn fmt_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{}k", n / 1_000)
    } else {
        n.to_string()
    }
}

// ── Per-entry rendering (content lines, pre-wrap) ────────────────────────────

/// The [`tui_markdown::StyleSheet`] used for the agent's own prose (assistant
/// text and thinking): identical to the crate's stock theme (already used for
/// the prompt modal) except inline code, recoloured to the active theme's
/// `mdCode` with no background — a symbol the agent mentions in prose should
/// read like the same symbol in a diff, not sit in its own box.
#[derive(Debug, Clone, Copy)]
struct ProseStyleSheet;

impl tui_markdown::StyleSheet for ProseStyleSheet {
    fn heading(&self, level: u8) -> Style {
        tui_markdown::DefaultStyleSheet.heading(level)
    }
    fn code(&self) -> Style {
        Style::default().fg(theme::active().md_code)
    }
    fn link(&self) -> Style {
        tui_markdown::DefaultStyleSheet.link()
    }
    fn blockquote(&self) -> Style {
        tui_markdown::DefaultStyleSheet.blockquote()
    }
    fn heading_meta(&self) -> Style {
        tui_markdown::DefaultStyleSheet.heading_meta()
    }
    fn metadata_block(&self) -> Style {
        tui_markdown::DefaultStyleSheet.metadata_block()
    }
}

/// Renders the agent's own prose (assistant text or thinking) as Markdown —
/// reusing the same `tui_markdown` parser the prompt modal already renders
/// with — patched onto `base_style` so plain text keeps the entry's usual
/// colour/italics while inline code, bold, headings, etc. layer their own
/// styling on top (`Style::patch` fills fg/bg from the Markdown span only
/// where it sets one, and unions modifiers, so Thinking's italic survives
/// under a bold or code span rather than being replaced by it).
fn render_prose(text: &str, base_style: Style) -> Vec<Line<'static>> {
    let options = tui_markdown::Options::new(ProseStyleSheet);
    let rendered = tui_markdown::from_str_with_options(text, &options);
    rendered
        .lines
        .into_iter()
        .map(|line| {
            Line::from(
                line.spans
                    .into_iter()
                    .map(|s| Span::styled(s.content.into_owned(), base_style.patch(s.style)))
                    .collect::<Vec<_>>(),
            )
        })
        .collect()
}

/// Whether `tool` is a first-class file-edit tool (Edit/Write/MultiEdit/
/// NotebookEdit) whose real content arrives as a separate [`EntryKind::FileEdit`]
/// card. These stay a lightweight, unboxed label rather than getting the
/// terminal-card treatment, so the diff isn't sandwiched inside an unrelated
/// box.
fn is_file_edit_tool(tool: &str) -> bool {
    matches!(tool, "Edit" | "Write" | "MultiEdit" | "NotebookEdit")
}

/// Whether `tool` is a todo/task-list tool, across the provider-specific
/// names seen in practice (Claude's `TodoWrite`/`TodoRead`, omp's lowercase
/// `todo`). Drives the sticky todo panel, which is populated regardless of
/// which provider produced the event.
fn is_todo_tool(tool: &str) -> bool {
    let t = tool.to_ascii_lowercase();
    t == "todo" || t.contains("todowrite") || t.contains("todoread")
}

/// Builds a checklist summary from a structured todo-tool call input, e.g.
/// Claude's `TodoWrite` shape `{"todos": [{"content", "status", ...}]}`.
/// Returns `None` when the input doesn't carry a non-empty `todos` array —
/// providers that report state via the result instead (see
/// [`is_substantial_todo_summary`]) simply don't match here.
fn todo_summary_from_input(input: &serde_json::Value) -> Option<String> {
    let todos = input.get("todos")?.as_array()?;
    if todos.is_empty() {
        return None;
    }
    let mut out = String::new();
    for t in todos {
        let content = t.get("content").and_then(|v| v.as_str()).unwrap_or("?");
        let status = t
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("pending");
        let mark = match status {
            "completed" => "x",
            "in_progress" => "~",
            _ => " ",
        };
        out.push_str(&format!("[{mark}] {content}\n"));
    }
    Some(out.trim_end().to_string())
}

/// Whether a todo tool's *result* text looks like a real board summary
/// (multi-line, or long enough to be more than a one-line acknowledgement)
/// rather than a generic "todos updated" confirmation that would otherwise
/// clobber a better summary already sourced from the call's structured input.
fn is_substantial_todo_summary(detail: &str) -> bool {
    let detail = detail.trim();
    !detail.is_empty() && (detail.lines().count() > 1 || detail.len() > 60)
}

/// Max chars of the current task shown in the block/window titles before
/// eliding — those have limited real estate, so a long todo line would
/// otherwise crowd out the spinner verb and status summary next to it.
const TITLE_TASK_MAX_CHARS: usize = 48;

/// Extracts the in-progress item (`[~] ...`) from the sticky todo board, if
/// any, as a short label for the block title and terminal window title. This
/// is what turns "vel auto › thinking" into "vel auto › {task} › thinking" —
/// otherwise the title only ever says what kind of step is running, never
/// what it's actually for. Returns `None` before the first todo update, or
/// once every item is done/pending with nothing `in_progress`.
fn current_task_label(state: &TuiState) -> Option<String> {
    let todo = state.sticky_todo.as_deref()?;
    let line = todo
        .lines()
        .find_map(|l| l.trim_start().strip_prefix("[~]"))?;
    let task = line.trim();
    if task.is_empty() {
        return None;
    }
    Some(truncate_str(task, TITLE_TASK_MAX_CHARS))
}

/// Infers a [`velor_core::file_edit::SyntaxKind`] for a tool result's output
/// body from the correlated call, when there's enough signal to be confident:
/// a Read call's `command` *is* the file path, so its extension is used
/// directly; a Bash `cat`/`bat <path>` invocation is unwrapped the same way.
/// Everything else (arbitrary shell output, `git diff`, JSON blobs, …)
/// returns `None` — guessing wrong would mis-colour output, which reads worse
/// than the plain text it replaces.
fn infer_output_syntax(
    tool: &str,
    command: Option<&str>,
) -> Option<velor_core::file_edit::SyntaxKind> {
    let command = command?;
    match tool.to_ascii_lowercase().as_str() {
        "read" => Some(velor_core::file_edit::infer_syntax(
            strip_line_range_suffix(command),
        )),
        "bash" | "command_execution" => {
            let trimmed = command.trim();
            ["cat ", "bat "].iter().find_map(|prefix| {
                trimmed.strip_prefix(prefix).and_then(|rest| {
                    rest.split_whitespace()
                        .find(|w| !w.starts_with('-'))
                        .map(|path| {
                            velor_core::file_edit::infer_syntax(strip_line_range_suffix(path))
                        })
                })
            })
        }
        _ => None,
    }
}

/// Strips a trailing `:N` or `:N-M` line-range suffix some providers append
/// to a Read call's path (e.g. `lib.rs:2728-2871`), which would otherwise
/// defeat extension-based language detection — `infer_syntax` would see the
/// "extension" `rs:2728-2871` and fall back to plain text. Only strips when
/// the entire suffix after the last `:` is digits/hyphens, so this is a
/// no-op for plain paths and doesn't misfire on a Windows drive letter.
fn strip_line_range_suffix(path: &str) -> &str {
    let Some(colon) = path.rfind(':') else {
        return path;
    };
    let suffix = &path[colon + 1..];
    let looks_like_range = !suffix.is_empty()
        && suffix
            .split('-')
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()));
    if looks_like_range {
        &path[..colon]
    } else {
        path
    }
}

/// Syntax-highlights one already-truncated output line in isolation (no
/// surrounding-file state — output bodies don't carry one), falling back to
/// `default_fg` for any span the highlighter doesn't classify (plain text,
/// punctuation, …) so unclassified text still matches the card's palette
/// instead of the terminal's raw default foreground.
fn highlight_plain_line(
    line: &str,
    engine: &mut HighlightEngine,
    syntax: velor_core::file_edit::SyntaxKind,
    default_fg: Color,
) -> Vec<Span<'static>> {
    let spans = engine.highlight(&HighlightRequest::full(syntax, line));
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut cursor = 0usize;
    let styled = |slice: &str, style: Style| Span::styled(slice.to_string(), style);
    let default_style = Style::default().fg(default_fg);
    for s in &spans {
        if s.start > cursor
            && let Some(slice) = line.get(cursor..s.start)
            && !slice.is_empty()
        {
            out.push(styled(slice, default_style));
        }
        if let Some(slice) = line.get(s.start..s.end)
            && !slice.is_empty()
        {
            let mut style = token_style(s.token);
            if style.fg.is_none() {
                style = style.fg(default_fg);
            }
            out.push(styled(slice, style));
        }
        cursor = s.end;
    }
    if cursor < line.len()
        && let Some(slice) = line.get(cursor..)
        && !slice.is_empty()
    {
        out.push(styled(slice, default_style));
    }
    if out.is_empty() && !line.is_empty() {
        out.push(styled(line, default_style));
    }
    out
}

/// The top or bottom rule of a tool card's box, optionally with an embedded
/// label (the command/target, on the opening rule). Filled with `bg` so the
/// rule reads as part of the same solid card as the content rows it
/// opens/closes, stroked in the active theme's `border`. Degrades to a bare
/// label when the width can't fit the corners + label, mirroring
/// [`render_iteration_divider`]'s narrow-width handling.
fn tool_card_rule(left: char, right: char, label: &str, width: usize, bg: Color) -> Line<'static> {
    let border = theme::active().border;
    let w = width.max(2);
    let rule_style = Style::default().fg(border).bg(bg);
    if label.is_empty() {
        let dashes = w.saturating_sub(2);
        return Line::from(Span::styled(
            format!("{left}{}{right}", "─".repeat(dashes)),
            rule_style,
        ));
    }
    let label_style = theme::active().text_style().bg(bg);
    let label_w = UnicodeWidthStr::width(label);
    // "{left}─ " (3) + label + " " (1) + dashes + "{right}" (1).
    let fixed_w = 5 + label_w;
    if w <= fixed_w {
        return Line::from(vec![Span::styled(label.to_string(), label_style)]);
    }
    let dashes = w - fixed_w;
    Line::from(vec![
        Span::styled(format!("{left}─ "), rule_style),
        Span::styled(label.to_string(), label_style),
        Span::styled(" ", Style::default().bg(bg)),
        Span::styled("─".repeat(dashes), rule_style),
        Span::styled(right.to_string(), rule_style),
    ])
}

/// One bordered content row inside a tool card: `"│ " + spans (padded) + " │"`,
/// filled edge-to-edge with `bg`.
fn tool_card_row(mut spans: Vec<Span<'static>>, width: usize, bg: Color) -> Line<'static> {
    let border_color = theme::active().border;
    let border = || Span::styled("│ ", Style::default().fg(border_color).bg(bg));
    let inner_w = width.saturating_sub(4); // "│ " + " │"
    let content_w: usize = spans
        .iter()
        .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
        .sum();
    for s in &mut spans {
        s.style = s.style.bg(bg);
    }
    let mut out = vec![border()];
    out.extend(spans);
    let pad = inner_w.saturating_sub(content_w);
    if pad > 0 {
        out.push(Span::styled(" ".repeat(pad), Style::default().bg(bg)));
    }
    out.push(Span::styled(" │", Style::default().fg(border_color).bg(bg)));
    Line::from(out)
}

fn render_entry(
    kind: &EntryKind,
    width: usize,
    engine: &mut HighlightEngine,
    opts: RenderOpts,
) -> Vec<Line<'static>> {
    match kind {
        EntryKind::Text(text) => {
            let mut lines = render_prose(text, theme::active().text_style());
            // Trailing blank row gives paragraphs breathing room, matching the
            // document-like spacing of a prose-first transcript. Safe for the
            // viewport/cache row accounting: the blank row belongs to this
            // entry, so it's counted like any other row.
            lines.push(Line::default());
            lines
        }

        EntryKind::Thinking(text) => render_prose(
            text,
            Style::default()
                .fg(theme::active().thinking_text)
                .add_modifier(Modifier::ITALIC),
        ),

        EntryKind::Usage { .. } => Vec::new(),

        EntryKind::ToolCall { tool, detail, .. } => {
            // Edit-tool calls stay a plain, unboxed announcement — the real
            // content is a separate FileEdit card, so this is the only place
            // the path/summary shows before it lands. Every other tool's call
            // renders nothing: its result opens a self-contained box (see the
            // ToolResult arm) with the correlated command as that box's own
            // header, so printing the same command again here would just be
            // the same line twice.
            if is_file_edit_tool(tool) {
                vec![Line::from(vec![
                    Span::styled(tool.clone(), Style::default().add_modifier(Modifier::BOLD)),
                    Span::raw(format!("  {detail}")),
                ])]
            } else {
                Vec::new()
            }
        }

        EntryKind::ToolResult {
            tool,
            detail,
            success,
            command,
        } => {
            let failed = *success == Some(false);
            if is_file_edit_tool(tool) {
                let color = if failed {
                    theme::active().error
                } else {
                    theme::active().dim
                };
                let text = detail.lines().next().unwrap_or("(no output)");
                return vec![Line::from(Span::styled(
                    truncate_str(text, 200),
                    Style::default().fg(color),
                ))];
            }
            // Fully self-contained box — never relies on a neighbouring
            // ToolCall to open it, so it renders correctly regardless of how
            // many other calls/results are interleaved around it in the
            // stream. The opening rule embeds the *command*, not just the
            // tool name, when the provider let us correlate the two (see
            // `AgentEvent::ToolResult::command`) — that's the box's header,
            // matching what a real embedded terminal would show.
            let theme = theme::active();
            let card_bg = if failed {
                theme.tool_error_bg
            } else {
                theme.tool_success_bg
            };
            let label = match command.as_deref() {
                Some(cmd) if tool == "Bash" => format!("$ {cmd}"),
                Some(cmd) => format!("{tool}  {cmd}"),
                None => tool.clone(),
            };
            let mut lines = vec![tool_card_rule('╭', '╮', &label, width, card_bg)];
            let body_fg = if failed {
                theme.error
            } else {
                theme.tool_output
            };
            let mut divider_spans = vec![
                Span::styled("── ", Style::default().fg(theme.dim)),
                Span::styled("Output", theme.text_style().add_modifier(Modifier::BOLD)),
            ];
            if failed {
                divider_spans.push(Span::styled(" (failed)", Style::default().fg(theme.error)));
            }
            lines.push(tool_card_row(divider_spans, width, card_bg));
            // A failure's plain red already carries the signal that matters;
            // syntax colour would just compete with it, so only highlight on
            // success. `infer_output_syntax` returns `None` for anything it
            // can't confidently place a language on (most Bash output), which
            // keeps that case exactly as before: plain body_fg text.
            let syntax = if failed {
                None
            } else {
                infer_output_syntax(tool, command.as_deref())
            };
            // A markdown file's content renders as markdown (headings, bold,
            // inline code, …) rather than as a wall of `**`/`` ` `` — the same
            // renderer the agent's own prose uses, just over the tool output.
            let is_markdown = syntax == Some(velor_core::file_edit::SyntaxKind::Markdown);
            let body_lines: Vec<Line<'static>> = if is_markdown {
                render_prose(detail, Style::default().fg(body_fg))
            } else {
                // Expanded mode also relaxes the per-line truncation — a long
                // line cut at 200 chars would still look truncated even with
                // every line present. wrap_spans_indented handles the actual
                // terminal wrapping either way.
                let per_line_cap = if opts.expand { 4000 } else { 200 };
                detail
                    .lines()
                    .map(|line| {
                        let truncated = truncate_str(line, per_line_cap);
                        let spans = match syntax {
                            Some(syn) => highlight_plain_line(&truncated, engine, syn, body_fg),
                            None => vec![Span::styled(truncated, Style::default().fg(body_fg))],
                        };
                        Line::from(spans)
                    })
                    .collect()
            };
            // Collapsed by default at a small preview size regardless of how
            // generous `max_entry_lines` is configured — that limit is a hard
            // safety cap (still applied even expanded, so one pathological
            // output can't freeze rendering), not a "nice preview" size.
            let collapsed_cap = COLLAPSED_PREVIEW_LINES.min(opts.max_entry_lines);
            let cap = if opts.expand {
                opts.max_entry_lines
            } else {
                collapsed_cap
            };
            if body_lines.is_empty() {
                lines.push(tool_card_row(
                    vec![Span::styled(
                        "(no output)".to_string(),
                        Style::default().fg(theme.dim),
                    )],
                    width,
                    card_bg,
                ));
            } else if body_lines.len() <= cap {
                for line in body_lines {
                    lines.push(tool_card_row(line.spans, width, card_bg));
                }
            } else {
                // Head + tail, with the hidden middle both counted and
                // recoverable: collapsed, via Ctrl+O; expanded-but-still-over
                // the hard cap, only via the run log (a real safety limit,
                // not a preview — Ctrl+O has nothing further to reveal).
                let keep = (cap.max(2) / 2).max(1);
                let omitted = body_lines.len() - 2 * keep;
                let marker = if opts.expand {
                    format!("… {omitted} more lines — full text is in the run log")
                } else {
                    format!("… {omitted} more lines (Ctrl+O: Expand)")
                };
                for line in &body_lines[..keep] {
                    lines.push(tool_card_row(line.spans.clone(), width, card_bg));
                }
                lines.push(tool_card_row(
                    vec![Span::styled(marker, Style::default().fg(theme.dim))],
                    width,
                    card_bg,
                ));
                for line in &body_lines[body_lines.len() - keep..] {
                    lines.push(tool_card_row(line.spans.clone(), width, card_bg));
                }
            }
            lines.push(tool_card_rule('╰', '╯', "", width, card_bg));
            lines.push(Line::default());
            lines
        }

        EntryKind::Error(msg) => msg
            .lines()
            .map(|l| {
                Line::from(Span::styled(
                    l.to_string(),
                    Style::default().fg(theme::active().error),
                ))
            })
            .collect(),

        EntryKind::Info(msg) => msg
            .lines()
            .map(|l| {
                Line::from(Span::styled(
                    l.to_string(),
                    Style::default().fg(theme::active().accent),
                ))
            })
            .collect(),

        EntryKind::Warning(msg) => msg
            .lines()
            .map(|l| {
                Line::from(Span::styled(
                    l.to_string(),
                    Style::default().fg(theme::active().warning),
                ))
            })
            .collect(),

        EntryKind::IterationDivider { number, maximum } => {
            vec![render_iteration_divider(*number, *maximum, width)]
        }

        EntryKind::FileEdit(edit) => render_file_edit(edit, width, engine),
    }
}

/// Renders a full-width separator marking the start of an iteration, with the
/// iteration label centred. The dashes fill the content width so the rule reads
/// as a single unbroken divider regardless of terminal size.
fn render_iteration_divider(number: u32, maximum: Option<u32>, width: usize) -> Line<'static> {
    let label = match maximum {
        Some(m) => format!(" Iteration {number} of {m} "),
        None => format!(" Iteration {number} "),
    };
    let label_w = UnicodeWidthStr::width(label.as_str());
    let w = width.max(1);
    let theme = theme::active();
    let rule_style = Style::default().fg(theme.border_muted);
    let label_style = Style::default()
        .fg(theme.muted)
        .add_modifier(Modifier::BOLD);
    if w <= label_w {
        // Too narrow for any rule; show just the label.
        return Line::from(vec![Span::styled(label, label_style)]);
    }
    let dashes = w - label_w;
    let left = dashes / 2;
    let right = dashes - left;
    Line::from(vec![
        Span::styled("─".repeat(left), rule_style),
        Span::styled(label, label_style),
        Span::styled("─".repeat(right), rule_style),
    ])
}

// ── File-edit rendering ─────────────────────────────────────────────────────

/// The display width of a file-edit line's gutter: the sign column, spacing,
/// the line number, and the separator. Shared by [`render_diff_line`] (which
/// lays it out) and [`entry_hang`] (which sizes the hanging indent so wrapped
/// rows align beneath the source).
fn gutter_width(num_width: usize) -> usize {
    num_width + 6
}

/// The hanging-indent width for an entry's wrapped rows, or `0` for entries
/// with no gutter/border. A `FileEdit`'s body carries the card border plus
/// its gutter; a boxed `ToolResult`'s body carries just the border — both
/// sourced from the same constants their own renderers use, so the indent
/// never drifts from what's actually on screen.
fn entry_hang(kind: &EntryKind) -> usize {
    match kind {
        EntryKind::FileEdit(edit) => gutter_width(line_number_width(edit)),
        EntryKind::ToolResult { tool, .. } if !is_file_edit_tool(tool) => TOOL_CARD_ROW_HANG,
        _ => 0,
    }
}

/// Width of a tool card's left border ("│ "), for aligning wrapped
/// continuation rows of an over-long body line beneath the content rather
/// than the border.
const TOOL_CARD_ROW_HANG: usize = 2;

/// Widest line-number digit count across the edit's hunks (minimum 1).
fn line_number_width(edit: &FileEdit) -> usize {
    let max = edit
        .hunks
        .iter()
        .flat_map(|h| h.lines.iter())
        .flat_map(|l| l.old_no.into_iter().chain(l.new_no))
        .map(|n| n.to_string().len())
        .max()
        .unwrap_or(1);
    max.max(1)
}

/// Renders a structured [`FileEdit`] to logical (not yet wrapped) lines: a
/// header carrying the path and kind, then — for text edits — gutter-prefixed
/// diff lines with syntax highlighting and diff styling. Binary and
/// capture-failed edits render a concise note instead of hunks. Wrapping +
/// hanging-indent alignment is applied later in [`build_layout`].
fn render_file_edit(
    edit: &FileEdit,
    _width: usize,
    engine: &mut HighlightEngine,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();

    // Header: "{Verb}: {lang} {elided path}  {suffix or +N/-M stat}". Plain
    // text, no card border — the gutter below already sets the block apart.
    let (color, suffix) = header_style(&edit.kind);
    let mut header = vec![Span::styled(
        format!("{}: ", edit_verb(&edit.kind)),
        Style::default().add_modifier(Modifier::BOLD),
    )];
    let lang = edit.syntax.syntect_hint();
    if !lang.is_empty() {
        header.push(Span::styled(
            format!("{lang} "),
            Style::default().fg(theme::active().dim),
        ));
    }
    header.push(Span::styled(
        elide_middle(&edit.path, 64),
        Style::default().fg(color),
    ));
    match suffix {
        Some(s) => header.push(Span::styled(
            format!("  {s}"),
            Style::default().fg(theme::active().dim),
        )),
        None => {
            let (adds, dels) = diff_stat(edit);
            if adds + dels > 0 {
                header.push(Span::styled(
                    format!("  (+{adds}/-{dels})"),
                    Style::default().fg(theme::active().dim),
                ));
            }
        }
    }
    lines.push(Line::from(header));

    match &edit.kind {
        FileEditKind::CaptureFailed { reason } => {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    format!("⚠ could not capture edit: {reason}"),
                    Style::default().fg(theme::active().warning),
                ),
            ]));
        }
        FileEditKind::Binary => {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    "binary file changed (contents not shown)",
                    Style::default()
                        .fg(theme::active().dim)
                        .add_modifier(Modifier::ITALIC),
                ),
            ]));
        }
        FileEditKind::Modified | FileEditKind::Created | FileEditKind::Deleted => {
            let num_width = line_number_width(edit);
            // Highlighting strategy:
            // - Composite languages (Svelte) need whole-file embedded-language
            //   state (a `<script lang="ts">` tag far above a hunk establishes
            //   its mode). We highlight the full source once and clip each diff
            //   line's spans to its byte range — so isolated hunks still get TS
            //   / CSS classifications they'd otherwise lose.
            // - Plain languages also benefit from stateful highlighting across
            //   the full source (block comments, raw strings, embedded JS in
            //   HTML), so we use the same path when full source is available.
            // - When no full source is carried (plain-language modifications
            //   where only hunks were captured), highlight per-line from the
            //   hunk text — line-oriented grammars still classify well enough.
            let highlighted = HighlightedEdit::build(engine, edit);
            for (i, hunk) in edit.hunks.iter().enumerate() {
                if i > 0
                    && let Some(gap) = inter_hunk_gap(edit.hunks.get(i - 1), hunk)
                {
                    lines.push(dim_line(format!("⋯ {gap} lines ⋯")));
                }
                lines.extend(render_hunk_lines(
                    &hunk.lines,
                    num_width,
                    &highlighted,
                    engine,
                ));
            }
            if edit.omitted_lines > 0 {
                // Unlike a tool result's output, a diff this large was
                // already bounded (head + tail kept) when the edit was
                // captured — the omitted middle never reached the TUI at
                // all, so there's nothing `Ctrl+O` could reveal here.
                lines.push(dim_line(format!(
                    "… {} more lines — full diff is in the run log",
                    edit.omitted_lines
                )));
            }
        }
    }

    lines.push(Line::default());
    lines
}

/// The verb a file-edit header reads with, matching the tool that produced it
/// (`Write` for a brand-new file, `Delete` for a removal, `Edit` otherwise).
const fn edit_verb(kind: &FileEditKind) -> &'static str {
    match kind {
        FileEditKind::Created => "Write",
        FileEditKind::Deleted => "Delete",
        FileEditKind::Modified | FileEditKind::Binary | FileEditKind::CaptureFailed { .. } => {
            "Edit"
        }
    }
}

/// Total added/removed diff lines across all hunks, for the header's
/// `(+N/-M)` stat.
fn diff_stat(edit: &FileEdit) -> (usize, usize) {
    let mut adds = 0usize;
    let mut dels = 0usize;
    for l in edit.hunks.iter().flat_map(|h| h.lines.iter()) {
        match l.kind {
            LineKind::Addition => adds += 1,
            LineKind::Removal => dels += 1,
            LineKind::Context => {}
        }
    }
    (adds, dels)
}

/// Shortens `s` to at most `max` characters by ellipsis-eliding the middle,
/// keeping the start and end (usually the most identifying parts of a path).
fn elide_middle(s: &str, max: usize) -> String {
    let len = s.chars().count();
    if len <= max || max < 5 {
        return s.to_string();
    }
    let keep = (max - 1) / 2;
    let chars: Vec<char> = s.chars().collect();
    let head: String = chars[..keep].iter().collect();
    let tail: String = chars[chars.len() - (max - 1 - keep)..].iter().collect();
    format!("{head}…{tail}")
}

/// `(path colour, optional status suffix)` for the file-edit header.
fn header_style(kind: &FileEditKind) -> (Color, Option<&'static str>) {
    let theme = theme::active();
    match kind {
        FileEditKind::Created => (theme.diff_added, Some("new file")),
        FileEditKind::Deleted => (theme.diff_removed, Some("deleted")),
        FileEditKind::Binary => (theme.muted, Some("binary")),
        FileEditKind::CaptureFailed { .. } => (theme.warning, Some("capture failed")),
        FileEditKind::Modified => (theme.accent, None),
    }
}

/// Pre-highlighted source for one file edit, used to render every diff line in
/// that edit from a single whole-file parse (so embedded-language state — a
/// `<script lang="ts">` tag above a hunk — is established before clipping).
///
/// Two modes:
/// - **Full-source** (composite languages, or when the adapter carried
///   `full_new_source`): the whole file was highlighted once; each diff line
///   slices its byte range out of that result. State-correct for Svelte etc.
/// - **Per-line fallback** (plain-language modifications with no carried
///   source): each line is highlighted in isolation. Line-oriented grammars
///   (Rust, Python, …) still classify well; only multi-line constructs (block
///   comments, raw strings) lose state, which is the pre-existing behaviour.
enum HighlightedEdit {
    /// Whole-file spans over `source`. Diff lines slice by byte range.
    Full {
        source: String,
        spans: Vec<SemanticSpan>,
        kind: velor_core::file_edit::SyntaxKind,
    },
    /// No carried source; highlight each line independently.
    PerLine {
        kind: velor_core::file_edit::SyntaxKind,
    },
}

impl HighlightedEdit {
    /// Highlights the edit's source once. Composite languages always use the
    /// full-source path (their `full_new_source` is populated by the adapter);
    /// plain languages use it too when available, else fall back to per-line.
    fn build(engine: &mut HighlightEngine, edit: &FileEdit) -> Self {
        if let Some(source) = edit.full_new_source.as_deref() {
            let spans = engine.highlight(&HighlightRequest::full(edit.syntax, source));
            Self::Full {
                source: source.to_string(),
                spans,
                kind: edit.syntax,
            }
        } else {
            Self::PerLine { kind: edit.syntax }
        }
    }

    /// The syntax kind (used by the per-line fallback path).
    const fn kind(&self) -> velor_core::file_edit::SyntaxKind {
        match self {
            Self::Full { kind, .. } | Self::PerLine { kind } => *kind,
        }
    }

    /// Returns the `(content, foreground_style)` spans for one diff line,
    /// expanding tabs to spaces for display. The caller applies the diff
    /// background tint independently (foreground only here).
    fn spans_for_line(
        &self,
        dl: &DiffLine,
        expanded: &str,
        engine: &mut HighlightEngine,
    ) -> Vec<(String, Style)> {
        match self {
            Self::Full { source, spans, .. } => {
                if let Some(range) = Self::line_range(source, dl) {
                    return Self::render_spans(spans, range, source, expanded);
                }
                // Line not found in carried source (e.g. a removal of a line
                // that no longer exists in the new file): fall back to an
                // isolated highlight of the line text so it still gets colour.
                Self::isolated_line(engine, self.kind(), expanded)
            }
            Self::PerLine { .. } => Self::isolated_line(engine, self.kind(), expanded),
        }
    }

    /// Locates a diff line's raw byte range in the full source by 1-based new
    /// line number. Returns `None` for removals whose old-only line isn't in the
    /// new source (caller falls back to isolated highlighting).
    fn line_range(source: &str, dl: &DiffLine) -> Option<std::ops::Range<usize>> {
        // Prefer the new-file line number (the carried source is the new side).
        let lineno = dl.new_no.or(dl.old_no)?; // 1-based
        if lineno == 0 {
            return None;
        }
        let mut start = 0usize;
        for (i, line) in source.split_inclusive('\n').enumerate() {
            let end = start + line.len();
            // split_inclusive keeps the terminator; line i is 0-based, lineno is 1-based.
            if i + 1 == lineno {
                // The diff line text has its terminator stripped; match the
                // body (terminator-free) range so spans line up with `expanded`.
                let body_end = start + strip_term(line).len();
                return Some(start..body_end);
            }
            start = end;
        }
        None
    }

    /// Slices the pre-highlighted `spans` to `range`, then re-segments against
    /// the tab-expanded display text so spans line up with the rendered columns.
    /// Returns `(content, foreground_style)` pairs.
    fn render_spans(
        spans: &[SemanticSpan],
        range: std::ops::Range<usize>,
        source: &str,
        expanded: &str,
    ) -> Vec<(String, Style)> {
        // Collect clipped spans localised to this line's body range.
        let local: Vec<(std::ops::Range<usize>, SemanticToken)> = spans
            .iter()
            .filter_map(|s| {
                if s.end <= range.start || s.start >= range.end {
                    return None;
                }
                let start = s.start.max(range.start) - range.start;
                let end = s.end.min(range.end) - range.start;
                if start >= end {
                    None
                } else {
                    Some((start..end, s.token))
                }
            })
            .collect();

        let raw_body = source.get(range).unwrap_or("");
        let mut out: Vec<(String, Style)> = Vec::new();
        let mut cursor = 0usize;
        for (lr, token) in local {
            // Fill any gap before this span (unhighlighted text) — expanded.
            if lr.start > cursor
                && let Some(slice) = raw_body.get(cursor..lr.start)
            {
                let e = slice.replace('\t', &" ".repeat(TAB_WIDTH));
                if !e.is_empty() {
                    out.push((e, Style::default()));
                }
            }
            if let Some(slice) = raw_body.get(lr.start..lr.end) {
                let e = slice.replace('\t', &" ".repeat(TAB_WIDTH));
                let style = token_style(token);
                if !e.is_empty() {
                    out.push((e, style));
                }
            }
            cursor = lr.end;
        }
        // Trailing unhighlighted tail.
        if cursor < raw_body.len()
            && let Some(slice) = raw_body.get(cursor..)
        {
            let e = slice.replace('\t', &" ".repeat(TAB_WIDTH));
            if !e.is_empty() {
                out.push((e, Style::default()));
            }
        }
        // If nothing was classified, emit the whole expanded line as one span.
        if out.is_empty() && !expanded.is_empty() {
            out.push((expanded.to_string(), Style::default()));
        }
        out
    }

    /// Fallback: highlight a single line in isolation. Used when no full source
    /// is available or a removal line isn't present in the new-file source.
    fn isolated_line(
        engine: &mut HighlightEngine,
        kind: velor_core::file_edit::SyntaxKind,
        line: &str,
    ) -> Vec<(String, Style)> {
        let spans = engine.highlight(&HighlightRequest::full(kind, line));
        let mut out: Vec<(String, Style)> = Vec::new();
        let mut cursor = 0usize;
        for s in &spans {
            if s.start > cursor
                && let Some(slice) = line.get(cursor..s.start)
            {
                let e = slice.replace('\t', &" ".repeat(TAB_WIDTH));
                if !e.is_empty() {
                    out.push((e, Style::default()));
                }
            }
            if let Some(slice) = line.get(s.start..s.end) {
                let e = slice.replace('\t', &" ".repeat(TAB_WIDTH));
                let style = token_style(s.token);
                if !e.is_empty() {
                    out.push((e, style));
                }
            }
            cursor = s.end;
        }
        if cursor < line.len()
            && let Some(slice) = line.get(cursor..)
        {
            let e = slice.replace('\t', &" ".repeat(TAB_WIDTH));
            if !e.is_empty() {
                out.push((e, Style::default()));
            }
        }
        if out.is_empty() && !line.is_empty() {
            out.push((line.to_string(), Style::default()));
        }
        out
    }
}

/// Strips a trailing line terminator (CR/LF) from a slice, mirroring how diff
/// lines store terminator-free source text.
fn strip_term(s: &str) -> &str {
    s.trim_end_matches(['\n', '\r'])
}

/// Renders one diff line: a stable gutter (sign + line number + separator)
/// followed by syntax-highlighted source.
///
/// **Composition contract** (the load-bearing fix): syntax highlighting owns the
/// **foreground only**; diff state owns the **background only**. The token style
/// from [`token_style`] is merged with the diff tint via [`Style::patch`], which
/// sets background without touching foreground — so syntax colours survive on
/// added/removed lines instead of being washed out by the green/red tint. The
/// gutter (sign + line number + separator) is styled independently.
/// Near-black foreground for the word-diff emphasis chip — fixed rather than
/// theme-derived, since it needs to stay legible against either chip colour
/// (the theme's own vivid `diff_added`/`diff_removed`) regardless of theme.
const WORD_DIFF_FG: Color = Color::Rgb(15, 15, 15);

fn render_diff_line(
    dl: &DiffLine,
    num_width: usize,
    highlighted: &HighlightedEdit,
    engine: &mut HighlightEngine,
    emphasis: &[std::ops::Range<usize>],
) -> Line<'static> {
    let theme = theme::active();
    let (sign, sign_color, tint, emphasis_bg) = match dl.kind {
        LineKind::Context => (" ", theme.diff_context, None, None),
        LineKind::Addition => (
            "+",
            theme.diff_added,
            Some(theme::Theme::dim_bg(theme.diff_added)),
            Some(theme.diff_added),
        ),
        LineKind::Removal => (
            "-",
            theme.diff_removed,
            Some(theme::Theme::dim_bg(theme.diff_removed)),
            Some(theme.diff_removed),
        ),
    };
    // Removals show the old number; additions/context show the new number.
    let lineno = dl.new_no.or(dl.old_no);
    let lineno_str = match lineno {
        Some(n) => format!("{n:>num_width$}"),
        None => " ".repeat(num_width),
    };

    let expanded = expand_tabs(&dl.text);
    let source_spans = highlighted.spans_for_line(dl, &expanded, engine);
    // Apply the diff tint as a *background* only: merge each span's foreground
    // token style with the tinted background. Style::patch keeps fg + modifiers
    // from the left side and only fills in bg from the right. Where an
    // intraline word diff flagged a sub-range as the actual changed tokens,
    // that sub-range gets the stronger emphasis chip instead of the tint.
    let mut source: Vec<Span<'static>> = Vec::new();
    let mut offset = 0usize;
    for (content, fg_style) in source_spans {
        let seg_len = content.len();
        if emphasis.is_empty() {
            let mut style = fg_style;
            if let Some(bg) = tint {
                style = style.bg(bg);
            }
            source.push(Span::styled(content, style));
        } else {
            for (emphasized, piece) in split_by_emphasis(&content, offset, emphasis) {
                let mut style = fg_style;
                if emphasized {
                    if let Some(bg) = emphasis_bg {
                        style = style.bg(bg).fg(WORD_DIFF_FG);
                    }
                } else if let Some(bg) = tint {
                    style = style.bg(bg);
                }
                source.push(Span::styled(piece, style));
            }
        }
        offset += seg_len;
    }

    let mut spans: Vec<Span<'static>> = vec![
        Span::raw(" "),
        Span::styled(
            sign.to_string(),
            Style::default().fg(sign_color).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(lineno_str, Style::default().fg(theme.dim)),
        Span::styled(" │ ".to_string(), Style::default().fg(theme.dim)),
    ];
    spans.extend(source);
    Line::from(spans)
}

/// Splits `content` (which occupies `[base, base + content.len())` in the full
/// line) into `(emphasized, piece)` runs against the sorted, non-overlapping
/// `emphasis` ranges. Adjacent bytes with the same emphasis flag stay in one
/// piece so runs of contiguous highlighted characters render as a single
/// coloured chip rather than one span per byte.
fn split_by_emphasis(
    content: &str,
    base: usize,
    emphasis: &[std::ops::Range<usize>],
) -> Vec<(bool, String)> {
    let mut out: Vec<(bool, String)> = Vec::new();
    let mut run_start = 0usize;
    let mut run_flag: Option<bool> = None;
    for (i, _) in content.char_indices() {
        let flag = emphasis.iter().any(|r| r.contains(&(base + i)));
        match run_flag {
            None => run_flag = Some(flag),
            Some(f) if f != flag => {
                out.push((f, content[run_start..i].to_string()));
                run_start = i;
                run_flag = Some(flag);
            }
            Some(_) => {}
        }
    }
    if let Some(f) = run_flag {
        out.push((f, content[run_start..].to_string()));
    }
    out
}

/// Renders one hunk's lines, pairing an isolated `Removal` immediately
/// followed by an isolated `Addition` (a single line replaced by another) so
/// the two get an intraline word diff — the specific changed tokens get a
/// vivid background chip on top of the usual soft red/green line tint.
/// Anything else (context lines, unpaired or multi-line runs of +/-) renders
/// with no emphasis, exactly as before.
fn render_hunk_lines(
    dls: &[DiffLine],
    num_width: usize,
    highlighted: &HighlightedEdit,
    engine: &mut HighlightEngine,
) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < dls.len() {
        let is_isolated_removal = dls[i].kind == LineKind::Removal
            && i + 1 < dls.len()
            && dls[i + 1].kind == LineKind::Addition
            && (i == 0 || dls[i - 1].kind != LineKind::Removal)
            && (i + 2 >= dls.len() || dls[i + 2].kind != LineKind::Addition);
        if is_isolated_removal {
            let (old_dl, new_dl) = (&dls[i], &dls[i + 1]);
            let old_expanded = expand_tabs(&old_dl.text);
            let new_expanded = expand_tabs(&new_dl.text);
            let (old_emphasis, new_emphasis) = word_diff_emphasis(&old_expanded, &new_expanded);
            out.push(render_diff_line(
                old_dl,
                num_width,
                highlighted,
                engine,
                &old_emphasis,
            ));
            out.push(render_diff_line(
                new_dl,
                num_width,
                highlighted,
                engine,
                &new_emphasis,
            ));
            i += 2;
        } else {
            out.push(render_diff_line(
                &dls[i],
                num_width,
                highlighted,
                engine,
                &[],
            ));
            i += 1;
        }
    }
    out
}

/// Computes intraline emphasis ranges for a single-line replacement: the byte
/// ranges (in each side's tab-expanded text) that a token-level LCS diff
/// identifies as actually changed. Returns `(old_ranges, new_ranges)`, both
/// empty when the lines are too long to diff cheaply or share too little in
/// common to read as a targeted edit (in which case the normal whole-line
/// tint alone communicates the change better than a near-total highlight).
fn word_diff_emphasis(
    old_expanded: &str,
    new_expanded: &str,
) -> (Vec<std::ops::Range<usize>>, Vec<std::ops::Range<usize>>) {
    let old_tokens = tokenize(old_expanded);
    let new_tokens = tokenize(new_expanded);
    let (n, m) = (old_tokens.len(), new_tokens.len());
    if n == 0 || m == 0 || n.saturating_mul(m) > 20_000 {
        return (Vec::new(), Vec::new());
    }

    // Classic LCS DP over token equality.
    let mut dp = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if old_tokens[i].1 == new_tokens[j].1 {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }

    let mut old_ranges = Vec::new();
    let mut new_ranges = Vec::new();
    // Matched *identifier* tokens only, for the relatedness gate below.
    // Punctuation and whitespace tokens match trivially between almost any
    // two code lines (every line has `(`, `.`, spaces, …), so counting them
    // would make unrelated lines look related; only shared identifiers and
    // keywords are real evidence the lines are "the same line, edited".
    let mut word_matches = 0usize;
    let (mut i, mut j) = (0usize, 0usize);
    while i < n && j < m {
        if old_tokens[i].1 == new_tokens[j].1 {
            if is_word_token(old_tokens[i].1) {
                word_matches += 1;
            }
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            old_ranges.push(old_tokens[i].0.clone());
            i += 1;
        } else {
            new_ranges.push(new_tokens[j].0.clone());
            j += 1;
        }
    }
    old_ranges.extend(old_tokens[i..].iter().map(|(r, _)| r.clone()));
    new_ranges.extend(new_tokens[j..].iter().map(|(r, _)| r.clone()));

    // Lines sharing too few identifiers (mostly a rewrite, not a targeted
    // edit) read better with just the whole-line tint than a near-total
    // emphasis chip.
    let old_words = old_tokens.iter().filter(|(_, t)| is_word_token(t)).count();
    let new_words = new_tokens.iter().filter(|(_, t)| is_word_token(t)).count();
    let min_words = old_words.min(new_words);
    if min_words > 0 && word_matches * 3 < min_words {
        return (Vec::new(), Vec::new());
    }

    (
        merge_ranges(old_ranges, old_expanded),
        merge_ranges(new_ranges, new_expanded),
    )
}

/// Whether `token` (as produced by [`tokenize`]) is an identifier/keyword
/// token rather than a punctuation or whitespace token.
fn is_word_token(token: &str) -> bool {
    token
        .chars()
        .next()
        .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '$')
}

/// Splits `s` into `(byte_range, token)` pairs: a maximal run of
/// identifier-ish characters (alphanumeric, `_`, `$`) is one token; every
/// other character (punctuation, whitespace) is its own single-char token.
/// This is the granularity word-diff tools conventionally use so e.g. adding
/// one argument to a call doesn't get diffed character-by-character.
fn tokenize(s: &str) -> Vec<(std::ops::Range<usize>, &str)> {
    let is_word = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    let mut out = Vec::new();
    let mut i = 0;
    while i < s.len() {
        let ch = s[i..]
            .chars()
            .next()
            .expect("i < s.len() guarantees a char");
        let start = i;
        if is_word(ch) {
            while let Some(c) = s[i..].chars().next() {
                if is_word(c) {
                    i += c.len_utf8();
                } else {
                    break;
                }
            }
        } else {
            i += ch.len_utf8();
        }
        out.push((start..i, &s[start..i]));
    }
    out
}

/// Merges adjacent/overlapping ranges into contiguous spans and drops any
/// range whose text is pure whitespace (a highlighted blank has no visible
/// glyph and just reads as a stray box).
fn merge_ranges(
    mut ranges: Vec<std::ops::Range<usize>>,
    source: &str,
) -> Vec<std::ops::Range<usize>> {
    ranges.retain(|r| !source[r.clone()].trim().is_empty());
    ranges.sort_by_key(|r| r.start);
    let mut out: Vec<std::ops::Range<usize>> = Vec::new();
    for r in ranges {
        if let Some(last) = out.last_mut()
            && r.start <= last.end
        {
            last.end = last.end.max(r.end);
            continue;
        }
        out.push(r);
    }
    out
}

/// Number of source lines skipped between two consecutive hunks (for the gap
/// marker), preferring old-file line numbers and falling back to new-file.
fn inter_hunk_gap(prev: Option<&FileHunk>, next: &FileHunk) -> Option<u64> {
    let prev = prev?;
    let prev_last = prev.lines.iter().rev().find_map(line_no_for_gap)?;
    let next_first = next.lines.iter().find_map(line_no_for_gap)?;
    if next_first > prev_last {
        Some(u64::try_from(next_first - prev_last - 1).unwrap_or(0))
    } else {
        None
    }
}

/// The line number used for gap math: old number if present, else new number.
fn line_no_for_gap(l: &DiffLine) -> Option<usize> {
    l.old_no.or(l.new_no)
}

/// A dim, full-span line used for inter-hunk gaps and truncation markers.
fn dim_line(text: String) -> Line<'static> {
    Line::from(vec![Span::styled(
        text,
        Style::default().fg(theme::active().dim),
    )])
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..s.floor_char_boundary(max)])
    }
}

// ── Modals ──────────────────────────────────────────────────────────────────

fn render_prompt_modal(f: &mut Frame, area: Rect, prompt: &str, scroll: u16) {
    let popup = center_rect(area, 85, 80);
    f.render_widget(Clear, popup);
    let text = tui_markdown::from_str(prompt);
    let para = Paragraph::new(text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" 📋 Prompt (p/Esc to close) "),
        )
        .wrap(ratatui::widgets::Wrap { trim: false })
        .scroll((scroll, 0));
    f.render_widget(para, popup);
}

fn render_error_modal(
    f: &mut Frame,
    area: Rect,
    entries: &[LiveEntry],
    scroll: u16,
    omitted: tui_transcript::OmittedStats,
) {
    let popup = center_rect(area, 85, 80);
    f.render_widget(Clear, popup);

    let mut lines: Vec<Line> = Vec::new();
    let mut count = 0usize;
    for (i, entry) in entries
        .iter()
        .filter(|e| matches!(e.kind, EntryKind::Error(_)))
        .enumerate()
    {
        count += 1;
        if i > 0 {
            lines.push(Line::from(""));
        }
        let ts = entry.ts.format("%H:%M:%S").to_string();
        let msg = match &entry.kind {
            EntryKind::Error(m) => m.as_str(),
            _ => unreachable!("filtered to errors above"),
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!("#{i}  "),
                Style::default()
                    .fg(theme::active().dim)
                    .add_modifier(Modifier::DIM),
            ),
            Span::styled(format!("{ts} "), Style::default().fg(theme::active().dim)),
            Span::styled("❌ ", Style::default().fg(theme::active().error)),
            Span::styled(msg.to_string(), Style::default().fg(theme::active().error)),
        ]));
    }

    if count == 0 {
        lines.push(Line::from(Span::styled(
            "No errors recorded so far.",
            Style::default().fg(theme::active().dim),
        )));
    } else if !omitted.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!(
                "Note: {} older entries were trimmed from the live view; see the run log for full history.",
                omitted.entries
            ),
            Style::default().fg(theme::active().dim).add_modifier(Modifier::DIM),
        )));
    }

    let title = format!(" ⚠ Errors: {count} (e/Esc to close) ");
    let para = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title(title))
        .wrap(ratatui::widgets::Wrap { trim: false })
        .scroll((scroll, 0));
    f.render_widget(para, popup);
}

/// The full keybinding reference shown (and searched) by the `?` help modal.
const HELP_KEYBINDINGS: &[(&str, &str)] = &[
    ("p", "Show the rendered prompt for this iteration"),
    (
        "↑↓ jk / PgUp PgDn",
        "Scroll the event log (anchored, streaming-safe)",
    ),
    ("gg", "Jump to the top of the iteration in view"),
    ("G", "Jump to the bottom of the iteration in view"),
    ("gT", "Jump to the absolute start of the chat"),
    ("gB / B", "Jump to the absolute bottom (re-enable live)"),
    ("t", "Toggle display of model thinking/reasoning tokens"),
    (
        "e",
        "Open the errors modal (errors are hidden from the main log)",
    ),
    ("s", "Toggle stopping after the current iteration completes"),
    (
        "c",
        "Steer the active Claude session with a one-shot message",
    ),
    (
        "a",
        "Edit the persistent append (folded into every later iteration)",
    ),
    ("l", "Open the JSONL run log in your pager"),
    (
        "o",
        "Toggle display of tool-result/file-edit output (calls stay visible)",
    ),
    (
        "Ctrl+O",
        "Expand/collapse tool-result output in place (full text vs. head+tail)",
    ),
    (
        "Ctrl+T",
        "Expand/collapse the sticky todo panel (small preview vs. full board)",
    ),
    (
        "y",
        "Copy the transcript since the last prompt to the clipboard (AI-friendly text)",
    ),
    (
        "Y",
        "Copy the entire transcript to the clipboard (AI-friendly text)",
    ),
    ("?", "Show this keybindings help"),
    (
        "/",
        "Search the keybindings list below (while this help is open)",
    ),
    (
        "m",
        "Show the provider, binary, and model this run is using",
    ),
    (
        "q / Ctrl+C×2",
        "Force stop immediately (Ctrl+C once does nothing)",
    ),
];

/// Column width the key name is padded to before the description, matching
/// the longest entry in [`HELP_KEYBINDINGS`] ("↑↓ jk / PgUp PgDn").
const HELP_KEY_COLUMN_WIDTH: usize = 18;

/// Whether a `(key, description)` entry matches an already-lowercased search
/// query — a substring match against either field, case-insensitively. An
/// empty query matches everything.
fn help_entry_matches(k: &str, desc: &str, query_lower: &str) -> bool {
    query_lower.is_empty()
        || k.to_ascii_lowercase().contains(query_lower)
        || desc.to_ascii_lowercase().contains(query_lower)
}

/// Splits `text` into spans, highlighting every case-insensitive occurrence
/// of `query_lower` (already lowercased) with `match_style` and leaving the
/// rest in `base_style`. Matching is ASCII-only (`to_ascii_lowercase`) so the
/// byte offsets found in the lowercased copy stay valid for slicing the
/// original — a locale-aware `to_lowercase` can change a string's byte
/// length and silently misalign them; the help text here is plain ASCII.
fn highlight_matches(
    text: &str,
    query_lower: &str,
    base_style: Style,
    match_style: Style,
) -> Vec<Span<'static>> {
    if query_lower.is_empty() {
        return vec![Span::styled(text.to_string(), base_style)];
    }
    let text_lower = text.to_ascii_lowercase();
    let mut spans = Vec::new();
    let mut idx = 0;
    while let Some(pos) = text_lower[idx..].find(query_lower) {
        let start = idx + pos;
        let end = start + query_lower.len();
        if start > idx {
            spans.push(Span::styled(text[idx..start].to_string(), base_style));
        }
        spans.push(Span::styled(text[start..end].to_string(), match_style));
        idx = end;
    }
    if idx < text.len() {
        spans.push(Span::styled(text[idx..].to_string(), base_style));
    }
    spans
}

/// Renders the `?` keybindings help modal. `search` is the current filter
/// query (empty means "show everything"); `search_active` is whether `/` is
/// actively capturing keystrokes into it, which switches the footer from a
/// static hint into a live search prompt with a cursor.
fn render_help_modal(f: &mut Frame, area: Rect, search: &str, search_active: bool) {
    let popup = center_rect(area, 70, 70);
    f.render_widget(Clear, popup);

    let theme = theme::active();
    let key_style = Style::default()
        .fg(theme.accent)
        .add_modifier(Modifier::BOLD);
    let desc_style = Style::default().fg(theme.muted);
    let match_style = Style::default()
        .fg(theme.warning)
        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
    let dim_style = Style::default().fg(theme.dim).add_modifier(Modifier::DIM);

    let query_lower = search.to_ascii_lowercase();
    let mut lines: Vec<Line> = Vec::new();
    let mut shown = 0usize;
    for (k, desc) in HELP_KEYBINDINGS {
        if !help_entry_matches(k, desc, &query_lower) {
            continue;
        }
        shown += 1;
        let mut spans = vec![Span::raw("  ")];
        spans.extend(highlight_matches(k, &query_lower, key_style, match_style));
        let pad = HELP_KEY_COLUMN_WIDTH.saturating_sub(UnicodeWidthStr::width(*k));
        spans.push(Span::raw(" ".repeat(pad)));
        spans.extend(highlight_matches(
            desc,
            &query_lower,
            desc_style,
            match_style,
        ));
        lines.push(Line::from(spans));
    }
    if shown == 0 {
        lines.push(Line::from(Span::styled(
            format!("No keybindings match \"{search}\"."),
            dim_style,
        )));
    }
    lines.push(Line::from(""));

    if search_active {
        lines.push(Line::from(vec![
            Span::styled("/", key_style),
            Span::styled(search.to_string(), key_style),
            Span::styled("▏", key_style),
        ]));
        lines.push(Line::from(Span::styled(
            "Enter: apply filter · Esc: clear · Backspace: edit",
            dim_style,
        )));
    } else {
        if !search.is_empty() {
            let total = HELP_KEYBINDINGS.len();
            let noun = if shown == 1 { "match" } else { "matches" };
            lines.push(Line::from(Span::styled(
                format!("/{search}  ({shown}/{total} {noun})"),
                dim_style,
            )));
        }
        lines.push(Line::from(Span::styled(
            "Live view is bounded; the complete transcript is always in the run log.",
            dim_style,
        )));
    }

    let title = if search_active {
        " ⌨️  Keybindings (Enter=apply search · Esc=clear) "
    } else {
        " ⌨️  Keybindings (/ to search · any key to close) "
    };
    let para = Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(para, popup);
}

/// Renders the `m` modal: the provider/binary/model this run was launched
/// with. `info` is `None` only in the brief window before `SetProviderInfo`
/// arrives (sent once, immediately, at startup), which reads as "resolving"
/// rather than an error.
fn render_provider_info_modal(f: &mut Frame, area: Rect, info: Option<&ProviderInfo>) {
    let popup = center_rect(area, 60, 30);
    f.render_widget(Clear, popup);

    let theme = theme::active();
    let key_style = Style::default()
        .fg(theme.accent)
        .add_modifier(Modifier::BOLD);
    let value_style = theme.text_style();
    let dim_style = Style::default().fg(theme.dim).add_modifier(Modifier::DIM);

    let mut lines: Vec<Line> = Vec::new();
    match info {
        Some(info) => {
            lines.push(Line::from(vec![
                Span::styled("  Provider  ", key_style),
                Span::styled(info.provider.clone(), value_style),
            ]));
            lines.push(Line::from(vec![
                Span::styled("  Binary    ", key_style),
                Span::styled(info.binary.clone(), value_style),
            ]));
            lines.push(Line::from(vec![
                Span::styled("  Model     ", key_style),
                match &info.model {
                    Some(model) => Span::styled(model.clone(), value_style),
                    None => Span::styled("(binary default)", dim_style),
                },
            ]));
        }
        None => {
            lines.push(Line::from(Span::styled("  Resolving…", dim_style)));
        }
    }

    let para = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" 🤖  Provider (any key to close) "),
    );
    f.render_widget(para, popup);
}

fn center_rect(area: Rect, pct_w: u16, pct_h: u16) -> Rect {
    let pop_w = area.width.saturating_mul(pct_w) / 100;
    let pop_h = area.height.saturating_mul(pct_h) / 100;
    let x = area.x + (area.width.saturating_sub(pop_w)) / 2;
    let y = area.y + (area.height.saturating_sub(pop_h)) / 2;
    Rect {
        x,
        y,
        width: pop_w,
        height: pop_h,
    }
}

#[cfg(test)]
mod render_tests {
    //! Render-path tests proving per-frame work is bounded by the viewport, not
    //! by total transcript length. Uses ratatui's `TestBackend` + the cache's
    //! instrumented render counter (no wall-clock timing).

    use super::*;
    use ratatui::backend::TestBackend;

    fn handler() -> crate::cancellation::CancellationHandler {
        crate::cancellation::CancellationHandler::new().0
    }

    #[test]
    fn render_lays_out_only_the_viewport_for_a_large_transcript() {
        let mut state = TuiState::new(TuiLimits {
            max_entries: 5000,
            max_bytes: usize::MAX,
            max_entry_lines: 100,
        });
        for i in 0..3000 {
            state
                .transcript
                .ingest(TuiEntry::now(EntryKind::Info(format!("entry {i}"))));
        }
        assert_eq!(state.transcript.entries().len(), 3000);

        let cancel_handler = handler();
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");

        state.cache.renders = 0;
        terminal
            .draw(|f| render(f, &mut state, &cancel_handler))
            .expect("draw");
        let rendered = state.cache.renders;

        // Viewport ≈ 22 rows + 24 overscan; each Info entry is 1 row. Rendering
        // must touch only ~that many entries — never all 3000.
        assert!(
            rendered < 80,
            "per-frame renders should be viewport-bounded, got {rendered}"
        );
    }

    #[test]
    fn help_modal_search_filters_live_and_esc_clears_it() {
        let mut state = TuiState::new(TuiLimits::default());
        let cancel = CancellationToken::new();
        let cancel_handler = handler();
        state.show_help = true;

        // `/` enters search mode without closing the modal.
        handle_key(
            event::KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE),
            &mut state,
            &cancel,
            &cancel_handler,
        );
        assert!(state.show_help);
        assert!(state.help_search_active);

        // Typed characters accumulate into the query live.
        for c in "steer".chars() {
            handle_key(
                event::KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
                &mut state,
                &cancel,
                &cancel_handler,
            );
        }
        assert_eq!(state.help_search, "steer");
        assert!(state.show_help, "typing must not close the modal");

        // Enter commits the filter but leaves the modal open.
        handle_key(
            event::KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &mut state,
            &cancel,
            &cancel_handler,
        );
        assert!(!state.help_search_active);
        assert!(state.show_help);
        assert_eq!(state.help_search, "steer");

        // With a committed filter, `/` resumes editing rather than closing.
        handle_key(
            event::KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE),
            &mut state,
            &cancel,
            &cancel_handler,
        );
        assert!(state.help_search_active);

        // Esc clears the query and leaves search mode, but keeps the modal open.
        handle_key(
            event::KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            &mut state,
            &cancel,
            &cancel_handler,
        );
        assert!(state.show_help);
        assert!(!state.help_search_active);
        assert!(state.help_search.is_empty());

        // Once out of search mode, any other key closes the modal (and resets
        // the search for next time it's opened).
        state.help_search = "steer".to_string();
        handle_key(
            event::KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
            &mut state,
            &cancel,
            &cancel_handler,
        );
        assert!(!state.show_help);
        assert!(state.help_search.is_empty());
    }

    #[test]
    fn m_key_opens_the_provider_info_modal_and_any_key_closes_it() {
        let mut state = TuiState::new(TuiLimits::default());
        let cancel = CancellationToken::new();
        let cancel_handler = handler();

        assert!(!state.show_provider_info);
        handle_key(
            event::KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE),
            &mut state,
            &cancel,
            &cancel_handler,
        );
        assert!(state.show_provider_info);

        handle_key(
            event::KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
            &mut state,
            &cancel,
            &cancel_handler,
        );
        assert!(
            !state.show_provider_info,
            "any key should close the provider modal without also acting on it"
        );
        assert!(
            !cancel.is_cancelled(),
            "the 'q' that closed the modal must be swallowed, not also trigger quit"
        );
    }

    #[test]
    fn set_provider_info_message_populates_state() {
        let mut state = TuiState::new(TuiLimits::default());
        assert!(state.provider_info.is_none());
        state.provider_info = Some(ProviderInfo {
            provider: "Codex".to_string(),
            binary: "codex".to_string(),
            model: Some("gpt-5.2".to_string()),
        });
        let info = state.provider_info.as_ref().expect("set above");
        assert_eq!(info.provider, "Codex");
        assert_eq!(info.model.as_deref(), Some("gpt-5.2"));
    }

    #[test]
    fn provider_info_modal_renders_without_panicking_when_resolved_and_pending() {
        let cancel_handler = handler();
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");

        let mut state = TuiState::new(TuiLimits::default());
        state.show_provider_info = true;
        // Pending (no SetProviderInfo yet).
        terminal
            .draw(|f| render(f, &mut state, &cancel_handler))
            .expect("draw pending");

        state.provider_info = Some(ProviderInfo {
            provider: "Claude".to_string(),
            binary: "claude".to_string(),
            model: None,
        });
        terminal
            .draw(|f| render(f, &mut state, &cancel_handler))
            .expect("draw resolved");
    }

    #[test]
    fn memory_bounded_by_limits_under_sustained_streaming() {
        // Coalescing means streamed text becomes one entry; the buffer must cap
        // at max_entries even after far more events than the cap.
        let mut state = TuiState::new(TuiLimits {
            max_entries: 500,
            max_bytes: usize::MAX,
            max_entry_lines: 100,
        });
        for i in 0..50_000 {
            state
                .transcript
                .ingest(TuiEntry::now(EntryKind::Info(format!("line {i}"))));
        }
        assert_eq!(
            state.transcript.entries().len(),
            500,
            "retained entries must plateau at the limit"
        );
        assert!(state.transcript.omitted().entries >= 49_500);
    }

    #[test]
    fn streamed_chunks_coalesce_to_one_cached_layout() {
        let mut state = TuiState::new(TuiLimits::default());
        for chunk in ["The ", "quick ", "brown ", "fox"] {
            state
                .transcript
                .ingest(TuiEntry::now(EntryKind::Text(chunk.to_string())));
        }
        assert_eq!(state.transcript.entries().len(), 1);
        // The single entry's revision bumped with each append; the cache key
        // includes rev, so the layout is recomputed only for that one entry.
        let cancel_handler = handler();
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        state.cache.renders = 0;
        terminal
            .draw(|f| render(f, &mut state, &cancel_handler))
            .expect("draw");
        assert!(state.cache.renders <= 2, "one coalesced entry → ≤2 renders");
    }

    // ── Vim-style jumps: gg / G / gT / gB ──────────────────────────────────────
    //
    // `gg` → top of the iteration in view; `G` → bottom of that iteration (live
    // tail when it's the latest); `gT` → absolute chat top; `gB` → absolute
    // bottom (re-enable live).

    fn ingest_n(state: &mut TuiState, n: usize) {
        for i in 0..n {
            state
                .transcript
                .ingest(TuiEntry::now(EntryKind::Info(format!("entry {i}"))));
        }
    }

    /// Marks a new iteration boundary exactly like the run loop does: arms a
    /// pending divider, then ingests `n` entries (the first of which emits the
    /// divider and indexes it as the iteration boundary for `gg`/`G`).
    fn start_iteration(state: &mut TuiState, current: u32, total: u32, n: usize) {
        state.iteration = Some((current, total));
        state.pending_iteration = Some((current, total));
        for i in 0..n {
            if let Some((c, t)) = state.pending_iteration.take() {
                state
                    .transcript
                    .ingest(TuiEntry::now(EntryKind::IterationDivider {
                        number: c,
                        maximum: Some(t),
                    }));
                if let Some(divider) = state.transcript.entries().last() {
                    state.iteration_starts.push(IterationBoundary {
                        id: divider.id,
                        number: c,
                    });
                }
            }
            state
                .transcript
                .ingest(TuiEntry::now(EntryKind::Info(format!(
                    "iter{current} entry {i}"
                ))));
        }
    }

    #[test]
    fn gg_jumps_to_top_of_iteration_in_view() {
        let mut state = TuiState::new(TuiLimits::default());
        // iter 1 (40) then iter 2 (40).
        start_iteration(&mut state, 1, 2, 40);
        let iter1_start = state.iteration_starts[0].id;
        start_iteration(&mut state, 2, 2, 40);
        let iter2_start = state.iteration_starts[1].id;
        // Copy ids out so we don't hold an immutable borrow across mutations.
        let (iter1_mid, iter2_mid) = {
            let entries = state.transcript.entries();
            (entries[10].id, entries[60].id)
        };

        // Anchor the viewport inside iteration 1.
        state.last_viewport_rows = 5;
        state.scroll = ScrollState::Anchored {
            entry_id: iter1_mid,
            hidden_rows: 0,
        };

        // gg → top of iteration 1.
        state.begin_g();
        assert!(state.pending_g);
        state.jump_to_iteration_top();
        let ScrollState::Anchored { entry_id, .. } = state.scroll else {
            panic!("expected anchored");
        };
        assert_eq!(entry_id, iter1_start, "gg from iter1 lands on iter1 start");

        // Now view iteration 2 and gg → its start.
        state.scroll = ScrollState::Anchored {
            entry_id: iter2_mid,
            hidden_rows: 0,
        };
        state.jump_to_iteration_top();
        let ScrollState::Anchored { entry_id, .. } = state.scroll else {
            panic!("expected anchored");
        };
        assert_eq!(entry_id, iter2_start, "gg from iter2 lands on iter2 start");
    }

    #[test]
    fn g_jumps_to_bottom_of_iteration_in_view() {
        let mut state = TuiState::new(TuiLimits::default());
        start_iteration(&mut state, 1, 2, 40);
        start_iteration(&mut state, 2, 2, 40);
        // Copy ids out up front. With dividers, iter1 = divider@0 + infos@1..40
        // and iter2 = divider@41 + infos@42..81.
        let (iter1_mid, iter1_last, iter2_mid) = {
            let entries = state.transcript.entries();
            (entries[10].id, entries[40].id, entries[60].id)
        };

        // Viewing iteration 1 → G lands on the entry just before iteration 2.
        state.last_viewport_rows = 5;
        state.scroll = ScrollState::Anchored {
            entry_id: iter1_mid,
            hidden_rows: 0,
        };
        state.jump_to_iteration_bottom();
        let ScrollState::Anchored { entry_id, .. } = state.scroll else {
            panic!("expected anchored");
        };
        assert_eq!(
            entry_id, iter1_last,
            "G from iter1 lands just before iter2 starts"
        );

        // Viewing iteration 2 (the latest) → G goes to live tail.
        state.scroll = ScrollState::Anchored {
            entry_id: iter2_mid,
            hidden_rows: 0,
        };
        state.jump_to_iteration_bottom();
        assert_eq!(
            state.scroll,
            ScrollState::Tail,
            "G from the latest iteration re-enables live tail"
        );
    }

    #[test]
    fn gg_falls_back_to_absolute_top_without_iteration_boundaries() {
        let mut state = TuiState::new(TuiLimits::default());
        ingest_n(&mut state, 50);
        state.last_viewport_rows = 5;
        state.scroll_up_by(10);
        // No iteration boundaries recorded.
        state.jump_to_iteration_top();
        let ScrollState::Anchored { entry_id, .. } = state.scroll else {
            panic!("expected anchored");
        };
        assert_eq!(
            entry_id,
            state.transcript.entries()[0].id,
            "gg falls back to absolute top with no iteration boundaries"
        );
    }

    #[test]
    fn iteration_divider_is_emitted_and_indexed_as_boundary() {
        let mut state = TuiState::new(TuiLimits::default());
        start_iteration(&mut state, 1, 2, 5);
        start_iteration(&mut state, 2, 2, 5);
        // Two iterations → two indexed boundaries.
        assert_eq!(state.iteration_starts.len(), 2);
        // Each indexed boundary must resolve to a retained IterationDivider entry,
        // and the divider numbers must match the iterations they introduce.
        for (i, boundary) in state.iteration_starts.iter().enumerate() {
            let entries = state.transcript.entries();
            let entry = entries
                .iter()
                .find(|e| e.id == boundary.id)
                .expect("indexed boundary id is retained");
            match &entry.kind {
                EntryKind::IterationDivider { number, maximum } => {
                    assert_eq!(*number, (i as u32) + 1, "divider number matches iteration");
                    assert_eq!(*maximum, Some(2));
                }
                other => panic!("indexed boundary is a divider, got {other:?}"),
            }
        }
    }

    #[test]
    fn viewing_iteration_tracks_the_scrolled_position() {
        let mut state = TuiState::new(TuiLimits::default());
        // Three iterations, 40 entries each: iter1 = divider@0 + infos@1..40,
        // iter2 = divider@41 + infos@42..81, iter3 = divider@82 + infos@83..122.
        start_iteration(&mut state, 1, 3, 40);
        start_iteration(&mut state, 2, 3, 40);
        start_iteration(&mut state, 3, 3, 40);
        // The agent is running iteration 3 (live tail).
        state.iteration = Some((3, 3));
        state.last_viewport_rows = 5;

        // Anchored inside iteration 1 → the reader is viewing iteration 1 even
        // though iteration 3 is running.
        let iter1_mid = state.transcript.entries()[10].id;
        state.scroll = ScrollState::Anchored {
            entry_id: iter1_mid,
            hidden_rows: 0,
        };
        assert_eq!(
            state.viewing_iteration_number(),
            Some(1),
            "scrolled into iteration 1 reports viewing 1"
        );

        // Anchored inside iteration 2 → viewing 2.
        let iter2_mid = state.transcript.entries()[51].id;
        state.scroll = ScrollState::Anchored {
            entry_id: iter2_mid,
            hidden_rows: 0,
        };
        assert_eq!(
            state.viewing_iteration_number(),
            Some(2),
            "scrolled into iteration 2 reports viewing 2"
        );

        // Back at the live tail (newest content = iteration 3) → viewing == running.
        state.scroll = ScrollState::Tail;
        assert_eq!(
            state.viewing_iteration_number(),
            Some(3),
            "tail reports the running iteration"
        );
    }

    #[test]
    fn g_t_jumps_to_absolute_top() {
        let mut state = TuiState::new(TuiLimits::default());
        start_iteration(&mut state, 1, 2, 30);
        start_iteration(&mut state, 2, 2, 30);
        state.last_viewport_rows = 5;
        // View the latest iteration.
        state.scroll = ScrollState::Tail;
        state.jump_to_absolute_top_cmd();
        let ScrollState::Anchored { entry_id, .. } = state.scroll else {
            panic!("expected anchored");
        };
        assert_eq!(
            entry_id,
            state.transcript.entries()[0].id,
            "gT jumps to the very first retained entry"
        );
    }

    #[test]
    fn g_b_jumps_to_absolute_bottom_live_tail() {
        let mut state = TuiState::new(TuiLimits::default());
        start_iteration(&mut state, 1, 2, 30);
        start_iteration(&mut state, 2, 2, 30);
        state.last_viewport_rows = 5;
        // Browse iteration 1.
        state.scroll_up_by(40);
        assert_ne!(state.scroll, ScrollState::Tail);
        state.jump_to_absolute_bottom();
        assert_eq!(state.scroll, ScrollState::Tail, "gB re-enables live tail");
    }

    #[test]
    fn pending_g_prefix_cancels_on_non_matching_key() {
        let mut state = TuiState::new(TuiLimits::default());
        ingest_n(&mut state, 50);
        state.begin_g();
        assert!(state.pending_g);
        // Simulate handle_key's "any other key cancels" branch.
        state.cancel_g();
        assert!(!state.pending_g);
    }

    #[test]
    fn tick_g_chord_cancels_on_timeout() {
        let mut state = TuiState::new(TuiLimits::default());
        ingest_n(&mut state, 50);
        state.begin_g();
        assert!(state.pending_g);
        // Forcibly expire the deadline.
        state.g_deadline = Some(Instant::now() - Duration::from_millis(1));
        state.tick_g_chord();
        assert!(!state.pending_g, "pending g cancelled after timeout");
    }

    // ── File-edit rendering ─────────────────────────────────────────────────

    fn row_text(line: &Line<'_>) -> String {
        line.spans.iter().flat_map(|s| s.content.chars()).collect()
    }

    fn one_line_replacement_edit() -> velor_core::file_edit::FileEdit {
        velor_core::file_edit::compute_file_edit(
            "src/lib.rs",
            Some(b"fn one() {}\nfn two() {}\nfn three() {}\n"),
            Some(b"fn one() {}\nfn TWO() {}\nfn three() {}\n"),
            velor_core::file_edit::DEFAULT_MAX_DIFF_LINES,
        )
        .expect("a real change produces an edit")
    }

    #[test]
    fn file_edit_layout_has_gutter_syntax_and_diff_styles() {
        let mut engine = crate::highlight::HighlightEngine::new();
        let kind = EntryKind::FileEdit(Box::new(one_line_replacement_edit()));
        let rows = build_layout(&kind, Local::now(), 80, &mut engine, RenderOpts::default());
        assert!(!rows.is_empty());

        // Flatten spans, checking for ANSI escapes and gathering evidence of
        // diff styling + syntax colouring.
        let mut text = String::new();
        let mut has_addition_bg = false;
        let mut has_removal_bg = false;
        let mut has_syntax_fg = false;
        let mut has_plus_sign = false;
        let mut has_minus_sign = false;
        let theme = theme::active();
        let addition_bg = theme::Theme::dim_bg(theme.diff_added);
        let removal_bg = theme::Theme::dim_bg(theme.diff_removed);
        for line in &rows {
            for span in &line.spans {
                text.push_str(&span.content);
                let style = span.style;
                if style.bg == Some(addition_bg) {
                    has_addition_bg = true;
                }
                if style.bg == Some(removal_bg) {
                    has_removal_bg = true;
                }
                if matches!(style.fg, Some(Color::Rgb(..))) {
                    has_syntax_fg = true;
                }
                if span.content == "+" && style.fg == Some(theme.diff_added) {
                    has_plus_sign = true;
                }
                if span.content == "-" && style.fg == Some(theme.diff_removed) {
                    has_minus_sign = true;
                }
            }
        }
        assert!(!text.contains('\x1b'), "no ANSI escapes embedded in spans");
        assert!(text.contains("│"), "gutter separator present");
        assert!(has_plus_sign, "added lines carry a themed + gutter marker");
        assert!(
            has_minus_sign,
            "removed lines carry a themed - gutter marker"
        );
        assert!(
            has_addition_bg,
            "added line source keeps syntax fg with a green background tint"
        );
        assert!(
            has_removal_bg,
            "removed line source keeps syntax fg with a red background tint"
        );
        assert!(has_syntax_fg, "syntax highlighting produced coloured spans");
    }

    #[test]
    fn file_edit_wrapped_continuation_aligns_under_source() {
        let mut engine = crate::highlight::HighlightEngine::new();
        // A created file with one very long line so it wraps at a narrow width.
        let long = format!("fn long() {{ /* {} */ }}", "x".repeat(200));
        let edit = velor_core::file_edit::compute_file_edit(
            "src/lib.rs",
            None,
            Some(long.as_bytes()),
            velor_core::file_edit::DEFAULT_MAX_DIFF_LINES,
        )
        .expect("created edit");
        let kind = EntryKind::FileEdit(Box::new(edit));
        let rows = build_layout(&kind, Local::now(), 40, &mut engine, RenderOpts::default());

        // rows[0] = header; rows[1] = first (guttered) row of the added line;
        // rows[2..] = wrapped continuation rows.
        assert!(
            rows.len() > 2,
            "a long line must wrap to multiple rows, got {}",
            rows.len()
        );
        // The guttered row begins with the gutter " + 1 │ " (width 7 for a
        // 1-digit line number: space + sign + space + num + " │ ").
        let first = row_text(&rows[1]);
        assert!(
            first.starts_with(" + 1 │ "),
            "first source row begins with the gutter, got {first:?}"
        );
        // The continuation row is indented by the gutter width (7 columns) so it
        // aligns beneath the source, not beneath the line number/marker.
        let cont = row_text(&rows[2]);
        assert!(
            cont.starts_with(&" ".repeat(7)),
            "wrapped continuation is indented to the source column, got {cont:?}"
        );
    }

    #[test]
    fn file_edit_renders_in_buffer_without_ansi() {
        let mut state = TuiState::new(TuiLimits::default());
        state
            .transcript
            .ingest(TuiEntry::now(EntryKind::FileEdit(Box::new(
                one_line_replacement_edit(),
            ))));

        let cancel_handler = handler();
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| render(f, &mut state, &cancel_handler))
            .expect("draw");
        let buf = terminal.backend().buffer();

        // Gather every rendered cell so we can assert on the whole frame.
        let mut all = String::new();
        for y in 0..24u16 {
            for x in 0..80u16 {
                all.push_str(buf[(x, y)].symbol());
            }
        }
        assert!(
            !all.contains('\x1b'),
            "rendered transcript must contain no ANSI escape sequences"
        );
        assert!(all.contains("src/lib.rs"), "file path header is rendered");
        assert!(all.contains('+'), "addition marker is rendered");
        assert!(all.contains('-'), "removal marker is rendered");
    }

    // ── Svelte highlighting (composition + state preservation) ───────────────

    /// A representative Svelte 5 component exercising every construct the task
    /// requires: `<script lang="ts">`, imports, types/generics, runes
    /// (`$state`/`$derived`/`$derived.by`/`$effect`/`$props`), control blocks
    /// (`{#if}`/`{#each}`/`{#await}`/`{:else}`/`{@const}`/`{@render}`), a
    /// snippet, directives (`bind:`/`class:`/`on:`), props, HTML, `<style>`
    /// CSS, and a trailing malformed fragment representative of a live diff.
    const SVELTE5_FIXTURE: &str = "\
<script lang=\"ts\">
  import { onMount } from 'svelte';
  import type { Snippet } from 'svelte';

  interface Item<T> {
    id: number;
    label: string;
    data: T;
  }

  let { items = [], header }: { items: Item<string>[]; header: Snippet } = $props();
  let count = $state(0);
  let selected = $state<Item<string> | null>(null);
  let total = $derived(items.length);
  let first = $derived.by(() => items[0]?.id ?? 0);
  let greeting = `hello #${count}`;

  $effect(() => {
    console.log('count is', count);
  });

  onMount(() => { count = 1; });

  function pick(item: Item<string>): void {
    selected = item;
  }
</script>

{#if items.length > 0}
  <ul>
    {#each items as item, i (item.id)}
      {@const upper = item.label.toUpperCase()}
      <li
        class:active={selected?.id === item.id}
        on:click={() => pick(item)}
        bind:data-idx={i}
      >
        {@render header({ item, upper })}
        <span>{item.label}</span>
      </li>
    {/each}
  </ul>
{:else}
  {#await loadBackup() then backup}
    <p>{backup}</p>
  {/await}
{/if}

<style>
  .active {
    color: red;
    padding: 4px;
    font-weight: bold;
  }
  ul > li {
    margin: 0;
  }
</style>
<div class=\"malformed attribute=\
";

    /// A Svelte edit where a hunk sits *inside* the `<script lang=\"ts\">` block
    /// (so it carries no `<script>` tag itself) — the regression case that
    /// isolated per-line highlighting could never classify correctly.
    fn svelte_script_hunk_edit() -> velor_core::file_edit::FileEdit {
        let old = format!(
            "<script lang=\"ts\">\n  let count = $state(0);\n  let name = 'world';\n</script>\n\n<p>hi</p>\n",
        );
        let new = format!(
            "<script lang=\"ts\">\n  let count = $state(0);\n  let name = $derived('hello');\n</script>\n\n<p>hi</p>\n",
        );
        velor_core::file_edit::compute_file_edit(
            "src/Comp.svelte",
            Some(old.as_bytes()),
            Some(new.as_bytes()),
            velor_core::file_edit::DEFAULT_MAX_DIFF_LINES,
        )
        .expect("a real change produces an edit")
    }

    #[test]
    fn svelte_edit_carries_full_source() {
        // The adapter must populate `full_new_source` for composite languages
        // so the highlighter can resolve embedded-language state.
        let edit = svelte_script_hunk_edit();
        assert_eq!(edit.syntax, velor_core::file_edit::SyntaxKind::Svelte);
        assert!(
            edit.full_new_source.is_some(),
            "Svelte edits carry the full new-side source"
        );
        let src = edit.full_new_source.as_deref().unwrap();
        assert!(src.contains("<script lang=\"ts\">"));
        assert!(src.contains("$derived"));
    }

    #[test]
    fn plain_edit_does_not_carry_full_source() {
        // Plain languages don't need it; the field stays None.
        let edit = one_line_replacement_edit();
        assert_eq!(edit.syntax, velor_core::file_edit::SyntaxKind::Rust);
        assert!(
            edit.full_new_source.is_none(),
            "Rust edits do not carry full source"
        );
    }

    #[test]
    fn svelte_hunk_inside_script_is_highlighted() {
        // The critical regression test: a diff line inside `<script lang="ts">`
        // (no script tag in the hunk) must still get TS classifications,
        // because the engine parsed the whole carried source for state.
        let mut engine = crate::highlight::HighlightEngine::new();
        let edit = svelte_script_hunk_edit();
        let highlighted = HighlightedEdit::build(&mut engine, &edit);

        // Find the addition line (the `$derived` line) and render its spans.
        let added = edit
            .hunks
            .iter()
            .flat_map(|h| h.lines.iter())
            .find(|l| l.kind == LineKind::Addition && l.text.contains("$derived"))
            .expect("an added $derived line exists");

        let expanded = expand_tabs(&added.text);
        let spans = highlighted.spans_for_line(added, &expanded, &mut engine);

        // `$derived` must be classified as a function, not left as plain text —
        // this is exactly what the pre-fix HTML-grammar path could not do.
        let rune_classified = spans
            .iter()
            .any(|(content, style)| content.contains("$derived") && style.fg.is_some());
        assert!(
            rune_classified,
            "$derived rune must receive a foreground colour: {spans:?}"
        );

        // And `'hello'` should be a string.
        let string_classified = spans
            .iter()
            .any(|(content, style)| content.contains('\'') && style.fg.is_some());
        assert!(
            string_classified,
            "the 'hello' string must be coloured: {spans:?}"
        );
    }

    #[test]
    fn added_line_background_preserves_syntax_foreground() {
        // The composition invariant: on an added line, a syntax-coloured span
        // must carry BOTH a non-None foreground (syntax) AND the green tint
        // background (diff). Neither side clobbers the other.
        let mut engine = crate::highlight::HighlightEngine::new();
        let edit = svelte_script_hunk_edit();
        let highlighted = HighlightedEdit::build(&mut engine, &edit);
        let num_width = line_number_width(&edit);

        let added = edit
            .hunks
            .iter()
            .flat_map(|h| h.lines.iter())
            .find(|l| l.kind == LineKind::Addition && l.text.contains("$derived"))
            .expect("an added $derived line exists");
        let line = render_diff_line(added, num_width, &highlighted, &mut engine, &[]);

        // Find a span covering a syntax token on the added line and assert it
        // has both fg (syntax) and the addition bg tint.
        let addition_bg = theme::Theme::dim_bg(theme::active().diff_added);
        let has_composed = line.spans.iter().any(|s| {
            s.style.bg == Some(addition_bg) && s.style.fg.is_some() && !s.content.trim().is_empty()
        });
        assert!(
            has_composed,
            "added line must preserve syntax fg under the diff bg tint: {:?}",
            line.spans
        );
    }

    #[test]
    fn removed_line_background_preserves_syntax_foreground() {
        let mut engine = crate::highlight::HighlightEngine::new();
        let edit = svelte_script_hunk_edit();
        let highlighted = HighlightedEdit::build(&mut engine, &edit);
        let num_width = line_number_width(&edit);

        let removed = edit
            .hunks
            .iter()
            .flat_map(|h| h.lines.iter())
            .find(|l| l.kind == LineKind::Removal)
            .expect("a removed line exists");
        let line = render_diff_line(removed, num_width, &highlighted, &mut engine, &[]);

        let removal_bg = theme::Theme::dim_bg(theme::active().diff_removed);
        // At least some non-empty content on the removal line carries the bg.
        let has_bg = line
            .spans
            .iter()
            .any(|s| s.style.bg == Some(removal_bg) && !s.content.trim().is_empty());
        assert!(has_bg, "removed line carries the red bg tint");
    }

    #[test]
    fn context_line_keeps_syntax_foreground_without_diff_bg() {
        // Context lines have no diff tint; syntax foreground shows alone.
        let mut engine = crate::highlight::HighlightEngine::new();
        let edit = svelte_script_hunk_edit();
        let highlighted = HighlightedEdit::build(&mut engine, &edit);
        let num_width = line_number_width(&edit);

        let ctx = edit
            .hunks
            .iter()
            .flat_map(|h| h.lines.iter())
            .find(|l| l.kind == LineKind::Context)
            .expect("a context line exists");
        let line = render_diff_line(ctx, num_width, &highlighted, &mut engine, &[]);
        // No source span on a context line should carry a diff bg tint.
        let theme = theme::active();
        let addition_bg = theme::Theme::dim_bg(theme.diff_added);
        let removal_bg = theme::Theme::dim_bg(theme.diff_removed);
        let source_spans = &line.spans;
        assert!(
            source_spans
                .iter()
                .all(|s| s.style.bg != Some(addition_bg) && s.style.bg != Some(removal_bg)),
            "context lines carry no diff background"
        );
    }

    #[test]
    fn svelte_full_fixture_renders_without_panic() {
        // The representative Svelte 5 fixture (including the malformed trailing
        // snippet) must render end-to-end without panicking.
        let edit = velor_core::file_edit::compute_file_edit(
            "src/Full.svelte",
            None,
            Some(SVELTE5_FIXTURE.as_bytes()),
            velor_core::file_edit::DEFAULT_MAX_DIFF_LINES,
        )
        .expect("fixture produces an edit");
        let mut engine = crate::highlight::HighlightEngine::new();
        let _rows = build_layout(
            &EntryKind::FileEdit(Box::new(edit)),
            Local::now(),
            100,
            &mut engine,
            RenderOpts::default(),
        );
        // Reaching here = no panic on the full Svelte 5 construct set.
    }

    #[test]
    fn svelte_full_fixture_classifies_runes_and_ts() {
        // Highlight the whole Svelte 5 fixture directly through the engine and
        // assert the headline classifications hold across the full construct set.
        let mut engine = crate::highlight::HighlightEngine::new();
        let spans = engine.highlight(&HighlightRequest::full(
            velor_core::file_edit::SyntaxKind::Svelte,
            SVELTE5_FIXTURE,
        ));
        let token_at = |needle: &str| -> Option<SemanticToken> {
            let idx = SVELTE5_FIXTURE.find(needle)?;
            spans
                .iter()
                .find(|s| s.start <= idx && idx < s.end)
                .map(|s| s.token)
        };
        // Runes are functions.
        assert_eq!(token_at("$state"), Some(SemanticToken::Function));
        assert_eq!(token_at("$derived"), Some(SemanticToken::Function));
        assert_eq!(token_at("$effect"), Some(SemanticToken::Function));
        assert_eq!(token_at("$props"), Some(SemanticToken::Function));
        // TS types and keywords inside lang="ts".
        assert_eq!(token_at("interface"), Some(SemanticToken::Keyword));
        assert_eq!(token_at("number"), Some(SemanticToken::Type));
        // Strings and numbers.
        assert_eq!(token_at("'count is'"), Some(SemanticToken::String));
        // HTML/Svelte attributes/directives. `token_at` finds the first
        // substring match, so the needles must land inside the intended token.
        assert_eq!(token_at("on:click"), Some(SemanticToken::Attribute));
        assert_eq!(token_at("bind:data-idx"), Some(SemanticToken::Attribute));
    }

    // ── Intraline word diff ───────────────────────────────────────────────────

    #[test]
    fn tokenize_splits_identifiers_from_punctuation() {
        let toks: Vec<&str> = tokenize("foo(bar, $baz)")
            .into_iter()
            .map(|(_, t)| t)
            .collect();
        assert_eq!(toks, vec!["foo", "(", "bar", ",", " ", "$baz", ")"]);
    }

    #[test]
    fn tokenize_reconstructs_the_source_exactly() {
        let s = "import { Effect, Layer } from \"effect\";";
        let joined: String = tokenize(s).into_iter().map(|(_, t)| t).collect();
        assert_eq!(joined, s);
    }

    #[test]
    fn word_diff_emphasis_isolates_a_single_inserted_identifier() {
        let old = "import { Effect, Layer, Queue, Schema } from \"effect\";";
        let new = "import { Effect, Layer, Queue, Runtime, Schema } from \"effect\";";
        let (old_ranges, new_ranges) = word_diff_emphasis(old, new);
        // Nothing was removed from the old line.
        assert!(
            old_ranges.is_empty(),
            "old side has no removed tokens: {old_ranges:?}"
        );
        // Exactly the inserted "Runtime, " (or a tight equivalent) is flagged —
        // not the whole line — on the new side.
        assert_eq!(
            new_ranges.len(),
            1,
            "one contiguous inserted run: {new_ranges:?}"
        );
        let r = new_ranges[0].clone();
        assert!(
            new[r].contains("Runtime"),
            "the flagged range must contain the new identifier"
        );
        assert!(
            new_ranges[0].end - new_ranges[0].start < 12,
            "the flagged range should be tight around the change, not the whole line"
        );
    }

    #[test]
    fn word_diff_emphasis_skips_when_lines_are_mostly_unrelated() {
        let old = "Effect.runFork(";
        let new = "Runtime.runFork(runtime)(";
        // Real example from the reference: still shares enough (".runFork(")
        // to pair, and the changed identifier gets flagged, not the whole line.
        let (old_ranges, new_ranges) = word_diff_emphasis(old, new);
        assert!(!old_ranges.is_empty() || !new_ranges.is_empty());

        let unrelated_old = "const queue = yield* Effect.acquireRelease(";
        let unrelated_new = "function pick(item: Item<string>): void {";
        let (o, n) = word_diff_emphasis(unrelated_old, unrelated_new);
        assert!(
            o.is_empty() && n.is_empty(),
            "unrelated lines get no emphasis: {o:?} {n:?}"
        );
    }

    #[test]
    fn word_diff_emphasis_drops_whitespace_only_ranges() {
        let old = "let x = 1;";
        let new = "let  x = 1;"; // one extra space only
        let (old_ranges, new_ranges) = word_diff_emphasis(old, new);
        assert!(
            old_ranges.is_empty(),
            "a pure whitespace diff must not be flagged: {old_ranges:?}"
        );
        assert!(
            new_ranges.is_empty(),
            "a pure whitespace diff must not be flagged: {new_ranges:?}"
        );
    }

    #[test]
    #[allow(
        clippy::single_range_in_vec_init,
        reason = "a slice with one Range value, not a Vec-fill idiom"
    )]
    fn split_by_emphasis_groups_contiguous_runs() {
        let parts = split_by_emphasis("Runtime, Schema", 0, &[0..8]);
        assert_eq!(
            parts,
            vec![
                (true, "Runtime,".to_string()),
                (false, " Schema".to_string())
            ]
        );
    }

    #[test]
    fn split_by_emphasis_with_no_ranges_is_one_unemphasized_run() {
        let parts = split_by_emphasis("plain text", 0, &[]);
        assert_eq!(parts, vec![(false, "plain text".to_string())]);
    }

    #[test]
    #[allow(
        clippy::single_range_in_vec_init,
        reason = "a slice with one Range value, not a Vec-fill idiom"
    )]
    fn split_by_emphasis_respects_a_nonzero_base_offset() {
        // "world" starts at byte 6 in the full line; emphasis range is
        // expressed in full-line coordinates, base is this segment's offset.
        let parts = split_by_emphasis("world", 6, &[6..11]);
        assert_eq!(parts, vec![(true, "world".to_string())]);
    }

    #[test]
    fn merge_ranges_joins_adjacent_and_drops_whitespace() {
        let merged = merge_ranges(vec![0..3, 3..5, 10..11], "fooba x y  ");
        // 0..3 ("foo") and 3..5 ("ba") touch, so they merge; 10..11 is a
        // trailing space and gets dropped.
        assert_eq!(merged, vec![0..5]);
    }

    #[test]
    fn render_hunk_lines_emphasizes_an_isolated_replacement_pair() {
        let mut engine = crate::highlight::HighlightEngine::new();
        let old = "import { Effect, Layer, Queue, Schema } from \"effect\";\nconst x = 1;\n";
        let new =
            "import { Effect, Layer, Queue, Runtime, Schema } from \"effect\";\nconst x = 1;\n";
        let edit = velor_core::file_edit::compute_file_edit(
            "src/lib.ts",
            Some(old.as_bytes()),
            Some(new.as_bytes()),
            velor_core::file_edit::DEFAULT_MAX_DIFF_LINES,
        )
        .expect("a real change produces an edit");
        let highlighted = HighlightedEdit::build(&mut engine, &edit);
        let num_width = line_number_width(&edit);
        let hunk = &edit.hunks[0];
        let rows = render_hunk_lines(&hunk.lines, num_width, &highlighted, &mut engine);

        let has_emphasis_chip = rows.iter().any(|line| {
            line.spans.iter().any(|s| {
                s.style.bg == Some(theme::active().diff_added) && s.content.contains("Runtime")
            })
        });
        assert!(
            has_emphasis_chip,
            "the added identifier must carry the word-diff emphasis chip: {rows:?}"
        );
    }

    // ── Tool-call / tool-result terminal card ───────────────────────────────────

    #[test]
    fn tool_card_row_pads_to_the_full_width_with_the_card_background() {
        let bg = theme::active().tool_success_bg;
        let line = tool_card_row(vec![Span::styled("hi", Style::default())], 10, bg);
        let total_w: usize = line
            .spans
            .iter()
            .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
            .sum();
        assert_eq!(total_w, 10);
        assert!(line.spans.iter().all(|s| s.style.bg == Some(bg)));
        assert!(row_text(&line).starts_with('│'));
        assert!(row_text(&line).ends_with('│'));
    }

    #[test]
    fn tool_card_rule_embeds_the_label_between_the_corners() {
        let bg = theme::active().tool_success_bg;
        let line = tool_card_rule('╭', '╮', "$ echo hi", 40, bg);
        let text = row_text(&line);
        assert!(text.starts_with('╭'), "got {text:?}");
        assert!(text.ends_with('╮'), "got {text:?}");
        assert!(text.contains("$ echo hi"), "got {text:?}");
        assert!(line.spans.iter().all(|s| s.style.bg == Some(bg)));
    }

    #[test]
    fn non_edit_tool_calls_render_nothing_the_result_box_shows_the_command() {
        // ToolCall never opens a box: calls can arrive several-in-a-row
        // before any result does (parallel tool use), so a call has no
        // reliable neighbour to pair a box with. Its result, once it arrives,
        // shows the correlated command as the box's own header (see
        // `tool_result_is_a_fully_self_contained_box_even_for_one_line`), so
        // printing the command again here would just be the same line twice.
        let mut engine = crate::highlight::HighlightEngine::new();
        for (tool, detail) in [("Bash", "echo hi"), ("Read", "src/lib.rs")] {
            let kind = EntryKind::ToolCall {
                tool: tool.to_string(),
                detail: detail.to_string(),
                input: serde_json::Value::Null,
            };
            assert_eq!(
                render_entry(&kind, 40, &mut engine, RenderOpts::default()),
                Vec::<Line>::new(),
                "{tool} call should render nothing"
            );
        }
    }

    #[test]
    fn tool_result_is_a_fully_self_contained_box_even_for_one_line() {
        // No dependency on a neighbouring ToolCall: the opening rule lives on
        // the result itself, so this renders correctly no matter what other
        // calls/results are interleaved around it in the stream.
        let mut engine = crate::highlight::HighlightEngine::new();
        let kind = EntryKind::ToolResult {
            tool: "Bash".to_string(),
            detail: "ok".to_string(),
            success: Some(true),
            command: None,
        };
        let rows = render_entry(&kind, 40, &mut engine, RenderOpts::default());
        // opening rule + divider + 1 output line + closing rule + trailing blank.
        assert_eq!(rows.len(), 5, "even a one-line result gets the full card");
        assert!(
            row_text(&rows[0]).starts_with('╭'),
            "got {:?}",
            row_text(&rows[0])
        );
        assert!(row_text(&rows[0]).contains("Bash"));
        assert!(row_text(&rows[1]).contains("Output"));
        assert!(row_text(&rows[3]).starts_with('╰'));
        assert!(rows[..4].iter().all(|l| {
            l.spans
                .iter()
                .all(|s| s.style.bg == Some(theme::active().tool_success_bg))
        }));
    }

    #[test]
    fn multiline_tool_result_gets_an_opening_rule_output_divider_and_closing_rule() {
        let mut engine = crate::highlight::HighlightEngine::new();
        let kind = EntryKind::ToolResult {
            tool: "Bash".to_string(),
            detail: "line one\nline two".to_string(),
            success: Some(true),
            command: None,
        };
        let rows = render_entry(&kind, 40, &mut engine, RenderOpts::default());
        // opening rule + divider + 2 output lines + closing rule + trailing blank.
        assert_eq!(rows.len(), 6);
        assert!(row_text(&rows[0]).starts_with('╭'));
        assert!(row_text(&rows[1]).contains("── Output"));
        assert!(row_text(&rows[4]).starts_with('╰'));
        assert!(rows[..5].iter().all(|l| {
            l.spans
                .iter()
                .all(|s| s.style.bg == Some(theme::active().tool_success_bg))
        }));
    }

    #[test]
    fn collapsed_tool_result_shows_head_tail_and_an_expand_marker() {
        let mut engine = crate::highlight::HighlightEngine::new();
        let body: Vec<String> = (0..20).map(|i| format!("line {i}")).collect();
        let kind = EntryKind::ToolResult {
            tool: "Bash".to_string(),
            detail: body.join("\n"),
            success: Some(true),
            command: None,
        };
        let opts = RenderOpts {
            expand: false,
            max_entry_lines: 4,
        };
        let rows = render_entry(&kind, 40, &mut engine, opts);
        let text = rows.iter().map(row_text).collect::<Vec<_>>().join("\n");
        assert!(text.contains("line 0"), "head is kept");
        assert!(text.contains("line 1"), "head is kept");
        assert!(text.contains("line 19"), "tail is kept");
        assert!(text.contains("line 18"), "tail is kept");
        assert!(
            !text.contains("line 10"),
            "middle is hidden while collapsed"
        );
        assert!(text.contains("16 more lines (Ctrl+O: Expand)"));
    }

    #[test]
    fn expanded_tool_result_shows_the_full_body_with_no_marker() {
        let mut engine = crate::highlight::HighlightEngine::new();
        let body: Vec<String> = (0..20).map(|i| format!("line {i}")).collect();
        let kind = EntryKind::ToolResult {
            tool: "Bash".to_string(),
            detail: body.join("\n"),
            success: Some(true),
            command: None,
        };
        // max_entry_lines is the hard safety cap even while expanded, so it
        // must exceed the body for "expanded shows everything" to hold —
        // unlike the collapsed-view test, which deliberately uses a tiny cap
        // to force folding.
        let opts = RenderOpts {
            expand: true,
            max_entry_lines: 400,
        };
        let rows = render_entry(&kind, 40, &mut engine, opts);
        let text = rows.iter().map(row_text).collect::<Vec<_>>().join("\n");
        for i in 0..20 {
            assert!(
                text.contains(&format!("line {i}")),
                "line {i} missing when expanded"
            );
        }
        assert!(
            !text.contains("Ctrl+O"),
            "no expand marker once already expanded"
        );
    }

    #[test]
    fn expanded_tool_result_still_folds_past_the_hard_safety_cap() {
        // max_entry_lines is a real safety limit, not just a preview size —
        // even Ctrl+O can't reveal more than this, so the marker must not
        // claim it can.
        let mut engine = crate::highlight::HighlightEngine::new();
        let body: Vec<String> = (0..20).map(|i| format!("line {i}")).collect();
        let kind = EntryKind::ToolResult {
            tool: "Bash".to_string(),
            detail: body.join("\n"),
            success: Some(true),
            command: None,
        };
        let opts = RenderOpts {
            expand: true,
            max_entry_lines: 4,
        };
        let rows = render_entry(&kind, 40, &mut engine, opts);
        let text = rows.iter().map(row_text).collect::<Vec<_>>().join("\n");
        assert!(text.contains("16 more lines"));
        assert!(
            !text.contains("Ctrl+O"),
            "must not claim Ctrl+O can reveal more once already expanded: {text:?}"
        );
        assert!(text.contains("run log"));
    }

    #[test]
    fn default_options_collapse_a_typical_multi_hundred_line_file_read() {
        // Regression: RenderOpts::default() used to use TuiLimits::default's
        // max_entry_lines (400) directly as the collapse threshold, so any
        // real file read under ~400 lines never actually collapsed and
        // Ctrl+O had nothing visible to toggle. The default collapse size is
        // now independent of that (much larger) hard safety cap.
        let mut engine = crate::highlight::HighlightEngine::new();
        let body: Vec<String> = (0..120).map(|i| format!("line {i}")).collect();
        let kind = EntryKind::ToolResult {
            tool: "Bash".to_string(),
            detail: body.join("\n"),
            success: Some(true),
            command: None,
        };
        let rows = render_entry(&kind, 40, &mut engine, RenderOpts::default());
        let text = rows.iter().map(row_text).collect::<Vec<_>>().join("\n");
        assert!(
            text.contains("Ctrl+O: Expand"),
            "a 120-line read must collapse by default"
        );
        assert!(
            !text.contains("line 60"),
            "the middle must be hidden while collapsed"
        );
    }

    #[test]
    fn short_tool_result_never_folds_regardless_of_expand() {
        let mut engine = crate::highlight::HighlightEngine::new();
        let kind = EntryKind::ToolResult {
            tool: "Bash".to_string(),
            detail: "only one line".to_string(),
            success: Some(true),
            command: None,
        };
        let opts = RenderOpts {
            expand: false,
            max_entry_lines: 4,
        };
        let rows = render_entry(&kind, 40, &mut engine, opts);
        let text = rows.iter().map(row_text).collect::<Vec<_>>().join("\n");
        assert!(text.contains("only one line"));
        assert!(!text.contains("Ctrl+O"));
    }

    #[test]
    fn layout_cache_toggle_expand_output_changes_the_cache_key() {
        // The whole expand feature hinges on this: toggling must not return a
        // stale cached (collapsed) layout for an entry already rendered once.
        let mut cache = LayoutCache::new(64, 4);
        let mut t = Transcript::new(TuiLimits::default());
        t.ingest(TuiEntry::now(EntryKind::ToolResult {
            tool: "Bash".to_string(),
            detail: (0..20)
                .map(|i| format!("line {i}"))
                .collect::<Vec<_>>()
                .join("\n"),
            success: Some(true),
            command: None,
        }));
        let entry = &t.entries()[0];
        cache.rows_for(entry, 40);
        let collapsed = cache.layout(entry, 40).expect("cached").to_vec();
        cache.toggle_expand_output();
        cache.rows_for(entry, 40);
        let expanded = cache.layout(entry, 40).expect("cached").to_vec();
        assert_ne!(
            collapsed.len(),
            expanded.len(),
            "expanded layout must differ from the collapsed one, not reuse a stale cache entry"
        );
    }

    #[test]
    fn a_bash_call_followed_by_an_unrelated_read_result_renders_two_clean_boxes() {
        // Regression: parallel tool calls (e.g. Read + Bash issued together,
        // results arriving in any order) must never merge into one garbled
        // box. Each ToolResult opens and closes its own box regardless of
        // what other entries surround it.
        let mut engine = crate::highlight::HighlightEngine::new();
        let read_call = EntryKind::ToolCall {
            tool: "Read".to_string(),
            detail: "SPEC.md".to_string(),
            input: serde_json::Value::Null,
        };
        let bash_call = EntryKind::ToolCall {
            tool: "Bash".to_string(),
            detail: "git diff".to_string(),
            input: serde_json::Value::Null,
        };
        let read_result = EntryKind::ToolResult {
            tool: "Read".to_string(),
            detail: "line one\nline two".to_string(),
            success: Some(true),
            command: Some("SPEC.md".to_string()),
        };
        let bash_result = EntryKind::ToolResult {
            tool: "Bash".to_string(),
            detail: "diff output".to_string(),
            success: Some(true),
            command: Some("git diff".to_string()),
        };
        // Neither call renders anything (the result's box header carries the
        // command instead), so their order/adjacency can't corrupt anything;
        // each result independently opens and closes its own box.
        for kind in [&read_call, &bash_call] {
            assert_eq!(
                render_entry(kind, 40, &mut engine, RenderOpts::default()),
                Vec::<Line>::new()
            );
        }
        for (kind, expected_header) in [(&read_result, "SPEC.md"), (&bash_result, "$ git diff")] {
            let rows = render_entry(kind, 40, &mut engine, RenderOpts::default());
            assert!(row_text(&rows[0]).starts_with('╭'));
            // The correlated command is the box's own header — not a
            // separate, possibly-disconnected line above it.
            assert!(
                row_text(&rows[0]).contains(expected_header),
                "got {:?}",
                row_text(&rows[0])
            );
            // Last row is the trailing spacer blank; the one before it closes the box.
            let close = &rows[rows.len() - 2];
            assert!(
                row_text(close).starts_with('╰'),
                "got {:?}",
                row_text(close)
            );
        }
    }

    #[test]
    fn edit_tool_result_stays_a_plain_unboxed_line() {
        // The real diff renders as its own FileEdit card; the Edit tool's own
        // result should stay a lightweight confirmation, not a second box.
        let mut engine = crate::highlight::HighlightEngine::new();
        let kind = EntryKind::ToolResult {
            tool: "Edit".to_string(),
            detail: "[src/lib.rs#abcd]".to_string(),
            success: Some(true),
            command: None,
        };
        let rows = render_entry(&kind, 40, &mut engine, RenderOpts::default());
        assert_eq!(rows.len(), 1);
        assert!(!row_text(&rows[0]).contains("Output"));
        assert!(rows[0].spans.iter().all(|s| s.style.bg.is_none()));
    }

    // ── Inline prose Markdown (render_prose) ────────────────────────────────────

    fn all_spans<'a>(lines: &'a [Line<'a>]) -> Vec<&'a Span<'a>> {
        lines.iter().flat_map(|l| l.spans.iter()).collect()
    }

    #[test]
    fn render_prose_colors_backtick_code_and_strips_the_markers() {
        let base = Style::default().fg(Color::White);
        let lines = render_prose("call `FeatureSnapshot::new` now", base);
        let spans = all_spans(&lines);
        let joined: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(
            joined, "call FeatureSnapshot::new now",
            "backticks are stripped"
        );
        let code = spans
            .iter()
            .find(|s| s.content.as_ref() == "FeatureSnapshot::new")
            .expect("code span present");
        assert_eq!(code.style.fg, Some(theme::active().md_code));
    }

    #[test]
    fn render_prose_bolds_double_asterisk_and_strips_the_markers() {
        let base = Style::default().fg(Color::White);
        let lines = render_prose("this is **important** context", base);
        let spans = all_spans(&lines);
        let joined: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(joined, "this is important context");
        let bold = spans
            .iter()
            .find(|s| s.content.as_ref() == "important")
            .expect("bold span present");
        assert!(bold.style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(
            bold.style.fg,
            Some(Color::White),
            "bold keeps the base colour"
        );
    }

    #[test]
    fn render_prose_preserves_the_base_style_on_plain_text() {
        let base = Style::default()
            .fg(Color::Gray)
            .add_modifier(Modifier::ITALIC);
        let lines = render_prose("plain sentence, no markdown", base);
        let spans = all_spans(&lines);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].style, base);
        assert_eq!(spans[0].content.as_ref(), "plain sentence, no markdown");
    }

    #[test]
    fn render_prose_code_span_keeps_italic_from_thinking_style() {
        // Thinking text is italic; a `code` span inside it should stay
        // italic while swapping only the foreground to green — Style::patch
        // fills fg from the code style but unions modifiers, so the base
        // italic survives.
        let base = Style::default()
            .fg(Color::Gray)
            .add_modifier(Modifier::ITALIC);
        let lines = render_prose("still thinking about `FeatureValues`", base);
        let spans = all_spans(&lines);
        let code = spans
            .iter()
            .find(|s| s.content.as_ref() == "FeatureValues")
            .expect("code span present");
        assert_eq!(code.style.fg, Some(theme::active().md_code));
        assert!(code.style.add_modifier.contains(Modifier::ITALIC));
    }

    #[test]
    fn render_prose_handles_an_unterminated_marker_without_panicking() {
        let base = Style::default().fg(Color::White);
        let lines = render_prose("dangling `backtick with no close", base);
        let spans = all_spans(&lines);
        let joined: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(joined.contains("dangling"));
        assert!(joined.contains("backtick with no close"));
    }

    // ── Tool-result output syntax inference/highlighting ────────────────────

    #[test]
    fn infer_output_syntax_uses_the_read_path_directly() {
        assert_eq!(
            infer_output_syntax("Read", Some("src/lib.rs")),
            Some(velor_core::file_edit::SyntaxKind::Rust)
        );
    }

    #[test]
    fn infer_output_syntax_unwraps_a_cat_command() {
        assert_eq!(
            infer_output_syntax("Bash", Some("cat src/lib.rs")),
            Some(velor_core::file_edit::SyntaxKind::Rust)
        );
        assert_eq!(
            infer_output_syntax("Bash", Some("bat -n src/lib.rs")),
            Some(velor_core::file_edit::SyntaxKind::Rust)
        );
    }

    #[test]
    fn infer_output_syntax_is_none_for_unrecognised_commands() {
        assert_eq!(infer_output_syntax("Bash", Some("git diff")), None);
        assert_eq!(infer_output_syntax("Grep", Some("TODO")), None);
        assert_eq!(infer_output_syntax("Bash", None), None);
    }

    #[test]
    fn infer_output_syntax_strips_a_line_range_suffix() {
        // Regression: some providers (omp) append ":start-end" to a Read
        // call's path; naive extension detection saw "rs:2728-2871" and fell
        // back to plain text instead of Rust.
        assert_eq!(
            infer_output_syntax(
                "read",
                Some("crates/domain/aq-fundamentals/src/lib.rs:2728-2871")
            ),
            Some(velor_core::file_edit::SyntaxKind::Rust)
        );
        assert_eq!(
            infer_output_syntax("read", Some("src/lib.rs:254")),
            Some(velor_core::file_edit::SyntaxKind::Rust)
        );
    }

    #[test]
    fn strip_line_range_suffix_only_strips_genuine_ranges() {
        assert_eq!(
            strip_line_range_suffix("src/lib.rs:2728-2871"),
            "src/lib.rs"
        );
        assert_eq!(strip_line_range_suffix("src/lib.rs:254"), "src/lib.rs");
        assert_eq!(strip_line_range_suffix("src/lib.rs"), "src/lib.rs");
        // A Windows drive letter isn't a line range (suffix isn't all digits).
        assert_eq!(strip_line_range_suffix(r"C:\Users\x"), r"C:\Users\x");
    }

    #[test]
    fn highlight_plain_line_classifies_rust_keywords() {
        let mut engine = crate::highlight::HighlightEngine::new();
        let spans = highlight_plain_line(
            "fn main() {}",
            &mut engine,
            velor_core::file_edit::SyntaxKind::Rust,
            Color::Gray,
        );
        let joined: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(joined, "fn main() {}", "no bytes lost/reordered");
        let kw = spans
            .iter()
            .find(|s| s.content.as_ref() == "fn")
            .expect("the `fn` keyword is its own span");
        assert_ne!(
            kw.style.fg,
            Some(Color::Gray),
            "a keyword gets a real colour"
        );
    }

    #[test]
    fn highlight_plain_line_falls_back_to_default_fg_for_unclassified_text() {
        let mut engine = crate::highlight::HighlightEngine::new();
        let spans = highlight_plain_line(
            "plain text, no code",
            &mut engine,
            velor_core::file_edit::SyntaxKind::PlainText,
            Color::Gray,
        );
        assert!(spans.iter().all(|s| s.style.fg == Some(Color::Gray)));
    }

    #[test]
    fn read_tool_result_output_is_syntax_highlighted() {
        let mut engine = crate::highlight::HighlightEngine::new();
        let kind = EntryKind::ToolResult {
            tool: "Read".to_string(),
            detail: "fn main() {}".to_string(),
            success: Some(true),
            command: Some("src/lib.rs".to_string()),
        };
        let rows = render_entry(&kind, 60, &mut engine, RenderOpts::default());
        // Row 2 is the (only) output line: rule, divider, output, rule, blank.
        let has_keyword_colour = rows[2]
            .spans
            .iter()
            .any(|s| s.content.as_ref() == "fn" && s.style.fg != Some(Color::Gray));
        assert!(
            has_keyword_colour,
            "expected a highlighted `fn` span: {:?}",
            rows[2]
        );
    }

    #[test]
    fn read_tool_result_of_a_markdown_file_renders_as_markdown() {
        let mut engine = crate::highlight::HighlightEngine::new();
        let kind = EntryKind::ToolResult {
            tool: "Read".to_string(),
            detail: "# Heading\n\ncall `FeatureSnapshot::new` and **be careful**.".to_string(),
            success: Some(true),
            command: Some("SPEC.md".to_string()),
        };
        let rows = render_entry(&kind, 80, &mut engine, RenderOpts::default());
        let text = rows.iter().map(row_text).collect::<Vec<_>>().join("\n");
        assert!(
            !text.contains('*'),
            "bold markers must be stripped, got {text:?}"
        );
        assert!(
            !text.contains('`'),
            "code markers must be stripped, got {text:?}"
        );
        assert!(text.contains("FeatureSnapshot::new"));
        assert!(text.contains("be careful"));
        // The inline-code span still gets the theme's code colour, same as
        // in the agent's own prose.
        let has_code_colour = rows.iter().flat_map(|l| l.spans.iter()).any(|s| {
            s.content.as_ref() == "FeatureSnapshot::new"
                && s.style.fg == Some(theme::active().md_code)
        });
        assert!(
            has_code_colour,
            "expected the inline-code span to use md_code"
        );
    }

    #[test]
    fn read_tool_result_of_a_non_markdown_file_keeps_raw_syntax_highlighting() {
        // Regression: markdown detection must not accidentally swallow code
        // files — a Rust file's `//` comment or `fn` keyword must not be
        // run through the markdown inline parser.
        let mut engine = crate::highlight::HighlightEngine::new();
        let kind = EntryKind::ToolResult {
            tool: "Read".to_string(),
            detail: "fn main() {}".to_string(),
            success: Some(true),
            command: Some("src/lib.rs".to_string()),
        };
        let rows = render_entry(&kind, 60, &mut engine, RenderOpts::default());
        let text = rows.iter().map(row_text).collect::<Vec<_>>().join("\n");
        assert!(text.contains("fn main() {}"));
    }

    #[test]
    fn failed_tool_result_output_is_not_syntax_highlighted() {
        // The plain-red failure signal shouldn't compete with syntax colour.
        let mut engine = crate::highlight::HighlightEngine::new();
        let kind = EntryKind::ToolResult {
            tool: "Read".to_string(),
            detail: "fn main() {}".to_string(),
            success: Some(false),
            command: Some("src/lib.rs".to_string()),
        };
        let rows = render_entry(&kind, 60, &mut engine, RenderOpts::default());
        // Unhighlighted output is a single content span (not split into
        // per-token pieces like the highlighted case), styled entirely red.
        let content = rows[2]
            .spans
            .iter()
            .find(|s| s.content.as_ref() == "fn main() {}")
            .expect("the whole line is one unsplit span");
        assert_eq!(content.style.fg, Some(theme::active().error));
    }

    // ── Sticky todo panel ────────────────────────────────────────────────────

    #[test]
    fn is_todo_tool_matches_known_provider_names() {
        assert!(is_todo_tool("todo"));
        assert!(is_todo_tool("TodoWrite"));
        assert!(is_todo_tool("TodoRead"));
        assert!(!is_todo_tool("Bash"));
        assert!(!is_todo_tool("Read"));
    }

    #[test]
    fn todo_summary_from_input_builds_a_marked_checklist() {
        let input = serde_json::json!({
            "todos": [
                {"content": "Ship the feature", "status": "completed"},
                {"content": "Write tests", "status": "in_progress"},
                {"content": "Update docs", "status": "pending"},
            ]
        });
        let summary = todo_summary_from_input(&input).expect("summary present");
        assert_eq!(
            summary,
            "[x] Ship the feature\n[~] Write tests\n[ ] Update docs"
        );
    }

    #[test]
    fn todo_summary_from_input_is_none_without_a_todos_array() {
        assert!(todo_summary_from_input(&serde_json::json!({"op": "done"})).is_none());
        assert!(todo_summary_from_input(&serde_json::json!({"todos": []})).is_none());
    }

    #[test]
    fn substantial_todo_summary_distinguishes_board_dumps_from_short_acks() {
        assert!(is_substantial_todo_summary(
            "Remaining items (1):\n  - Update handoff\nOverall: 6/7 done, 1 open."
        ));
        assert!(is_substantial_todo_summary(&"x".repeat(61)));
        assert!(!is_substantial_todo_summary("Todos updated successfully."));
        assert!(!is_substantial_todo_summary(""));
        assert!(!is_substantial_todo_summary("   "));
    }

    #[test]
    fn current_task_label_extracts_the_in_progress_item() {
        let mut state = TuiState::new(TuiLimits::default());
        state.sticky_todo = Some("[x] Ship the feature\n[~] Write tests\n[ ] Update docs".into());
        assert_eq!(current_task_label(&state).as_deref(), Some("Write tests"));
    }

    #[test]
    fn current_task_label_is_none_without_an_in_progress_item() {
        let mut state = TuiState::new(TuiLimits::default());
        assert!(current_task_label(&state).is_none());
        state.sticky_todo = Some("[x] Ship the feature\n[ ] Update docs".into());
        assert!(current_task_label(&state).is_none());
    }

    #[test]
    fn current_task_label_truncates_long_tasks() {
        let mut state = TuiState::new(TuiLimits::default());
        let long_task = "x".repeat(100);
        state.sticky_todo = Some(format!("[~] {long_task}"));
        let label = current_task_label(&state).expect("label present");
        assert_eq!(label.chars().count(), TITLE_TASK_MAX_CHARS + 1);
        assert!(label.ends_with('…'));
    }

    #[test]
    fn window_title_includes_the_current_task_when_present() {
        let mut state = TuiState::new(TuiLimits::default());
        state.spinner_verb = "editing";
        assert_eq!(window_title(&state), "vel auto — editing");
        state.sticky_todo = Some("[~] Refactor the parser".into());
        assert_eq!(
            window_title(&state),
            "vel auto — Refactor the parser — editing"
        );
    }

    #[test]
    fn highlight_matches_returns_one_span_for_an_empty_query() {
        let base = Style::default();
        let hit = Style::default().add_modifier(Modifier::BOLD);
        let spans = highlight_matches("Toggle stopping", "", base, hit);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content, "Toggle stopping");
        assert_eq!(spans[0].style, base);
    }

    #[test]
    fn highlight_matches_splits_around_each_match_case_insensitively() {
        let base = Style::default();
        let hit = Style::default().add_modifier(Modifier::BOLD);
        let spans = highlight_matches("Toggle STOP after", "stop", base, hit);
        let rendered: Vec<(String, bool)> = spans
            .iter()
            .map(|s| (s.content.to_string(), s.style == hit))
            .collect();
        assert_eq!(
            rendered,
            vec![
                ("Toggle ".to_string(), false),
                ("STOP".to_string(), true),
                (" after".to_string(), false),
            ]
        );
    }

    #[test]
    fn highlight_matches_handles_repeated_matches() {
        let base = Style::default();
        let hit = Style::default().add_modifier(Modifier::BOLD);
        let spans = highlight_matches("aXaXa", "a", base, hit);
        let joined: String = spans.iter().map(|s| s.content.to_string()).collect();
        assert_eq!(joined, "aXaXa");
        assert_eq!(spans.iter().filter(|s| s.style == hit).count(), 3);
    }

    #[test]
    fn help_entry_matches_checks_both_key_and_description_case_insensitively() {
        assert!(help_entry_matches("Ctrl+O", "expand output", ""));
        assert!(help_entry_matches("Ctrl+O", "expand output", "ctrl"));
        assert!(help_entry_matches("Ctrl+O", "expand output", "expand"));
        assert!(!help_entry_matches("Ctrl+O", "expand output", "steer"));
    }

    #[test]
    fn help_keybindings_cover_the_search_key_itself() {
        assert!(
            HELP_KEYBINDINGS.iter().any(|(k, _)| *k == "/"),
            "the help modal's own search keybinding should be documented in its list"
        );
    }

    #[test]
    fn sticky_todo_height_is_zero_with_no_todo() {
        assert_eq!(sticky_todo_height(None, 40, false), 0);
        assert_eq!(sticky_todo_height(Some(""), 40, false), 0);
        assert_eq!(sticky_todo_height(Some("   "), 40, false), 0);
        assert_eq!(sticky_todo_height(None, 40, true), 0);
    }

    #[test]
    fn sticky_todo_height_fits_short_content_plus_border() {
        assert_eq!(sticky_todo_height(Some("[x] one\n[ ] two"), 40, false), 4);
        // Short content fits the same either way — expanding only matters
        // once the board exceeds the collapsed cap.
        assert_eq!(sticky_todo_height(Some("[x] one\n[ ] two"), 40, true), 4);
    }

    #[test]
    fn sticky_todo_height_caps_at_max_lines_plus_border() {
        let long = (0..50)
            .map(|i| format!("[ ] item {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            sticky_todo_height(Some(&long), 40, false),
            (MAX_STICKY_TODO_LINES_COLLAPSED + 2) as u16
        );
    }

    #[test]
    fn sticky_todo_height_expanded_shows_more_but_is_still_capped() {
        let long = (0..50)
            .map(|i| format!("[ ] item {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let expanded = sticky_todo_height(Some(&long), 40, true);
        let collapsed = sticky_todo_height(Some(&long), 40, false);
        assert!(expanded > collapsed, "Ctrl+T must actually show more");
        assert_eq!(expanded, (MAX_STICKY_TODO_LINES_EXPANDED + 2) as u16);
    }

    #[test]
    fn sticky_todo_height_never_exceeds_the_short_terminal_budget() {
        // terminal_height=10: budget = 10 - (3 log min + 1 spinner + 1 hints) = 5.
        let long = (0..20)
            .map(|i| format!("[ ] item {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(sticky_todo_height(Some(&long), 10, false), 5);
        // The budget caps expanded height too — it can't crowd out the log
        // area's guaranteed minimum even when the user asks for more.
        assert_eq!(sticky_todo_height(Some(&long), 10, true), 5);
    }

    #[test]
    fn todo_line_span_colours_by_checklist_marker() {
        let theme = theme::active();
        assert_eq!(
            todo_line_span("[x] done").spans[0].style.fg,
            Some(theme.success)
        );
        assert_eq!(
            todo_line_span("[~] in progress").spans[0].style.fg,
            Some(theme.warning)
        );
        assert_eq!(
            todo_line_span("[ ] pending").spans[0].style.fg,
            Some(theme.muted)
        );
        assert_eq!(
            todo_line_span("Remaining items (1):").spans[0].style.fg,
            theme.text
        );
    }

    // ── File-edit header ─────────────────────────────────────────────────────

    #[test]
    fn elide_middle_keeps_short_strings_intact() {
        assert_eq!(elide_middle("short.rs", 20), "short.rs");
    }

    #[test]
    fn elide_middle_shortens_long_paths_around_an_ellipsis() {
        let long = "a/very/long/repo/relative/path/that/keeps/going/lib.rs";
        let elided = elide_middle(long, 20);
        assert!(elided.chars().count() <= 20);
        assert!(elided.contains('…'));
        assert!(elided.starts_with("a/very"));
        assert!(elided.ends_with("lib.rs"));
    }

    #[test]
    fn diff_stat_counts_additions_and_removals() {
        let edit = velor_core::file_edit::compute_file_edit(
            "src/lib.rs",
            Some(b"fn one() {}\nfn two() {}\nfn three() {}\n"),
            Some(b"fn one() {}\nfn TWO() {}\nfn three() {}\n"),
            velor_core::file_edit::DEFAULT_MAX_DIFF_LINES,
        )
        .expect("a real change produces an edit");
        assert_eq!(diff_stat(&edit), (1, 1));
    }

    // ── Steer/append modal sizing ────────────────────────────────────────────

    #[test]
    fn append_modal_wraps_long_input_across_multiple_rows_and_keeps_tail_visible() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let long_buffer = "word ".repeat(60) + "TAIL";
        terminal
            .draw(|f| {
                let area = f.area();
                render_steering_modal(f, area, &long_buffer, &SubmissionState::Editing, true)
            })
            .expect("draw");
        let buf = terminal.backend().buffer();

        let mut rows_with_word = 0;
        let mut tail_visible = false;
        for y in 0..buf.area.height {
            let mut row = String::new();
            for x in 0..buf.area.width {
                row.push_str(buf[(x, y)].symbol());
            }
            if row.contains("word") {
                rows_with_word += 1;
            }
            if row.contains("TAIL") {
                tail_visible = true;
            }
        }
        assert!(
            rows_with_word > 1,
            "a long append buffer should wrap across multiple visible rows, got {rows_with_word}"
        );
        assert!(
            tail_visible,
            "the tail of the input (where typing happens) must stay visible, not be clipped or scrolled off"
        );
    }

    #[test]
    fn append_modal_stays_at_minimum_size_for_a_short_buffer() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| {
                let area = f.area();
                render_steering_modal(f, area, "short append", &SubmissionState::Editing, true)
            })
            .expect("draw");
        let buf = terminal.backend().buffer();

        let mut nonblank_rows = 0;
        for y in 0..buf.area.height {
            let mut row = String::new();
            for x in 0..buf.area.width {
                row.push_str(buf[(x, y)].symbol());
            }
            if !row.trim().is_empty() {
                nonblank_rows += 1;
            }
        }
        // Popup border rows are always non-blank, plus a handful of content
        // rows; a short buffer shouldn't balloon the popup to near-fullscreen.
        assert!(
            nonblank_rows <= (MIN_MODAL_INNER_ROWS + 2) as usize,
            "a short buffer should keep the popup at its minimum size, got {nonblank_rows} non-blank rows"
        );
    }
}
