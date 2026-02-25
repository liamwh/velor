//! System tray implementation for Velor GUI.
//!
//! The system tray provides quick access to common actions:
//! - Show/hide the main window
//! - Start/stop the background daemon
//! - Quit the application

use color_eyre::Result;
use tauri::{
    AppHandle, Emitter, Manager,
    menu::{MenuBuilder, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};
use tracing::{debug, info, instrument};

/// Tray menu item identifiers.
pub mod tray_ids {
    /// Show/Hide window menu item.
    pub const SHOW_HIDE: &str = "show-hide";
    /// Start daemon menu item.
    pub const START_DAEMON: &str = "start-daemon";
    /// Stop daemon menu item.
    pub const STOP_DAEMON: &str = "stop-daemon";
    /// Quit menu item.
    pub const QUIT: &str = "quit";
}

/// Default tray icon ID.
pub const TRAY_ID: &str = "main";

/// Builds and initializes the system tray for the application.
///
/// # Errors
///
/// Returns an error if tray creation fails.
#[instrument(skip(app), level = "debug", err)]
pub fn build_tray(app: &AppHandle) -> Result<()> {
    info!("Building system tray");

    // Clone app handle for event handlers
    let app_handle_menu = app.clone();
    let app_handle_icon = app.clone();

    // Create menu items
    let show_hide = MenuItem::with_id(app, tray_ids::SHOW_HIDE, "Show Velor", true, None::<&str>)?;
    let start_daemon = MenuItem::with_id(
        app,
        tray_ids::START_DAEMON,
        "Start Daemon",
        true,
        Some("CmdOrCtrl+D"),
    )?;
    let stop_daemon = MenuItem::with_id(
        app,
        tray_ids::STOP_DAEMON,
        "Stop Daemon",
        true,
        Some("CmdOrCtrl+Shift+D"),
    )?;
    let quit = MenuItem::with_id(app, tray_ids::QUIT, "Quit", true, Some("CmdOrCtrl+Q"))?;

    // Build the menu using MenuBuilder
    let menu = MenuBuilder::new(app)
        .item(&show_hide)
        .separator()
        .item(&start_daemon)
        .item(&stop_daemon)
        .separator()
        .item(&quit)
        .build()?;

    // Create the tray with menu and event handlers
    let _tray = TrayIconBuilder::with_id(TRAY_ID)
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(move |app, event| {
            handle_menu_event(app, &app_handle_menu, event);
        })
        .on_tray_icon_event(move |tray, event| {
            handle_tray_icon_event(tray, &app_handle_icon, event);
        })
        .build(app)?;

    // Get the app state for initial daemon status
    let state = app.state::<std::sync::Arc<crate::state::AppState>>();
    let rt = tokio::runtime::Runtime::new()?;
    let is_daemon_running = rt.block_on(state.is_daemon_running());

    // Set initial menu item states
    update_daemon_menu_items(app, is_daemon_running).ok();

    info!("System tray initialized");
    Ok(())
}

/// Handles tray menu item events.
#[instrument(skip(app, app_handle, event), level = "trace")]
fn handle_menu_event(app: &AppHandle, app_handle: &AppHandle, event: tauri::menu::MenuEvent) {
    debug!(id = %event.id().0, "Tray menu event received");

    match event.id().0.as_str() {
        tray_ids::SHOW_HIDE => {
            if let Some(window) = app.get_webview_window("main") {
                if window.is_visible().unwrap_or(false) {
                    debug!("Hiding main window");
                    let _ = window.hide();
                    let _ = update_show_hide_item(app_handle, Some(false));
                } else {
                    debug!("Showing main window");
                    let _ = window.show();
                    let _ = window.set_focus();
                    let _ = update_show_hide_item(app_handle, Some(true));
                }
            }
        }
        tray_ids::START_DAEMON => {
            info!("Start daemon requested from tray");
            // Emit event to frontend to start daemon
            let _ = app.emit("daemon-start-requested", ());
        }
        tray_ids::STOP_DAEMON => {
            info!("Stop daemon requested from tray");
            // Emit event to frontend to stop daemon
            let _ = app.emit("daemon-stop-requested", ());
        }
        tray_ids::QUIT => {
            info!("Quit requested from tray");
            // Emit quit event for cleanup
            let _ = app.emit("app-quit-requested", ());
            app.exit(0);
        }
        id => {
            debug!("Unknown tray menu item: {}", id);
        }
    }
}

/// Handles tray icon click events.
#[instrument(skip(tray, app_handle, event), level = "trace")]
fn handle_tray_icon_event(
    tray: &tauri::tray::TrayIcon,
    app_handle: &AppHandle,
    event: TrayIconEvent,
) {
    if let TrayIconEvent::Click {
        button: MouseButton::Left,
        button_state: MouseButtonState::Up,
        ..
    } = event
    {
        debug!("Tray icon clicked, toggling window visibility");
        let app = tray.app_handle();

        if let Some(window) = app.get_webview_window("main") {
            if window.is_visible().unwrap_or(false) {
                let _ = window.hide();
                let _ = update_show_hide_item(app_handle, Some(false));
            } else {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
                let _ = update_show_hide_item(app_handle, Some(true));
            }
        }
    }
}

/// Updates the show/hide menu item text based on window visibility.
///
/// # Errors
///
/// Returns an error if menu item update fails.
#[instrument(skip(app), level = "trace", err)]
fn update_show_hide_item(app: &AppHandle, is_visible: Option<bool>) -> Result<()> {
    // Get the tray by ID
    if let Some(_tray) = app.tray_by_id(TRAY_ID) {
        // Use set_menu to rebuild with updated item
        // Since we can't easily get individual items, we'll rebuild the menu
        rebuild_tray_menu(app, is_visible, None)?;
    }

    Ok(())
}

/// Updates the daemon menu items based on daemon running state.
///
/// # Errors
///
/// Returns an error if menu item update fails.
#[instrument(skip(app), level = "trace", err)]
fn update_daemon_menu_items(app: &AppHandle, is_running: bool) -> Result<()> {
    // Get the tray by ID
    if let Some(_tray) = app.tray_by_id(TRAY_ID) {
        // Rebuild menu with updated daemon state
        // None means don't change show/hide state, keep current
        rebuild_tray_menu(app, None, Some(is_running))?;
    }

    Ok(())
}

/// Rebuilds the tray menu with updated states.
///
/// # Arguments
///
/// * `app` - The AppHandle
/// * `is_visible` - Optional window visibility state. If None, keeps current.
/// * `daemon_running` - Optional daemon running state. If None, keeps current.
///
/// # Errors
///
/// Returns an error if menu rebuild fails.
#[instrument(skip(app), level = "debug", err)]
fn rebuild_tray_menu(
    app: &AppHandle,
    is_visible: Option<bool>,
    daemon_running: Option<bool>,
) -> Result<()> {
    // Get current states if not provided
    let window_visible = is_visible.unwrap_or_else(|| {
        app.get_webview_window("main")
            .and_then(|w| w.is_visible().ok())
            .unwrap_or(false)
    });

    let state = app.state::<std::sync::Arc<crate::state::AppState>>();
    let rt = tokio::runtime::Runtime::new()?;
    let is_daemon_running =
        daemon_running.unwrap_or_else(|| rt.block_on(state.is_daemon_running()));

    // Create new menu items with updated states
    let show_hide_text = if window_visible {
        "Hide Velor"
    } else {
        "Show Velor"
    };

    let show_hide =
        MenuItem::with_id(app, tray_ids::SHOW_HIDE, show_hide_text, true, None::<&str>)?;
    let start_daemon = MenuItem::with_id(
        app,
        tray_ids::START_DAEMON,
        "Start Daemon",
        !is_daemon_running, // enabled if daemon not running
        Some("CmdOrCtrl+D"),
    )?;
    let stop_daemon = MenuItem::with_id(
        app,
        tray_ids::STOP_DAEMON,
        "Stop Daemon",
        is_daemon_running, // enabled if daemon running
        Some("CmdOrCtrl+Shift+D"),
    )?;
    let quit = MenuItem::with_id(app, tray_ids::QUIT, "Quit", true, Some("CmdOrCtrl+Q"))?;

    // Build the new menu
    let menu = MenuBuilder::new(app)
        .item(&show_hide)
        .separator()
        .item(&start_daemon)
        .item(&stop_daemon)
        .separator()
        .item(&quit)
        .build()?;

    // Update the tray menu
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        tray.set_menu(Some(menu))?;
    }

    debug!(window_visible, is_daemon_running, "Tray menu rebuilt");

    Ok(())
}

/// Updates all tray menu items to reflect current application state.
///
/// This should be called when:
/// - Window visibility changes
/// - Daemon state changes
///
/// # Errors
///
/// Returns an error if updating menu items fails.
#[instrument(skip(app), level = "debug", err)]
pub fn update_tray_state(app: &AppHandle) -> Result<()> {
    // Rebuild menu with current states
    rebuild_tray_menu(app, None, None)?;

    debug!("Tray state updated via rebuild");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tray_ids_are_unique() {
        // Verify all tray IDs are unique
        let ids = [
            tray_ids::SHOW_HIDE,
            tray_ids::START_DAEMON,
            tray_ids::STOP_DAEMON,
            tray_ids::QUIT,
        ];

        let unique_ids: std::collections::HashSet<_> = ids.iter().collect();
        assert_eq!(unique_ids.len(), ids.len(), "All tray IDs must be unique");
    }

    #[test]
    fn test_tray_ids_non_empty() {
        // Verify all tray IDs are non-empty strings
        let ids = [
            tray_ids::SHOW_HIDE,
            tray_ids::START_DAEMON,
            tray_ids::STOP_DAEMON,
            tray_ids::QUIT,
        ];

        for id in ids {
            assert!(!id.is_empty(), "Tray ID must not be empty");
        }
    }

    #[test]
    fn test_tray_id_is_non_empty() {
        assert!(!TRAY_ID.is_empty(), "Tray ID must not be empty");
    }

    #[test]
    fn test_action_ids_distinct() {
        // Verify all action IDs are distinct
        let ids = [
            tray_ids::SHOW_HIDE,
            tray_ids::START_DAEMON,
            tray_ids::STOP_DAEMON,
            tray_ids::QUIT,
        ];

        for (i, id1) in ids.iter().enumerate() {
            for (j, id2) in ids.iter().enumerate() {
                if i != j {
                    assert_ne!(
                        id1, id2,
                        "Action ID '{}' at index {} should not equal '{}' at index {}",
                        id1, i, id2, j
                    );
                }
            }
        }
    }
}
