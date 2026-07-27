//! Theme: maps [`SemanticToken`] to a terminal foreground style.
//!
//! **Foreground only.** No [`Style`] returned here ever sets a background —
//! backgrounds are owned by the renderer (diff line tints, intraline changes,
//! selections, search matches, gutter). This is the load-bearing invariant that
//! keeps syntax colours from being washed out by diff backgrounds: the renderer
//! composes `token_style(token) | diff_background` rather than letting either
//! side clobber the other.
//!
//! The palette mirrors `base16-ocean.dark` (the syntect theme velor previously
//! loaded) so the migration is colour-neutral for languages that already
//! highlighted correctly.

use ratatui::style::{Color, Modifier, Style};

use super::token::SemanticToken;

/// The `base16-ocean.dark` palette as flat `(r, g, b)` tuples, in declaration
/// order. Centralising it here lets [`token_style`] colour tokens and lets the
/// syntect provider recover a [`SemanticToken`] from a syntect-resolved
/// foreground colour (the reverse direction), because syntect applies the theme
/// internally and only exposes the resulting colour.
pub(crate) const PALETTE: Palette = Palette;

/// A static colour-keyed reverse lookup for the theme palette. Used by the
/// syntect provider to map an already-themed foreground colour back onto a
/// [`SemanticToken`], so both engines feed the renderer the same classification.
pub(crate) struct Palette;

impl Palette {
    /// Forward direction: the colour for a token (mirrors [`token_style`]).
    #[must_use]
    pub const fn color_of(token: SemanticToken) -> Option<(u8, u8, u8)> {
        match token {
            SemanticToken::Comment => Some((0x8f, 0xa1, 0xb3)),
            SemanticToken::Keyword => Some((0xb4, 0x8e, 0xad)),
            SemanticToken::Control => Some((0xb4, 0x8e, 0xad)),
            SemanticToken::String => Some((0xa3, 0xbe, 0x8c)),
            SemanticToken::Number => Some((0xd0, 0x87, 0x70)),
            SemanticToken::Constant => Some((0xeb, 0xcb, 0x8b)),
            SemanticToken::Function => Some((0x8f, 0xa1, 0xb3)),
            SemanticToken::Type => Some((0x96, 0xb5, 0xb4)),
            SemanticToken::Tag => Some((0xbf, 0x61, 0x6a)),
            SemanticToken::Attribute => Some((0xeb, 0xcb, 0x8b)),
            SemanticToken::Property => Some((0xeb, 0xcb, 0x8b)),
            // Reset/None foregrounds — not a specific palette colour.
            SemanticToken::Text
            | SemanticToken::Variable
            | SemanticToken::VariableBuiltin
            | SemanticToken::Punctuation
            | SemanticToken::Operator
            | SemanticToken::Label => None,
        }
    }

    /// Reverse direction: recover the most likely [`SemanticToken`] for a themed
    /// foreground colour. Ambiguous colours resolve to the most specific token
    /// (e.g. blue is shared by Comment and Function; we pick Function because
    /// syntect's default text colour is also blue-ish and we'd rather
    /// over-classify plain text as a function than miscolour a real function).
    /// Returns `None` for colours outside the palette (treated as [`SemanticToken::Text`]).
    #[must_use]
    pub fn classify(&self, rgb: (u8, u8, u8)) -> Option<SemanticToken> {
        // Walk the canonical token list and return the first exact match.
        // Order biases ambiguous colours towards the most useful classification.
        let candidates = [
            SemanticToken::Tag,
            SemanticToken::String,
            SemanticToken::Number,
            SemanticToken::Constant,
            SemanticToken::Type,
            SemanticToken::Keyword,
            SemanticToken::Function,
            SemanticToken::Comment,
            SemanticToken::Attribute,
            SemanticToken::Property,
        ];
        candidates
            .into_iter()
            .find(|t| Self::color_of(*t) == Some(rgb))
        // No palette match -> None; the caller treats that as SemanticToken::Text.
    }
}

/// The foreground style for a semantic token: colour plus any font modifiers.
///
/// Returns `Style::default()` (no fg, no modifiers) for [`SemanticToken::Text`]
/// so the terminal default foreground shows — identical to the prior behaviour
/// where syntect's unspecified-colour sentinel mapped to no ratatui fg.
#[must_use]
pub fn token_style(token: SemanticToken) -> Style {
    // base16-ocean.dark palette (RGB), matching the old syntect theme so
    // existing languages keep their colours.
    const RED: Color = Color::Rgb(0xbf, 0x61, 0x6a);
    const GREEN: Color = Color::Rgb(0xa3, 0xbe, 0x8c);
    const YELLOW: Color = Color::Rgb(0xeb, 0xcb, 0x8b);
    const BLUE: Color = Color::Rgb(0x8f, 0xa1, 0xb3);
    const MAGENTA: Color = Color::Rgb(0xb4, 0x8e, 0xad);
    const CYAN: Color = Color::Rgb(0x96, 0xb5, 0xb4);
    const ORANGE: Color = Color::Rgb(0xd0, 0x87, 0x70);

    let mut style = Style::default();
    match token {
        SemanticToken::Text => {} // terminal default foreground
        SemanticToken::Comment => style = style.fg(BLUE).add_modifier(Modifier::ITALIC),
        // Keywords: purple, control-flow slightly bolder.
        SemanticToken::Keyword => style = style.fg(MAGENTA),
        SemanticToken::Control => style = style.fg(MAGENTA).add_modifier(Modifier::BOLD),
        SemanticToken::String => style = style.fg(GREEN),
        SemanticToken::Number => style = style.fg(ORANGE),
        SemanticToken::Constant => style = style.fg(YELLOW),
        SemanticToken::Function => style = style.fg(BLUE),
        // Types: teal; built-in type qualifier via bold.
        SemanticToken::Type => style = style.fg(CYAN),
        SemanticToken::Tag => style = style.fg(RED),
        // Attributes: orange/yellow to stand out from tags.
        SemanticToken::Attribute => style = style.fg(YELLOW),
        SemanticToken::Property => style = style.fg(YELLOW),
        SemanticToken::Variable => style = style.fg(Color::Reset),
        SemanticToken::VariableBuiltin => style = style.fg(RED).add_modifier(Modifier::ITALIC),
        SemanticToken::Punctuation => style = style.fg(Color::Reset),
        SemanticToken::Operator => style = style.fg(Color::Reset),
        // Labels (svelte blocks, decorators): magenta + bold so {#if}/{@render} read.
        SemanticToken::Label => style = style.fg(MAGENTA).add_modifier(Modifier::BOLD),
    }
    style
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_has_no_foreground() {
        // Text maps to default style: no fg, so the renderer's diff background
        // can tint without any foreground to clash with.
        let s = token_style(SemanticToken::Text);
        assert!(s.fg.is_none(), "Text must not set a foreground");
    }

    #[test]
    fn no_token_sets_a_background() {
        // The load-bearing invariant: syntax never owns the background.
        for t in [
            SemanticToken::Text,
            SemanticToken::Comment,
            SemanticToken::Keyword,
            SemanticToken::String,
            SemanticToken::Number,
            SemanticToken::Tag,
            SemanticToken::Attribute,
        ] {
            assert!(
                token_style(t).bg.is_none(),
                "{t:?} must not set a background"
            );
        }
    }

    #[test]
    fn keyword_and_string_differ() {
        assert_ne!(
            token_style(SemanticToken::Keyword),
            token_style(SemanticToken::String)
        );
    }
}
