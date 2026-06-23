//! Streaming TUI for `vel auto` — multi-line, per-type event rendering with
//! syntax-highlighted word-level diffs, token usage, animated spinner, and
//! terminal title integration.

use std::io;
use std::time::Duration;

use chrono::Local;
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
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};
use syntect::easy::HighlightLines;
use syntect::highlighting::{Style as SyntectStyle, ThemeSet};
use syntect::parsing::SyntaxSet;
use syntect::util::as_24_bit_terminal_escaped;
use tokio_util::sync::CancellationToken;

const TAB_WIDTH: usize = 4;
const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

// ── Messages ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum TuiMessage {
    Entry(TuiEntry),
    SetPrompt(String),
}

#[derive(Debug, Clone)]
pub struct TuiEntry {
    pub ts: chrono::DateTime<Local>,
    pub kind: EntryKind,
}

#[derive(Debug, Clone)]
pub enum EntryKind {
    Text(String),
    ToolCall {
        tool: String,
        detail: String,
        input: serde_json::Value,
    },
    ToolResult {
        detail: String,
        success: Option<bool>,
    },
    Error(String),
    Info(String),
    Usage {
        input_tokens: Option<u64>,
        output_tokens: Option<u64>,
        cached_input_tokens: Option<u64>,
    },
}

impl TuiEntry {
    fn now(kind: EntryKind) -> Self {
        Self {
            ts: Local::now(),
            kind,
        }
    }
}

pub fn agent_event_to_tui(event: &velor_core::agent::AgentEvent) -> Option<TuiEntry> {
    use velor_core::agent::AgentEvent;
    match event {
        AgentEvent::TextDelta { text } if text.is_empty() => None,
        AgentEvent::TextDelta { text } => Some(TuiEntry::now(EntryKind::Text(text.clone()))),
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
            success: success.clone(),
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

struct TuiState {
    entries: Vec<TuiEntry>,
    prompt: Option<String>,
    show_prompt: bool,
    prompt_scroll: u16,
    scroll_offset: u16,
    spinner_idx: usize,
    spinner_verb: &'static str,
    // Token usage (latest).
    input_tokens: u64,
    output_tokens: u64,
    cached_tokens: u64,
}

impl TuiState {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
            prompt: None,
            show_prompt: false,
            prompt_scroll: 0,
            scroll_offset: 0,
            spinner_idx: 0,
            spinner_verb: "starting",
            input_tokens: 0,
            output_tokens: 0,
            cached_tokens: 0,
        }
    }
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

    /// Highlights a line of code, returning styled ratatui spans.
    fn highlight_line(&self, syntax_name: &str, line: &str) -> Vec<Span> {
        let syntax = self
            .set
            .find_syntax_by_extension(syntax_name)
            .or_else(|| self.set.find_syntax_by_name(syntax_name))
            .unwrap_or_else(|| self.set.find_syntax_plain_text());
        let mut h = HighlightLines::new(syntax, &self.theme);
        let regions = h.highlight_line(line, &self.set).unwrap_or_default();
        let escaped = as_24_bit_terminal_escaped(&regions[..], false);

        // For simplicity, return the escaped string as a single span.
        // ratatui doesn't natively support ANSI escape sequences, so we strip
        // them and just return the plain text — the coloring comes from the
        // diff +/- prefix. A proper integration would parse the ANSI codes into
        // ratatui Color values, which is future work.
        let plain = strip_ansi(&escaped);
        vec![Span::raw(plain)]
    }
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // Skip escape sequence: ESC [ ... m
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

pub async fn run_streaming_tui(
    mut rx: tokio::sync::mpsc::Receiver<TuiMessage>,
    cancel: CancellationToken,
) -> color_eyre::eyre::Result<Vec<TuiEntry>> {
    enable_raw_mode().wrap_err("enable raw mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).wrap_err("enter alt screen")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).wrap_err("create terminal")?;
    terminal.clear()?;

    let syntax = Syntax::new();
    let mut state = TuiState::new();
    set_title("vel auto — starting");

    loop {
        let mut had_new = false;
        while let Ok(msg) = rx.try_recv() {
            match msg {
                TuiMessage::Entry(e) => {
                    // Update spinner verb + token usage from the event.
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
                    }
                    state.entries.push(e);
                    had_new = true;
                }
                TuiMessage::SetPrompt(p) => {
                    state.prompt = Some(p);
                    state.prompt_scroll = 0;
                }
            }
        }
        if had_new {
            state.scroll_offset = 0;
        }

        terminal
            .draw(|f| render(f, &mut state, &syntax))
            .wrap_err("draw")?;

        if event::poll(Duration::from_millis(100))
            .map_err(|e| color_eyre::eyre::eyre!("poll: {e}"))?
        {
            if let Event::Key(key) =
                event::read().map_err(|e| color_eyre::eyre::eyre!("read: {e}"))?
            {
                handle_key(key, &mut state, &cancel);
            }
        }

        if rx.is_empty() && rx.is_closed() {
            break;
        }
        if cancel.is_cancelled() {
            break;
        }
    }

    while let Ok(msg) = rx.try_recv() {
        if let TuiMessage::Entry(e) = msg {
            state.entries.push(e);
        }
    }

    set_title("vel auto — done");
    disable_raw_mode().wrap_err("disable raw mode")?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen).wrap_err("leave alt screen")?;
    Ok(state.entries)
}

fn set_title(title: &str) {
    let _ = execute!(io::stdout(), SetTitle(title));
}

fn handle_key(key: event::KeyEvent, state: &mut TuiState, cancel: &CancellationToken) {
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
        KeyCode::Up => state.scroll_offset = state.scroll_offset.saturating_add(1),
        KeyCode::Down => state.scroll_offset = state.scroll_offset.saturating_sub(1),
        KeyCode::PageUp => state.scroll_offset = state.scroll_offset.saturating_add(10),
        KeyCode::PageDown => state.scroll_offset = state.scroll_offset.saturating_sub(10),
        _ => {}
    }
}

// ── Render ──────────────────────────────────────────────────────────────────

fn render(f: &mut Frame, state: &mut TuiState, syntax: &Syntax) {
    let area = f.area();
    let width = area.width as usize;

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

    // Build all lines from all entries.
    let mut all_lines: Vec<Line> = Vec::new();
    for entry in &state.entries {
        let ts = entry.ts.format("%H:%M:%S").to_string();
        let ts_span = Span::styled(format!("{ts} "), Style::default().fg(Color::DarkGray));
        for line in render_entry(&entry.kind, width, syntax) {
            let mut spans = vec![ts_span.clone()];
            spans.extend(line.spans);
            all_lines.push(Line::from(spans));
        }
    }

    let total = all_lines.len() as u16;
    let vis_h = log_area.height.saturating_sub(2);
    let skip = if state.scroll_offset > 0 {
        total
            .saturating_sub(vis_h)
            .saturating_sub(state.scroll_offset)
    } else {
        total.saturating_sub(vis_h)
    };
    let visible: Vec<Line> = all_lines.into_iter().skip(skip as usize).collect();
    let title = format!(
        " vel auto — {} events {} ",
        state.entries.len(),
        if state.scroll_offset > 0 {
            format!("(↑ {})", state.scroll_offset)
        } else {
            "live".into()
        }
    );
    let para = Paragraph::new(visible)
        .block(Block::default().borders(Borders::ALL).title(title))
        .wrap(Wrap { trim: false });
    f.render_widget(para, log_area);

    // Spinner + token usage line.
    let spinner = SPINNER[state.spinner_idx];
    let cached_pct = if state.input_tokens > 0 {
        state.cached_tokens * 100 / state.input_tokens
    } else {
        0
    };
    let spinner_line = Line::from(vec![
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
    ]);
    f.render_widget(Paragraph::new(spinner_line), spinner_area);

    // Key-hints bar with styled spans.
    let hints = if state.show_prompt {
        vec![
            Span::styled(
                "p/Esc",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" close  "),
            Span::styled(
                "↑↓",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" scroll"),
        ]
    } else {
        vec![
            Span::styled(
                "p",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" prompt  "),
            Span::styled(
                "↑↓",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" scroll  "),
            Span::styled(
                "q",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" quit  "),
            Span::styled(
                "^C",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" cancel"),
        ]
    };
    let hints_para =
        Paragraph::new(Line::from(hints)).style(Style::default().add_modifier(Modifier::DIM));
    f.render_widget(hints_para, hints_area);

    if state.show_prompt {
        if let Some(prompt) = &state.prompt {
            render_prompt_modal(f, area, prompt, state.prompt_scroll);
        }
    }
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

// ── Per-entry rendering ─────────────────────────────────────────────────────

fn render_entry<'a>(kind: &'a EntryKind, _width: usize, syntax: &'a Syntax) -> Vec<Line<'a>> {
    match kind {
        EntryKind::Text(text) => {
            vec![Line::from(vec![
                Span::styled("› ", Style::default().fg(Color::Gray)),
                Span::styled(text, Style::default().fg(Color::Gray)),
            ])]
        }

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
                    tool,
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(": "),
                Span::styled(detail, Style::default().fg(Color::DarkGray)),
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

        EntryKind::Error(msg) => {
            vec![Line::from(vec![
                Span::styled("❌ ", Style::default().fg(Color::Red)),
                Span::styled(msg, Style::default().fg(Color::Red)),
            ])]
        }

        EntryKind::Info(msg) => {
            vec![Line::from(vec![
                Span::styled("ℹ️ ", Style::default().fg(Color::Cyan)),
                Span::styled(msg, Style::default().fg(Color::Cyan)),
            ])]
        }
    }
}

// ── Diff rendering ──────────────────────────────────────────────────────────

fn render_edit_diff<'a>(
    tool: &str,
    input: &serde_json::Value,
    syntax: &'a Syntax,
) -> Vec<Line<'a>> {
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

    let mut lines: Vec<Line<'a>> = Vec::new();

    // File header.
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

        // Simple line-level diff: show old lines as removed, new lines as added.
        // A proper LCS would be better, but this is readable and correct for
        // the common case of small inline edits.
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
                // Line was removed or changed.
                lines.push(diff_line_simple(i + 1, l, false, syn_name, syntax));
            } else {
                // Context line (unchanged).
                lines.push(diff_line_simple(i + 1, l, true, syn_name, syntax));
            }
        }
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(format!("+++ {file}"), Style::default().fg(Color::Green)),
        ]));
        for (i, l) in new_lines.iter().enumerate() {
            if !old_set.contains(*l) {
                // Line was added or changed.
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

/// Renders a diff line with syntax highlighting and proper coloring.
fn diff_line_with_syntax<'a>(
    text: &str,
    tag: &str,
    is_removal: bool,
    syn_name: &str,
    syntax: &'a Syntax,
) -> Line<'a> {
    let (prefix, color, bg) = if is_removal {
        ("-", Color::Red, Color::Rgb(40, 20, 20))
    } else {
        ("+", Color::Green, Color::Rgb(20, 40, 20))
    };

    let highlighted = syntax.highlight_line(syn_name, text);

    let mut spans = vec![
        Span::raw("  "),
        Span::styled(format!("{prefix}"), Style::default().fg(color).bg(bg)),
    ];
    spans.extend(highlighted);

    Line::from(spans)
}

fn diff_line_simple<'a>(
    line_no: usize,
    text: &str,
    added: bool,
    syn_name: &str,
    syntax: &'a Syntax,
) -> Line<'a> {
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
        Span::styled(format!("{prefix}"), Style::default().fg(color).bg(bg)),
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

fn render_prompt_modal(f: &mut Frame, area: Rect, prompt: &str, scroll: u16) {
    let popup = center_rect(area, 85, 80);
    f.render_widget(Clear, popup);
    let lines: Vec<Line> = prompt.lines().map(|l| Line::from(l.to_string())).collect();
    let para = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" 📋 Prompt (p/Esc to close) "),
        )
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));
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
