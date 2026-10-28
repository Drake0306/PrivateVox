//! Tauri commands used by the pill and panel pages.

use crate::pipeline::Cmd;
use crate::state::AppState;
use lvw_core::settings::Settings;
use lvw_core::store::Dictation;
use tauri::{AppHandle, Emitter, Manager, State};

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> Result<Settings, String> {
    state.settings.lock().map(|s| s.clone()).map_err(|e| e.to_string())
}

/// Validates, saves and applies new settings. Invalid settings are rejected and the old ones kept.
#[tauri::command]
pub fn set_settings(state: State<'_, AppState>, settings: Settings) -> Result<(), String> {
    apply_settings(&state.settings, &state.paths.settings(), settings)?;
    state.send(Cmd::SettingsChanged);
    Ok(())
}

/// Opens a macOS Privacy & Security pane: "microphone", "accessibility" or "input_monitoring".
#[tauri::command]
pub fn open_privacy(pane: String) -> Result<(), String> {
    let anchor = match pane.as_str() {
        "microphone" => "Privacy_Microphone",
        "accessibility" => "Privacy_Accessibility",
        "input_monitoring" => "Privacy_ListenEvent",
        _ => return Err("unknown pane".into()),
    };
    std::process::Command::new("open")
        .arg(format!("x-apple.systempreferences:com.apple.preference.security?{anchor}"))
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Permission state for the first-run checklist.
#[derive(serde::Serialize)]
pub struct Permissions {
    pub accessibility: bool,
    pub input_monitoring: bool,
    pub model_present: bool,
    pub stale_lock: bool,
}

#[tauri::command]
pub fn permissions(state: State<'_, AppState>) -> Permissions {
    let model_id = state.settings.lock().map(|s| s.model_id.clone()).unwrap_or_default();
    Permissions {
        accessibility: crate::paste::accessibility_trusted(false),
        input_monitoring: crate::hotkeys::input_monitoring_granted(false),
        model_present: state.paths.model_file(&model_id).exists(),
        stale_lock: state.stale_lock,
    }
}

/// Starts (or restarts) the download of the configured model.
#[tauri::command]
pub fn download_model(app: AppHandle, state: State<'_, AppState>) {
    let model_id = state.settings.lock().map(|s| s.model_id.clone()).unwrap_or_default();
    let tx = state.tx.lock().map(|t| t.clone());
    if let Ok(tx) = tx {
        let h = app.clone();
        crate::models::ensure_model(app.clone(), &model_id, &state.paths.models_dir(), tx, move |s| {
            let _ = h.emit("pill-state", &s);
        });
    }
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

/// The pill asks for this when its page loads, since the first `pill-state` event may precede the page.
#[tauri::command]
pub fn current_pill_state(state: State<'_, AppState>) -> Option<crate::pipeline::PillState> {
    state.last_state.current()
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

/// Shared by the command and its test: validate, persist, swap in.
pub fn apply_settings(current: &std::sync::Mutex<Settings>, path: &std::path::Path, new: Settings) -> Result<(), String> {
    new.validate()?;
    new.save(path).map_err(|e| e.to_string())?;
    let mut s = current.lock().map_err(|e| e.to_string())?;
    *s = new;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn set_settings_rejects_invalid_and_keeps_old() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let current = Mutex::new(Settings::default());
        let mut bad = Settings::default();
        bad.idle_unload_minutes = Some(0);
        assert!(apply_settings(&current, &path, bad).is_err());
        assert_eq!(current.lock().unwrap().idle_unload_minutes, Some(10));
        assert!(!path.exists(), "nothing written for invalid settings");
        let mut good = Settings::default();
        good.idle_unload_minutes = Some(2);
        apply_settings(&current, &path, good).unwrap();
        assert_eq!(current.lock().unwrap().idle_unload_minutes, Some(2));
        assert_eq!(Settings::load(&path).unwrap().idle_unload_minutes, Some(2));
    }
}
