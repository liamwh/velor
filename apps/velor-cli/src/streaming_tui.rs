//! Streaming TUI for `vel auto` — multi-line, per-type event rendering with
//! rendered Edit diffs (git-diff style: red background for removed, green for
//! added, line numbers on the left).

use std::io;
use std::time::Duration;

use chrono::Local;
use color_eyre::eyre::WrapErr;
use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
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
use tokio_util::sync::CancellationToken;

// ── Messages ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum TuiMessage {
    Entry(TuiEntry),
    SetPrompt(String),
}

/// One entry in the streaming log. Rich enough to render multi-line content,
/// diffs, and tool details.
#[derive(Debug, Clone)]
pub struct TuiEntry {
    pub ts: chrono::DateTime<Local>,
    pub kind: EntryKind,
}

/// The type-specific content of an entry. Each variant knows how to render
/// itself as multiple ratatui [`Line`]s.
#[derive(Debug, Clone)]
pub enum EntryKind {
    /// Assistant text output.
    Text(String),
    /// A tool call started. `input` carries the raw tool args JSON.
    ToolCall {
        tool: String,
        detail: String,
        input: serde_json::Value,
    },
    /// A tool result.
    ToolResult {
        detail: String,
        success: Option<bool>,
    },
    /// An error.
    Error(String),
    /// A lifecycle/status message.
    Info(String),
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
        AgentEvent::Usage { .. } => None,
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
    /// The verb to show in the spinner, derived from the last event type.
    spinner_verb: &'static str,
}

const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

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
        }
    }
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

    let mut state = TuiState::new();

    loop {
        let mut had_new = false;
        while let Ok(msg) = rx.try_recv() {
            match msg {
                TuiMessage::Entry(e) => {
                    // Update spinner verb based on what just happened.
                    state.spinner_verb = match &e.kind {
                        EntryKind::ToolCall { tool, .. } => match tool.as_str() {
                            "Bash" => "running command",
                            "Read" => "reading file",
                            "Edit" | "Write" => "editing",
                            "Grep" => "searching",
                            "Glob" => "finding files",
                            _ => "working",
                        },
                        EntryKind::ToolResult { .. } => "thinking",
                        EntryKind::Text(_) => "generating",
                        EntryKind::Error(_) => "error",
                        EntryKind::Info(_) => state.spinner_verb,
                    };
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

        terminal.draw(|f| render(f, &mut state)).wrap_err("draw")?;

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

    disable_raw_mode().wrap_err("disable raw mode")?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen).wrap_err("leave alt screen")?;
    Ok(state.entries)
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

fn render(f: &mut Frame, state: &mut TuiState) {
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

    // Advance spinner animation on each render.
    state.spinner_idx = (state.spinner_idx + 1) % SPINNER.len();

    // Spinner line: shown when the run is active (channel still open, no errors).
    let spinner = SPINNER[state.spinner_idx];
    let spinner_line = Line::from(vec![
        Span::styled(format!("{spinner} "), Style::default().fg(Color::Cyan)),
        Span::styled(
            state.spinner_verb,
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
        ),
        Span::raw("…"),
    ]);
    f.render_widget(Paragraph::new(spinner_line), spinner_area);

    // Build all lines from all entries (each entry may produce multiple lines).
    let mut all_lines: Vec<Line> = Vec::new();
    for entry in &state.entries {
        let ts = entry.ts.format("%H:%M:%S").to_string();
        let ts_span = Span::styled(format!("{ts} "), Style::default().fg(Color::DarkGray));
        for line in render_entry(&entry.kind, width) {
            // Prepend timestamp to the first line of each entry.
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

    let hints = if state.show_prompt {
        " p/Esc: close prompt  ↑↓: scroll prompt "
    } else {
        " p: prompt  ↑↓: scroll  q: quit  Ctrl+C: cancel "
    };
    let hints_para = Paragraph::new(hints).style(
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM),
    );
    f.render_widget(hints_para, hints_area);

    if state.show_prompt {
        if let Some(prompt) = &state.prompt {
            render_prompt_modal(f, area, prompt, state.prompt_scroll);
        }
    }
}

/// Renders one entry as multiple [`Line`]s (without timestamp — the caller prepends it).
fn render_entry<'a>(kind: &'a EntryKind, _width: usize) -> Vec<Line<'a>> {
    match kind {
        EntryKind::Text(text) => {
            vec![Line::from(vec![
                Span::styled("› ", Style::default().fg(Color::Gray)),
                Span::styled(text, Style::default().fg(Color::Gray)),
            ])]
        }

        EntryKind::ToolCall {
            tool,
            detail,
            input,
        } => {
            let mut lines = Vec::new();

            // Header line: 🔧 TOOL: detail
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

            // For Edit/Write: render a git-diff-style view.
            if tool == "Edit" || tool == "Write" {
                lines.extend(render_edit_diff(tool, input));
            }

            lines
        }

        EntryKind::ToolResult { detail, success } => {
            let (icon, color) = if success == &Some(false) {
                ("⚠️", Color::Red)
            } else {
                ("✅", Color::Green)
            };
            // Render multi-line: split the detail on newlines.
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

/// Renders an Edit/Write tool call's old→new strings as a git-diff-style view.
fn render_edit_diff<'a>(tool: &'a str, input: &'a serde_json::Value) -> Vec<Line<'a>> {
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

    let mut lines = Vec::new();

    // File header (like git diff).
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
        for (i, l) in old.lines().enumerate() {
            lines.push(diff_line(i + 1, l, false));
        }
    }

    if !new.is_empty() {
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(format!("+++ {file}"), Style::default().fg(Color::Green)),
        ]));
        for (i, l) in new.lines().enumerate() {
            lines.push(diff_line(i + 1, l, true));
        }
    }

    lines
}

/// Renders one diff line with line number, +/- prefix, and colour.
fn diff_line(line_no: usize, text: &str, added: bool) -> Line {
    let (prefix, color, bg) = if added {
        ("+", Color::Green, Color::Rgb(20, 40, 20))
    } else {
        ("-", Color::Red, Color::Rgb(40, 20, 20))
    };
    Line::from(vec![
        Span::raw("  "),
        Span::styled(
            format!("{line_no:>4} "),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(format!("{prefix}{text}"), Style::default().fg(color).bg(bg)),
    ])
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
