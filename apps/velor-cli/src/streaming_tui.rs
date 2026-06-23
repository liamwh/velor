//! Streaming TUI for `vel auto` — shows agent events with timestamps.
//!
//! Inspired by the codex CLI: a clean log view with a bottom key-hints bar,
//! a modal popup for viewing the prompt (`p`), and crossterm-based input.

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

/// Messages sent from the auto loop to the TUI.
#[derive(Debug, Clone)]
pub enum TuiMessage {
    /// A log entry (tool call, result, text delta, etc.).
    Entry(TuiEntry),
    /// Update the prompt shown by the `p` modal.
    SetPrompt(String),
}

/// One entry in the streaming log.
#[derive(Debug, Clone)]
pub struct TuiEntry {
    pub ts: chrono::DateTime<Local>,
    pub icon: &'static str,
    pub text: String,
    pub color: Color,
}

impl TuiEntry {
    fn new(icon: &'static str, color: Color, text: impl Into<String>) -> Self {
        Self {
            ts: Local::now(),
            icon,
            color,
            text: text.into(),
        }
    }

    pub fn tool_call(tool: &str, detail: &str) -> Self {
        Self::new("🔧", Color::Yellow, format!("{tool}: {detail}"))
    }
    pub fn tool_result(detail: &str, success: Option<bool>) -> Self {
        let (icon, c) = if success == Some(false) {
            ("⚠️", Color::Red)
        } else {
            ("✅", Color::Green)
        };
        Self::new(icon, c, detail.to_string())
    }
    pub fn error(text: impl Into<String>) -> Self {
        Self::new("❌", Color::Red, text)
    }
    pub fn text_delta(text: &str) -> Option<Self> {
        if text.is_empty() {
            None
        } else {
            Some(Self::new("›", Color::Gray, text.to_string()))
        }
    }
    pub fn info(text: impl Into<String>) -> Self {
        Self::new("ℹ️", Color::Cyan, text)
    }
}

/// Converts an [`AgentEvent`] to a [`TuiEntry`].
pub fn agent_event_to_tui(event: &velor_core::agent::AgentEvent) -> Option<TuiEntry> {
    use velor_core::agent::AgentEvent;
    match event {
        AgentEvent::TextDelta { text } => TuiEntry::text_delta(text),
        AgentEvent::ToolCall { tool, detail } => Some(TuiEntry::tool_call(tool, detail)),
        AgentEvent::ToolResult {
            detail, success, ..
        } => Some(TuiEntry::tool_result(detail, *success)),
        AgentEvent::Error { message } => Some(TuiEntry::error(message.clone())),
        AgentEvent::Status { message } if message.starts_with("session: ") => None,
        AgentEvent::Status { message } if message.starts_with("thread started: ") => None,
        AgentEvent::Status { message } => Some(TuiEntry::info(message.clone())),
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
}

impl TuiState {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
            prompt: None,
            show_prompt: false,
            prompt_scroll: 0,
            scroll_offset: 0,
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
        // Drain pending messages.
        let mut had_new = false;
        while let Ok(msg) = rx.try_recv() {
            match msg {
                TuiMessage::Entry(e) => {
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

        // Input (100 ms poll tick).
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
    // In prompt modal, handle modal-specific keys first.
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

    // Split: log area (top) + key-hints bar (bottom 1 line).
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(1)])
        .split(area);
    let log_area = chunks[0];
    let hints_area = chunks[1];

    // Build log lines.
    let lines: Vec<Line> = state
        .entries
        .iter()
        .map(|e| {
            let ts = e.ts.format("%H:%M:%S").to_string();
            Line::from(vec![
                Span::styled(format!("{ts} │ "), Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("{} ", e.icon),
                    Style::default().fg(e.color).add_modifier(Modifier::BOLD),
                ),
                Span::raw(&e.text),
            ])
        })
        .collect();

    let total = lines.len() as u16;
    let vis_h = log_area.height.saturating_sub(2);
    let skip = if state.scroll_offset > 0 {
        total
            .saturating_sub(vis_h)
            .saturating_sub(state.scroll_offset)
    } else {
        total.saturating_sub(vis_h)
    };
    let visible: Vec<Line> = lines.into_iter().skip(skip as usize).collect();
    let title = format!(
        " vel auto — {} events {} ",
        state.entries.len(),
        if state.scroll_offset > 0 {
            format!("(↑ {})", state.scroll_offset)
        } else {
            "live".into()
        }
    );
    let para = Paragraph::new(visible).block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(para, log_area);

    // Key-hints bar.
    let hints = if state.show_prompt {
        " p/Esc: close  ↑↓: scroll  "
    } else {
        " p: prompt  ↑↓: scroll  q: quit  Ctrl+C: cancel "
    };
    let hints_para = Paragraph::new(hints).style(
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM),
    );
    f.render_widget(hints_para, hints_area);

    // Prompt modal overlay.
    if state.show_prompt {
        if let Some(prompt) = &state.prompt {
            render_prompt_modal(f, area, prompt, state.prompt_scroll);
        }
    }
}

fn render_prompt_modal(f: &mut Frame, area: Rect, prompt: &str, scroll: u16) {
    // Centered popup: 85% width, 80% height.
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

/// Returns a centered rect with the given percentage width/height.
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
