//! Streaming TUI for `vel auto` — shows agent events with timestamps.
//!
//! Uses ratatui + crossterm in an alternate screen. Events arrive on an mpsc
//! channel from the agent's `run_with_events` callback and the auto loop's
//! lifecycle messages. The TUI auto-scrolls to the latest event.

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
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use tokio_util::sync::CancellationToken;

/// One entry in the streaming log.
#[derive(Debug, Clone)]
pub struct TuiEntry {
    /// Wall-clock timestamp.
    pub ts: chrono::DateTime<Local>,
    /// Short icon/emoji.
    pub icon: &'static str,
    /// The message text (may be multi-line).
    pub text: String,
    /// Colour for the icon.
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

    /// Info / lifecycle message.
    pub fn info(text: impl Into<String>) -> Self {
        Self::new("ℹ️", Color::Cyan, text)
    }

    /// Tool call started.
    pub fn tool_call(tool: &str, detail: &str) -> Self {
        Self::new("🔧", Color::Yellow, format!("{tool}: {detail}"))
    }

    /// Tool result.
    pub fn tool_result(detail: &str, success: Option<bool>) -> Self {
        let (icon, color) = if success == Some(false) {
            ("⚠️", Color::Red)
        } else {
            ("✅", Color::Green)
        };
        Self::new(icon, color, detail.to_string())
    }

    /// Error message.
    pub fn error(text: impl Into<String>) -> Self {
        Self::new("❌", Color::Red, text)
    }

    /// Text delta (assistant output). Returns `None` if empty.
    pub fn text_delta(text: &str) -> Option<Self> {
        if text.is_empty() {
            None
        } else {
            Some(Self::new("›", Color::Gray, text.to_string()))
        }
    }
}

/// Converts an [`AgentEvent`](velor_core::agent::AgentEvent) to a [`TuiEntry`].
pub fn agent_event_to_tui(event: &velor_core::agent::AgentEvent) -> Option<TuiEntry> {
    use velor_core::agent::AgentEvent;
    match event {
        AgentEvent::TextDelta { text } => TuiEntry::text_delta(text),
        AgentEvent::ToolCall { tool, detail } => Some(TuiEntry::tool_call(tool, detail)),
        AgentEvent::ToolResult {
            detail, success, ..
        } => Some(TuiEntry::tool_result(detail, *success)),
        AgentEvent::Error { message } => Some(TuiEntry::error(message.clone())),
        // Suppress internal session/thread metadata.
        AgentEvent::Status { message } if message.starts_with("session: ") => None,
        AgentEvent::Status { message } if message.starts_with("thread started: ") => None,
        AgentEvent::Status { message } => Some(TuiEntry::info(message.clone())),
        AgentEvent::Usage { .. } => None,
    }
}

/// Runs the streaming TUI, rendering entries from `rx` until the channel closes
/// or `cancel` fires. Restores the terminal on exit.
///
/// # Errors
/// Returns an error on terminal setup/teardown failure.
pub async fn run_streaming_tui(
    mut rx: tokio::sync::mpsc::Receiver<TuiEntry>,
    cancel: CancellationToken,
) -> color_eyre::eyre::Result<Vec<TuiEntry>> {
    // Setup terminal.
    enable_raw_mode().wrap_err("enable raw mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).wrap_err("enter alt screen")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).wrap_err("create terminal")?;
    terminal.clear()?;

    let mut entries: Vec<TuiEntry> = Vec::new();
    let mut scroll_offset: u16 = 0;

    loop {
        // Drain pending events (non-blocking).
        let mut had_new = false;
        while let Ok(entry) = rx.try_recv() {
            entries.push(entry);
            had_new = true;
        }
        if had_new {
            scroll_offset = 0; // auto-scroll to bottom
        }

        // Render.
        terminal
            .draw(|f| render(f, &entries, scroll_offset))
            .wrap_err("draw")?;

        // Poll for input (100 ms timeout — also acts as render-refresh tick).
        if event::poll(Duration::from_millis(100))
            .map_err(|e| color_eyre::eyre::eyre!("poll: {e}"))?
        {
            if let Event::Key(key) =
                event::read().map_err(|e| color_eyre::eyre::eyre!("read: {e}"))?
            {
                match key.code {
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        cancel.cancel();
                        break;
                    }
                    KeyCode::Char('q') => {
                        cancel.cancel();
                        break;
                    }
                    KeyCode::Up => {
                        scroll_offset = scroll_offset.saturating_add(1);
                    }
                    KeyCode::Down => {
                        scroll_offset = scroll_offset.saturating_sub(1);
                    }
                    KeyCode::PageUp => {
                        scroll_offset = scroll_offset.saturating_add(10);
                    }
                    KeyCode::PageDown => {
                        scroll_offset = scroll_offset.saturating_sub(10);
                    }
                    _ => {}
                }
            }
        }

        // Exit when channel closed (sender dropped = run complete) and no more events.
        if rx.is_empty() && rx.is_closed() {
            break;
        }

        if cancel.is_cancelled() {
            break;
        }
    }

    // Drain any straggler events.
    while let Ok(entry) = rx.try_recv() {
        entries.push(entry);
    }

    // Restore terminal.
    disable_raw_mode().wrap_err("disable raw mode")?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen).wrap_err("leave alt screen")?;

    Ok(entries)
}

/// Renders the streaming log.
fn render(f: &mut Frame, entries: &[TuiEntry], scroll_offset: u16) {
    let area = f.area();

    // Build the log lines: each entry → one or more lines (timestamp │ icon text).
    let mut lines: Vec<Line> = Vec::new();
    for entry in entries {
        let ts = entry.ts.format("%H:%M:%S").to_string();
        let ts_span = Span::styled(format!("{ts} │ "), Style::default().fg(Color::DarkGray));
        let icon_span = Span::styled(
            format!("{} ", entry.icon),
            Style::default()
                .fg(entry.color)
                .add_modifier(Modifier::BOLD),
        );
        let text_span = Span::raw(&entry.text);
        lines.push(Line::from(vec![ts_span, icon_span, text_span]));
    }

    // Calculate the visible window (auto-scroll to bottom unless user scrolled up).
    let total_lines = lines.len() as u16;
    let visible_height = area.height.saturating_sub(2); // -2 for border
    let skip = if scroll_offset > 0 {
        total_lines
            .saturating_sub(visible_height)
            .saturating_sub(scroll_offset)
    } else {
        total_lines.saturating_sub(visible_height)
    };

    let visible: Vec<Line> = lines.into_iter().skip(skip as usize).collect();

    let title = format!(
        " vel auto — {} events {} ",
        entries.len(),
        if scroll_offset > 0 {
            format!("(↑ {} scrolled)", scroll_offset)
        } else {
            "(live)".to_string()
        }
    );

    let para = Paragraph::new(visible).scroll((0, 0)).block(
        ratatui::widgets::Block::default()
            .borders(ratatui::widgets::Borders::ALL)
            .title(title),
    );

    f.render_widget(para, area);
}
