//! Bounded semantic transcript and viewport/scroll selection for the streaming TUI.
//!
//! The *complete* run transcript lives in the durable JSONL run log (see
//! [`crate::run_logger`]); this module keeps only a bounded *semantic* working
//! set in TUI memory. It also answers viewport queries whose cost is
//! proportional to the visible area, not the total retained history.
//!
//! All logic here is pure (no rendering, no I/O) so it is unit-testable
//! deterministically. The wrapped-row counting that layout depends on is
//! supplied by a [`RowMeter`] the caller implements — the layout cache in
//! [`crate::streaming_tui`] in production, a stub in tests.

use std::cmp;

use jiff::Zoned;

// ── Entry model ──────────────────────────────────────────────────────────────

/// One semantic unit of the transcript. Re-exported from [`crate::streaming_tui`]
/// so callers depend on a single definition.
#[derive(Debug, Clone)]
pub enum EntryKind {
    /// Assistant text output.
    Text(String),
    /// Model thinking/reasoning output (toggleable via `t`).
    Thinking(String),
    /// A tool/action invocation. `input` carries raw tool input for rich
    /// rendering (e.g. Edit/Write diffs).
    ToolCall {
        tool: String,
        detail: String,
        input: serde_json::Value,
    },
    /// A tool/action result.
    ToolResult {
        /// The tool/action name, matching the [`EntryKind::ToolCall`] this
        /// result completes. Used at render time to decide styling (e.g.
        /// first-class edit tools skip the terminal-card treatment since the
        /// real diff renders as an [`EntryKind::FileEdit`]) without needing
        /// to look at neighbouring entries.
        tool: String,
        detail: String,
        success: Option<bool>,
        /// The originating call's `detail` (command/path), when the provider
        /// exposed a correlating call id. Drives the box's opening-rule label
        /// and output-syntax inference at render time. `None` when the
        /// provider can't correlate (e.g. no id in the protocol).
        command: Option<String>,
    },
    /// A captured file edit: the real before/after diff for a file the agent
    /// modified (syntax-highlighted when rendered). Boxed to keep the enum small;
    /// the [`velor_core::file_edit::FileEdit`] is framework-agnostic.
    FileEdit(Box<velor_core::file_edit::FileEdit>),
    /// An error event (routed to the errors modal, kept out of the main log).
    Error(String),
    /// A lifecycle/status line.
    Info(String),
    /// A non-fatal warning surfaced inline in the transcript — e.g. a retryable
    /// provider failure (overload / rate-limit) that Velor is backing off and
    /// retrying. Unlike [`EntryKind::Error`] this renders in the main log (so the
    /// user can see the run is retrying, not frozen) and flips the spinner to
    /// "retrying".
    Warning(String),
    /// A visual separator marking the start of a new Vel iteration. `number` is
    /// the 1-based iteration counter; `maximum` is the configured total, when one
    /// is meaningful. Dividers are derived from the run loop (not from an agent
    /// event): the TUI emits one immediately before the first entry of each new
    /// iteration.
    ///
    /// Dividers are deliberately inert for sizing: they occupy exactly one
    /// logical line and never coalesce with adjacent entries (see [`approx_bytes`]
    /// / [`logical_line_count`]), so they trim as whole entries and never merge
    /// into a text/thinking run.
    IterationDivider {
        /// The 1-based iteration number this divider introduces.
        number: u32,
        /// The configured total iteration count, when meaningful.
        maximum: Option<u32>,
    },
    /// Token usage. Transient — never stored as a transcript entry; consumed by
    /// the caller to update scalar state.
    Usage {
        input_tokens: Option<u64>,
        output_tokens: Option<u64>,
        cached_input_tokens: Option<u64>,
    },
}

/// A freshly-arrived entry as produced from an agent event, before ingest decides
/// whether to store, coalesce, or drop it.
#[derive(Debug, Clone)]
pub struct TuiEntry {
    pub ts: Zoned,
    pub kind: EntryKind,
}

impl TuiEntry {
    /// Convenience constructor stamped "now".
    #[must_use]
    pub fn now(kind: EntryKind) -> Self {
        Self {
            ts: Zoned::now(),
            kind,
        }
    }
}

/// Monotonically increasing, stable identifier for a retained entry. Stable
/// across trimming and streaming, so scroll anchors survive both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EntryId(u64);

impl EntryId {
    /// Raw underlying counter; useful for diagnostics/tests.
    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// A retained transcript entry with stable identity plus a revision counter that
/// bumps whenever its mutable content changes (streaming appends), driving
/// layout-cache invalidation.
#[derive(Debug, Clone)]
pub struct LiveEntry {
    pub id: EntryId,
    pub ts: Zoned,
    pub kind: EntryKind,
    /// Increments on each streaming append; part of the layout-cache key.
    pub rev: u64,
    /// Approximate retained size in UTF-8 bytes. Maintained for byte-bounding.
    pub approx_bytes: usize,
    /// Logical line count (newline-separated) of the entry's rendered form, used
    /// for accurate omitted-line accounting when an entry is trimmed.
    pub logical_lines: u64,
}

// ── Limits ───────────────────────────────────────────────────────────────────

/// Configured bounds for the live transcript. All limits are enforced together;
/// whichever trips first triggers a trim of the oldest complete entry.
#[derive(Debug, Clone, Copy)]
pub struct TuiLimits {
    /// Maximum retained semantic entries.
    pub max_entries: usize,
    /// Maximum retained bytes across all entries.
    pub max_bytes: usize,
    /// Maximum live logical lines for a single tool result / diff entry (head +
    /// tail kept, middle folded). The full content stays in the run log.
    pub max_entry_lines: usize,
}

impl Default for TuiLimits {
    fn default() -> Self {
        Self {
            // ~tens of thousands of entries is plenty for interactive scrolling
            // while keeping live memory in the low-MiB range for typical entries.
            max_entries: 15_000,
            max_bytes: 16 * 1024 * 1024, // 16 MiB
            // Keeps a single huge command/diff output from freezing the TUI.
            max_entry_lines: 400,
        }
    }
}

impl TuiLimits {
    /// Builds limits from optional config values, clamping each to a sane
    /// minimum so zero/absent values cannot disable a safety bound or cause
    /// division/loop pathology.
    #[must_use]
    pub fn from_options(
        max_entries: Option<usize>,
        max_bytes: Option<usize>,
        max_entry_lines: Option<usize>,
    ) -> Self {
        let clamp_min = |v: Option<usize>, min: usize, default: usize| match v {
            Some(n) => cmp::max(n, min),
            None => default,
        };
        // Lower bounds guard against pathological config: at least enough to
        // hold a viewport of content and a useful per-entry window.
        let max_entries = clamp_min(max_entries, 64, Self::default().max_entries);
        let max_bytes = clamp_min(max_bytes, 256 * 1024, Self::default().max_bytes);
        let max_entry_lines = clamp_min(max_entry_lines, 20, Self::default().max_entry_lines);
        Self {
            max_entries,
            max_bytes,
            max_entry_lines,
        }
    }
}

/// Cumulative accounting for content trimmed out of the live view. The complete
/// content remains in the durable run log.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OmittedStats {
    pub entries: u64,
    pub lines: u64,
    pub bytes: u64,
}

impl OmittedStats {
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.entries == 0
    }
}

// ── Bounded transcript ───────────────────────────────────────────────────────

/// A bounded, semantic transcript. Coalesces streamed text/thinking into a
/// single active entry, bounds oversized tool output, and trims the oldest
/// complete entries when a configured limit is exceeded.
pub struct Transcript {
    entries: Vec<LiveEntry>,
    next_id: u64,
    total_bytes: usize,
    limits: TuiLimits,
    omitted: OmittedStats,
}

impl Transcript {
    /// Creates an empty transcript with the given limits.
    #[must_use]
    pub fn new(limits: TuiLimits) -> Self {
        Self {
            entries: Vec::new(),
            next_id: 0,
            total_bytes: 0,
            limits,
            omitted: OmittedStats::default(),
        }
    }

    /// Returns the retained entries, oldest first.
    #[must_use]
    pub fn entries(&self) -> &[LiveEntry] {
        &self.entries
    }

    /// Returns the cumulative trimmed-from-live accounting.
    #[must_use]
    pub const fn omitted(&self) -> OmittedStats {
        self.omitted
    }

    /// Total retained bytes (for the footer indicator).
    #[must_use]
    pub const fn retained_bytes(&self) -> usize {
        self.total_bytes
    }

    /// Ingests a freshly-arrived entry, applying coalescing, oversized-entry
    /// bounding, and oldest-first trimming. Usage entries are not stored.
    pub fn ingest(&mut self, entry: TuiEntry) {
        // Usage is transient — never retained.
        if matches!(entry.kind, EntryKind::Usage { .. }) {
            return;
        }

        let kind = bound_oversized(entry.kind, self.limits.max_entry_lines);

        // Coalesce streamed assistant text/thinking into the active entry of the
        // SAME variant only (a Text run and a Thinking run stay separate). This
        // turns thousands of token chunks into one entry per segment.
        let coalesced = match (&kind, self.entries.last_mut()) {
            (EntryKind::Text(inc), Some(last)) if matches!(last.kind, EntryKind::Text(_)) => {
                coalesce_into(last, inc, self.limits.max_bytes, &mut self.total_bytes);
                true
            }
            (EntryKind::Thinking(inc), Some(last))
                if matches!(last.kind, EntryKind::Thinking(_)) =>
            {
                coalesce_into(last, inc, self.limits.max_bytes, &mut self.total_bytes);
                true
            }
            _ => false,
        };
        if coalesced {
            self.trim();
            return;
        }

        let id = EntryId(self.next_id);
        self.next_id = self.next_id.wrapping_add(1);
        let approx_bytes = approx_bytes(&kind);
        let logical_lines = logical_line_count(&kind);
        let live = LiveEntry {
            id,
            ts: entry.ts,
            kind,
            rev: 0,
            approx_bytes,
            logical_lines,
        };
        self.total_bytes = self.total_bytes.saturating_add(approx_bytes);
        self.entries.push(live);
        self.trim();
    }

    /// Trims oldest complete entries until all limits are satisfied. Never
    /// splits an entry.
    fn trim(&mut self) {
        while self.entries.len() > self.limits.max_entries
            || self.total_bytes > self.limits.max_bytes
        {
            let Some(removed) = self.entries.first() else {
                break;
            };
            // Guard: never trim the very last entry, so the buffer always holds
            // at least the newest content.
            if self.entries.len() <= 1 {
                break;
            }
            let bytes = removed.approx_bytes;
            let lines = removed.logical_lines;
            let id = removed.id;
            self.entries.remove(0);
            self.total_bytes = self.total_bytes.saturating_sub(bytes);
            self.omitted.entries += 1;
            self.omitted.lines = self.omitted.lines.saturating_add(lines);
            self.omitted.bytes = self.omitted.bytes.saturating_add(bytes as u64);
            // next_id is monotonic; trimmed ids are simply gone. Anchors that
            // reference a trimmed id are clamped at viewport selection.
            let _ = id;
        }
    }
}

/// Appends `inc` to a streamed text/thinking entry's payload, bumping its rev,
/// byte accounting, and line count. Applies [`cap_streamed_text`] so a single
/// runaway message can't exceed the byte budget.
fn coalesce_into(last: &mut LiveEntry, inc: &str, max_bytes: usize, total: &mut usize) {
    let Some(text) = streamed_text_mut(&mut last.kind) else {
        return;
    };
    if inc.is_empty() {
        return;
    }
    let old_len = text.len();
    text.push_str(inc);
    cap_streamed_text(text, max_bytes);
    let new_len = text.len();
    if new_len >= old_len {
        *total = total.saturating_add(new_len - old_len);
    } else {
        *total = total.saturating_sub(old_len - new_len);
    }
    last.approx_bytes = new_len;
    last.rev = last.rev.wrapping_add(1);
    last.logical_lines = logical_line_count(&last.kind);
}

/// Mutable accessor to a streamed text/thinking payload.
fn streamed_text_mut(kind: &mut EntryKind) -> Option<&mut String> {
    match kind {
        EntryKind::Text(s) | EntryKind::Thinking(s) => Some(s),
        _ => None,
    }
}

/// Caps a streamed payload to `max_bytes`, keeping a head + tail with a visible
/// marker. No-op when already within budget; only a single pathological entry
/// (longer than the whole transcript budget) ever triggers it.
fn cap_streamed_text(s: &mut String, max_bytes: usize) {
    if s.len() <= max_bytes {
        return;
    }
    let keep = (max_bytes / 2).max(1);
    let head_end = s.floor_char_boundary(keep);
    let tail_start = s.ceil_char_boundary(s.len() - keep).max(head_end);
    let omitted = s.len() - head_end - (s.len() - tail_start);
    let head = &s[..head_end];
    let tail = &s[tail_start..];
    // Unlike tool-result output, streamed text/thinking isn't covered by the
    // Ctrl+O expand toggle — the omitted middle is discarded right here,
    // before the entry is even stored.
    *s = format!("{head}\n ⋯ {omitted} more bytes — full text is in the run log ⋯\n{tail}");
}

/// Bounds an oversized Edit/Write diff input so one entry cannot hold tens of
/// thousands of live lines. Returns the (possibly folded) kind.
///
/// `ToolResult` output is deliberately *not* folded here: the TUI renders it
/// collapsed (head + tail) by default and expands to the full retained text
/// on demand (the `Ctrl+O` toggle — see `streaming_tui::LayoutCache`), so
/// discarding the middle at ingest would make expansion show nothing more
/// than the collapsed view. The full transcript byte budget
/// (`TuiLimits::max_bytes`) remains the memory bound.
fn bound_oversized(kind: EntryKind, max_lines: usize) -> EntryKind {
    let max_lines = max_lines.max(2);
    match kind {
        EntryKind::ToolCall {
            tool,
            detail,
            input,
        } => EntryKind::ToolCall {
            tool,
            detail,
            input: bound_edit_input(input, max_lines),
        },
        other => other,
    }
}

/// Folds a multi-line string to a head + tail with a visible omission marker.
fn fold_multiline(s: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    if lines.len() <= max_lines {
        return s.to_string();
    }
    let keep = max_lines / 2;
    let head = &lines[..keep];
    let tail = &lines[lines.len() - keep..];
    let omitted = lines.len() - 2 * keep;
    let mut out = String::with_capacity(s.len());
    for l in head {
        out.push_str(l);
        out.push('\n');
    }
    out.push_str(&format!("  … {omitted} more lines (Ctrl+O: Expand)"));
    out.push('\n');
    for l in tail {
        out.push_str(l);
        out.push('\n');
    }
    out.trim_end_matches('\n').to_string()
}

/// Bounds the `old_string`/`new_string` of an Edit/Write tool input so a giant
/// diff cannot dominate live memory.
fn bound_edit_input(input: serde_json::Value, max_lines: usize) -> serde_json::Value {
    let Some(obj) = input.as_object().cloned() else {
        return input;
    };
    let mut obj = obj;
    for key in ["old_string", "new_string"] {
        if let Some(s) = obj.get(key).and_then(|v| v.as_str()) {
            let folded = fold_multiline(s, max_lines);
            if folded.len() != s.len() {
                obj.insert(key.to_string(), serde_json::Value::String(folded));
            }
        }
    }
    serde_json::Value::Object(obj)
}

/// Approximate retained byte cost of a kind (UTF-8 length of its stringy parts).
fn approx_bytes(kind: &EntryKind) -> usize {
    match kind {
        EntryKind::Text(s)
        | EntryKind::Thinking(s)
        | EntryKind::Error(s)
        | EntryKind::Info(s)
        | EntryKind::Warning(s) => s.len(),
        EntryKind::ToolResult { detail, .. } => detail.len(),
        EntryKind::FileEdit(edit) => edit.approx_byte_size(),
        EntryKind::ToolCall {
            tool,
            detail,
            input,
        } => tool.len() + detail.len() + input.to_string().len(),
        // Dividers render to a short, width-spanning rule; use a fixed
        // conservative estimate so byte-bounding stays simple (there are at most
        // one per iteration, and very few ever live in the buffer).
        EntryKind::IterationDivider { .. } => 64,
        EntryKind::Usage { .. } => 0,
    }
}

/// Estimated logical line count of a kind's rendered form (no syntax engine).
fn logical_line_count(kind: &EntryKind) -> u64 {
    match kind {
        EntryKind::Text(s) | EntryKind::Thinking(s) => line_count(s),
        EntryKind::ToolResult { detail, .. } => line_count(detail),
        EntryKind::FileEdit(edit) => {
            // Header + diff lines + one row per inter-hunk gap (estimate).
            1u64.saturating_add(u64::try_from(edit.diff_line_count()).unwrap_or(u64::MAX))
                .saturating_add(u64::try_from(edit.hunks.len()).unwrap_or(u64::MAX))
        }
        EntryKind::ToolCall {
            tool: _,
            detail,
            input,
        } => {
            let mut n = 1u64 + line_count(detail);
            for key in ["old_string", "new_string"] {
                if let Some(s) = input.get(key).and_then(|v| v.as_str()) {
                    n = n.saturating_add(line_count(s));
                }
            }
            n
        }
        EntryKind::Error(s) | EntryKind::Info(s) | EntryKind::Warning(s) => line_count(s).max(1),
        // A divider renders as exactly one logical line.
        EntryKind::IterationDivider { .. } => 1,
        EntryKind::Usage { .. } => 0,
    }
}

fn line_count(s: &str) -> u64 {
    if s.is_empty() {
        return 1;
    }
    let mut n = u64::try_from(s.lines().count()).unwrap_or(u64::MAX);
    if s.ends_with('\n') {
        n = n.saturating_sub(1).max(1);
    }
    n.max(1)
}

// ── Plain-text export ────────────────────────────────────────────────────────
//
// Builds a clipboard-friendly rendering of the transcript for pasting into
// another AI session: no ANSI/markup, one section per entry, headers marking
// tool calls/results/thinking, and file edits as a unified diff.

/// Which entry kinds a plain-text export includes, mirroring the live view's
/// visibility toggles so a copy matches what's on screen.
#[derive(Debug, Clone, Copy)]
pub struct PlainTextOptions {
    /// Include `Thinking` entries (the `t` toggle).
    pub show_thinking: bool,
    /// Include `ToolResult`/`FileEdit` content (the `o` toggle). The
    /// `ToolCall` invocation itself is always included.
    pub show_tool_output: bool,
}

/// Renders `entries` (already sliced to the desired range) as plain text.
/// `todos`, when non-empty after trimming, is prefixed as the current
/// todo-board state (open and completed items alike) since that reflects live
/// run state rather than a transcript entry.
#[must_use]
pub fn render_plain_text(
    entries: &[LiveEntry],
    todos: Option<&str>,
    opts: PlainTextOptions,
) -> String {
    let mut out = String::new();
    if let Some(todos) = todos.map(str::trim).filter(|s| !s.is_empty()) {
        out.push_str("## Current todos\n");
        out.push_str(todos);
        out.push_str("\n\n");
    }
    for entry in entries {
        if let Some(section) = render_entry_plain_text(&entry.kind, opts) {
            out.push_str(&section);
            out.push_str("\n\n");
        }
    }
    out.truncate(out.trim_end().len());
    out
}

/// Renders one entry to a plain-text section, or `None` when the entry is
/// hidden by `opts` or carries no content worth exporting (e.g. transient
/// usage, or an empty text run).
fn render_entry_plain_text(kind: &EntryKind, opts: PlainTextOptions) -> Option<String> {
    match kind {
        EntryKind::Text(s) => {
            let s = s.trim();
            (!s.is_empty()).then(|| s.to_string())
        }
        EntryKind::Thinking(s) => {
            let s = s.trim();
            (opts.show_thinking && !s.is_empty()).then(|| format!("[Thinking]\n{s}"))
        }
        EntryKind::ToolCall { tool, detail, .. } => Some(format!("[Tool call: {tool}] {detail}")),
        EntryKind::ToolResult {
            tool,
            detail,
            success,
            ..
        } => {
            if !opts.show_tool_output {
                return None;
            }
            let status = if *success == Some(false) {
                " (failed)"
            } else {
                ""
            };
            Some(format!("[Tool result: {tool}{status}]\n{detail}"))
        }
        EntryKind::FileEdit(edit) => opts
            .show_tool_output
            .then(|| render_file_edit_plain_text(edit)),
        // Errors live in the separate errors modal, never the main log — the
        // export mirrors that.
        EntryKind::Error(_) => None,
        EntryKind::Info(s) => Some(format!("[Info] {s}")),
        EntryKind::Warning(s) => Some(format!("[Warning] {s}")),
        EntryKind::IterationDivider { number, maximum } => Some(match maximum {
            Some(m) => format!("=== Iteration {number}/{m} ==="),
            None => format!("=== Iteration {number} ==="),
        }),
        EntryKind::Usage { .. } => None,
    }
}

/// Renders a captured file edit as a unified diff (`--- a/`/`+++ b/` header,
/// `@@`-hunks, `+`/`-`/` `-prefixed lines).
fn render_file_edit_plain_text(edit: &velor_core::file_edit::FileEdit) -> String {
    use velor_core::file_edit::{FileEditKind, LineKind};

    let path = &edit.path;
    match &edit.kind {
        FileEditKind::Binary => return format!("[Binary file changed: {path}]"),
        FileEditKind::CaptureFailed { reason } => {
            return format!("[File edit (capture failed): {path} — {reason}]");
        }
        FileEditKind::Created => {}
        FileEditKind::Deleted => {}
        FileEditKind::Modified => {}
    }

    let header = match &edit.kind {
        FileEditKind::Created => format!("[File created: {path}]\n"),
        FileEditKind::Deleted => format!("[File deleted: {path}]\n"),
        FileEditKind::Modified | FileEditKind::Binary | FileEditKind::CaptureFailed { .. } => {
            format!("[File edited: {path}]\n")
        }
    };

    let mut out = header;
    out.push_str(&format!("--- a/{path}\n+++ b/{path}\n"));
    for hunk in &edit.hunks {
        let old_start = hunk.old_start.unwrap_or(0);
        let new_start = hunk.new_start.unwrap_or(0);
        let old_len = hunk
            .lines
            .iter()
            .filter(|l| l.kind != LineKind::Addition)
            .count();
        let new_len = hunk
            .lines
            .iter()
            .filter(|l| l.kind != LineKind::Removal)
            .count();
        out.push_str(&format!(
            "@@ -{old_start},{old_len} +{new_start},{new_len} @@\n"
        ));
        for line in &hunk.lines {
            let prefix = match line.kind {
                LineKind::Context => ' ',
                LineKind::Addition => '+',
                LineKind::Removal => '-',
            };
            out.push(prefix);
            out.push_str(&line.text);
            out.push('\n');
        }
    }
    if edit.omitted_lines > 0 {
        out.push_str(&format!(
            "… {} more diff lines omitted (see run log for the full diff) …\n",
            edit.omitted_lines
        ));
    }
    out.trim_end().to_string()
}

// ── Viewport + scroll ────────────────────────────────────────────────────────

/// Yields the wrapped-row count of an entry at a given content width. The layout
/// cache implements this (rendering + caching on miss); tests supply a stub.
pub trait RowMeter {
    /// Returns the number of wrapped display rows `entry` occupies at `width`.
    /// Implementations must return at least 1 for any entry.
    fn rows(&mut self, entry: &LiveEntry, width: u16) -> u32;
}

/// Scroll position. [`ScrollState::Tail`] pins to the newest content; an anchor
/// pins the viewport to a specific entry + row so streaming/trimming never moves
/// a reader who is browsing history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScrollState {
    /// Pinned to the newest content; new output scrolls into view.
    #[default]
    Tail,
    /// The viewport top sits at row `hidden_rows` within entry `entry_id`.
    /// `hidden_rows` counts rows of that entry scrolled off above the viewport.
    Anchored { entry_id: EntryId, hidden_rows: u32 },
}

/// A resolved viewport over the transcript: render entries `[start, start+count)`
/// and skip the first `top_skip` rows of the `start` entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Viewport {
    pub start: usize,
    pub count: usize,
    pub top_skip: u32,
}

impl Viewport {
    pub const EMPTY: Self = Self {
        start: 0,
        count: 0,
        top_skip: 0,
    };
}

/// Returns the wrapped-row count of `entries[idx]`, or 0 when the entry is not
/// `visible` (e.g. hidden thinking). Visible entries report at least 1 row.
fn rows_of(
    meter: &mut impl RowMeter,
    entries: &[LiveEntry],
    idx: usize,
    width: u16,
    visible: &impl Fn(&LiveEntry) -> bool,
) -> u32 {
    if !visible(&entries[idx]) {
        return 0;
    }
    meter.rows(&entries[idx], width).max(1)
}

/// Selects the viewport for the current scroll state. Cost is proportional to
/// `viewport_rows + overscan` (the entries traversed), never to total history.
/// `visible` filters entries out of both row counting and rendering.
pub fn select_viewport(
    entries: &[LiveEntry],
    meter: &mut impl RowMeter,
    width: u16,
    viewport_rows: u32,
    overscan: u32,
    scroll: ScrollState,
    visible: impl Fn(&LiveEntry) -> bool,
) -> Viewport {
    let len = entries.len();
    if len == 0 || viewport_rows == 0 {
        return Viewport::EMPTY;
    }
    let target = viewport_rows.saturating_add(overscan);

    match scroll {
        ScrollState::Tail => {
            // Walk backward from the newest entry until we cover the viewport
            // (plus overscan), then reverse for rendering.
            let mut acc: u32 = 0;
            let mut start = len - 1;
            for i in (0..len).rev() {
                let rows = rows_of(meter, entries, i, width, &visible);
                acc = acc.saturating_add(rows);
                start = i;
                if acc >= target {
                    break;
                }
            }
            let start_rows = rows_of(meter, entries, start, width, &visible);
            // Rows above the live viewport (overscan) belong to the start entry.
            let top_skip = acc.saturating_sub(viewport_rows).min(start_rows);
            Viewport {
                start,
                count: len - start,
                top_skip,
            }
        }
        ScrollState::Anchored {
            entry_id,
            hidden_rows,
        } => {
            let start = match entries.iter().position(|e| e.id == entry_id) {
                Some(i) => i,
                // Anchored entry was trimmed: deterministically pin to the oldest
                // retained content.
                None => return forward_window(entries, meter, width, 0, 0, target, &visible),
            };
            forward_window(entries, meter, width, start, hidden_rows, target, &visible)
        }
    }
}

/// Forward window from `start_idx` with `hidden_rows` of the start entry scrolled
/// off the top, covering `target` visible rows (plus whole-entry remainder).
fn forward_window(
    entries: &[LiveEntry],
    meter: &mut impl RowMeter,
    width: u16,
    start_idx: usize,
    hidden_rows: u32,
    target: u32,
    visible: &impl Fn(&LiveEntry) -> bool,
) -> Viewport {
    let len = entries.len();
    if start_idx >= len {
        return Viewport::EMPTY;
    }
    let mut acc: u32 = 0;
    let mut end = start_idx;
    for i in start_idx..len {
        let rows = rows_of(meter, entries, i, width, visible);
        let entry_visible = if i == start_idx {
            rows.saturating_sub(hidden_rows)
        } else {
            rows
        };
        acc = acc.saturating_add(entry_visible);
        end = i;
        if acc >= target {
            break;
        }
    }
    let anchor_rows = rows_of(meter, entries, start_idx, width, visible);
    Viewport {
        start: start_idx,
        count: end - start_idx + 1,
        top_skip: hidden_rows.min(anchor_rows),
    }
}

/// Scrolls the viewport up (toward older content) by `n` rows. Stable under
/// streaming and trimming: an anchor references a specific entry id.
pub fn scroll_up(
    entries: &[LiveEntry],
    meter: &mut impl RowMeter,
    width: u16,
    viewport_rows: u32,
    scroll: ScrollState,
    n: u32,
    visible: impl Fn(&LiveEntry) -> bool,
) -> ScrollState {
    if entries.is_empty() {
        return ScrollState::Tail;
    }
    let n = n.max(1);
    match scroll {
        ScrollState::Tail => {
            // From tail, the viewport top sits ~viewport_rows above the bottom.
            // Walk back (viewport_rows + n) rows to find the new anchor.
            let target_from_bottom = viewport_rows.saturating_add(n);
            walk_up_from_bottom(entries, meter, width, target_from_bottom, &visible)
        }
        ScrollState::Anchored {
            entry_id,
            hidden_rows,
        } => {
            let Some(idx) = entries.iter().position(|e| e.id == entry_id) else {
                return ScrollState::Anchored {
                    entry_id: entries[0].id,
                    hidden_rows: 0,
                };
            };
            let mut cur_idx = idx;
            let mut cur_hidden = hidden_rows;
            let mut remaining = n;
            while remaining > 0 {
                if cur_idx == 0 && cur_hidden == 0 {
                    break; // reached the very top
                }
                if cur_hidden > 0 {
                    let step = remaining.min(cur_hidden);
                    cur_hidden -= step;
                    remaining -= step;
                } else {
                    // Top of current entry: step into the previous entry's last row.
                    cur_idx -= 1;
                    let prev_rows = rows_of(meter, entries, cur_idx, width, &visible);
                    cur_hidden = prev_rows.saturating_sub(1);
                    remaining -= 1;
                }
            }
            ScrollState::Anchored {
                entry_id: entries[cur_idx].id,
                hidden_rows: cur_hidden,
            }
        }
    }
}

/// Walks backward from the newest entry until `rows_from_bottom` rows are
/// passed, returning the anchor at that boundary.
fn walk_up_from_bottom(
    entries: &[LiveEntry],
    meter: &mut impl RowMeter,
    width: u16,
    rows_from_bottom: u32,
    visible: &impl Fn(&LiveEntry) -> bool,
) -> ScrollState {
    let len = entries.len();
    let mut acc: u32 = 0;
    for i in (0..len).rev() {
        let rows = rows_of(meter, entries, i, width, visible);
        acc = acc.saturating_add(rows);
        if acc > rows_from_bottom {
            // Boundary falls inside entry i.
            let hidden = acc
                .saturating_sub(rows_from_bottom)
                .min(rows.saturating_sub(1));
            return ScrollState::Anchored {
                entry_id: entries[i].id,
                hidden_rows: hidden,
            };
        }
        if i == 0 {
            break;
        }
    }
    // Ran off the top: pin to the oldest entry.
    ScrollState::Anchored {
        entry_id: entries[0].id,
        hidden_rows: 0,
    }
}

/// Scrolls the viewport down (toward newer content) by `n` rows, returning
/// [`ScrollState::Tail`] once the bottom is reached.
pub fn scroll_down(
    entries: &[LiveEntry],
    meter: &mut impl RowMeter,
    width: u16,
    scroll: ScrollState,
    n: u32,
    visible: impl Fn(&LiveEntry) -> bool,
) -> ScrollState {
    if entries.is_empty() {
        return ScrollState::Tail;
    }
    let n = n.max(1);
    let ScrollState::Anchored {
        entry_id,
        mut hidden_rows,
    } = scroll
    else {
        return ScrollState::Tail; // already at tail
    };
    let Some(mut idx) = entries.iter().position(|e| e.id == entry_id) else {
        return ScrollState::Tail;
    };
    let len = entries.len();
    let mut remaining = n;
    while remaining > 0 {
        let rows = rows_of(meter, entries, idx, width, &visible);
        if hidden_rows < rows.saturating_sub(1) {
            let step = remaining.min(rows.saturating_sub(1) - hidden_rows);
            hidden_rows += step;
            remaining -= step;
        } else if idx == len - 1 {
            // At the last row of the last entry: bottom reached.
            return ScrollState::Tail;
        } else {
            idx += 1;
            hidden_rows = 0;
            remaining -= 1;
        }
    }
    ScrollState::Anchored {
        entry_id: entries[idx].id,
        hidden_rows,
    }
}

#[cfg(test)]
mod tests;
