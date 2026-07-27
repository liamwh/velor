//! Syntax highlighting: a stable, engine-agnostic classification layer.
//!
//! The renderer talks only to [`HighlightEngine`] and
//! [`crate::highlight::token::SemanticToken`]; it never sees raw syntect scope
//! names or tree-sitter capture names. Two providers sit behind the engine:
//!
//! - [`syntect::SyntectProvider`] for the ~14 languages the bundled grammar set
//!   covers. State is preserved across the full source line-by-line so embedded
//!   languages (HTML `<script>`, Rust raw strings, block comments) carry forward.
//! - [`tree_sitter::TreeSitterProvider`] for composite template languages
//!   (Svelte, and later Vue/Astro) where syntect cannot load the grammar.
//!
//! Both return [`token::SemanticSpan`]s over the **whole source**; the engine
//! clips them to a requested hunk's byte range (a diff line, typically). This
//! guarantees embedded-language state is established before clipping, which is
//! the structural fix for the Svelte regression where isolated diff lines lost
//! their `<script lang="ts">` context.
//!
//! # Composition contract
//! Syntax owns the **foreground** only (see [`theme`]). Backgrounds — diff line
//! tints, intraline changes, selections, search matches, gutter — are owned by
//! the renderer and composed independently, so syntax colours are never washed
//! out by a saturated diff background.

pub mod syntect;
pub mod theme;
pub mod token;
pub mod tree_sitter;

use velor_core::file_edit::SyntaxKind;

pub use token::SemanticSpan;
#[allow(unused_imports)]
pub use token::SemanticToken;

use self::syntect::SyntectProvider;
use self::tree_sitter::TreeSitterProvider;

/// A byte range over the highlighted source, used to clip spans to a diff hunk.
/// `None` means "the whole source" (no clipping).
pub type ByteRange = std::ops::Range<usize>;

/// A highlight request: what language, what source, and (optionally) what slice
/// of that source to return spans for.
///
/// `full_source` is the whole file when available (composite languages carry it
/// on [`velor_core::file_edit::FileEdit`]); for plain languages it may be just
/// the visible hunk text, since state preservation across the whole file is not
/// required for line-oriented grammars.
#[derive(Debug, Clone)]
pub struct HighlightRequest<'a> {
    /// The language family — the routing key for provider selection.
    pub kind: SyntaxKind,
    /// The source to highlight. For composite languages this should be the full
    /// file so embedded-language state resolves; for plain languages a single
    /// line or hunk is fine.
    pub source: &'a str,
    /// When set, returned spans are clipped to this byte range and any span
    /// straddling a boundary is split (on a UTF-8 boundary). `None` returns
    /// spans for the whole `source`.
    pub clip: Option<ByteRange>,
}

impl<'a> HighlightRequest<'a> {
    /// Convenience constructor for a whole-source highlight (no clipping).
    #[must_use]
    pub fn full(kind: SyntaxKind, source: &'a str) -> Self {
        Self {
            kind,
            source,
            clip: None,
        }
    }

    /// Convenience constructor for a clipped highlight over `range`. (Used in
    /// tests; retained on the public API for callers that want explicit clipping.)
    #[must_use]
    #[allow(dead_code)]
    pub fn clipped(kind: SyntaxKind, source: &'a str, range: ByteRange) -> Self {
        Self {
            kind,
            source,
            clip: Some(range),
        }
    }
}

/// The top-level highlighter. Dispatches to the right provider by
/// [`SyntaxKind`] and clips results to the requested range. Built once at TUI
/// startup and reused for every render (providers cache grammars/queries).
///
/// `&mut self` is required because both providers own a parser that is advanced
/// in place; the caller holds the engine across the cache-miss render path only
/// (never per-frame).
pub struct HighlightEngine {
    syntect: SyntectProvider,
    svelte: TreeSitterProvider,
}

impl std::fmt::Debug for HighlightEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HighlightEngine")
            .field("syntect", &self.syntect)
            .field("svelte", &self.svelte)
            .finish()
    }
}

impl HighlightEngine {
    /// Builds both providers once (compiles grammars/queries). This is the only
    /// place grammars are constructed — never on the render hot path.
    #[must_use]
    pub fn new() -> Self {
        Self {
            syntect: SyntectProvider::new(),
            svelte: TreeSitterProvider::new(),
        }
    }

    /// Highlights per the request, returning semantic spans (clipped to
    /// `req.clip` when set). Never panics on malformed input: providers fall
    /// back to plain text, and clipping is boundary-safe.
    pub fn highlight(&mut self, req: &HighlightRequest<'_>) -> Vec<SemanticSpan> {
        let mut spans = if req.kind.is_composite() {
            // Composite languages (Svelte) need the full file for state; if the
            // caller only passed a hunk we still parse what we have (graceful).
            self.svelte.highlight(req.kind, req.source)
        } else {
            self.syntect.highlight(req.kind, req.source)
        };
        if let Some(range) = req.clip.clone() {
            spans = clip_spans(spans, req.source, range);
        }
        spans
    }
}

impl Default for HighlightEngine {
    fn default() -> Self {
        Self::new()
    }
}

/// Clips `spans` to `range`, splitting any span that straddles a boundary on a
/// UTF-8 character boundary of `source`. Returns spans whose `[start, end)` is
/// fully inside `[range.start, range.end)`, with start/end reinterpreted as
/// offsets relative to the **original source** (the caller slices the source
/// text itself). Spans entirely outside the range are dropped.
///
/// This preserves embedded-language state across the clip: the engine
/// highlighted the whole source, so a span deep inside `<script lang="ts">`
/// already carries its TypeScript classification even though the visible hunk
/// doesn't include the `<script>` tag.
fn clip_spans(spans: Vec<SemanticSpan>, source: &str, range: ByteRange) -> Vec<SemanticSpan> {
    // Clamp the range to the source length and snap both ends to UTF-8
    // boundaries (defensive — inputs should already be boundary-aligned).
    let end = range.end.min(source.len());
    let mut start = range.start.min(end);
    let mut end = end;
    if !source.is_char_boundary(start) {
        start = next_boundary(source, start);
    }
    if end < source.len() && !source.is_char_boundary(end) {
        end = next_boundary(source, end);
    }
    if start >= end {
        return Vec::new();
    }

    let mut out = Vec::with_capacity(spans.len());
    for s in spans {
        // Drop spans entirely outside the clip window.
        if s.end <= start || s.start >= end {
            continue;
        }
        let new_start = s.start.max(start);
        let new_end = s.end.min(end);
        if new_start >= new_end {
            continue;
        }
        out.push(SemanticSpan::new(new_start, new_end, s.token));
    }
    out
}

/// Advances `i` to the next UTF-8 character boundary in `source` (or returns
/// `source.len()` if there isn't one).
fn next_boundary(source: &str, mut i: usize) -> usize {
    while i < source.len() && !source.is_char_boundary(i) {
        i += 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn svelte_routes_to_tree_sitter() {
        // A Svelte request must produce TS-aware classifications that syntect
        // alone could never produce (no TS grammar is bundled).
        let mut engine = HighlightEngine::new();
        let src = "<script lang=\"ts\">let n: number = 7;</script>\n";
        let spans = engine.highlight(&HighlightRequest::full(SyntaxKind::Svelte, src));
        let has_ts_type = spans
            .iter()
            .any(|s| s.token == SemanticToken::Type && src[s.start..s.end].contains("number"));
        assert!(has_ts_type, "Svelte routes to tree-sitter: {spans:?}");
    }

    #[test]
    fn rust_routes_to_syntect() {
        let mut engine = HighlightEngine::new();
        let src = "fn main() { let x = 1; }\n";
        let spans = engine.highlight(&HighlightRequest::full(SyntaxKind::Rust, src));
        assert!(
            spans.iter().any(|s| s.token == SemanticToken::Keyword),
            "Rust keyword classified: {spans:?}"
        );
    }

    #[test]
    fn clip_keeps_only_in_range_spans() {
        let mut engine = HighlightEngine::new();
        let src = "fn a() {} fn b() {} fn c() {}\n";
        // Clip to the middle `fn b` region.
        let mid = src.find("fn b").unwrap_or(0);
        let clip = mid..mid + 8;
        let spans = engine.highlight(&HighlightRequest::clipped(
            SyntaxKind::Rust,
            src,
            clip.clone(),
        ));
        for s in &spans {
            assert!(s.start >= clip.start, "span {s:?} before clip start");
            assert!(s.end <= clip.end, "span {s:?} after clip end");
        }
    }

    #[test]
    fn clip_splits_straddling_spans_on_utf8_boundary() {
        // Manually construct spans and clip across a multibyte char.
        let src = "ab🦀cd";
        let spans = vec![
            // One span covering "b🦀c" (bytes 1..8: b=1, 🦀=2..6, c=6..7 -> wait)
            SemanticSpan::new(1, 7, SemanticToken::String), // b 🦀 c
        ];
        // Clip to bytes [3, 6): inside the emoji (bytes 2..6). start=3 is mid-emoji.
        let clipped = clip_spans(spans, src, 3..6);
        // Every returned span boundary must be a UTF-8 boundary.
        for s in &clipped {
            assert!(src.is_char_boundary(s.start), "start {s:?}");
            assert!(src.is_char_boundary(s.end), "end {s:?}");
        }
    }

    #[test]
    fn unknown_language_falls_back_gracefully() {
        let mut engine = HighlightEngine::new();
        let src = "some text with no grammar\n";
        let spans = engine.highlight(&HighlightRequest::full(SyntaxKind::PlainText, src));
        // Plain text: all spans are Text, no panic.
        assert!(spans.iter().all(|s| s.token == SemanticToken::Text));
    }
}
