//! Syntect-backed highlighter for the languages the bundled grammar set covers.
//!
//! The previous implementation highlighted each diff line in isolation with a
//! fresh [`HighlightLines`], which destroyed embedded-language state: the HTML
//! grammar only enters its `<script>`/`<style>` embedding contexts after seeing
//! the opening tag, and that state must persist across lines. The fix is to
//! highlight the **full logical source** with a single persistent
//! [`HighlightLines`] and then let the engine clip the resulting spans to a
//! diff hunk's byte range. This restores correct embedded-JS/CSS/Rust state for
//! every language, not just Svelte.
//!
//! The bundled `load_defaults_newlines()` set is retained as-is; it covers the
//! ~14 languages velor already advertised. Svelte, TS, TSX, TOML and Dockerfile
//! are structurally unsupported by syntect 5.3 (missing grammars / unsupported
//! grammar-format features) and route through the tree-sitter provider or fall
//! back to plain text elsewhere.

use std::sync::OnceLock;

use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, Theme, ThemeSet};
use syntect::parsing::{SyntaxReference, SyntaxSet};
use velor_core::file_edit::SyntaxKind;

use super::token::{SemanticSpan, SemanticToken};

/// The syntect provider. Owns the loaded grammar set and theme (built once and
/// reused — never reconstructed per frame). It is **stateless across calls**;
/// per-file state lives in a fresh [`HighlightLines`] constructed inside
/// [`SyntectProvider::highlight`], because the full source changes between calls
/// (different file / different revision).
pub struct SyntectProvider {
    set: SyntaxSet,
    theme: Theme,
}

impl std::fmt::Debug for SyntectProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyntectProvider")
            .field("syntaxes", &self.set.syntaxes().len())
            .finish()
    }
}

impl SyntectProvider {
    /// Loads the default grammar set and the base16-ocean.dark theme exactly
    /// once across the whole process (the binary data is large and static).
    #[must_use]
    pub fn new() -> Self {
        let (set, theme) = static_set();
        Self {
            set: set.clone(),
            theme: theme.clone(),
        }
    }

    /// Highlights `source` as a single document of `kind`, returning semantic
    /// spans over the whole source. The caller (the engine) clips to a hunk.
    ///
    /// State is preserved across lines: a single [`HighlightLines`] is fed the
    /// source line-by-line so embedded-language contexts (`<script>`, Rust raw
    /// strings, block comments, …) carry forward correctly.
    ///
    /// Never panics: syntect errors on a malformed line are swallowed and the
    /// line is emitted as [`SemanticToken::Text`].
    pub fn highlight(&self, kind: SyntaxKind, source: &str) -> Vec<SemanticSpan> {
        let syntax = self.syntax_for(kind);
        let mut hl = HighlightLines::new(syntax, &self.theme);
        let mut spans: Vec<SemanticSpan> = Vec::new();
        // Split on line boundaries but operate on byte offsets into `source`.
        // We must feed each line including its terminator when present, because
        // some grammars transition state on the newline.
        let mut offset = 0usize;
        for line in source.split_inclusive('\n') {
            let line_end = offset + line.len();
            let body_end = line.len(); // for offset math
            let _ = body_end;
            match hl.highlight_line(line, &self.set) {
                Ok(regions) => {
                    let mut line_offset = offset;
                    for (style, text) in regions {
                        let text_len = text.len();
                        let start = line_offset;
                        let end = line_offset + text_len;
                        line_offset = end;
                        if text.is_empty() {
                            continue;
                        }
                        let token = style_to_token(style);
                        // Coalesce adjacent identical tokens to keep the span
                        // count small (syntect emits many tiny regions).
                        if let Some(last) = spans.last_mut()
                            && last.token == token
                            && last.end == start
                        {
                            last.end = end;
                        } else {
                            spans.push(SemanticSpan::new(start, end, token));
                        }
                    }
                }
                Err(_) => {
                    // Fall back to plain text for this line; state may be
                    // inconsistent afterwards but syntect recovers on the next
                    // well-formed line.
                    if !line.is_empty() {
                        spans.push(SemanticSpan::new(offset, line_end, SemanticToken::Text));
                    }
                }
            }
            offset = line_end;
        }
        spans
    }

    /// Resolves the syntect [`SyntaxReference`] for a [`SyntaxKind`], falling
    /// back to plain text when the grammar is absent from the bundled set.
    #[must_use]
    fn syntax_for(&self, kind: SyntaxKind) -> &SyntaxReference {
        let hint = kind.syntect_hint();
        if hint.is_empty() {
            return self.set.find_syntax_plain_text();
        }
        self.set
            .find_syntax_by_extension(hint)
            .or_else(|| self.set.find_syntax_by_name(hint))
            .unwrap_or_else(|| self.set.find_syntax_plain_text())
    }
}

impl Default for SyntectProvider {
    fn default() -> Self {
        Self::new()
    }
}

/// Maps a syntect [`syntect::highlighting::Style`] to a [`SemanticToken`] using
/// the scope-derived classification. Syntect doesn't expose scope names on the
/// per-region `Style` directly (the theme is already applied), so we classify by
/// the resolved foreground colour and font style against the known theme palette.
/// This is less precise than scope-matching but stable, and the colour palette
/// is the same `base16-ocean.dark` we theme from.
fn style_to_token(style: syntect::highlighting::Style) -> SemanticToken {
    use super::theme::PALETTE;
    let fg = style.foreground;
    if fg.a == 0 {
        return SemanticToken::Text;
    }
    let key = (fg.r, fg.g, fg.b);
    // Look the colour up in the palette; if it matches a known semantic colour,
    // use the reverse mapping. This ties foreground colour -> token so the
    // renderer's independent theme can recolour consistently.
    if let Some(token) = PALETTE.classify(key) {
        return token;
    }
    // Bold/italic hints as a last resort.
    if style.font_style.contains(FontStyle::ITALIC) {
        return SemanticToken::Comment;
    }
    SemanticToken::Text
}

/// Loads the default syntax set + theme once and shares clones (syntect's
/// `SyntaxSet`/`Theme` are cheaply cloneable: they wrap `Arc`ed compiled data).
fn static_set() -> (&'static SyntaxSet, &'static Theme) {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    static THEME: OnceLock<Theme> = OnceLock::new();
    let set = SET.get_or_init(SyntaxSet::load_defaults_newlines);
    let theme = THEME.get_or_init(|| {
        ThemeSet::load_defaults()
            .themes
            .get("base16-ocean.dark")
            .cloned()
            .unwrap_or_default()
    });
    (set, theme)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_body_is_highlighted_with_state() {
        // A multi-line Rust fn: previously, isolated-per-line highlighting
        // dropped most of the body to default. With state preservation the
        // keywords/strings/numbers are classified.
        let src = "fn main() {\n    let x = 42;\n    let s = \"hi\";\n}\n";
        let p = SyntectProvider::new();
        let spans = p.highlight(SyntaxKind::Rust, src);
        let tokens: Vec<_> = spans
            .iter()
            .map(|s| (s.token, src.get(s.start..s.end).unwrap_or("").to_string()))
            .collect();
        // `fn` and `let` are keywords; `42` a number; `"hi"` a string.
        assert!(
            tokens.iter().any(|(t, _)| *t == SemanticToken::Keyword),
            "keywords classified: {tokens:?}"
        );
        assert!(
            tokens
                .iter()
                .any(|(t, txt)| *t == SemanticToken::Number && txt.contains('4')),
            "number classified: {tokens:?}"
        );
        assert!(
            tokens.iter().any(|(t, _)| *t == SemanticToken::String),
            "string classified: {tokens:?}"
        );
    }

    #[test]
    fn unsupported_kind_falls_back_to_plain_text_without_panic() {
        let p = SyntectProvider::new();
        // TypeScript has no bundled grammar -> plain text, but must not panic.
        let spans = p.highlight(SyntaxKind::TypeScript, "let x: number = 1;\n");
        assert!(!spans.is_empty());
        // Plain text means all spans are Text.
        assert!(spans.iter().all(|s| s.token == SemanticToken::Text));
    }

    #[test]
    fn html_embedded_js_survives_across_lines() {
        // The regression that caused the Svelte bug: HTML grammar must keep its
        // embedded-JS state across the script body lines.
        let src = "<script>\n  let count = 0;\n  console.log(count);\n</script>\n";
        let p = SyntectProvider::new();
        let spans = p.highlight(SyntaxKind::Html, src);
        let has_keyword = spans
            .iter()
            .any(|s| s.token == SemanticToken::Keyword && src[s.start..s.end].contains("let"));
        assert!(has_keyword, "embedded JS `let` is a keyword: {spans:?}");
    }

    #[test]
    fn malformed_source_does_not_panic() {
        let p = SyntectProvider::new();
        let _ = p.highlight(SyntaxKind::Rust, "fn { <<<< \n  incomplete");
        let _ = p.highlight(SyntaxKind::Html, "<script> <<< \n broken");
        // Reaching here means no panic.
    }

    #[test]
    fn unicode_spans_cover_full_scalars() {
        let src = "let emoji = \"🦀x\";\n";
        let p = SyntectProvider::new();
        let spans = p.highlight(SyntaxKind::Rust, src);
        // Every span boundary must land on a UTF-8 char boundary.
        for s in &spans {
            assert!(src.is_char_boundary(s.start), "start {s:?}");
            assert!(src.is_char_boundary(s.end), "end {s:?}");
        }
    }
}
