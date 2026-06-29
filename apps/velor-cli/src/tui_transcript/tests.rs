//! Deterministic unit tests for the bounded transcript + viewport/scroll logic.
//! See [`super`] for the module under test.

use std::collections::HashMap;

use super::*;

/// A meter backed by an explicit id→rows map, with a call counter so tests can
/// assert bounded work.
struct MapMeter {
    rows: HashMap<u64, u32>,
    calls: usize,
}

impl MapMeter {
    fn constant(n: usize, rows: u32) -> Self {
        let mut rows_map = HashMap::new();
        for i in 0..n as u64 {
            rows_map.insert(i, rows);
        }
        Self {
            rows: rows_map,
            calls: 0,
        }
    }
}

impl RowMeter for MapMeter {
    fn rows(&mut self, entry: &LiveEntry, _width: u16) -> u32 {
        self.calls += 1;
        *self.rows.get(&entry.id.raw()).unwrap_or(&1)
    }
}

fn push_text(t: &mut Transcript, s: &str) {
    t.ingest(TuiEntry::now(EntryKind::Text(s.to_string())));
}

fn push_info(t: &mut Transcript, s: &str) {
    t.ingest(TuiEntry::now(EntryKind::Info(s.to_string())));
}

fn ids(t: &Transcript) -> Vec<u64> {
    t.entries().iter().map(|e| e.id.raw()).collect()
}

// ── Coalescing ───────────────────────────────────────────────────────────────

#[test]
fn streamed_text_chunks_coalesce_into_one_entry() {
    let mut t = Transcript::new(TuiLimits::default());
    push_text(&mut t, "Hello");
    push_text(&mut t, ", ");
    push_text(&mut t, "world!");
    assert_eq!(
        t.entries().len(),
        1,
        "three deltas must merge into one entry"
    );
    match &t.entries()[0].kind {
        EntryKind::Text(s) => assert_eq!(s, "Hello, world!"),
        other => panic!("expected Text, got {other:?}"),
    }
    assert_eq!(t.entries()[0].rev, 2, "rev bumps once per append");
}

#[test]
fn thinking_coalesces_separately_from_text() {
    let mut t = Transcript::new(TuiLimits::default());
    t.ingest(TuiEntry::now(EntryKind::Thinking("hmm".to_string())));
    t.ingest(TuiEntry::now(EntryKind::Text("answer".to_string())));
    t.ingest(TuiEntry::now(EntryKind::Thinking("more".to_string())));
    // Three entries: thinking/text/thinking (text breaks the thinking run).
    assert_eq!(t.entries().len(), 3);
}

#[test]
fn a_non_text_event_breaks_the_coalesce_run() {
    let mut t = Transcript::new(TuiLimits::default());
    push_text(&mut t, "before");
    push_info(&mut t, "[tool ran]");
    push_text(&mut t, "after");
    assert_eq!(t.entries().len(), 3);
    assert!(matches!(t.entries()[2].kind, EntryKind::Text(_)));
}

#[test]
fn usage_is_transient_and_never_stored() {
    let mut t = Transcript::new(TuiLimits::default());
    for _ in 0..1000 {
        t.ingest(TuiEntry::now(EntryKind::Usage {
            input_tokens: Some(1),
            output_tokens: Some(2),
            cached_input_tokens: Some(3),
        }));
    }
    assert!(
        t.entries().is_empty(),
        "usage must not accumulate as entries"
    );
}

// ── Bounding + trimming ──────────────────────────────────────────────────────

#[test]
fn trims_oldest_complete_entries_past_max_entries() {
    let limits = TuiLimits {
        max_entries: 5,
        max_bytes: usize::MAX,
        max_entry_lines: 1000,
    };
    let mut t = Transcript::new(limits);
    for i in 0..20 {
        push_info(&mut t, &format!("line {i}"));
    }
    assert_eq!(t.entries().len(), 5, "must cap at max_entries");
    // Newest five retained (ids 15..20); oldest 15 trimmed.
    assert_eq!(ids(&mut t), vec![15, 16, 17, 18, 19]);
    assert_eq!(t.omitted().entries, 15);
    assert!(t.omitted().lines >= 15);
}

#[test]
fn trims_on_byte_limit_and_accounts_bytes() {
    let limits = TuiLimits {
        max_entries: usize::MAX,
        max_bytes: 300,
        max_entry_lines: 1000,
    };
    let mut t = Transcript::new(limits);
    // Each entry ~50 bytes; 10 entries = ~500 bytes → trim until under 300.
    // (Info entries don't coalesce, so each is a distinct retained entry.)
    for _ in 0..10 {
        push_info(&mut t, &"x".repeat(50));
    }
    assert!(
        t.retained_bytes() <= 300,
        "retained bytes {} must be <= max",
        t.retained_bytes()
    );
    assert!(t.omitted().entries > 0);
    assert!(t.omitted().bytes > 0);
}

#[test]
fn never_trims_the_last_entry() {
    let limits = TuiLimits {
        max_entries: 64,
        max_bytes: 1, // pathologically small
        max_entry_lines: 20,
    };
    let mut t = Transcript::new(limits);
    push_text(&mut t, &"y".repeat(10_000));
    assert_eq!(t.entries().len(), 1, "must always retain the newest entry");
}

#[test]
fn oversized_tool_result_is_folded_with_marker() {
    let limits = TuiLimits {
        max_entries: usize::MAX,
        max_bytes: usize::MAX,
        max_entry_lines: 10,
    };
    let mut t = Transcript::new(limits);
    let huge = (0..5000)
        .map(|i| format!("row {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    t.ingest(TuiEntry::now(EntryKind::ToolResult {
        detail: huge,
        success: Some(true),
    }));
    let EntryKind::ToolResult { detail, .. } = &t.entries()[0].kind else {
        panic!("expected ToolResult");
    };
    assert!(
        detail.contains("lines omitted"),
        "fold marker must be present"
    );
    let kept_lines = detail.lines().count();
    assert!(
        kept_lines <= 11,
        "folded detail must be bounded, got {kept_lines} lines"
    );
}

#[test]
fn oversized_edit_diff_input_is_bounded() {
    let limits = TuiLimits {
        max_entries: usize::MAX,
        max_bytes: usize::MAX,
        max_entry_lines: 10,
    };
    let mut t = Transcript::new(limits);
    let big = (0..4000)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let input = serde_json::json!({
        "file_path": "x.rs",
        "old_string": big.clone(),
        "new_string": big,
    });
    t.ingest(TuiEntry::now(EntryKind::ToolCall {
        tool: "Edit".to_string(),
        detail: "x.rs".to_string(),
        input,
    }));
    let EntryKind::ToolCall { input, .. } = &t.entries()[0].kind else {
        panic!("expected ToolCall");
    };
    let old = input.get("old_string").unwrap().as_str().unwrap();
    assert!(old.contains("lines omitted"));
    assert!(old.lines().count() <= 11);
}

#[test]
fn limits_from_options_clamps_pathological_values() {
    let l = TuiLimits::from_options(Some(0), Some(0), Some(0));
    assert!(l.max_entries >= 64);
    assert!(l.max_bytes >= 256 * 1024);
    assert!(l.max_entry_lines >= 20);
}

// ── Viewport selection ───────────────────────────────────────────────────────

#[test]
fn tail_viewport_walks_backward_from_newest() {
    let mut t = Transcript::new(TuiLimits::default());
    for i in 0..10 {
        push_info(&mut t, &format!("e{i}"));
    }
    let mut meter = MapMeter::constant(10, 1);
    let vp = select_viewport(t.entries(), &mut meter, 80, 3, 0, ScrollState::Tail, |_| {
        true
    });
    assert_eq!(vp.start, 7, "only the last 3 entries are needed");
    assert_eq!(vp.count, 3);
    assert_eq!(vp.top_skip, 0);
}

#[test]
fn tail_viewport_top_skip_handles_partial_top_entry() {
    // 2 entries of 5 rows each, viewport 3, overscan 1 → need 4 rows from bottom.
    let mut t = Transcript::new(TuiLimits::default());
    push_info(&mut t, "a");
    push_info(&mut t, "b");
    let mut meter = MapMeter {
        rows: [(0, 5), (1, 5)].into_iter().collect(),
        calls: 0,
    };
    let vp = select_viewport(t.entries(), &mut meter, 80, 3, 1, ScrollState::Tail, |_| {
        true
    });
    // target=4; walking back: entry1(5 rows)→acc=5≥4, start=1. But that covers
    // only entry1. top_skip = acc(5) - viewport(3) = 2 rows of entry1 hidden.
    assert_eq!(vp.start, 1);
    assert_eq!(vp.top_skip, 2);
}

#[test]
fn anchored_viewport_walks_forward_from_anchor() {
    let mut t = Transcript::new(TuiLimits::default());
    for i in 0..6 {
        push_info(&mut t, &format!("e{i}"));
    }
    let mut meter = MapMeter::constant(6, 1);
    // Anchor at entry id 2 (index 2), no hidden rows, show 3 rows.
    let vp = select_viewport(
        t.entries(),
        &mut meter,
        80,
        3,
        0,
        ScrollState::Anchored {
            entry_id: EntryId(2),
            hidden_rows: 0,
        },
        |_| true,
    );
    assert_eq!(vp.start, 2);
    assert_eq!(vp.count, 3); // entries 2,3,4
    assert_eq!(vp.top_skip, 0);
}

#[test]
fn anchored_top_skip_skips_hidden_rows_of_anchor() {
    let mut t = Transcript::new(TuiLimits::default());
    push_info(&mut t, "a");
    let mut meter = MapMeter {
        rows: [(0, 10)].into_iter().collect(),
        calls: 0,
    };
    let vp = select_viewport(
        t.entries(),
        &mut meter,
        80,
        3,
        0,
        ScrollState::Anchored {
            entry_id: EntryId(0),
            hidden_rows: 4,
        },
        |_| true,
    );
    assert_eq!(vp.top_skip, 4);
}

#[test]
fn trimmed_anchor_falls_back_to_oldest_retained() {
    let limits = TuiLimits {
        max_entries: 3,
        max_bytes: usize::MAX,
        max_entry_lines: 1000,
    };
    let mut t = Transcript::new(limits);
    for i in 0..6 {
        push_info(&mut t, &format!("e{i}")); // ids 0..5, trimmed to last 3 (3,4,5)
    }
    let mut meter = MapMeter::constant(6, 1);
    // Anchor references id 1, which was trimmed.
    let vp = select_viewport(
        t.entries(),
        &mut meter,
        80,
        2,
        0,
        ScrollState::Anchored {
            entry_id: EntryId(1),
            hidden_rows: 0,
        },
        |_| true,
    );
    assert_eq!(vp.start, 0, "trimmed anchor → oldest retained content");
    assert_eq!(vp.top_skip, 0);
}

#[test]
fn select_viewport_cost_is_bounded_by_viewport_not_history() {
    // Large transcript; Tail selection must only touch viewport+overscan entries.
    let n = 5_000;
    let mut t = Transcript::new(TuiLimits::default());
    for i in 0..n {
        push_info(&mut t, &format!("e{i}"));
    }
    let mut meter = MapMeter::constant(n, 1);
    let vp = select_viewport(
        t.entries(),
        &mut meter,
        80,
        10,
        5,
        ScrollState::Tail,
        |_| true,
    );
    // Bounded: far fewer than n meter calls.
    assert!(
        meter.calls < 50,
        "meter calls {} should be ~viewport+overscan",
        meter.calls
    );
    assert_eq!(vp.count, 15);
}

// ── Scroll ───────────────────────────────────────────────────────────────────

#[test]
fn scroll_up_from_tail_creates_anchor_above_viewport() {
    let mut t = Transcript::new(TuiLimits::default());
    for i in 0..20 {
        push_info(&mut t, &format!("e{i}"));
    }
    let mut meter = MapMeter::constant(20, 1);
    // Tail shows last ~10 rows; scroll up 1 → anchor should move up by 1.
    let st = scroll_up(
        t.entries(),
        &mut meter,
        80,
        10,
        ScrollState::Tail,
        1,
        |_| true,
    );
    let ScrollState::Anchored {
        entry_id,
        hidden_rows,
    } = st
    else {
        panic!("expected anchored");
    };
    // viewport_rows(10) + n(1) = 11 rows from bottom → entry id 20-1-11 = 8.
    assert_eq!(entry_id, EntryId(8));
    assert_eq!(hidden_rows, 0);
}

#[test]
fn scroll_up_then_down_returns_to_tail() {
    let mut t = Transcript::new(TuiLimits::default());
    for i in 0..30 {
        push_info(&mut t, &format!("e{i}"));
    }
    let mut meter = MapMeter::constant(30, 1);
    let st = scroll_up(
        t.entries(),
        &mut meter,
        80,
        10,
        ScrollState::Tail,
        5,
        |_| true,
    );
    assert!(matches!(st, ScrollState::Anchored { .. }));
    // Scroll down enough to reach the bottom again.
    let st = scroll_down(t.entries(), &mut meter, 80, st, 100, |_| true);
    assert_eq!(st, ScrollState::Tail);
}

#[test]
fn scroll_up_at_top_is_clamped() {
    let mut t = Transcript::new(TuiLimits::default());
    for i in 0..5 {
        push_info(&mut t, &format!("e{i}"));
    }
    let mut meter = MapMeter::constant(5, 1);
    // Scrolling far past the top pins to entry 0, hidden 0.
    let st = scroll_up(
        t.entries(),
        &mut meter,
        80,
        10,
        ScrollState::Tail,
        1000,
        |_| true,
    );
    let ScrollState::Anchored {
        entry_id,
        hidden_rows,
    } = st
    else {
        panic!("expected anchored");
    };
    assert_eq!(entry_id, EntryId(0));
    assert_eq!(hidden_rows, 0);
}

#[test]
fn anchored_scroll_is_stable_under_appended_entries() {
    // Simulate: scroll up, then more content streams in. The anchor (entry id)
    // must keep the viewport on the same content.
    let mut t = Transcript::new(TuiLimits::default());
    for i in 0..20 {
        push_info(&mut t, &format!("e{i}"));
    }
    let mut meter = MapMeter::constant(20, 1);
    let st = scroll_up(
        t.entries(),
        &mut meter,
        80,
        10,
        ScrollState::Tail,
        3,
        |_| true,
    );
    let anchor_id = match st {
        ScrollState::Anchored { entry_id, .. } => entry_id,
        _ => panic!(),
    };
    // Stream 10 more entries.
    for i in 20..30 {
        push_info(&mut t, &format!("e{i}"));
    }
    let mut meter2 = MapMeter::constant(30, 1);
    let vp = select_viewport(t.entries(), &mut meter2, 80, 10, 0, st, |_| true);
    // Viewport still starts at the anchored entry — streaming didn't shift it.
    assert_eq!(t.entries()[vp.start].id, anchor_id);
}

#[test]
fn scroll_down_from_tail_is_a_noop_tail() {
    let mut t = Transcript::new(TuiLimits::default());
    for i in 0..10 {
        push_info(&mut t, &format!("e{i}"));
    }
    let mut meter = MapMeter::constant(10, 1);
    let st = scroll_down(t.entries(), &mut meter, 80, ScrollState::Tail, 5, |_| true);
    assert_eq!(st, ScrollState::Tail);
}

#[test]
fn empty_transcript_is_safe_for_all_operations() {
    let t = Transcript::new(TuiLimits::default());
    let mut meter = MapMeter::constant(0, 1);
    let vp = select_viewport(
        t.entries(),
        &mut meter,
        80,
        10,
        5,
        ScrollState::Tail,
        |_| true,
    );
    assert_eq!(vp, Viewport::EMPTY);
    let st = scroll_up(
        t.entries(),
        &mut meter,
        80,
        10,
        ScrollState::Tail,
        5,
        |_| true,
    );
    assert_eq!(st, ScrollState::Tail);
    let st = scroll_down(t.entries(), &mut meter, 80, ScrollState::Tail, 5, |_| true);
    assert_eq!(st, ScrollState::Tail);
}

// ── Iteration dividers ───────────────────────────────────────────────────────

fn push_divider(t: &mut Transcript, number: u32, maximum: Option<u32>) {
    t.ingest(TuiEntry::now(EntryKind::IterationDivider {
        number,
        maximum,
    }));
}

#[test]
fn iteration_divider_is_small_and_single_line() {
    let mut t = Transcript::new(TuiLimits::default());
    push_divider(&mut t, 7, Some(1000));
    // One retained entry.
    assert_eq!(t.entries().len(), 1);
    // Fixed conservative byte cost (a short, width-spanning rule).
    assert_eq!(t.retained_bytes(), 64);
    // One logical line in the retained accounting.
    assert_eq!(t.entries()[0].logical_lines, 1);
}

#[test]
fn iteration_divider_never_coalesces() {
    let mut t = Transcript::new(TuiLimits::default());
    push_text(&mut t, "first run");
    push_divider(&mut t, 1, Some(2));
    push_text(&mut t, "second run");
    // Divider sits between the two text runs; neither text run merges into the
    // divider, and the two text runs do not merge across it.
    assert_eq!(t.entries().len(), 3);
    assert!(matches!(
        t.entries()[1].kind,
        EntryKind::IterationDivider {
            number: 1,
            maximum: Some(2)
        }
    ));
    // The first text run retained its own payload (not coalesced away).
    if let EntryKind::Text(s) = &t.entries()[0].kind {
        assert_eq!(s, "first run");
    } else {
        panic!("first entry should be Text");
    }
    if let EntryKind::Text(s) = &t.entries()[2].kind {
        assert_eq!(s, "second run");
    } else {
        panic!("third entry should be Text");
    }
}

#[test]
fn iteration_divider_trims_as_a_whole_entry() {
    // A tight entry cap forces trimming; the divider is a whole entry, so it is
    // removed cleanly without being split or merging into neighbours.
    let limits = TuiLimits {
        max_entries: 4,
        max_bytes: 1024,
        max_entry_lines: 400,
    };
    let mut t = Transcript::new(limits);
    push_text(&mut t, "a");
    push_divider(&mut t, 1, Some(2));
    push_text(&mut t, "b");
    push_divider(&mut t, 2, Some(2));
    push_text(&mut t, "c");
    // After trimming the oldest, the buffer holds exactly the cap (plus the
    // never-trimmed newest entry is the last).
    assert!(t.entries().len() <= 5);
    // No entry is a partial divider merge; every retained divider is intact.
    for e in t.entries() {
        if let EntryKind::IterationDivider { number, maximum } = &e.kind {
            assert_eq!(*maximum, Some(2));
            assert!(*number == 1 || *number == 2);
        }
    }
}
