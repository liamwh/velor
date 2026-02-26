//! Velor Tauri GUI - Rust backend.
//!
//! This module provides the Tauri application entry point and commands
//! for the Velor Agent GUI.

// Prevents additional console window on Windows in release
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

pub mod commands;
pub mod daemon;
pub mod session_store;
pub mod state;
pub mod tray;

use std::sync::Arc;
use tauri::Manager;
use tracing::info;

use state::AppState;
use tray::build_tray;

// Import all commands for use in invoke_handler
use commands::{
    cancel_execution, check_binary_available, create_automation, delete_automation, delete_session,
    discover_git_root, get_automation, get_automation_runs, get_config, get_execution_history,
    get_execution_status, get_home_config, get_repo_config, get_session, get_session_stats,
    list_automations, list_sessions, run_automation_now, save_config, start_daemon,
    start_execution, stop_daemon, test_notification, toggle_automation, update_automation,
};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            info!("Initializing Velor Tauri application");

            // Initialize app state
            let app_state = AppState::new();

            // Load default configs
            let home_path = velor_core::FileConfig::home_config_path()
                .unwrap_or_else(|_| std::path::PathBuf::from("~/.velor/velor.toml"));

            // Discover git root for repo config
            let cwd = std::env::current_dir().ok();
            let git_root = cwd
                .as_ref()
                .and_then(|p| velor_core::git::discover_git_root(p).ok());
            let repo_path = git_root
                .as_ref()
                .map(|p| p.join(".velor").join("velor.toml"));

            let rt = tokio::runtime::Runtime::new().expect("tokio runtime should be created");

            rt.block_on(async {
                // Load configs
                if let Err(e) = app_state.load_configs(Some(home_path), repo_path).await {
                    tracing::warn!("Failed to load initial configs: {}", e);
                }

                // Set git root and initialize automation store if available
                if let Some(root) = git_root {
                    app_state.set_git_root(root.clone()).await;
                    let db_path = root.join(".velor").join("automations.db");
                    if let Err(e) = app_state.init_automation_store(db_path).await {
                        tracing::warn!("Failed to initialize automation store: {}", e);
                    }
                    // Initialize session store in the same database
                    let session_db_path = root.join(".velor").join("sessions.db");
                    if let Err(e) = app_state.init_session_store(session_db_path).await {
                        tracing::warn!("Failed to initialize session store: {}", e);
                    }
                }
            });

            app.manage(Arc::new(app_state));

            // Initialize system tray
            let app_handle = app.handle();
            if let Err(e) = build_tray(app_handle) {
                tracing::warn!("Failed to initialize system tray: {}", e);
            }

            info!("Application state initialized");
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // Config commands
            get_config,
            get_home_config,
            get_repo_config,
            save_config,
            // Execution commands
            start_execution,
            cancel_execution,
            get_execution_status,
            get_execution_history,
            // Session commands
            list_sessions,
            get_session,
            delete_session,
            get_session_stats,
            // Automation commands
            list_automations,
            get_automation,
            create_automation,
            update_automation,
            delete_automation,
            toggle_automation,
            run_automation_now,
            get_automation_runs,
            start_daemon,
            stop_daemon,
            // Notification commands
            test_notification,
            // System commands
            discover_git_root,
            check_binary_available,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
