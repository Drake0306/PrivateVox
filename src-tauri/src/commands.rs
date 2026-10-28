//! Tauri commands used by the pill and panel pages.

use crate::pipeline::Cmd;
use crate::state::AppState;
use lvw_core::settings::Settings;
use lvw_core::store::Dictation;
use tauri::{AppHandle, Manager, State};

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> Result<Settings, String> {
    state.settings.lock().map(|s| s.clone()).map_err(|e| e.to_string())
}

/// Validates, saves and applies new settings. Invalid settings are rejected and the old ones kept.
#[tauri::command]
pub fn set_settings(state: State<'_, AppState>, settings: Settings) -> Result<(), String> {
    settings.validate()?;
    settings.save(&state.paths.settings()).map_err(|e| e.to_string())?;
    if let Ok(mut s) = state.settings.lock() {
        *s = settings;
    }
    state.send(Cmd::SettingsChanged);
    Ok(())
}

#[tauri::command]
pub fn list_history(state: State<'_, AppState>, limit: u32, offset: u32) -> Result<Vec<Dictation>, String> {
    state.store.lock().map_err(|e| e.to_string())?.list(limit, offset).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn search_history(state: State<'_, AppState>, query: String) -> Result<Vec<Dictation>, String> {
    state.store.lock().map_err(|e| e.to_string())?.search(&query, 100).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn delete_history(state: State<'_, AppState>, id: i64) -> Result<(), String> {
    state.store.lock().map_err(|e| e.to_string())?.delete(id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn history_count(state: State<'_, AppState>) -> Result<u32, String> {
    state.store.lock().map_err(|e| e.to_string())?.count().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn list_input_devices() -> Vec<String> {
    lvw_core::audio::input_devices()
}

#[tauri::command]
pub fn open_panel(app: AppHandle) -> Result<(), String> {
    let w = app.get_webview_window("panel").ok_or("panel window missing")?;
    w.show().map_err(|e| e.to_string())?;
    w.set_focus().map_err(|e| e.to_string())
}

/// Debug helpers for driving the pipeline without a hotkey (panel devtools or tests).
#[tauri::command]
pub fn debug_start(state: State<'_, AppState>) {
    state.send(Cmd::Start);
}

#[tauri::command]
pub fn debug_stop(state: State<'_, AppState>) {
    state.send(Cmd::Stop);
}

#[tauri::command]
pub fn debug_cancel(state: State<'_, AppState>) {
    state.send(Cmd::Cancel);
}
