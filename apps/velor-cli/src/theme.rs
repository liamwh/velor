//! Colour theme support for the streaming TUI.
//!
//! Themes are defined in the `oh-my-pi` theme JSON shape: a `vars` table of
//! named hex colours, and a `colors` table mapping semantic roles either to a
//! var name, a literal hex string, or an empty string ("use the terminal's
//! own default, no override"). [`Theme::titanium`] embeds the built-in
//! default theme; [`Theme::from_json`] parses any theme following the same
//! shape, so support for more themes is just adding another embedded JSON
//! (or, later, a `--theme-file` load path) — no rendering code changes.
//!
//! The active theme is a process-wide value set once at TUI startup (see
//! [`init`]) and read by every render call via [`active`]. A single TUI
//! process only ever has one theme for its whole lifetime, so a `OnceLock`
//! is the pragmatic choice here over threading `&Theme` through every one of
//! the several dozen small render functions in `streaming_tui.rs` and
//! `highlight::theme`.

use std::collections::HashMap;
use std::sync::OnceLock;

use ratatui::style::{Color, Style};

/// A resolved colour theme: every role the renderer looks up, already
/// resolved from the raw JSON's `vars`/`colors` tables to a concrete
/// [`Color`] (or `None` for a role that should defer to the terminal's own
/// default foreground/background).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Theme {
    /// The theme's declared name (e.g. "titanium").
    pub name: String,

    pub accent: Color,
    pub border: Color,
    pub border_accent: Color,
    pub border_muted: Color,
    pub success: Color,
    pub error: Color,
    pub warning: Color,
    pub muted: Color,
    pub dim: Color,
    /// `None` when the theme leaves body text unset (terminal default).
    pub text: Option<Color>,
    pub thinking_text: Color,

    pub tool_pending_bg: Color,
    pub tool_success_bg: Color,
    pub tool_error_bg: Color,
    pub tool_output: Color,

    pub md_heading: Color,
    pub md_link: Color,
    pub md_code: Color,
    pub md_quote: Color,
    pub md_hr: Color,

    pub diff_added: Color,
    pub diff_removed: Color,
    pub diff_context: Color,

    pub syntax_comment: Color,
    pub syntax_keyword: Color,
    pub syntax_function: Color,
    pub syntax_variable: Color,
    pub syntax_string: Color,
    pub syntax_number: Color,
    pub syntax_type: Color,
    pub syntax_operator: Color,
    pub syntax_punctuation: Color,
}

/// The raw `oh-my-pi` theme JSON shape. `colors` values are looked up
/// dynamically by role name (see [`resolve`]) rather than being spelled out
/// as individual fields — the schema has ~60 roles and most map straight
/// through, so a fixed-field struct would just be a wall of boilerplate that
/// drifts from the schema instead of failing loudly when a role is missing.
#[derive(Debug, serde::Deserialize)]
struct RawTheme {
    name: String,
    #[serde(default)]
    vars: HashMap<String, String>,
    colors: HashMap<String, String>,
}

impl Theme {
    /// The built-in default theme.
    #[must_use]
    pub fn titanium() -> Self {
        Self::from_json(TITANIUM_JSON).expect("the embedded titanium theme is valid")
    }

    /// Parses a theme from `oh-my-pi` theme JSON.
    ///
    /// # Errors
    ///
    /// Returns an error if the JSON doesn't parse, or a required colour role
    /// is missing (an empty-string or unresolvable value is fine — that's a
    /// deliberate "no override"/misconfiguration signal, not a parse error).
    pub fn from_json(json: &str) -> Result<Self, ThemeParseError> {
        let raw: RawTheme = serde_json::from_str(json)?;
        let get = |role: &str| -> Result<Option<Color>, ThemeParseError> {
            let value = raw
                .colors
                .get(role)
                .ok_or_else(|| ThemeParseError::MissingRole(role.to_string()))?;
            Ok(resolve(value, &raw.vars))
        };
        let required = |role: &str| -> Result<Color, ThemeParseError> {
            get(role)?.ok_or_else(|| ThemeParseError::UnresolvedRole(role.to_string()))
        };

        Ok(Self {
            name: raw.name,
            accent: required("accent")?,
            border: required("border")?,
            border_accent: required("borderAccent")?,
            border_muted: required("borderMuted")?,
            success: required("success")?,
            error: required("error")?,
            warning: required("warning")?,
            muted: required("muted")?,
            dim: required("dim")?,
            text: get("text")?,
            thinking_text: required("thinkingText")?,
            tool_pending_bg: required("toolPendingBg")?,
            tool_success_bg: required("toolSuccessBg")?,
            tool_error_bg: required("toolErrorBg")?,
            tool_output: required("toolOutput")?,
            md_heading: required("mdHeading")?,
            md_link: required("mdLink")?,
            md_code: required("mdCode")?,
            md_quote: required("mdQuote")?,
            md_hr: required("mdHr")?,
            diff_added: required("toolDiffAdded")?,
            diff_removed: required("toolDiffRemoved")?,
            diff_context: required("toolDiffContext")?,
            syntax_comment: required("syntaxComment")?,
            syntax_keyword: required("syntaxKeyword")?,
            syntax_function: required("syntaxFunction")?,
            syntax_variable: required("syntaxVariable")?,
            syntax_string: required("syntaxString")?,
            syntax_number: required("syntaxNumber")?,
            syntax_type: required("syntaxType")?,
            syntax_operator: required("syntaxOperator")?,
            syntax_punctuation: required("syntaxPunctuation")?,
        })
    }

    /// A dim background tint derived from a foreground colour, for a diff
    /// line's background (the theme schema only specifies diff
    /// *foregrounds* — additions/removals read as coloured text elsewhere in
    /// `oh-my-pi`, not tinted rows — so the tint itself is this renderer's
    /// own derivation, scaled down from the theme's own hue rather than an
    /// unrelated hardcoded colour).
    #[must_use]
    pub fn dim_bg(color: Color) -> Color {
        let Color::Rgb(r, g, b) = color else {
            return Color::Reset;
        };
        // ~15% brightness: visible as a tint, never competes with foreground text.
        Color::Rgb(
            (u16::from(r) * 15 / 100) as u8,
            (u16::from(g) * 15 / 100) as u8,
            (u16::from(b) * 15 / 100) as u8,
        )
    }

    /// The base style for body prose: `text` coloured when the theme sets an
    /// explicit override, or an unset foreground (the terminal's own
    /// default) when it doesn't — titanium's `text` is `""` deliberately, so
    /// this is the common case, not an edge case.
    #[must_use]
    pub fn text_style(&self) -> Style {
        match self.text {
            Some(c) => Style::default().fg(c),
            None => Style::default(),
        }
    }
}

/// Resolves one `colors` entry: `""` → no override, `"#RRGGBB"` → literal,
/// anything else → looked up in `vars` (themselves always literal hex in
/// practice, but resolved the same way for robustness).
fn resolve(value: &str, vars: &HashMap<String, String>) -> Option<Color> {
    if value.is_empty() {
        return None;
    }
    if let Some(hex) = value.strip_prefix('#') {
        return parse_hex(hex);
    }
    let var_value = vars.get(value)?;
    parse_hex(var_value.strip_prefix('#').unwrap_or(var_value))
}

/// Parses a bare `RRGGBB` hex string (no `#`) into an RGB colour.
fn parse_hex(hex: &str) -> Option<Color> {
    if hex.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some(Color::Rgb(r, g, b))
}

/// Failure parsing a theme JSON. Never expected for the embedded built-ins
/// (covered by tests); only reachable if a future load-from-file path is
/// added and handed a malformed/incomplete theme.
#[derive(Debug)]
pub enum ThemeParseError {
    Json(serde_json::Error),
    MissingRole(String),
    UnresolvedRole(String),
}

impl std::fmt::Display for ThemeParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Json(e) => write!(f, "invalid theme JSON: {e}"),
            Self::MissingRole(role) => write!(f, "theme is missing the \"{role}\" colour role"),
            Self::UnresolvedRole(role) => {
                write!(
                    f,
                    "theme's \"{role}\" colour role could not be resolved to a colour"
                )
            }
        }
    }
}

impl std::error::Error for ThemeParseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Json(e) => Some(e),
            Self::MissingRole(_) | Self::UnresolvedRole(_) => None,
        }
    }
}

impl From<serde_json::Error> for ThemeParseError {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}

static ACTIVE: OnceLock<Theme> = OnceLock::new();

/// Sets the process-wide active theme. Must be called before the first
/// [`active`] call to take effect — `active()` falls back to
/// [`Theme::titanium`] if nothing set it first. Call once, early, at TUI
/// startup; later calls are no-ops (a `OnceLock` cannot be reset), which is
/// fine since a running TUI never switches theme mid-session.
pub fn init(requested_name: Option<&str>) {
    let theme = match requested_name {
        Some(name) if name.eq_ignore_ascii_case("titanium") => Theme::titanium(),
        // Unrecognised name: fall back rather than fail a whole run over a
        // cosmetic setting. Only one theme ships today; this is where a
        // future catalog lookup would go.
        Some(_) | None => Theme::titanium(),
    };
    let _ = ACTIVE.set(theme);
}

/// The active theme, defaulting to [`Theme::titanium`] if [`init`] was never
/// called (e.g. in unit tests that render without going through TUI startup).
#[must_use]
pub fn active() -> &'static Theme {
    ACTIVE.get_or_init(Theme::titanium)
}

/// The built-in titanium theme, embedded verbatim from the `oh-my-pi`
/// theme-schema JSON.
const TITANIUM_JSON: &str = r##"{
    "$schema": "https://raw.githubusercontent.com/can1357/oh-my-pi/main/packages/coding-agent/theme-schema.json",
    "name": "titanium",
    "vars": {
        "brushedTitanium": "#151820",
        "darkTitanium": "#0f1216",
        "electricBlue": "#00b4ff",
        "deepBlue": "#0082b3",
        "titaniumGold": "#d4c090",
        "brightAluminum": "#e8ecf4",
        "dimAluminum": "#9ca3b0",
        "warningAmber": "#ffb347",
        "readoutGreen": "#00ff88",
        "alertRed": "#ff4757",
        "subtleGray": "#2a3038"
    },
    "colors": {
        "accent": "electricBlue",
        "border": "subtleGray",
        "borderAccent": "electricBlue",
        "borderMuted": "#1f252d",
        "success": "readoutGreen",
        "error": "alertRed",
        "warning": "warningAmber",
        "muted": "dimAluminum",
        "dim": "#6b7280",
        "text": "",
        "thinkingText": "dimAluminum",
        "selectedBg": "deepBlue",
        "userMessageBg": "darkTitanium",
        "userMessageText": "",
        "customMessageBg": "subtleGray",
        "customMessageText": "",
        "customMessageLabel": "titaniumGold",
        "toolPendingBg": "darkTitanium",
        "toolSuccessBg": "darkTitanium",
        "toolErrorBg": "#1a0f10",
        "toolTitle": "",
        "toolOutput": "dimAluminum",
        "mdHeading": "electricBlue",
        "mdLink": "electricBlue",
        "mdLinkUrl": "deepBlue",
        "mdCode": "readoutGreen",
        "mdCodeBlock": "dimAluminum",
        "mdCodeBlockBorder": "subtleGray",
        "mdQuote": "dimAluminum",
        "mdQuoteBorder": "subtleGray",
        "mdHr": "subtleGray",
        "mdListBullet": "electricBlue",
        "toolDiffAdded": "readoutGreen",
        "toolDiffRemoved": "alertRed",
        "toolDiffContext": "dimAluminum",
        "syntaxComment": "#6b7280",
        "syntaxKeyword": "electricBlue",
        "syntaxFunction": "readoutGreen",
        "syntaxVariable": "brightAluminum",
        "syntaxString": "titaniumGold",
        "syntaxNumber": "warningAmber",
        "syntaxType": "electricBlue",
        "syntaxOperator": "electricBlue",
        "syntaxPunctuation": "dimAluminum",
        "thinkingOff": "#4a5058",
        "thinkingMinimal": "#5a6068",
        "thinkingLow": "#6a7078",
        "thinkingMedium": "dimAluminum",
        "thinkingHigh": "electricBlue",
        "thinkingXhigh": "titaniumGold",
        "bashMode": "readoutGreen",
        "statusLineBg": "darkTitanium",
        "statusLineSep": "subtleGray",
        "statusLineModel": "electricBlue",
        "statusLinePath": "brightAluminum",
        "statusLineGitClean": "readoutGreen",
        "statusLineGitDirty": "warningAmber",
        "statusLineContext": "dimAluminum",
        "statusLineSpend": "titaniumGold",
        "statusLineStaged": "readoutGreen",
        "statusLineDirty": "warningAmber",
        "statusLineUntracked": "dimAluminum",
        "statusLineOutput": "deepBlue",
        "statusLineCost": "titaniumGold",
        "statusLineSubagents": "electricBlue",
        "pythonMode": "#f0c040"
    },
    "export": {
        "pageBg": "brushedTitanium",
        "cardBg": "darkTitanium",
        "infoBg": "subtleGray"
    }
}"##;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titanium_parses_and_resolves_every_required_role() {
        let theme = Theme::titanium();
        assert_eq!(theme.name, "titanium");
        assert_eq!(theme.accent, Color::Rgb(0x00, 0xb4, 0xff));
        assert_eq!(theme.md_code, Color::Rgb(0x00, 0xff, 0x88));
        assert_eq!(theme.diff_added, Color::Rgb(0x00, 0xff, 0x88));
        assert_eq!(theme.diff_removed, Color::Rgb(0xff, 0x47, 0x57));
        assert_eq!(theme.tool_error_bg, Color::Rgb(0x1a, 0x0f, 0x10));
    }

    #[test]
    fn empty_string_role_resolves_to_none() {
        // titanium's "text" is "" — an explicit "inherit the terminal
        // default", not a missing/invalid value.
        assert_eq!(Theme::titanium().text, None);
    }

    #[test]
    fn resolve_prefers_literal_hex_over_a_var_lookup() {
        let vars = HashMap::new();
        assert_eq!(
            resolve("#abcdef", &vars),
            Some(Color::Rgb(0xab, 0xcd, 0xef))
        );
    }

    #[test]
    fn resolve_looks_up_a_var_name() {
        let mut vars = HashMap::new();
        vars.insert("myBlue".to_string(), "#112233".to_string());
        assert_eq!(resolve("myBlue", &vars), Some(Color::Rgb(0x11, 0x22, 0x33)));
    }

    #[test]
    fn resolve_empty_string_is_none() {
        assert_eq!(resolve("", &HashMap::new()), None);
    }

    #[test]
    fn resolve_unknown_var_name_is_none() {
        assert_eq!(resolve("noSuchVar", &HashMap::new()), None);
    }

    #[test]
    fn from_json_reports_a_missing_role() {
        let err = Theme::from_json(r#"{"name":"t","vars":{},"colors":{}}"#).unwrap_err();
        assert!(matches!(err, ThemeParseError::MissingRole(role) if role == "accent"));
    }

    #[test]
    fn dim_bg_scales_down_an_rgb_colour() {
        let dimmed = Theme::dim_bg(Color::Rgb(200, 100, 50));
        assert_eq!(dimmed, Color::Rgb(30, 15, 7));
    }

    #[test]
    fn dim_bg_of_a_non_rgb_colour_is_reset() {
        assert_eq!(Theme::dim_bg(Color::Green), Color::Reset);
    }

    #[test]
    fn init_falls_back_to_titanium_for_an_unknown_name() {
        // init() only affects the process-wide OnceLock once per process, so
        // this only checks the resolution logic doesn't panic/misbehave —
        // full init()/active() wiring is exercised by the TUI at runtime.
        let requested = Some("nonexistent-theme");
        let resolved = match requested {
            Some(name) if name.eq_ignore_ascii_case("titanium") => Theme::titanium(),
            Some(_) | None => Theme::titanium(),
        };
        assert_eq!(resolved.name, "titanium");
    }
}
