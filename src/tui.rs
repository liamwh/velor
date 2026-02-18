//! Interactive terminal user interface for velor.
//!
//! Provides a menu-based TUI for selecting commands when no subcommand is provided.

use color_eyre::eyre::WrapErr;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Alignment, Rect},
    style::{Color, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Padding, Paragraph},
};
use std::io;

/// Result of the TUI menu selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MenuChoice {
    /// Run a single Claude invocation
    Once,
    /// Run multiple iterations until complete or max iterations reached
    Auto,
    /// Initialise a new repository with a .velor directory and velor.toml config
    Init,
    /// Generate an implementation plan from spec files using OpenAI
    Plan,
    /// Exit the program
    Quit,
}

/// Menu item for display.
#[derive(Debug)]
struct MenuItem {
    label: &'static str,
    description: &'static str,
    choice: MenuChoice,
}

/// All available menu items.
const MENU_ITEMS: &[MenuItem] = &[
    MenuItem {
        label: "Run Once",
        description: "Run a single Claude invocation",
        choice: MenuChoice::Once,
    },
    MenuItem {
        label: "Run Auto",
        description: "Run multiple iterations until complete",
        choice: MenuChoice::Auto,
    },
    MenuItem {
        label: "Init Repository",
        description: "Initialize with .velor directory and velor.toml config",
        choice: MenuChoice::Init,
    },
    MenuItem {
        label: "Generate Plan",
        description: "Generate an implementation plan from spec files using OpenAI",
        choice: MenuChoice::Plan,
    },
    MenuItem {
        label: "Quit",
        description: "Exit the program",
        choice: MenuChoice::Quit,
    },
];

/// Runs the interactive TUI menu and returns the user's selection.
///
/// # Errors
///
/// Returns an error if terminal setup fails or a drawing error occurs.
#[tracing::instrument(level = "debug", ret)]
pub fn run_menu() -> color_eyre::eyre::Result<MenuChoice> {
    enable_raw_mode().wrap_err("failed to enable raw mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)
        .wrap_err("failed to enter alternate screen")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).wrap_err("failed to create terminal")?;

    let mut selected = 0usize;
    let result = loop {
        terminal
            .draw(|frame| {
                draw_ui(frame, selected);
            })
            .wrap_err("failed to draw frame")?;

        // Handle input
        match event::read().wrap_err("failed to read event")? {
            Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    if selected > 0 {
                        selected -= 1;
                    } else {
                        selected = MENU_ITEMS.len() - 1;
                    }
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    selected = (selected + 1) % MENU_ITEMS.len();
                }
                KeyCode::Enter => {
                    break MENU_ITEMS[selected].choice;
                }
                KeyCode::Char('q') | KeyCode::Esc => {
                    break MenuChoice::Quit;
                }
                _ => {}
            },
            _ => {}
        }
    };

    // Restore terminal
    disable_raw_mode().wrap_err("failed to disable raw mode")?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )
    .wrap_err("failed to leave alternate screen")?;
    terminal.show_cursor().wrap_err("failed to show cursor")?;

    Ok(result)
}

/// Draws the TUI interface.
fn draw_ui(frame: &mut Frame, selected: usize) {
    let area = frame.area();

    // VELOR banner art
    let velor_banner = vec![
        Line::from("██╗   ██╗███████╗██╗      ██████╗ ██████╗".fg(Color::Cyan)),
        Line::from("██║   ██║██╔════╝██║     ██╔═══██╗██╔══██╗".fg(Color::Cyan)),
        Line::from("██║   ██║█████╗  ██║     ██║   ██║██████╔╝".fg(Color::Cyan)),
        Line::from("╚██╗ ██╔╝██╔══╝  ██║     ██║   ██║██╔══██╗".fg(Color::Cyan)),
        Line::from(" ╚████╔╝ ███████╗███████╗╚██████╔╝██║  ██║".fg(Color::Cyan)),
        Line::from("  ╚═══╝  ╚══════╝╚══════╝ ╚═════╝ ╚═╝  ╚═╝".fg(Color::Cyan)),
        Line::from(""),
        Line::from("Agent CLI".bold().white()),
    ];

    // Use reduced padding for small terminals to ensure footer is visible
    let is_small_terminal = area.height < 15;
    let banner_padding = if is_small_terminal {
        Padding::new(1, 1, 0, 0) // No vertical padding on small terminals
    } else {
        Padding::uniform(1)
    };
    let banner = Paragraph::new(velor_banner)
        .alignment(Alignment::Center)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .padding(banner_padding),
        );
    let banner_height = if is_small_terminal { 7 } else { 9 };
    frame.render_widget(banner, Rect::new(0, 0, area.width, banner_height));

    // Calculate menu area with dynamic footer positioning to avoid overlap
    let menu_top = banner_height + 1;
    let footer_y = std::cmp::max(banner_height + 1, area.height.saturating_sub(3));
    let menu_bottom = footer_y;
    let menu_height = menu_bottom.saturating_sub(menu_top);

    // Build menu text
    let menu_lines: Vec<Line> = MENU_ITEMS
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let is_selected = i == selected;
            let prefix = if is_selected { " > " } else { "   " };
            let style = if is_selected {
                Style::default().fg(Color::Green).bold()
            } else {
                Style::default().fg(Color::Gray)
            };
            Line::from(vec![
                Span::styled(prefix, style),
                Span::styled(item.label, style),
                Span::raw(" - "),
                Span::styled(item.description, Style::default().fg(Color::DarkGray)),
            ])
        })
        .collect();

    let menu = Paragraph::new(menu_lines).block(Block::default().padding(Padding::horizontal(2)));

    // Center the menu vertically
    let menu_area = Rect::new(2, menu_top, area.width - 4, menu_height);
    frame.render_widget(menu, menu_area);

    // Footer with hints
    let footer_text = vec![Line::from(vec![
        Span::raw(" Use "),
        Span::styled("↑/↓", Style::default().fg(Color::Cyan).bold()),
        Span::raw(" or "),
        Span::styled("j/k", Style::default().fg(Color::Cyan).bold()),
        Span::raw(" to navigate, "),
        Span::styled("Enter", Style::default().fg(Color::Cyan).bold()),
        Span::raw(" to select, "),
        Span::styled("q", Style::default().fg(Color::Cyan).bold()),
        Span::raw(" to quit "),
    ])];
    let footer = Paragraph::new(footer_text)
        .alignment(Alignment::Center)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray))
                .padding(Padding::uniform(1)),
        );
    // Render footer at dynamic position to avoid overlap with banner
    let footer_height = area.height.saturating_sub(footer_y);
    frame.render_widget(footer, Rect::new(0, footer_y, area.width, footer_height));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_menu_choice_equality() {
        assert_eq!(MenuChoice::Once, MenuChoice::Once);
        assert_ne!(MenuChoice::Once, MenuChoice::Auto);
    }

    #[test]
    fn test_menu_items_not_empty() {
        assert!(!MENU_ITEMS.is_empty());
        assert_eq!(MENU_ITEMS.len(), 5);
    }

    #[test]
    fn test_menu_item_labels_unique() {
        let labels: Vec<_> = MENU_ITEMS.iter().map(|m| m.label).collect();
        let unique = labels.to_vec();
        assert_eq!(labels, unique);
    }

    #[test]
    fn test_menu_item_choices_unique() {
        let choices: Vec<_> = MENU_ITEMS.iter().map(|m| m.choice).collect();
        let unique: std::collections::HashSet<_> = choices.iter().copied().collect();
        assert_eq!(choices.len(), unique.len());
    }
}
