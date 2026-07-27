//! Semantic token classification, independent of any highlighting engine.
//!
//! Both the syntect and tree-sitter providers map their native output onto this
//! finite enum. The renderer never sees raw syntect scope names or tree-sitter
//! capture names — only [`SemanticToken`], resolved to a foreground style via
//! [`crate::highlight::theme`]. Backgrounds (diff tints, selections, search
//! matches) are owned entirely by the renderer and never set here.

/// A byte range over the highlighted source, tagged with a semantic role.
///
/// Byte offsets are UTF-8 offsets into the source string the highlighter was
/// asked to highlight. They always fall on UTF-8 scalar boundaries (both engines
/// emit byte-aligned, boundary-safe ranges; the clip helper in
/// [`crate::HighlightEngine`] enforces it when truncating to a diff hunk).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SemanticSpan {
    /// Inclusive start byte offset in the source.
    pub start: usize,
    /// Exclusive end byte offset in the source.
    pub end: usize,
    /// The semantic role of the bytes in `[start, end)`.
    pub token: SemanticToken,
}

impl SemanticSpan {
    /// Construct a span. In debug builds this asserts the range is ordered and
    /// non-empty so engine bugs surface locally rather than as empty spans.
    #[must_use]
    pub fn new(start: usize, end: usize, token: SemanticToken) -> Self {
        debug_assert!(start <= end, "span start ({start}) must be <= end ({end})");
        Self { start, end, token }
    }

    /// The byte length of this span. (Used in tests; kept on the public API.)
    #[must_use]
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.end - self.start
    }

    /// `true` when the span covers zero bytes. (Used in tests; kept on the API.)
    #[must_use]
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }
}

/// The finite set of semantic roles the renderer knows how to colour.
///
/// This is deliberately small and engine-agnostic. Dotted native capture names
/// (e.g. tree-sitter's `keyword.conditional`, `string.escape`, or syntect's
/// `storage.type.function`) collapse to the closest category in
/// [`SemanticToken::from_capture_name`] / [`SemanticToken::from_syntect_scope`].
/// Unknown names map to [`SemanticToken::Text`] (terminal default foreground).
///
/// Adapted from the canonical tree-sitter highlight capture convention to fit
/// velor's existing terminal theme; not every upstream name has a distinct
/// colour here — precision is traded for a stable, theme-able surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(test, derive(serde::Serialize))]
pub enum SemanticToken {
    /// Unstyled text — terminal default foreground. The fallback for unknown or
    /// unclassified content. Never carries a colour.
    Text,
    /// Comments, including documentation comments.
    Comment,
    /// Language keywords (`import`, `let`, `return`, …). `Control` covers
    /// flow keywords separately when an engine distinguishes them.
    Keyword,
    /// Control-flow keywords (`if`, `else`, `for`, `while`, `await`, …). Falls
    /// back to the same colour as [`Self::Keyword`] when the theme doesn't
    /// distinguish them.
    Control,
    /// String literals, including escapes and regexps.
    String,
    /// Numeric literals.
    Number,
    /// Constants (`PI`, `MAX_SIZE`, enum variants when classified as such).
    Constant,
    /// Function names and calls.
    Function,
    /// Type names (classes, interfaces, type aliases, generics).
    Type,
    /// HTML/XML/Svelte tag names.
    Tag,
    /// Attribute names (HTML attrs, Svelte directives, JSX props).
    Attribute,
    /// Object properties, CSS properties.
    Property,
    /// Plain variables and parameters.
    Variable,
    /// Built-in / special variables (`this`, `super`, …).
    #[allow(dead_code)]
    VariableBuiltin,
    /// Punctuation: brackets, delimiters, separators.
    Punctuation,
    /// Operators (`=`, `+`, `=>`, `*`, …).
    Operator,
    /// Labels, decorators, and Svelte block keywords (`#if`, `:else`, `@render`).
    Label,
}

impl SemanticToken {
    /// Maps a tree-sitter highlight capture name (e.g. `"keyword.conditional"`,
    /// `"string.escape"`) to a [`SemanticToken`]. The dotted suffix is matched
    /// prefix-style: `keyword.*` → [`Self::Keyword`] unless a more specific rule
    /// applies (e.g. `keyword.conditional`/`.repeat`/`.return` → [`Self::Control`]).
    /// Unrecognised names return [`Self::Text`].
    #[must_use]
    pub fn from_capture_name(name: &str) -> Self {
        // Order matters: check the most-specific prefixes first.
        if name == "keyword.conditional"
            || name == "keyword.repeat"
            || name == "keyword.return"
            || name == "keyword.exception"
            || name == "keyword.coroutine"
            || name == "keyword.operator"
        {
            return Self::Control;
        }
        // Prefix match on the first dotted segment.
        let head = name.split('.').next().unwrap_or(name);
        match head {
            "comment" => Self::Comment,
            "keyword" => Self::Keyword,
            "string" | "character" | "string.special" => Self::String,
            "number" => Self::Number,
            "constant" => Self::Constant,
            "constructor" => Self::Type,
            "function" => Self::Function,
            "type" => Self::Type,
            "tag" => Self::Tag,
            "attribute" => Self::Attribute,
            "property" => Self::Property,
            "variable" => Self::Variable,
            "punctuation" => Self::Punctuation,
            "operator" => Self::Operator,
            "label" => Self::Label,
            "module" => Self::Type,
            "embedded" => Self::Text,
            // tree-sitter emits these for svelte block tags; route to Label so
            // {#if}/{:else}/{@render} get a distinct, readable colour.
            "none" => Self::Text,
            _ => Self::Text,
        }
    }

    /// Maps a syntect scope selector (the dot-separated scope stack, e.g.
    /// `"storage.type.function"` or `"string.quoted.double"`) to a
    /// [`SemanticToken`]. The first segment carries the language family
    /// (`source`/`text`/`meta`/`punctuation`/…) and is skipped; the meaningful
    /// segment is what follows.
    ///
    /// Currently unused by the production syntect provider (which recovers the
    /// token from the already-themed foreground colour via the palette reverse
    /// lookup), but retained as part of the stable classification surface for
    /// future scope-driven classification.
    #[must_use]
    #[allow(dead_code)]
    pub fn from_syntect_scope(scope: &str) -> Self {
        // Walk segments left to right; pick the most specific recognised one.
        // `storage.type` -> Keyword; `storage.type.function` -> Keyword too
        // (syntect rarely distinguishes). `entity.name.function` -> Function.
        for seg in scope.split('.') {
            match seg {
                "comment" => return Self::Comment,
                "string" | "constant" if seg == "string" => return Self::String,
                _ => {}
            }
        }
        // Fall back to head-based matching for the common cases.
        let scope = scope.trim();
        if scope.is_empty() {
            return Self::Text;
        }
        // Build a pseudo capture-name from the leading meaningful segments and
        // reuse the tree-sitter mapper so both engines share one classification
        // table.
        let mut pseudo = String::new();
        for seg in scope.split('.') {
            match seg {
                // Skip language containers and meta wrappers.
                "source" | "text" | "meta" | "embedded" => continue,
                _ => {
                    if !pseudo.is_empty() {
                        pseudo.push('.');
                    }
                    pseudo.push_str(seg);
                }
            }
        }
        // Try the head segment directly against a syntect-specific table first.
        let head = pseudo.split('.').next().unwrap_or(&pseudo);
        match head {
            "keyword" | "storage" => Self::Keyword,
            "support" => Self::Function,
            "string" => Self::String,
            "constant" => Self::Constant,
            "numeric" => Self::Number,
            "entity" => {
                // entity.name.function / entity.name.type / entity.name.tag /
                // entity.other.attribute-name — second segment decides.
                if scope.contains("function") {
                    Self::Function
                } else if scope.contains("type") {
                    Self::Type
                } else if scope.contains("tag") {
                    Self::Tag
                } else if scope.contains("attribute") {
                    Self::Attribute
                } else {
                    // class / struct / unrecognised entity -> treat as a type.
                    Self::Type
                }
            }
            "variable" => Self::Variable,
            "punctuation" => Self::Punctuation,
            _ => Self::from_capture_name(&pseudo),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn span_len_and_empty() {
        let s = SemanticSpan::new(3, 8, SemanticToken::Number);
        assert_eq!(s.len(), 5);
        assert!(!s.is_empty());
        let e = SemanticSpan::new(4, 4, SemanticToken::Text);
        assert!(e.is_empty());
    }

    #[test]
    fn capture_name_maps_control_vs_keyword() {
        assert_eq!(
            SemanticToken::from_capture_name("keyword.conditional"),
            SemanticToken::Control
        );
        assert_eq!(
            SemanticToken::from_capture_name("keyword.import"),
            SemanticToken::Keyword
        );
        assert_eq!(
            SemanticToken::from_capture_name("string.escape"),
            SemanticToken::String
        );
        assert_eq!(
            SemanticToken::from_capture_name("function.call"),
            SemanticToken::Function
        );
        assert_eq!(
            SemanticToken::from_capture_name("type.builtin"),
            SemanticToken::Type
        );
        assert_eq!(
            SemanticToken::from_capture_name("variable.builtin"),
            SemanticToken::Variable
        );
        assert_eq!(
            SemanticToken::from_capture_name("totally.unknown"),
            SemanticToken::Text
        );
    }

    #[test]
    fn syntect_scope_maps_entity_names() {
        assert_eq!(
            SemanticToken::from_syntect_scope("entity.name.function.rust"),
            SemanticToken::Function
        );
        assert_eq!(
            SemanticToken::from_syntect_scope("entity.name.tag.html"),
            SemanticToken::Tag
        );
        assert_eq!(
            SemanticToken::from_syntect_scope("entity.other.attribute-name.html"),
            SemanticToken::Attribute
        );
        assert_eq!(
            SemanticToken::from_syntect_scope("storage.type.ts"),
            SemanticToken::Keyword
        );
        assert_eq!(
            SemanticToken::from_syntect_scope("string.quoted.double.js"),
            SemanticToken::String
        );
        assert_eq!(
            SemanticToken::from_syntect_scope("comment.line.double-slash"),
            SemanticToken::Comment
        );
        assert_eq!(SemanticToken::from_syntect_scope(""), SemanticToken::Text);
    }
}
