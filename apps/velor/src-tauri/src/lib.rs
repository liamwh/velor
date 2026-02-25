//! Velor Tauri GUI - Rust backend.
//!
//! This module provides the Tauri application entry point and commands
//! for the Velor Agent GUI.

// Prevents additional console window on Windows in release
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

pub mod state;

use std::sync::Arc;
use tauri::Manager;
use tracing::info;

use state::AppState;

/// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            info!("Initializing Velor Tauri application");

            // Initialize app state
            let app_state = AppState::new();
            app.manage(Arc::new(app_state));

            info!("Application state initialized");
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![greet])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
