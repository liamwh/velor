//! Structured file-edit transcript events with line-based diff hunks.
//!
//! When an agent edits a file, the adapter captures the real before/after file
//! contents and builds a [`FileEdit`] — a framework-agnostic (no Ratatui, no
//! syntect) description of the change as ordered [`FileHunk`]s of [`DiffLine`]s.
//! The TUI later renders a [`FileEdit`] with syntax highlighting; the durable run
//! log serialises it verbatim.
//!
//! # Responsibilities kept separate
//!
//! This module owns only the *pure* concerns:
//! 1. inferring a [`SyntaxKind`] from a path ([`infer_syntax`]);
//! 2. computing three-context unified hunks from before/after bytes
//!    ([`compute_file_edit`]);
//! 3. bounding very large edits (head + tail kept, middle reported).
//!
//! It does **not** read files, highlight syntax, or render — those live in the
//! adapter (capture) and the CLI (highlight + Ratatui) respectively. Everything
//! here is pure and unit-testable.

use diffy::{DiffOptions, Hunk, Line};

/// Semantic role of a single line within a file-edit hunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LineKind {
    /// An unchanged context line (present in both old and new).
    Context,
    /// A line present only in the new file.
    Addition,
    /// A line present only in the old file.
    Removal,
}

/// One line of a file-edit hunk: its old/new line numbers (where applicable), the
/// source text (no ANSI escapes, no line terminator), and its semantic kind.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DiffLine {
    /// 1-based line number in the old file, or `None` for pure additions.
    pub old_no: Option<usize>,
    /// 1-based line number in the new file, or `None` for pure removals.
    pub new_no: Option<usize>,
    /// The line's source text, terminators stripped and tabs left intact.
    pub text: String,
    /// Whether this line is context, added, or removed.
    pub kind: LineKind,
}

/// A contiguous group of differing lines (a unified-diff hunk): the 1-based
/// starting line in each file (where the hunk has any), and the ordered lines.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FileHunk {
    /// 1-based starting line of this hunk in the old file, or `None` when the
    /// hunk is a pure insertion.
    pub old_start: Option<usize>,
    /// 1-based starting line of this hunk in the new file, or `None` when the
    /// hunk is a pure deletion.
    pub new_start: Option<usize>,
    /// The ordered context/added/removed lines making up the hunk.
    pub lines: Vec<DiffLine>,
}

impl FileHunk {
    /// Builds a hunk from an ordered line list, taking its start positions from
    /// the first line that carries each number. Used when reconstructing hunks
    /// after truncation.
    fn from_lines(lines: Vec<DiffLine>) -> Self {
        let old_start = lines.iter().find_map(|l| l.old_no);
        let new_start = lines.iter().find_map(|l| l.new_no);
        Self {
            old_start,
            new_start,
            lines,
        }
    }
}

/// The language family inferred for a file, carried in the domain event so the
/// TUI can select a syntax definition without re-inferring. Mapped to a syntect
/// hint via [`SyntaxKind::syntect_hint`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyntaxKind {
    /// Rust.
    Rust,
    /// TypeScript.
    TypeScript,
    /// JavaScript.
    JavaScript,
    /// Svelte (highlighted via the HTML family when no Svelte grammar is loaded).
    Svelte,
    /// Python.
    Python,
    /// JSON.
    Json,
    /// YAML.
    Yaml,
    /// TOML.
    Toml,
    /// Markdown.
    Markdown,
    /// Shell scripts.
    Shell,
    /// SQL.
    Sql,
    /// HTML.
    Html,
    /// CSS.
    Css,
    /// Dockerfile.
    Dockerfile,
    /// Go.
    Go,
    /// C.
    C,
    /// C++.
    Cpp,
    /// Java.
    Java,
    /// Unrecognised / plain text.
    PlainText,
}

impl SyntaxKind {
    /// Returns the syntect lookup hint (extension or grammar name) the TUI uses
    /// to resolve a [`syntect::parsing::SyntaxReference`]. Plain text resolves to
    /// syntect's built-in plain-text grammar in the renderer.
    #[must_use]
    pub const fn syntect_hint(self) -> &'static str {
        match self {
            Self::Rust => "rs",
            Self::TypeScript => "ts",
            Self::JavaScript => "js",
            // The bundled syntect default set has no Svelte grammar; fall back to
            // the HTML family so the template/markup still gets coloured.
            Self::Svelte => "html",
            Self::Python => "py",
            Self::Json => "json",
            Self::Yaml => "yaml",
            Self::Toml => "toml",
            Self::Markdown => "md",
            Self::Shell => "sh",
            Self::Sql => "sql",
            Self::Html => "html",
            Self::Css => "css",
            Self::Dockerfile => "Dockerfile",
            Self::Go => "go",
            Self::C => "c",
            Self::Cpp => "cpp",
            Self::Java => "java",
            Self::PlainText => "",
        }
    }

    /// Returns `true` for composite template languages whose highlighting
    /// requires surrounding file context (an embedded `<script>`/`<style>` tag
    /// determines the language mode of a hunk far below it). For these, the
    /// adapter carries the full post-edit source on [`FileEdit`] so the
    /// highlighter can parse whole-file state before clipping to a hunk.
    ///
    /// Plain languages return `false`; they highlight correctly line-by-line.
    #[must_use]
    pub const fn is_composite(self) -> bool {
        matches!(self, Self::Svelte)
    }
}

/// The nature of a captured file edit, for header/summary rendering.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FileEditKind {
    /// One or more lines changed within an existing file (hunks present).
    Modified,
    /// A new file was created (all-additions hunks).
    Created,
    /// An existing file was deleted (all-removals hunks).
    Deleted,
    /// A binary (non-text) file changed; contents are not shown.
    Binary,
    /// The real before/after state could not be captured (e.g. a read failure).
    /// Carries the reason so the transcript shows the failure rather than
    /// silently presenting the agent's claimed patch as the result.
    CaptureFailed {
        /// Why the edit could not be captured.
        reason: String,
    },
}

/// A structured file-edit transcript event: the path, inferred syntax, the
/// nature of the change, the ordered hunks (when applicable), and how many diff
/// lines were omitted to keep the entry bounded.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FileEdit {
    /// The edited file's path (as reported by the agent, usually repo-relative).
    pub path: String,
    /// The syntax family inferred from the path.
    pub syntax: SyntaxKind,
    /// The nature of the change.
    pub kind: FileEditKind,
    /// The ordered diff hunks. Empty for binary files and capture failures.
    pub hunks: Vec<FileHunk>,
    /// Diff lines omitted to bound a very large edit (head + tail kept). Zero
    /// when nothing was dropped; the full diff remains in the run log.
    pub omitted_lines: u64,
    /// The full post-edit source for composite languages (Svelte, Vue, Astro)
    /// whose highlighting needs surrounding context — an embedded `<script>` tag
    /// far above a diff hunk establishes the language mode for that hunk. Only
    /// populated when [`SyntaxKind::is_composite`] is true and the new-side
    /// bytes were available at capture time; `None` otherwise (plain languages
    /// never need it). Bounded to [`DEFAULT_MAX_DIFF_LINES`].
    pub full_new_source: Option<String>,
}

impl FileEdit {
    /// Total number of diff lines across all hunks (context + added + removed).
    /// Used for transcript sizing; not the rendered row count.
    #[must_use]
    pub fn diff_line_count(&self) -> usize {
        self.hunks.iter().map(|h| h.lines.len()).sum()
    }

    /// Approximate retained byte cost: the sum of all diff-line text plus the
    /// path. Used by the bounded transcript for byte accounting.
    #[must_use]
    pub fn approx_byte_size(&self) -> usize {
        let lines = self.diff_line_count();
        let text: usize = self
            .hunks
            .iter()
            .flat_map(|h| h.lines.iter())
            .map(|l| l.text.len())
            .sum();
        self.path
            .len()
            .saturating_add(text)
            .saturating_add(lines * 8)
    }
}

/// Default cap on the number of diff lines kept in a single edit event. Head and
/// tail are preserved; the elided middle is reported via [`FileEdit::omitted_lines`].
pub const DEFAULT_MAX_DIFF_LINES: usize = 1000;

/// Infers the [`SyntaxKind`] for a path from its filename then its extension,
/// falling back to plain text. Recognised special filenames (e.g. `Dockerfile`)
/// are matched case-insensitively before the extension is considered.
#[must_use]
pub fn infer_syntax(path: &str) -> SyntaxKind {
    let file_name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let lower = file_name.to_ascii_lowercase();
    if lower == "dockerfile" || lower.starts_with("dockerfile.") {
        return SyntaxKind::Dockerfile;
    }
    let ext = file_name.rsplit('.').next().unwrap_or("");
    // No extension (or the whole name is the "extension"): plain text unless it
    // was a recognised special filename above.
    if !file_name.contains('.') {
        return SyntaxKind::PlainText;
    }
    match ext {
        "rs" => SyntaxKind::Rust,
        "ts" => SyntaxKind::TypeScript,
        "tsx" => SyntaxKind::TypeScript,
        "js" | "mjs" | "cjs" => SyntaxKind::JavaScript,
        "jsx" => SyntaxKind::JavaScript,
        "svelte" => SyntaxKind::Svelte,
        "py" | "pyi" => SyntaxKind::Python,
        "json" => SyntaxKind::Json,
        "yaml" | "yml" => SyntaxKind::Yaml,
        "toml" => SyntaxKind::Toml,
        "md" | "markdown" => SyntaxKind::Markdown,
        "sh" | "bash" | "zsh" | "fish" => SyntaxKind::Shell,
        "sql" => SyntaxKind::Sql,
        "html" | "htm" => SyntaxKind::Html,
        "css" => SyntaxKind::Css,
        "go" => SyntaxKind::Go,
        "c" | "h" => SyntaxKind::C,
        "cpp" | "cc" | "cxx" | "hpp" | "hxx" => SyntaxKind::Cpp,
        "java" => SyntaxKind::Java,
        _ => SyntaxKind::PlainText,
    }
}

/// Builds a [`FileEdit`] from the real before/after file bytes, or `None` when
/// the edit made no effective change.
///
/// `old` / `new` are `Some(bytes)` when the file existed before / after the edit
/// and `None` when it did not (creation / deletion). `max_lines` bounds the total
/// diff lines kept (head + tail); use [`DEFAULT_MAX_DIFF_LINES`] for the standard
/// policy. This function performs no I/O and no highlighting — it is pure so it
/// can be unit-tested exhaustively.
///
/// # Handling
///
/// * **Binary** files (NUL bytes or invalid UTF-8) produce a concise
///   [`FileEditKind::Binary`] entry with no hunks.
/// * **Creation / deletion** become all-additions / all-removals hunks.
/// * **CRLF vs LF** is normalised so line-ending style alone never produces false
///   edits.
/// * A pure **missing-final-newline** change is shown as a minimal last-line hunk
///   rather than silently dropped.
#[must_use]
pub fn compute_file_edit(
    path: &str,
    old: Option<&[u8]>,
    new: Option<&[u8]>,
    max_lines: usize,
) -> Option<FileEdit> {
    let syntax = infer_syntax(path);
    let max_lines = max_lines.max(2);

    let (kind, hunks) = match (old, new) {
        (None, None) => return None,
        (None, Some(new)) => {
            if new.is_empty() {
                return None;
            }
            if is_binary(new) {
                (FileEditKind::Created, Vec::new())
            } else {
                let lines = content_lines(new);
                (FileEditKind::Created, created_hunks(&lines))
            }
        }
        (Some(old), None) => {
            if old.is_empty() {
                return None;
            }
            if is_binary(old) {
                (FileEditKind::Deleted, Vec::new())
            } else {
                let lines = content_lines(old);
                (FileEditKind::Deleted, deleted_hunks(&lines))
            }
        }
        (Some(old), Some(new)) => {
            if old == new {
                return None;
            }
            if is_binary(old) || is_binary(new) {
                (FileEditKind::Modified.binary_equivalent(), Vec::new())
            } else {
                let diff = text_diff_hunks(old, new);
                if diff.is_empty() {
                    return None;
                }
                (FileEditKind::Modified, diff)
            }
        }
    };

    let (hunks, omitted) = truncate_hunks(hunks, max_lines);

    // A bounded/empty result for a modification with no lines is not a change.
    if hunks.is_empty() && matches!(kind, FileEditKind::Modified) {
        return None;
    }

    Some(FileEdit {
        path: path.to_string(),
        syntax,
        kind,
        hunks,
        omitted_lines: omitted,
        // Only composite template languages need whole-file context; for those
        // we carry the new-side source (bounded) so the highlighter can resolve
        // embedded `<script>`/`<style>` state before clipping to a diff hunk.
        full_new_source: full_source_for(syntax, new),
    })
}

/// Returns the full new-side source for composite languages (so the renderer's
/// highlighter can parse whole-file embedded-language state), or `None` for
/// plain languages / missing / binary input. The source is bounded to
/// [`DEFAULT_MAX_DIFF_LINES`] lines to cap durable-log + transcript memory.
fn full_source_for(syntax: SyntaxKind, new: Option<&[u8]>) -> Option<String> {
    if !syntax.is_composite() {
        return None;
    }
    let bytes = new?;
    if is_binary(bytes) {
        return None;
    }
    let text = decode_text(bytes);
    // Bound by line count to avoid carrying a multi-MB minified bundle on the
    // event (the full diff already lives in the run log). Trailing content past
    // the cap is dropped — the head carries the structural context that matters.
    let cap = DEFAULT_MAX_DIFF_LINES;
    let mut out = String::with_capacity(text.len().min(64 * 1024));
    for (i, line) in text.lines().enumerate() {
        if i >= cap {
            break;
        }
        out.push_str(line);
        out.push('\n');
    }
    Some(out)
}

impl FileEditKind {
    /// The binary flavour of a kind — creation/deletion of a binary file and a
    /// binary modification all collapse to a concise binary entry.
    fn binary_equivalent(self) -> Self {
        match self {
            FileEditKind::Modified => FileEditKind::Binary,
            other => other,
        }
    }
}

/// Returns `true` for binary content: a NUL byte or invalid UTF-8. This mirrors
/// the heuristic used by `git` (and avoids feeding non-text bytes to the diff).
fn is_binary(bytes: &[u8]) -> bool {
    bytes.contains(&0u8) || std::str::from_utf8(bytes).is_err()
}

/// Decodes non-binary bytes to a string. Only called after [`is_binary`] has
/// ruled out invalid UTF-8, so the fallback is unreachable in practice.
fn decode_text(bytes: &[u8]) -> &str {
    std::str::from_utf8(bytes).unwrap_or("")
}

/// Normalises CRLF (and CR) line endings to LF so a pure line-ending change
/// never registers as an edit. Applied identically to both sides.
fn normalize_eol(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\r', "\n")
}

/// The logical source lines of a (non-binary) file, terminators stripped.
fn content_lines(bytes: &[u8]) -> Vec<String> {
    decode_text(bytes)
        .replace("\r\n", "\n")
        .lines()
        .map(String::from)
        .collect()
}

/// Computes the three-context unified hunks for a text modification. Handles the
/// missing-final-newline edge (a real byte difference that produces no content
/// diff) by synthesising a minimal last-line hunk.
fn text_diff_hunks(old: &[u8], new: &[u8]) -> Vec<FileHunk> {
    let old_norm = normalize_eol(decode_text(old));
    let new_norm = normalize_eol(decode_text(new));

    let mut opts = DiffOptions::new();
    opts.set_context_len(3);
    let patch = opts.create_patch(&old_norm, &new_norm);
    let hunks: Vec<FileHunk> = patch.hunks().iter().map(map_hunk).collect();

    if !hunks.is_empty() {
        return hunks;
    }

    // Bytes differed (caller checked `old != new`) but normalised content is
    // line-for-line identical: the only possible difference is a final newline.
    // Show it as a minimal last-line change rather than dropping the edit.
    if old_norm.lines().eq(new_norm.lines()) {
        return trailing_newline_hunk(&old_norm, &new_norm);
    }

    Vec::new()
}

/// Maps a diffy [`Hunk`] to a [`FileHunk`], numbering lines from each range start
/// and walking context/delete/insert lines exactly as unified diff numbering
/// requires.
fn map_hunk(hunk: &Hunk<str>) -> FileHunk {
    let mut old_n = hunk.old_range().start();
    let mut new_n = hunk.new_range().start();
    let mut lines = Vec::with_capacity(hunk.lines().len());
    for line in hunk.lines() {
        // diffy embeds the line terminator in each `Line` text; strip it so the
        // transcript carries clean source text with no ANSI escapes or newlines.
        let text = strip_terminator(line.value());
        match line {
            Line::Context(_) => {
                lines.push(DiffLine {
                    old_no: Some(old_n),
                    new_no: Some(new_n),
                    text,
                    kind: LineKind::Context,
                });
                old_n = old_n.saturating_add(1);
                new_n = new_n.saturating_add(1);
            }
            Line::Delete(_) => {
                lines.push(DiffLine {
                    old_no: Some(old_n),
                    new_no: None,
                    text,
                    kind: LineKind::Removal,
                });
                old_n = old_n.saturating_add(1);
            }
            Line::Insert(_) => {
                lines.push(DiffLine {
                    old_no: None,
                    new_no: Some(new_n),
                    text,
                    kind: LineKind::Addition,
                });
                new_n = new_n.saturating_add(1);
            }
        }
    }
    FileHunk::from_lines(lines)
}

/// Strips a trailing CR/LF line terminator from a diffy line value. diffy keeps
/// the terminator (including a possible final `\n`) inside each `Line`; the
/// transcript stores terminator-free source text.
fn strip_terminator(s: &str) -> String {
    s.trim_end_matches(['\n', '\r']).to_string()
}

/// Extension trait giving diffy's `Line` an accessor that works for all variants.
trait LineValue {
    fn value(&self) -> &str;
}

impl LineValue for Line<'_, str> {
    fn value(&self) -> &str {
        match self {
            Line::Context(t) | Line::Delete(t) | Line::Insert(t) => t,
        }
    }
}

/// Builds an all-additions hunk for a created file (new line numbers from 1).
fn created_hunks(lines: &[String]) -> Vec<FileHunk> {
    if lines.is_empty() {
        return Vec::new();
    }
    let diff_lines = lines
        .iter()
        .enumerate()
        .map(|(i, text)| DiffLine {
            old_no: None,
            new_no: Some(i + 1),
            text: text.clone(),
            kind: LineKind::Addition,
        })
        .collect();
    vec![FileHunk {
        old_start: None,
        new_start: Some(1),
        lines: diff_lines,
    }]
}

/// Builds an all-removals hunk for a deleted file (old line numbers from 1).
fn deleted_hunks(lines: &[String]) -> Vec<FileHunk> {
    if lines.is_empty() {
        return Vec::new();
    }
    let diff_lines = lines
        .iter()
        .enumerate()
        .map(|(i, text)| DiffLine {
            old_no: Some(i + 1),
            new_no: None,
            text: text.clone(),
            kind: LineKind::Removal,
        })
        .collect();
    vec![FileHunk {
        old_start: Some(1),
        new_start: None,
        lines: diff_lines,
    }]
}

/// Synthesises a one-line hunk representing a pure trailing-newline change. The
/// last line is shown removed then re-added so the change is visible even though
/// its text is unchanged.
fn trailing_newline_hunk(old_norm: &str, new_norm: &str) -> Vec<FileHunk> {
    let count = old_norm.lines().count().max(new_norm.lines().count());
    let n = if count == 0 { 1 } else { count };
    let last_old = old_norm.lines().next_back().unwrap_or("").to_string();
    let last_new = new_norm.lines().next_back().unwrap_or("").to_string();
    vec![FileHunk {
        old_start: Some(n),
        new_start: Some(n),
        lines: vec![
            DiffLine {
                old_no: Some(n),
                new_no: None,
                text: last_old,
                kind: LineKind::Removal,
            },
            DiffLine {
                old_no: None,
                new_no: Some(n),
                text: last_new,
                kind: LineKind::Addition,
            },
        ],
    }]
}

/// Bounds a hunk list to `max_lines` total diff lines, keeping a head and a tail
/// and reporting how many were elided. Hunks are never partially split unless a
/// single hunk alone exceeds the budget; reconstruction preserves line numbers.
fn truncate_hunks(hunks: Vec<FileHunk>, max_lines: usize) -> (Vec<FileHunk>, u64) {
    let total: usize = hunks.iter().map(|h| h.lines.len()).sum();
    if total <= max_lines {
        return (hunks, 0);
    }
    let head = (max_lines / 2).max(1);
    let tail = max_lines.saturating_sub(head).max(1);

    // Flatten to (hunk-index, line) so kept lines can be regrouped into hunks.
    let flat: Vec<(usize, DiffLine)> = hunks
        .iter()
        .enumerate()
        .flat_map(|(idx, h)| h.lines.iter().map(move |l| (idx, l.clone())))
        .collect();
    let total = flat.len();
    let head_end = head.min(total);
    let tail_start = total.saturating_sub(tail).min(total).max(head_end);
    let mut kept: Vec<(usize, DiffLine)> = flat[..head_end].to_vec();
    kept.extend(flat[tail_start..].iter().cloned());
    let kept_count = kept.len();
    let omitted = total.saturating_sub(kept_count);

    (rebuild_hunks(kept), omitted as u64)
}

/// Regroups a flat list of `(hunk-index, line)` into [`FileHunk`]s, preserving
/// the original grouping so adjacency (and thus inter-hunk gaps) is retained.
fn rebuild_hunks(kept: Vec<(usize, DiffLine)>) -> Vec<FileHunk> {
    let mut out: Vec<FileHunk> = Vec::new();
    let mut current: Option<(usize, Vec<DiffLine>)> = None;
    for (idx, line) in kept {
        match &mut current {
            Some((cur_idx, lines)) if *cur_idx == idx => lines.push(line),
            _ => {
                if let Some((_, lines)) = current.take() {
                    out.push(FileHunk::from_lines(lines));
                }
                current = Some((idx, vec![line]));
            }
        }
    }
    if let Some((_, lines)) = current {
        out.push(FileHunk::from_lines(lines));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(path: &str, old: Option<&[u8]>, new: Option<&[u8]>) -> Option<FileEdit> {
        compute_file_edit(path, old, new, DEFAULT_MAX_DIFF_LINES)
    }

    fn modified_edit(old: &str, path: &str, new: &str) -> FileEdit {
        compute_file_edit(
            path,
            Some(old.as_bytes()),
            Some(new.as_bytes()),
            DEFAULT_MAX_DIFF_LINES,
        )
        .unwrap_or_else(|| panic!("expected a modified edit for {path}"))
    }

    // ── Diff construction ────────────────────────────────────────────────────

    #[test]
    fn one_line_replacement_has_three_context_lines_each_side() {
        let old = "a\nb\nc\ntarget\nd\ne\nf\ng\n";
        let new = "a\nb\nc\nCHANGED\nd\ne\nf\ng\n";
        let e = modified_edit(old, "src/lib.rs", new);
        assert_eq!(e.kind, FileEditKind::Modified);
        assert_eq!(e.hunks.len(), 1, "one changed region → one hunk");
        let h = &e.hunks[0];
        // 3 context before + 1 removal + 1 addition + 3 context after = 8 lines.
        assert_eq!(h.lines.len(), 8);
        assert_eq!(h.lines[0].text, "a");
        assert_eq!(h.lines[0].kind, LineKind::Context);
        assert_eq!(h.lines[3].text, "target");
        assert_eq!(h.lines[3].kind, LineKind::Removal);
        assert_eq!(h.lines[3].old_no, Some(4));
        assert_eq!(h.lines[3].new_no, None);
        assert_eq!(h.lines[4].text, "CHANGED");
        assert_eq!(h.lines[4].kind, LineKind::Addition);
        assert_eq!(h.lines[4].new_no, Some(4));
        // 3 context after the change = d, e, f (line 8 'g' is beyond the window).
        assert_eq!(h.lines[7].text, "f");
        assert_eq!(h.lines[7].kind, LineKind::Context);
    }

    #[test]
    fn edit_at_beginning_of_file_shows_fewer_leading_context() {
        let old = "target\nb\nc\nd\ne\nf\ng\n";
        let new = "CHANGED\nb\nc\nd\ne\nf\ng\n";
        let e = modified_edit(old, "f.rs", new);
        let h = &e.hunks[0];
        // No lines before line 1, so leading context is empty.
        assert_eq!(h.lines.first().map(|l| l.text.as_str()), Some("target"));
        assert_eq!(h.lines.first().map(|l| l.kind), Some(LineKind::Removal));
    }

    #[test]
    fn edit_at_end_of_file_shows_fewer_trailing_context() {
        let old = "a\nb\nc\nd\ne\nf\ntarget\n";
        let new = "a\nb\nc\nd\ne\nf\nCHANGED\n";
        let e = modified_edit(old, "f.rs", new);
        let h = &e.hunks[0];
        assert_eq!(h.lines.last().map(|l| l.text.as_str()), Some("CHANGED"));
        assert_eq!(h.lines.last().map(|l| l.kind), Some(LineKind::Addition));
    }

    #[test]
    fn file_shorter_than_context_shows_all_lines() {
        let old = "x\ny\n";
        let new = "x\nz\n";
        let e = modified_edit(old, "f.rs", new);
        let h = &e.hunks[0];
        // Both context lines + the change; no padding.
        assert!(
            h.lines
                .iter()
                .any(|l| l.text == "x" && l.kind == LineKind::Context)
        );
        assert!(
            h.lines
                .iter()
                .any(|l| l.text == "y" && l.kind == LineKind::Removal)
        );
        assert!(
            h.lines
                .iter()
                .any(|l| l.text == "z" && l.kind == LineKind::Addition)
        );
    }

    #[test]
    fn pure_insertion_is_shown_as_addition() {
        let old = "a\nb\n";
        let new = "a\ninserted\nb\n";
        let e = modified_edit(old, "f.rs", new);
        let added: Vec<&DiffLine> = e
            .hunks
            .iter()
            .flat_map(|h| h.lines.iter())
            .filter(|l| l.kind == LineKind::Addition)
            .collect();
        assert_eq!(added.len(), 1);
        assert_eq!(added[0].text, "inserted");
        assert_eq!(added[0].new_no, Some(2));
        assert!(added[0].old_no.is_none());
    }

    #[test]
    fn pure_deletion_is_shown_as_removal() {
        let old = "a\nb\nc\n";
        let new = "a\nc\n";
        let e = modified_edit(old, "f.rs", new);
        let removed: Vec<&DiffLine> = e
            .hunks
            .iter()
            .flat_map(|h| h.lines.iter())
            .filter(|l| l.kind == LineKind::Removal)
            .collect();
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].text, "b");
        assert_eq!(removed[0].old_no, Some(2));
    }

    #[test]
    fn file_creation_is_all_additions() {
        let new = "fn main() {}\n";
        let e = edit("src/new.rs", None, Some(new.as_bytes())).expect("created edit");
        assert_eq!(e.kind, FileEditKind::Created);
        assert_eq!(e.hunks.len(), 1);
        assert!(
            e.hunks[0]
                .lines
                .iter()
                .all(|l| l.kind == LineKind::Addition)
        );
        assert_eq!(e.hunks[0].new_start, Some(1));
    }

    #[test]
    fn file_deletion_is_all_removals() {
        let old = "gone\n";
        let e = edit("src/gone.rs", Some(old.as_bytes()), None).expect("deleted edit");
        assert_eq!(e.kind, FileEditKind::Deleted);
        assert!(e.hunks[0].lines.iter().all(|l| l.kind == LineKind::Removal));
        assert_eq!(e.hunks[0].old_start, Some(1));
    }

    #[test]
    fn multiple_distant_hunks_stay_separate() {
        let mut old = String::new();
        let mut new = String::new();
        for i in 0..50 {
            old.push_str(&format!("line {i}\n"));
            new.push_str(&format!("line {i}\n"));
        }
        // Two distant changes (> 2*context apart).
        new = new.replacen("line 1\n", "line ONE\n", 1);
        new = new.replacen("line 40\n", "line FORTY\n", 1);
        let e = modified_edit(&old, "f.rs", &new);
        assert_eq!(e.hunks.len(), 2, "distant changes → separate hunks");
    }

    #[test]
    fn nearby_edits_merge_into_one_hunk() {
        let old = "a\nb\nc\nd\ne\n";
        let new = "A\nB\nc\nD\nE\n";
        let e = modified_edit(old, "f.rs", new);
        assert_eq!(e.hunks.len(), 1, "close changes merge into one hunk");
    }

    #[test]
    fn overlapping_context_is_not_duplicated() {
        // Two changes within one context window: context lines must appear once.
        let old = "a\nb\nc\nd\ne\n";
        let new = "a\nB\nC\nd\ne\n";
        let e = modified_edit(old, "f.rs", new);
        assert_eq!(e.hunks.len(), 1);
        // Count context occurrences of "a" — must be exactly one.
        let a_count = e.hunks[0].lines.iter().filter(|l| l.text == "a").count();
        assert_eq!(a_count, 1);
    }

    #[test]
    fn blank_lines_are_preserved() {
        let old = "a\n\nb\n";
        let new = "a\n\n\nb\n";
        let e = modified_edit(old, "f.rs", new);
        assert!(
            e.hunks
                .iter()
                .flat_map(|h| h.lines.iter())
                .any(|l| l.kind == LineKind::Addition && l.text.is_empty())
        );
    }

    // ── Line-ending + newline edge cases ────────────────────────────────────

    #[test]
    fn crlf_input_introduces_no_false_edits() {
        let old = "a\r\nb\r\n";
        let new = "a\r\nb\r\n";
        assert!(edit("f.rs", Some(old.as_bytes()), Some(new.as_bytes())).is_none());

        // A real change in a CRLF file shows only the changed line.
        let old = "a\r\nb\r\nc\r\n";
        let new = "a\r\nB\r\nc\r\n";
        let e = modified_edit(old, "f.rs", new);
        let removed: Vec<&DiffLine> = e
            .hunks
            .iter()
            .flat_map(|h| h.lines.iter())
            .filter(|l| l.kind == LineKind::Removal)
            .collect();
        assert_eq!(removed.len(), 1);
        assert_eq!(
            removed[0].text, "b",
            "no stray carriage returns in line text"
        );
    }

    #[test]
    fn missing_final_newline_change_is_still_visible() {
        // Same line content, but one ends with a newline and the other does not.
        let old = "only line";
        let new = "only line\n";
        let e = edit("f.rs", Some(old.as_bytes()), Some(new.as_bytes()))
            .expect("a trailing-newline-only change is still an edit");
        assert_eq!(e.hunks.len(), 1);
        assert!(!e.hunks[0].lines.is_empty());
    }

    #[test]
    fn identical_content_with_no_newline_difference_is_no_edit() {
        let old = "x\ny\n";
        let new = "x\ny\n";
        assert!(edit("f.rs", Some(old.as_bytes()), Some(new.as_bytes())).is_none());
    }

    // ── Binary + truncation ──────────────────────────────────────────────────

    #[test]
    fn binary_file_change_is_concise_entry_without_hunks() {
        let old = b"\x89PNG\r\n\x1a\n\x00\x00\x00";
        let new = b"\x89PNG\r\n\x1a\n\x00\x00\x01";
        let e = edit("asset.png", Some(old), Some(new)).expect("binary edit");
        assert_eq!(e.kind, FileEditKind::Binary);
        assert!(e.hunks.is_empty());
        // Path is still recorded for the header.
        assert_eq!(e.path, "asset.png");
    }

    #[test]
    fn binary_creation_is_binary_entry() {
        let e = edit("blob.bin", None, Some(b"\x00\x01\x02")).expect("binary created");
        assert!(matches!(e.kind, FileEditKind::Created));
        assert!(e.hunks.is_empty());
    }

    #[test]
    fn large_edit_is_truncated_head_and_tail_with_report() {
        // 200 distinct lines changed in place; cap at 10 kept lines.
        let old: String = (0..200).map(|i| format!("old {i}\n")).collect();
        let new: String = (0..200).map(|i| format!("new {i}\n")).collect();
        let e = compute_file_edit("f.rs", Some(old.as_bytes()), Some(new.as_bytes()), 10)
            .expect("large edit produces a truncated event");
        let kept = e.diff_line_count();
        assert!(kept <= 12, "kept lines ({kept}) must be near the cap");
        assert!(e.omitted_lines > 0, "omitted count must be reported");
        // Head and tail of the edit are preserved.
        let texts: Vec<&str> = e
            .hunks
            .iter()
            .flat_map(|h| h.lines.iter())
            .map(|l| l.text.as_str())
            .collect();
        assert!(
            texts.contains(&"old 0") || texts.contains(&"new 0"),
            "head preserved"
        );
        assert!(
            texts.contains(&"old 199") || texts.contains(&"new 199"),
            "tail preserved"
        );
    }

    // ── Syntax inference ─────────────────────────────────────────────────────

    #[test]
    fn known_extension_selects_expected_syntax() {
        assert_eq!(infer_syntax("src/lib.rs"), SyntaxKind::Rust);
        assert_eq!(infer_syntax("app.ts"), SyntaxKind::TypeScript);
        assert_eq!(infer_syntax("Component.svelte"), SyntaxKind::Svelte);
        assert_eq!(infer_syntax("main.py"), SyntaxKind::Python);
        assert_eq!(infer_syntax("a.json"), SyntaxKind::Json);
        assert_eq!(infer_syntax("a.yml"), SyntaxKind::Yaml);
        assert_eq!(infer_syntax("conf.toml"), SyntaxKind::Toml);
        assert_eq!(infer_syntax("README.md"), SyntaxKind::Markdown);
        assert_eq!(infer_syntax("run.sh"), SyntaxKind::Shell);
        assert_eq!(infer_syntax("query.sql"), SyntaxKind::Sql);
        assert_eq!(infer_syntax("page.html"), SyntaxKind::Html);
        assert_eq!(infer_syntax("style.css"), SyntaxKind::Css);
    }

    #[test]
    fn special_filenames_match_before_extension() {
        assert_eq!(infer_syntax("Dockerfile"), SyntaxKind::Dockerfile);
        assert_eq!(infer_syntax("dockerfile.prod"), SyntaxKind::Dockerfile);
    }

    #[test]
    fn unknown_extension_falls_back_to_plain_text() {
        assert_eq!(infer_syntax("weird.zzz"), SyntaxKind::PlainText);
        assert_eq!(infer_syntax("Makefile"), SyntaxKind::PlainText);
        assert_eq!(infer_syntax("LICENSE"), SyntaxKind::PlainText);
    }

    #[test]
    fn syntect_hint_is_stable_string() {
        assert_eq!(SyntaxKind::Rust.syntect_hint(), "rs");
        assert_eq!(SyntaxKind::PlainText.syntect_hint(), "");
    }

    // ── Sizing helpers ───────────────────────────────────────────────────────

    #[test]
    fn diff_line_count_and_byte_size_are_consistent() {
        let e = modified_edit("a\nb\nc\nd\ne\n", "f.rs", "a\nB\nc\nD\ne\n");
        assert!(e.diff_line_count() > 0);
        assert!(e.approx_byte_size() >= e.path.len());
    }

    #[test]
    fn no_effective_change_emits_nothing() {
        // Empty file created or deleted is not an edit worth showing.
        assert!(edit("empty.rs", None, Some(b"")).is_none());
        assert!(edit("empty.rs", Some(b""), None).is_none());
        assert!(edit("x.rs", Some(b"same\n"), Some(b"same\n")).is_none());
    }
}
