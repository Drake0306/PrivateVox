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
pub fn set_settings(app: AppHandle, state: State<'_, AppState>, mut settings: Settings) -> Result<(), String> {
    if let Some(c) = settings.toggle.combo.clone() {
        settings.toggle.combo = Some(validate_combo(&c)?);
    }
    if let Some(c) = settings.ptt.combo.clone() {
        settings.ptt.combo = Some(validate_combo(&c)?);
    }
    if let Some(c) = settings.undo.combo.clone() {
        settings.undo.combo = Some(validate_combo(&c)?);
    }
    apply_settings(&state.settings, &state.paths.settings(), settings.clone())?;
    let tx = state.tx.lock().map(|t| t.clone()).map_err(|e| e.to_string())?;
    if let Err(e) = crate::hotkeys::install(&app, &settings, tx) {
        log::error!("rebind hotkeys: {e}");
        return Err(format!("Settings saved, but the hotkeys could not be registered: {e}"));
    }
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
pub fn download_model(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let model_id = state.settings.lock().map(|s| s.model_id.clone()).unwrap_or_default();
    download_model_by_id(app, state, model_id)
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

/// Removes a model's file (and any partial download). The active model cannot be deleted.
pub fn delete_model_file(models_dir: &std::path::Path, id: &str, active_id: &str) -> Result<(), String> {
    if id == active_id {
        return Err("That model is in use; switch to another one first".into());
    }
    let path = lvw_core::models::model_file(models_dir, id).ok_or_else(|| format!("unknown model {id}"))?;
    let part = models_dir.join(format!("{}.part", path.file_name().and_then(|f| f.to_str()).unwrap_or("")));
    let _ = std::fs::remove_file(&part);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

#[derive(serde::Serialize)]
pub struct ModelRow {
    pub id: String,
    pub label: String,
    pub file: String,
    pub bytes: u64,
    pub quant: String,
    pub english_only: bool,
    pub downloaded: bool,
    pub downloading: bool,
    pub active: bool,
    pub stats: Option<lvw_core::store::ModelStats>,
}

#[tauri::command]
pub fn list_models(state: State<'_, AppState>) -> Result<Vec<ModelRow>, String> {
    let active = state.settings.lock().map(|s| s.model_id.clone()).map_err(|e| e.to_string())?;
    let store = state.store.lock().map_err(|e| e.to_string())?;
    let dir = state.paths.models_dir();
    let downloading = state.downloading.lock().map(|d| d.clone()).unwrap_or_default();
    Ok(lvw_core::models::catalog()
        .into_iter()
        .map(|m| ModelRow {
            id: m.id.into(),
            label: m.label.into(),
            file: m.file.into(),
            bytes: m.bytes,
            quant: m.quant.into(),
            english_only: m.english_only,
            downloaded: lvw_core::download::is_present(&m.spec, &dir),
            downloading: downloading.contains(m.id),
            active: m.id == active,
            stats: store.model_stats(m.id).ok().flatten(),
        })
        .collect())
}

#[tauri::command]
pub fn switch_model(state: State<'_, AppState>, id: String) -> Result<(), String> {
    lvw_core::models::info(&id).ok_or_else(|| format!("unknown model {id}"))?;
    state.send(Cmd::SwitchModel(id));
    Ok(())
}

#[tauri::command]
pub fn download_model_by_id(app: AppHandle, state: State<'_, AppState>, id: String) -> Result<(), String> {
    lvw_core::models::info(&id).ok_or_else(|| format!("unknown model {id}"))?;
    if let Ok(mut d) = state.downloading.lock() {
        if !d.insert(id.clone()) {
            return Ok(()); // already running
        }
    }
    let tx = state.tx.lock().map(|t| t.clone()).map_err(|e| e.to_string())?;
    let h = app.clone();
    let active = state.settings.lock().map(|s| s.model_id.clone()).unwrap_or_default();
    crate::models::ensure_model(app.clone(), &id, &state.paths.models_dir(), tx, active == id, move |s| {
        let _ = h.emit("pill-state", &s);
    });
    Ok(())
}

#[tauri::command]
pub fn delete_model(app: AppHandle, state: State<'_, AppState>, id: String) -> Result<(), String> {
    let active = state.settings.lock().map(|s| s.model_id.clone()).map_err(|e| e.to_string())?;
    delete_model_file(&state.paths.models_dir(), &id, &active)?;
    let _ = app.emit("models-changed", ());
    Ok(())
}

#[tauri::command]
pub fn benchmark_model(state: State<'_, AppState>, id: String) -> Result<(), String> {
    lvw_core::models::info(&id).ok_or_else(|| format!("unknown model {id}"))?;
    let dir = state.paths.models_dir();
    if lvw_core::models::model_file(&dir, &id).map(|p| p.exists()) != Some(true) {
        return Err("Download the model first".into());
    }
    state.send(Cmd::Benchmark(id));
    Ok(())
}

/// Builds the strip's input: measured footprints from the store, "others" = system used minus what we hold.
pub fn build_memory_input(store: &lvw_core::store::Store, selected: &str, ours_resident: u64, system: (u64, u64), llm_bytes: u64) -> lvw_core::memory::MemoryInput {
    let mut footprints = std::collections::HashMap::new();
    for m in lvw_core::models::catalog() {
        if let Ok(Some(st)) = store.model_stats(m.id) {
            if let Some(b) = st.mem_bytes {
                footprints.insert(m.id.to_string(), b);
            }
        }
    }
    lvw_core::memory::MemoryInput {
        total: system.0,
        used_by_others: system.1.saturating_sub(ours_resident),
        footprints,
        llm_bytes,
        selected: selected.to_string(),
    }
}

#[tauri::command]
pub fn memory_report(state: State<'_, AppState>) -> Result<lvw_core::memory::MemoryReport, String> {
    let selected = state.settings.lock().map(|s| s.model_id.clone()).map_err(|e| e.to_string())?;
    let ours = match state.engine.try_lock() {
        Ok(e) => e.resident_bytes(),
        Err(_) => {
            // Busy decoding: it is loaded; use the measured footprint if we have one.
            state.store.lock().ok().and_then(|st| st.model_stats(&selected).ok().flatten()).and_then(|st| st.mem_bytes).unwrap_or(0)
        }
    };
    let store = state.store.lock().map_err(|e| e.to_string())?;
    let input = build_memory_input(&store, &selected, ours, lvw_core::memory::system_memory(), 0);
    Ok(lvw_core::memory::budget(&input))
}

/// Normalises a key combination ("ctrl+shift+space" → "Ctrl+Shift+Space") and checks the shortcut plugin
/// can register it. A lone modifier is rejected: that is the hold key's job.
pub fn validate_combo(combo: &str) -> Result<String, String> {
    let mut mods: Vec<&str> = Vec::new();
    let mut key: Option<String> = None;
    for raw in combo.split('+') {
        let t = raw.trim();
        if t.is_empty() {
            return Err("Empty key".into());
        }
        let lower = t.to_lowercase();
        let m = match lower.as_str() {
            "ctrl" | "control" => Some("Ctrl"),
            "shift" => Some("Shift"),
            "alt" | "option" => Some("Alt"),
            "cmd" | "command" | "super" | "meta" => Some("Cmd"),
            "cmdorctrl" | "commandorcontrol" => Some("CmdOrCtrl"),
            _ => None,
        };
        match m {
            Some(m) => {
                if !mods.contains(&m) {
                    mods.push(m);
                }
            }
            None => {
                if key.is_some() {
                    return Err("Only one non-modifier key is allowed".into());
                }
                let mut chars = lower.chars();
                let cap = match chars.next() {
                    Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
                    None => String::new(),
                };
                key = Some(cap);
            }
        }
    }
    let Some(key) = key else {
        return Err("A key combination needs a key besides modifiers (use the hold key for a lone modifier)".into());
    };
    let parts: Vec<&str> = mods.iter().copied().chain(std::iter::once(key.as_str())).collect();
    let normalised = parts.join("+");
    normalised
        .parse::<tauri_plugin_global_shortcut::Shortcut>()
        .map_err(|e| format!("Unrecognised key combination {normalised:?}: {e}"))?;
    Ok(normalised)
}

#[tauri::command]
pub fn check_combo(combo: String) -> Result<String, String> {
    validate_combo(&combo)
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
    fn delete_active_model_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("ggml-large-v3-q5_0.bin");
        std::fs::write(&f, b"stub").unwrap();
        std::fs::write(dir.path().join("ggml-large-v3-q5_0.bin.part"), b"p").unwrap();
        assert!(delete_model_file(dir.path(), "large-v3", "large-v3").is_err());
        assert!(f.exists());
        delete_model_file(dir.path(), "large-v3", "large-v3-turbo").unwrap();
        assert!(!f.exists());
        assert!(!dir.path().join("ggml-large-v3-q5_0.bin.part").exists());
        assert!(delete_model_file(dir.path(), "nope", "large-v3-turbo").is_err());
    }

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

#[cfg(test)]
mod memory_tests {
    use super::*;
    use lvw_core::store::{ModelStats, Store};

    #[test]
    fn memory_report_uses_measured_footprint_when_present() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(&dir.path().join("lvw.sqlite")).unwrap();
        store.set_model_stats("large-v3-turbo", &ModelStats { load_ms: None, speed_x: None, mem_bytes: Some(703_000_000) }).unwrap();
        let input = build_memory_input(&store, "large-v3-turbo", 0, (16_000_000_000, 4_000_000_000), 0);
        assert_eq!(input.footprints.get("large-v3-turbo"), Some(&703_000_000));
        assert_eq!(input.used_by_others, 4_000_000_000, "nothing of ours resident: others = used");
        let report = lvw_core::memory::budget(&input);
        assert_eq!(report.selected_bytes, 703_000_000);
        // With turbo resident (703 MB counted in `used`), others must exclude it.
        let input2 = build_memory_input(&store, "large-v3-turbo", 703_000_000, (16_000_000_000, 4_000_000_000), 0);
        assert_eq!(input2.used_by_others, 4_000_000_000 - 703_000_000);
    }
}

#[cfg(test)]
mod hotkey_tests {
    use super::*;

    #[test]
    fn validate_combo_accepts_ctrl_shift_space_and_rejects_bare_modifier() {
        assert_eq!(validate_combo("Ctrl+Shift+Space").unwrap(), "Ctrl+Shift+Space");
        assert_eq!(validate_combo("ctrl+shift+space").unwrap(), "Ctrl+Shift+Space", "normalised casing");
        assert!(validate_combo("Alt").is_err(), "a lone modifier needs the hold-key path, not a combo");
        assert!(validate_combo("").is_err());
        assert!(validate_combo("Banana+Q").is_err());
        assert_eq!(validate_combo("CmdOrCtrl+Shift+Z").unwrap(), "CmdOrCtrl+Shift+Z");
    }
}
