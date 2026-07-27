//! Tree-sitter-backed highlighter for composite template languages.
//!
//! Used for **Svelte** (and extensible to Vue/Astro later), where syntect's
//! grammar engine cannot load the modern Svelte `.sublime-syntax` — it relies
//! on `extends`/`meta_prepend`, which syntect 5.3 does not merge. Tree-sitter
//! parses the whole file once (state is native to the parser; no per-line
//! concern) and resolves embedded-language injections (`<script lang="ts">` →
//! TypeScript, plain `<script>` → JavaScript, `<style>` → CSS) automatically via
//! `tree-sitter-highlight`'s injection layering.
//!
//! All grammars vendor their C source and compile at build time via `cc`; no
//! runtime grammar download. The query strings (`HIGHLIGHTS_QUERY`, …) are
//! `include_str!`'d into the grammar crates, so they ship in the binary.

use std::sync::OnceLock;

use tree_sitter::Language;
use tree_sitter_highlight::{HighlightConfiguration, HighlightEvent, Highlighter};
use velor_core::file_edit::SyntaxKind;

use super::token::{SemanticSpan, SemanticToken};

/// The recognised tree-sitter capture names, in the order the
/// [`HighlightConfiguration::configure`] indexing scheme expects. Each maps to a
/// [`SemanticToken`] via [`SemanticToken::from_capture_name`]; the index returned
/// by tree-sitter (`Highlight(usize)`) is a position into this slice.
const CAPTURE_NAMES: &[&str] = &[
    "comment",
    "keyword",
    "keyword.conditional",
    "keyword.repeat",
    "keyword.return",
    "keyword.exception",
    "keyword.operator",
    "function",
    "function.call",
    "function.builtin",
    "type",
    "type.builtin",
    "string",
    "string.special",
    "number",
    "constant",
    "constant.builtin",
    "tag",
    "tag.attribute",
    "attribute",
    "property",
    "variable",
    "variable.builtin",
    "variable.parameter",
    "variable.member",
    "operator",
    "punctuation",
    "punctuation.bracket",
    "punctuation.delimiter",
    "label",
];

/// The reverse lookup table: capture-name index → [`SemanticToken`]. Built once.
fn capture_tokens() -> &'static [SemanticToken] {
    static TOKENS: OnceLock<Vec<SemanticToken>> = OnceLock::new();
    TOKENS.get_or_init(|| {
        CAPTURE_NAMES
            .iter()
            .map(|n| SemanticToken::from_capture_name(n))
            .collect()
    })
}

/// Caches the (svelte + injected ts/js/css/html) [`HighlightConfiguration`]s so
/// grammars and queries are compiled exactly once across the process.
struct Configs {
    svelte: HighlightConfiguration,
    ts: HighlightConfiguration,
    js: HighlightConfiguration,
    css: HighlightConfiguration,
    html: HighlightConfiguration,
}

impl std::fmt::Debug for Configs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Configs").finish()
    }
}

/// The tree-sitter provider for composite languages. Owns the compiled query
/// configurations and one [`Highlighter`] (which owns a parser + query cursor,
/// reused across calls). Built once at startup.
pub struct TreeSitterProvider {
    configs: Configs,
    highlighter: Highlighter,
}

impl std::fmt::Debug for TreeSitterProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TreeSitterProvider")
            .field("configs", &self.configs)
            .finish()
    }
}

impl TreeSitterProvider {
    /// Compiles the svelte + injection grammars and their highlight queries.
    ///
    /// # Panics
    /// Only panics if a bundled grammar's query is malformed — i.e. a bug in a
    /// vendored crate version, not in user input. The configurations are static
    /// data shipped with the binary.
    #[must_use]
    pub fn new() -> Self {
        let mut svelte = HighlightConfiguration::new(
            Language::from(tree_sitter_svelte_ng::LANGUAGE),
            "svelte",
            // Svelte's highlights.scm begins with `; inherits: html`; concatenate the
            // html highlights so the base tag/attribute patterns are present.
            &format!(
                "{}\n{}",
                tree_sitter_html::HIGHLIGHTS_QUERY,
                tree_sitter_svelte_ng::HIGHLIGHTS_QUERY
            ),
            tree_sitter_svelte_ng::INJECTIONS_QUERY,
            tree_sitter_svelte_ng::LOCALS_QUERY,
        )
        .expect("svelte grammar/query must be valid");

        let mut ts = HighlightConfiguration::new(
            Language::from(tree_sitter_typescript::LANGUAGE_TYPESCRIPT),
            "typescript",
            tree_sitter_typescript::HIGHLIGHTS_QUERY,
            "",
            tree_sitter_typescript::LOCALS_QUERY,
        )
        .expect("typescript grammar/query must be valid");

        let mut js = HighlightConfiguration::new(
            Language::from(tree_sitter_javascript::LANGUAGE),
            "javascript",
            tree_sitter_javascript::HIGHLIGHT_QUERY,
            tree_sitter_javascript::INJECTIONS_QUERY,
            tree_sitter_javascript::LOCALS_QUERY,
        )
        .expect("javascript grammar/query must be valid");

        let mut css = HighlightConfiguration::new(
            Language::from(tree_sitter_css::LANGUAGE),
            "css",
            tree_sitter_css::HIGHLIGHTS_QUERY,
            "",
            "",
        )
        .expect("css grammar/query must be valid");

        let mut html = HighlightConfiguration::new(
            Language::from(tree_sitter_html::LANGUAGE),
            "html",
            tree_sitter_html::HIGHLIGHTS_QUERY,
            tree_sitter_html::INJECTIONS_QUERY,
            "",
        )
        .expect("html grammar/query must be valid");

        // Configure each config with the same capture-name list so Highlight
        // indices line up across all languages.
        for c in [&mut svelte, &mut ts, &mut js, &mut css, &mut html] {
            c.configure(CAPTURE_NAMES);
        }

        Self {
            configs: Configs {
                svelte,
                ts,
                js,
                css,
                html,
            },
            highlighter: Highlighter::new(),
        }
    }

    /// Highlights the whole `source` as Svelte, returning semantic spans.
    /// Embedded `<script lang="ts">`/`<script>`/`<style>` blocks are handled by
    /// tree-sitter injections. Never panics on malformed source — tree-sitter's
    /// error-recovery keeps parsing, and query errors fall back to plain text.
    pub fn highlight(&mut self, kind: SyntaxKind, source: &str) -> Vec<SemanticSpan> {
        let (root_config, _lang_name) = match kind {
            SyntaxKind::Svelte => (&self.configs.svelte, "svelte"),
            // Other composite kinds (Vue/Astro) would route here once added;
            // for now Svelte is the only composite kind.
            other => {
                debug_assert!(
                    false,
                    "TreeSitterProvider only handles composite kinds, got {other:?}"
                );
                return Self::plain_text_spans(source);
            }
        };

        let bytes = source.as_bytes();
        let tokens = capture_tokens();
        let events = self
            .highlighter
            .highlight(root_config, bytes, None, |lang| {
                // Injection callback: tree-sitter-svelte-ng injects "typescript" /
                // "javascript" / "scss" (for all CSS variants) / "pug".
                match lang {
                    "typescript" => Some(&self.configs.ts),
                    "javascript" => Some(&self.configs.js),
                    "css" | "scss" | "postcss" | "less" | "stylus" => Some(&self.configs.css),
                    "html" => Some(&self.configs.html),
                    _ => None,
                }
            });

        let events = match events {
            Ok(e) => e,
            Err(_) => return Self::plain_text_spans(source),
        };

        let mut spans: Vec<SemanticSpan> = Vec::new();
        let mut stack: Vec<usize> = Vec::new();
        for ev in events {
            match ev {
                Ok(HighlightEvent::Source { start, end }) => {
                    if start >= end {
                        continue;
                    }
                    let token = stack
                        .last()
                        .copied()
                        .and_then(|i| tokens.get(i).copied())
                        .unwrap_or(SemanticToken::Text);
                    // Coalesce adjacent spans with the same token + contiguous range.
                    if let Some(last) = spans.last_mut()
                        && last.token == token
                        && last.end == start
                    {
                        last.end = end;
                    } else {
                        spans.push(SemanticSpan::new(start, end, token));
                    }
                }
                Ok(HighlightEvent::HighlightStart(h)) => stack.push(h.0),
                Ok(HighlightEvent::HighlightEnd) => {
                    stack.pop();
                }
                Err(_) => {
                    // On a query/pars error mid-stream, fall back to text for the
                    // remainder rather than panicking.
                    break;
                }
            }
        }

        // Guarantee full coverage: any unspanned bytes become Text so the
        // renderer never sees a gap. (Tree-sitter covers the whole source in
        // practice, but this is a cheap safety net.)
        spans = fill_gaps(spans, source.len());

        // Enforce UTF-8 boundary safety on every span.
        for s in &spans {
            debug_assert!(
                source.is_char_boundary(s.start) && source.is_char_boundary(s.end),
                "tree-sitter span not on a UTF-8 boundary: {s:?}"
            );
        }

        spans
    }

    /// Returns a single plain-text span covering the whole source — the graceful
    /// fallback when the parser/query is unavailable or fails.
    fn plain_text_spans(source: &str) -> Vec<SemanticSpan> {
        if source.is_empty() {
            return Vec::new();
        }
        vec![SemanticSpan::new(0, source.len(), SemanticToken::Text)]
    }
}

impl Default for TreeSitterProvider {
    fn default() -> Self {
        Self::new()
    }
}

/// Fills unspanned byte ranges with [`SemanticToken::Text`] so the renderer has
/// contiguous coverage of `[0, len)`. Input spans must be sorted + non-overlapping.
fn fill_gaps(mut spans: Vec<SemanticSpan>, len: usize) -> Vec<SemanticSpan> {
    if spans.is_empty() {
        if len == 0 {
            return spans;
        }
        spans.push(SemanticSpan::new(0, len, SemanticToken::Text));
        return spans;
    }
    spans.sort_by_key(|s| s.start);
    let mut out = Vec::with_capacity(spans.len() + 1);
    let mut cursor = 0usize;
    for s in spans {
        if s.start > cursor {
            out.push(SemanticSpan::new(cursor, s.start, SemanticToken::Text));
        }
        if s.end > cursor {
            out.push(s);
            cursor = s.end;
        }
    }
    if cursor < len {
        out.push(SemanticSpan::new(cursor, len, SemanticToken::Text));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token_at(spans: &[SemanticSpan], source: &str, needle: &str) -> Option<SemanticToken> {
        let idx = source.find(needle)?;
        spans
            .iter()
            .find(|s| s.start <= idx && idx < s.end)
            .map(|s| s.token)
    }

    fn highlight_svelte(src: &str) -> Vec<SemanticSpan> {
        let mut p = TreeSitterProvider::new();
        p.highlight(SyntaxKind::Svelte, src)
    }

    #[test]
    fn svelte_routes_here() {
        // Sanity: a real Svelte file produces non-trivial spans (not just Text).
        let src = "<script lang=\"ts\">let x = 1;</script>\n";
        let spans = highlight_svelte(src);
        assert!(
            spans.iter().any(|s| s.token != SemanticToken::Text),
            "expected some classification, got {spans:?}"
        );
    }

    #[test]
    fn typescript_in_script_lang_ts_is_classified() {
        let src = "<script lang=\"ts\">\nimport { f } from 'svelte';\ninterface Item { id: number; }\nlet n: number = 42;\n</script>\n";
        let spans = highlight_svelte(src);
        // `import` and `interface` are keywords; `number` a type; `42` a number;
        // `'svelte'` a string.
        assert_eq!(
            token_at(&spans, src, "import"),
            Some(SemanticToken::Keyword)
        );
        assert_eq!(
            token_at(&spans, src, "interface"),
            Some(SemanticToken::Keyword)
        );
        assert_eq!(token_at(&spans, src, "number"), Some(SemanticToken::Type));
        assert_eq!(token_at(&spans, src, "42"), Some(SemanticToken::Number));
        assert_eq!(
            token_at(&spans, src, "'svelte'"),
            Some(SemanticToken::String)
        );
    }

    #[test]
    fn javascript_in_plain_script_is_classified() {
        let src = "<script>\nlet count = 0;\nfunction go() { return count; }\n</script>\n";
        let spans = highlight_svelte(src);
        assert_eq!(token_at(&spans, src, "let"), Some(SemanticToken::Keyword));
        assert_eq!(token_at(&spans, src, "0"), Some(SemanticToken::Number));
        assert_eq!(token_at(&spans, src, "go"), Some(SemanticToken::Function));
    }

    #[test]
    fn html_tags_and_attributes_distinguished() {
        let src = "<button class=\"x\" on:click={go}>hi</button>\n";
        let spans = highlight_svelte(src);
        // `button` is a tag; `class` an attribute.
        assert_eq!(token_at(&spans, src, "button"), Some(SemanticToken::Tag));
        assert_eq!(
            token_at(&spans, src, "class"),
            Some(SemanticToken::Attribute)
        );
    }

    #[test]
    fn svelte_runes_classified() {
        let src = "<script lang=\"ts\">\nlet count = $state(0);\nlet d = $derived(count * 2);\n$effect(() => {});\nlet { name } = $props();\n</script>\n";
        let spans = highlight_svelte(src);
        // Runes ($state, $derived, $effect, $props) should get a meaningful
        // classification (function), not Text.
        assert_eq!(
            token_at(&spans, src, "$state"),
            Some(SemanticToken::Function)
        );
        assert_eq!(
            token_at(&spans, src, "$derived"),
            Some(SemanticToken::Function)
        );
        assert_eq!(
            token_at(&spans, src, "$effect"),
            Some(SemanticToken::Function)
        );
        assert_eq!(
            token_at(&spans, src, "$props"),
            Some(SemanticToken::Function)
        );
    }

    #[test]
    fn css_in_style_is_classified() {
        let src = "<style>\n.btn { color: red; padding: 4px; }\n</style>\n";
        let spans = highlight_svelte(src);
        // CSS should produce some non-Text spans (the injection ran).
        let css_start = src.find(".btn").unwrap_or(0);
        let css_end = src.find("</style>").unwrap_or(src.len());
        let css_spans: Vec<_> = spans
            .iter()
            .filter(|s| s.start >= css_start && s.end <= css_end)
            .collect();
        assert!(
            css_spans.iter().any(|s| s.token != SemanticToken::Text),
            "CSS should be classified: {css_spans:?}"
        );
    }

    #[test]
    fn malformed_source_does_not_panic() {
        let _ = highlight_svelte("<script>\n<<<< broken\n");
        let _ = highlight_svelte("<div>\n{#if \n  incomplete");
        let _ = highlight_svelte("{#await");
        // Reaching here = no panic.
    }

    #[test]
    fn unicode_spans_are_boundary_safe() {
        let src = "<p>🦀 emoji and ¥ symbols</p>\n";
        let spans = highlight_svelte(src);
        for s in &spans {
            assert!(src.is_char_boundary(s.start), "start {s:?}");
            assert!(src.is_char_boundary(s.end), "end {s:?}");
        }
    }

    #[test]
    fn highlighting_is_deterministic() {
        let src = "<script lang=\"ts\">let x = 1; let y = 's';</script>\n<button>A</button>\n";
        let a = highlight_svelte(src);
        let b = highlight_svelte(src);
        assert_eq!(a, b, "two highlights of the same source must be identical");
    }

    #[test]
    fn fill_gaps_covers_whole_source() {
        let spans = vec![SemanticSpan::new(2, 5, SemanticToken::Number)];
        let out = fill_gaps(spans, 10);
        // Expect [0,2)=Text, [2,5)=Number, [5,10)=Text.
        assert_eq!(out.len(), 3);
        assert_eq!(out[0], SemanticSpan::new(0, 2, SemanticToken::Text));
        assert_eq!(out[2], SemanticSpan::new(5, 10, SemanticToken::Text));
    }

    #[test]
    fn svelte_blocks_and_directives_classified() {
        let src = "{#if x > 0}\n  <p>a</p>\n{:else}\n  <p>b</p>\n{/if}\n";
        let spans = highlight_svelte(src);
        // The `if`/`else` keywords inside Svelte blocks should be classified
        // (Keyword/Control), not left as plain Text.
        let block_tokens: Vec<_> = spans
            .iter()
            .filter(|s| {
                let t = &src[s.start..s.end];
                t.contains("if") || t.contains("else")
            })
            .map(|s| s.token)
            .collect();
        assert!(
            block_tokens
                .iter()
                .any(|t| matches!(t, SemanticToken::Keyword | SemanticToken::Control)),
            "svelte block keywords classified: {block_tokens:?}"
        );
    }
}
