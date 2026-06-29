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
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};
use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;
use syntect::util::as_24_bit_terminal_escaped;
use tokio_util::sync::CancellationToken;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::tui_transcript::{
    self, LiveEntry, RowMeter, ScrollState, Transcript, TuiLimits, Viewport,
};

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

// ── Messages ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum TuiMessage {
    Entry(TuiEntry),
    SetPrompt(String),
    SetLogPath(String),
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
            detail, success, ..
        } => Some(TuiEntry::now(EntryKind::ToolResult {
            detail: detail.clone(),
            success: *success,
        })),
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

struct TuiState {
    transcript: Transcript,
    scroll: ScrollState,
    cache: LayoutCache,
    prompt: Option<String>,
    log_path: Option<String>,
    show_prompt: bool,
    show_help: bool,
    show_errors: bool,
    prompt_scroll: u16,
    error_scroll: u16,
    spinner_idx: usize,
    spinner_verb: &'static str,
    show_thinking: bool,
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
    /// Entry ids marking where each iteration began, in order. Used to resolve
    /// "this iteration" from the viewport for `gg`/`G` navigation. The first
    /// entry ingested after an iteration change is recorded here.
    iteration_starts: Vec<crate::tui_transcript::EntryId>,
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
}

impl TuiState {
    fn new(limits: TuiLimits) -> Self {
        Self {
            transcript: Transcript::new(limits),
            scroll: ScrollState::Tail,
            cache: LayoutCache::new(CACHE_CAPACITY),
            prompt: None,
            log_path: None,
            show_prompt: false,
            show_help: false,
            show_errors: false,
            prompt_scroll: 0,
            error_scroll: 0,
            spinner_idx: 0,
            spinner_verb: "starting",
            show_thinking: true,
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
        }
    }

    /// Scrolls toward older content by `n` rows; stable under streaming/trimming.
    fn scroll_up_by(&mut self, n: u32) {
        let entries = self.transcript.entries();
        let show = self.show_thinking;
        let pred = move |e: &LiveEntry| !matches!(e.kind, EntryKind::Thinking(_)) || show;
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
        let show = self.show_thinking;
        let pred = move |e: &LiveEntry| !matches!(e.kind, EntryKind::Thinking(_)) || show;
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
    /// `(start_id, end_id)` boundary ids. `end_id` is the entry just before the
    /// next iteration begins (or the newest entry for the last iteration).
    fn current_iteration_bounds(
        &self,
    ) -> Option<(
        crate::tui_transcript::EntryId,
        crate::tui_transcript::EntryId,
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
        // `iteration_starts` are entry ids; resolve each to its retained index.
        let mut start_idx = 0;
        let mut start_id = entries[0].id;
        for &id in &self.iteration_starts {
            if let Some(i) = entries.iter().position(|e| e.id == id) {
                if i <= view_idx {
                    start_idx = i;
                    start_id = entries[i].id;
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
            .filter_map(|&id| entries.iter().position(|e| e.id == id))
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
        Some((begin, end_id))
    }

    /// `gg` — jump to the top of the iteration currently in view.
    fn jump_to_iteration_top(&mut self) {
        match self.current_iteration_bounds() {
            Some((start_id, _)) => self.jump_to_entry(start_id),
            None => self.jump_to_absolute_top(),
        }
    }

    /// `G` — jump to the bottom of the iteration currently in view. For the
    /// latest iteration this is the live tail, so it re-enables streaming.
    fn jump_to_iteration_bottom(&mut self) {
        let Some((_, end_id)) = self.current_iteration_bounds() else {
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
}

/// The wrapped, timestamped display rows for one entry at one width.
struct CachedLayout {
    rows: Vec<Line<'static>>,
}

/// A bounded FIFO cache of per-entry rendered layouts. Keyed by entry id +
/// revision + width, so it naturally misses when an entry streams (rev bumps)
/// or the terminal is resized (width changes). Capacity-bounded by eviction.
struct LayoutCache {
    map: HashMap<CacheKey, CachedLayout>,
    order: VecDeque<CacheKey>,
    capacity: usize,
    syntax: Syntax,
    /// Entries rendered since construction (cache misses). Instrumented so tests
    /// can assert per-frame work is bounded by the viewport, not total history.
    renders: usize,
}

impl LayoutCache {
    fn new(capacity: usize) -> Self {
        Self {
            map: HashMap::new(),
            order: VecDeque::new(),
            capacity,
            syntax: Syntax::new(),
            renders: 0,
        }
    }

    /// Ensures a layout exists for `entry` at `width`, returning its row count.
    /// Rendering + wrapping happen on cache miss only. This is the [`RowMeter`]
    /// hook the viewport selector uses.
    fn rows_for(&mut self, entry: &LiveEntry, width: u16) -> u32 {
        let key = CacheKey {
            id: entry.id.raw(),
            rev: entry.rev,
            width,
        };
        if !self.map.contains_key(&key) {
            let rows = build_layout(&entry.kind, entry.ts, width, &self.syntax);
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
fn build_layout(
    kind: &EntryKind,
    ts: DateTime<Local>,
    width: u16,
    syntax: &Syntax,
) -> Vec<Line<'static>> {
    let content_lines = render_entry(kind, width as usize, syntax);
    let no_timestamp = matches!(kind, EntryKind::IterationDivider { .. });
    let ts_span = Span::styled(
        ts.format("%H:%M:%S ").to_string(),
        Style::default().fg(Color::DarkGray),
    );
    let w = (width as usize).max(1);
    let mut rows: Vec<Line<'static>> = Vec::new();
    for (i, line) in content_lines.into_iter().enumerate() {
        let mut spans: Vec<Span<'static>> = Vec::with_capacity(line.spans.len() + 1);
        if i == 0 && !no_timestamp {
            spans.push(ts_span.clone());
        }
        spans.extend(line.spans);
        rows.extend(wrap_spans(&spans, w));
    }
    if rows.is_empty() {
        rows.push(Line::default());
    }
    rows
}

/// Greedily wraps a sequence of styled spans to `width` display columns, never
/// splitting a character. Empty input yields one empty line.
fn wrap_spans(spans: &[Span<'static>], width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
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

struct Syntax {
    set: SyntaxSet,
    theme: syntect::highlighting::Theme,
}

impl Syntax {
    fn new() -> Self {
        Self {
            set: SyntaxSet::load_defaults_newlines(),
            theme: ThemeSet::load_defaults().themes["base16-ocean.dark"].clone(),
        }
    }

    /// Highlights a line of code, returning owned styled ratatui spans.
    fn highlight_line(&self, syntax_name: &str, line: &str) -> Vec<Span<'static>> {
        let syntax = self
            .set
            .find_syntax_by_extension(syntax_name)
            .or_else(|| self.set.find_syntax_by_name(syntax_name))
            .unwrap_or_else(|| self.set.find_syntax_plain_text());
        let mut h = HighlightLines::new(syntax, &self.theme);
        let regions = h.highlight_line(line, &self.set).unwrap_or_default();
        let escaped = as_24_bit_terminal_escaped(&regions[..], false);

        // ratatui doesn't natively consume ANSI escapes, so strip them and rely
        // on the diff +/- prefix for colour. A proper ANSI→Color integration is
        // future work.
        let plain = strip_ansi(&escaped);
        vec![Span::raw(plain)]
    }
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if c == 'm' {
                        break;
                    }
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn syntax_for_file(path: &str) -> &str {
    let ext = path.rsplit('.').next().unwrap_or("");
    match ext {
        "rs" => "rs",
        "py" => "py",
        "js" | "mjs" | "ts" | "tsx" | "jsx" => "js",
        "go" => "go",
        "java" => "java",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" | "hxx" => "cpp",
        "sh" | "bash" => "sh",
        "toml" => "toml",
        "json" => "json",
        "yaml" | "yml" => "yaml",
        "md" => "md",
        "html" => "html",
        "css" => "css",
        "sql" => "sql",
        "dockerfile" => "dockerfile",
        _ => "txt",
    }
}

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

    loop {
        while let Ok(msg) = rx.try_recv() {
            match msg {
                TuiMessage::Entry(e) => {
                    // Update spinner verb + token usage from the event kind, then
                    // ingest (coalesces/bounds/trims; transient Usage is dropped).
                    match &e.kind {
                        EntryKind::ToolCall { tool, .. } => {
                            state.spinner_verb = match tool.as_str() {
                                "Bash" => "running command",
                                "Read" => "reading file",
                                "Edit" | "Write" => "editing",
                                "Grep" => "searching",
                                "Glob" => "finding files",
                                _ => "working",
                            };
                            set_title(&format!("vel auto — {}", state.spinner_verb));
                        }
                        EntryKind::ToolResult { .. } => {
                            state.spinner_verb = "thinking";
                            set_title("vel auto — thinking");
                        }
                        EntryKind::Text(_) => {
                            state.spinner_verb = "generating";
                            set_title("vel auto — generating");
                        }
                        EntryKind::Thinking(_) => {
                            state.spinner_verb = "reasoning";
                            set_title("vel auto — reasoning");
                        }
                        EntryKind::Error(_) => {
                            state.spinner_verb = "error";
                            set_title("vel auto — error");
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
                        // Dividers are visual separators; they carry no spinner
                        // verb or usage payload.
                        EntryKind::IterationDivider { .. } => {}
                    }
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
                            state.iteration_starts.push(divider.id);
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
                    break;
                }
            }
        }

        // Poll any pending steering/append acknowledgement (non-blocking) so
        // submissions resolve within the polling loop.
        state.poll_pending_submission();

        terminal
            .draw(|f| render(f, &mut state, &cancel_handler))
            .wrap_err("draw")?;

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
    // The help modal closes on any key, and swallows the keypress.
    if state.show_help {
        state.show_help = false;
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
        KeyCode::Char('l') => {
            state.open_log = true;
        }
        // `g` begins a prefix chord: `gg` → top of this iteration,
        // `gT` → absolute top of the chat, `gB` → absolute bottom (live tail).
        KeyCode::Char('g') => state.begin_g(),
        // `G` → bottom of this iteration (live tail when viewing the latest).
        KeyCode::Char('G') => state.jump_to_iteration_bottom(),
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

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(area);
    let log_area = chunks[0];
    let spinner_area = chunks[1];
    let hints_area = chunks[2];

    state.spinner_idx = (state.spinner_idx + 1) % SPINNER.len();

    // Content width excludes the left/right borders. Layouts are pre-wrapped to
    // this width, so the Paragraph needs no wrap pass.
    let content_width = log_area.width.saturating_sub(2);
    state.last_width = content_width;
    let viewport_rows = log_area.height.saturating_sub(2) as u32;
    state.last_viewport_rows = viewport_rows;

    let entries = state.transcript.entries();
    let show_thinking = state.show_thinking;
    let pred = move |e: &LiveEntry| !matches!(e.kind, EntryKind::Thinking(_)) || show_thinking;

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
            if matches!(entry.kind, EntryKind::Thinking(_)) && !show_thinking {
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
            Span::styled("↑ ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!(
                    "Earlier transcript content omitted from the live view — {} entries, ~{} KiB; full history remains in the run log.",
                    omitted.entries,
                    omitted.bytes / 1024
                ),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            ),
        ]);
        visible_lines.insert(0, marker);
    }

    let error_count = entries
        .iter()
        .filter(|e| matches!(e.kind, EntryKind::Error(_)))
        .count();

    let title = build_title(state, entries.len(), error_count);
    let para =
        Paragraph::new(visible_lines).block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(para, log_area);

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
        render_help_modal(f, area);
    }
}

/// Renders the `c` steering / `a` append editor modal.
fn render_steering_modal(
    f: &mut Frame,
    area: Rect,
    buffer: &str,
    submission: &SubmissionState,
    is_append: bool,
) {
    let popup = center_rect(area, 80, 9);
    f.render_widget(Clear, popup);
    let (title, prefix) = if is_append {
        (" ✏️  Append (Enter=save · Esc=cancel) ", "append › ")
    } else {
        (" 🎯  Steer (Enter=send · Esc=cancel) ", "steer › ")
    };
    let prompt_style = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let mut lines: Vec<Line> = Vec::new();
    // Compose the input line, wrapping long buffers across rows.
    let input = format!("{prefix}{buffer}");
    let input_line = Line::from(vec![Span::styled(input, prompt_style)]);
    lines.push(input_line);
    let footer = match submission {
        SubmissionState::Editing => Line::from(Span::styled(
            if is_append {
                "Empty clears the append. It is folded into every later iteration."
            } else {
                "Sends one message to the active session. Not replayed in later iterations."
            },
            Style::default().fg(Color::DarkGray),
        )),
        SubmissionState::Submitting => Line::from(Span::styled(
            "Submitting…",
            Style::default().fg(Color::Yellow),
        )),
        SubmissionState::Failed { message } => Line::from(vec![
            Span::styled("✗ ", Style::default().fg(Color::Red)),
            Span::styled(message.clone(), Style::default().fg(Color::Red)),
        ]),
    };
    lines.push(Line::from(""));
    lines.push(footer);
    let para = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(Style::default().fg(Color::Cyan)),
    );
    f.render_widget(para, popup);
}

/// Renders a transient one-line status near the bottom of the screen.
fn render_transient_status(f: &mut Frame, area: Rect, msg: &str) {
    let height = area.height.max(3);
    let y = height.saturating_sub(3);
    let rect = Rect::new(area.x, area.y + y, area.width, 1);
    f.render_widget(Clear, rect);
    let line = Line::from(vec![
        Span::styled("ⓘ ", Style::default().fg(Color::Cyan)),
        Span::styled(msg.to_string(), Style::default().fg(Color::Gray)),
    ]);
    f.render_widget(Paragraph::new(line), rect);
}

/// Builds the log-area title: event count, scroll state, retained size, and
/// compact omitted/error indicators.
fn build_title(state: &TuiState, n: usize, error_count: usize) -> String {
    let scroll_label = match state.scroll {
        ScrollState::Tail => "live",
        ScrollState::Anchored { .. } => "history",
    };
    let kib = state.transcript.retained_bytes() / 1024;
    let mut s = format!(" vel auto — {n} events · {kib} KiB live ");
    if error_count > 0 {
        s += &format!("· ⚠ {error_count} err ");
    }
    let omitted = state.transcript.omitted();
    if !omitted.is_empty() {
        s += &format!("· {} trimmed (in log) ", omitted.entries);
    }
    s += scroll_label;
    s.push(' ');
    s
}

fn render_spinner(f: &mut Frame, state: &TuiState, area: Rect) {
    let spinner = SPINNER[state.spinner_idx];
    let cached_pct = (state.cached_tokens * 100)
        .checked_div(state.input_tokens)
        .unwrap_or(0);
    let mut spans = vec![
        Span::styled(format!("{spinner} "), Style::default().fg(Color::Cyan)),
        Span::styled(
            state.spinner_verb,
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
        ),
        Span::raw("…  "),
        Span::styled(
            format!(
                "↑ {} ↓ {} · {}% cached",
                fmt_tokens(state.input_tokens),
                fmt_tokens(state.output_tokens),
                cached_pct
            ),
            Style::default().fg(Color::DarkGray),
        ),
    ];
    if let Some((current, total)) = state.iteration {
        spans.push(Span::raw("  ·  "));
        spans.push(Span::styled(
            format!("🔁 {current}/{total}"),
            Style::default().fg(Color::Yellow),
        ));
    }
    // Persistent-append indicator (set by the controller via SetPersistentAppend).
    if let Some(append) = &state.persistent_append {
        let preview: String = append.as_str().chars().take(20).collect();
        spans.push(Span::raw("  ·  "));
        spans.push(Span::styled(
            format!("✎ append: {preview}"),
            Style::default().fg(Color::Green),
        ));
    }
    // Live-steering availability, shown only when relevant.
    if !matches!(state.live_steering_status, LiveSteeringStatus::Unsupported) {
        let (label, color) = match state.live_steering_status {
            LiveSteeringStatus::Ready => ("steer: ready", Color::Green),
            LiveSteeringStatus::Inactive => ("steer: idle", Color::DarkGray),
            LiveSteeringStatus::Closing => ("steer: closing", Color::Yellow),
            LiveSteeringStatus::Unsupported => ("", Color::DarkGray),
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
                .fg(Color::Yellow)
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
            (format!(" errors:{} ", error_count), Color::Red)
        } else {
            (" errors ".to_string(), Color::DarkGray)
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
            key("?"),
            Span::raw(" help  "),
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

fn render_entry(kind: &EntryKind, _width: usize, syntax: &Syntax) -> Vec<Line<'static>> {
    match kind {
        EntryKind::Text(text) => text
            .lines()
            .map(|l| {
                Line::from(vec![
                    Span::styled("› ", Style::default().fg(Color::Gray)),
                    Span::styled(l.to_string(), Style::default().fg(Color::Gray)),
                ])
            })
            .collect(),

        EntryKind::Thinking(text) => text
            .lines()
            .map(|l| {
                Line::from(vec![
                    Span::styled(
                        "💭 ",
                        Style::default()
                            .fg(Color::Magenta)
                            .add_modifier(Modifier::DIM),
                    ),
                    Span::styled(
                        l.to_string(),
                        Style::default()
                            .fg(Color::DarkGray)
                            .add_modifier(Modifier::ITALIC),
                    ),
                ])
            })
            .collect(),

        EntryKind::Usage { .. } => Vec::new(),

        EntryKind::ToolCall {
            tool,
            detail,
            input,
        } => {
            let mut lines = Vec::new();
            lines.push(Line::from(vec![
                Span::styled(
                    "🔧 ",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    tool.clone(),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(": "),
                Span::styled(detail.clone(), Style::default().fg(Color::DarkGray)),
            ]));

            if tool == "Edit" || tool == "Write" {
                lines.extend(render_edit_diff(tool, input, syntax));
            }

            lines
        }

        EntryKind::ToolResult { detail, success } => {
            let (icon, color) = if success == &Some(false) {
                ("⚠️", Color::Red)
            } else {
                ("✅", Color::Green)
            };
            let mut lines = Vec::new();
            for (i, line) in detail.lines().enumerate() {
                if i == 0 {
                    lines.push(Line::from(vec![
                        Span::styled(format!("{icon} "), Style::default().fg(color)),
                        Span::styled(
                            truncate_str(line, 200),
                            Style::default().fg(Color::DarkGray),
                        ),
                    ]));
                } else {
                    lines.push(Line::from(vec![
                        Span::raw("  "),
                        Span::styled(
                            truncate_str(line, 200),
                            Style::default().fg(Color::DarkGray),
                        ),
                    ]));
                }
            }
            if lines.is_empty() {
                lines.push(Line::from(Span::styled(
                    format!("{icon} (no output)"),
                    Style::default().fg(color),
                )));
            }
            lines
        }

        EntryKind::Error(msg) => msg
            .lines()
            .map(|l| {
                Line::from(vec![
                    Span::styled("❌ ", Style::default().fg(Color::Red)),
                    Span::styled(l.to_string(), Style::default().fg(Color::Red)),
                ])
            })
            .collect(),

        EntryKind::Info(msg) => msg
            .lines()
            .map(|l| {
                Line::from(vec![
                    Span::styled("ℹ️ ", Style::default().fg(Color::Cyan)),
                    Span::styled(l.to_string(), Style::default().fg(Color::Cyan)),
                ])
            })
            .collect(),

        EntryKind::IterationDivider { number, maximum } => {
            vec![render_iteration_divider(*number, *maximum, _width)]
        }
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
    let rule_style = Style::default().fg(Color::DarkGray);
    let label_style = Style::default()
        .fg(Color::Gray)
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

// ── Diff rendering ──────────────────────────────────────────────────────────

fn render_edit_diff(tool: &str, input: &serde_json::Value, syntax: &Syntax) -> Vec<Line<'static>> {
    let file = input
        .get("file_path")
        .or_else(|| input.get("file_name"))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let old = input
        .get("old_string")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let new = input
        .get("new_string")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let syn_name = syntax_for_file(file);

    let mut lines: Vec<Line<'static>> = Vec::new();

    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(
            if tool == "Write" {
                format!("--- {file} (new file)")
            } else {
                format!("--- {file}")
            },
            Style::default().fg(Color::Red),
        ),
    ]));

    if tool == "Edit" && !old.is_empty() {
        let old_expanded = expand_tabs(old);
        let new_expanded = expand_tabs(new);
        let old_lines: Vec<&str> = old_expanded.lines().collect();
        let new_lines: Vec<&str> = new_expanded.lines().collect();

        let mut old_set = std::collections::HashSet::new();
        for l in &new_lines {
            old_set.insert(*l);
        }
        let mut new_set = std::collections::HashSet::new();
        for l in &old_lines {
            new_set.insert(*l);
        }

        for (i, l) in old_lines.iter().enumerate() {
            if !new_set.contains(*l) {
                lines.push(diff_line_simple(i + 1, l, false, syn_name, syntax));
            } else {
                lines.push(diff_line_simple(i + 1, l, true, syn_name, syntax));
            }
        }
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(format!("+++ {file}"), Style::default().fg(Color::Green)),
        ]));
        for (i, l) in new_lines.iter().enumerate() {
            if !old_set.contains(*l) {
                lines.push(diff_line_simple(i + 1, l, true, syn_name, syntax));
            }
        }
    }

    if tool == "Write" && !new.is_empty() {
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(format!("+++ {file}"), Style::default().fg(Color::Green)),
        ]));
        for (i, l) in expand_tabs(new).lines().enumerate() {
            let highlighted = syntax.highlight_line(syn_name, &expand_tabs(l));
            let mut spans = vec![
                Span::raw("  "),
                Span::styled(
                    format!("{:>4} ", i + 1),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled("+", Style::default().fg(Color::Green)),
            ];
            spans.extend(highlighted);
            lines.push(Line::from(spans));
        }
    }

    lines
}

fn diff_line_simple(
    line_no: usize,
    text: &str,
    added: bool,
    syn_name: &str,
    syntax: &Syntax,
) -> Line<'static> {
    let (prefix, color, bg) = if added {
        ("+", Color::Green, Color::Rgb(20, 40, 20))
    } else {
        ("-", Color::Red, Color::Rgb(40, 20, 20))
    };

    let highlighted = syntax.highlight_line(syn_name, text);

    let mut spans = vec![
        Span::raw("  "),
        Span::styled(
            format!("{line_no:>4} "),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(prefix.to_string(), Style::default().fg(color).bg(bg)),
    ];
    spans.extend(highlighted);

    Line::from(spans)
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
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            ),
            Span::styled(format!("{ts} "), Style::default().fg(Color::DarkGray)),
            Span::styled("❌ ", Style::default().fg(Color::Red)),
            Span::styled(msg.to_string(), Style::default().fg(Color::Red)),
        ]));
    }

    if count == 0 {
        lines.push(Line::from(Span::styled(
            "No errors recorded so far.",
            Style::default().fg(Color::DarkGray),
        )));
    } else if !omitted.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!(
                "Note: {} older entries were trimmed from the live view; see the run log for full history.",
                omitted.entries
            ),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
        )));
    }

    let title = format!(" ⚠ Errors: {count} (e/Esc to close) ");
    let para = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title(title))
        .wrap(ratatui::widgets::Wrap { trim: false })
        .scroll((scroll, 0));
    f.render_widget(para, popup);
}

fn render_help_modal(f: &mut Frame, area: Rect) {
    let popup = center_rect(area, 70, 70);
    f.render_widget(Clear, popup);

    let key_style = Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD);
    let desc_style = Style::default().fg(Color::Gray);

    let mut lines: Vec<Line> = Vec::new();
    for (k, desc) in [
        ("p", "Show the rendered prompt for this iteration"),
        (
            "↑↓ jk / PgUp PgDn",
            "Scroll the event log (anchored, streaming-safe)",
        ),
        ("gg", "Jump to the top of the iteration in view"),
        ("G", "Jump to the bottom of the iteration in view"),
        ("gT", "Jump to the absolute start of the chat"),
        ("gB", "Jump to the absolute bottom (re-enable live)"),
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
        ("?", "Show this keybindings help"),
        (
            "q / Ctrl+C×2",
            "Force stop immediately (Ctrl+C once does nothing)",
        ),
    ] {
        lines.push(Line::from(vec![
            Span::styled(format!("  {k:<15}"), key_style),
            Span::styled(desc, desc_style),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Live view is bounded; the complete transcript is always in the run log.",
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM),
    )));

    let para = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" ⌨️  Keybindings (any key to close) "),
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
                    state.iteration_starts.push(divider.id);
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
        let iter1_start = state.iteration_starts[0];
        start_iteration(&mut state, 2, 2, 40);
        let iter2_start = state.iteration_starts[1];
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
        for (i, &boundary_id) in state.iteration_starts.iter().enumerate() {
            let entries = state.transcript.entries();
            let entry = entries
                .iter()
                .find(|e| e.id == boundary_id)
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
}
