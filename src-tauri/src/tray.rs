//! Menu bar icon and menu (design §4.7).

use crate::pipeline::Cmd;
use crate::state::AppState;
use privatevox_core::polish::Mode;
use std::sync::{Mutex, OnceLock};
use tauri::menu::{CheckMenuItem, CheckMenuItemBuilder, MenuBuilder, MenuItemBuilder, SubmenuBuilder};
use tauri::tray::{TrayIcon, TrayIconBuilder};
use tauri::{AppHandle, Manager, Wry};

/// The mode radio items, so the panel and the tray stay in step.
static MODE_ITEMS: OnceLock<Mutex<Vec<(Mode, CheckMenuItem<Wry>)>>> = OnceLock::new();
/// The model items (one per catalog entry) and the two toggles, refreshed from settings and the models folder.
static MODEL_ITEMS: OnceLock<Mutex<Vec<(String, CheckMenuItem<Wry>)>>> = OnceLock::new();
static TOGGLES: OnceLock<Mutex<(CheckMenuItem<Wry>, CheckMenuItem<Wry>)>> = OnceLock::new();

/// One tray entry per catalog model (§4.7): the active one is ticked, missing ones are greyed and say so.
pub struct TrayModelEntry {
    pub id: String,
    pub label: String,
    pub checked: bool,
    pub enabled: bool,
}

/// `models` is (id, label, downloaded).
pub fn model_entries(models: &[(String, String, bool)], current: &str) -> Vec<TrayModelEntry> {
    models
        .iter()
        .map(|(id, label, downloaded)| TrayModelEntry {
            id: id.clone(),
            label: if *downloaded { label.clone() } else { format!("{label} (not downloaded)") },
            checked: id == current,
            enabled: *downloaded,
        })
        .collect()
}

/// (id, label, downloaded) for every catalog model, plus the active id.
pub fn catalog_rows_for(dir: &std::path::Path, active: &str) -> (Vec<(String, String, bool)>, String) {
    let rows = privatevox_core::models::catalog()
        .into_iter()
        .map(|m| (m.id.to_string(), m.label.to_string(), privatevox_core::download::is_present(&m.spec, dir)))
        .collect();
    (rows, active.to_string())
}

fn catalog_rows(state: &AppState) -> (Vec<(String, String, bool)>, String) {
    let active = state.settings.lock().map(|s| s.model_id.clone()).unwrap_or_default();
    catalog_rows_for(&state.paths.models_dir(), &active)
}

/// Re-tick and re-enable the model items and the two toggles (after a switch, a download, or a settings save).
pub fn refresh(app: &AppHandle) {
    let state = app.state::<AppState>();
    let (rows, active) = catalog_rows(&state);
    if let Some(items) = MODEL_ITEMS.get().and_then(|m| m.lock().ok()) {
        for e in model_entries(&rows, &active) {
            if let Some((_, item)) = items.iter().find(|(id, _)| *id == e.id) {
                let _ = item.set_checked(e.checked);
                let _ = item.set_enabled(e.enabled);
                let _ = item.set_text(&e.label);
            }
        }
    }
    let flags = state.settings.lock().map(|s| (s.launch_at_login, s.show_pill)).ok();
    if let (Some(t), Some((login, pill))) = (TOGGLES.get().and_then(|t| t.lock().ok()), flags) {
        let _ = t.0.set_checked(login);
        let _ = t.1.set_checked(pill);
    }
}

/// Tick the active mode in the tray menu.
pub fn sync_mode(mode: Mode) {
    if let Some(items) = MODE_ITEMS.get() {
        if let Ok(items) = items.lock() {
            for (m, item) in items.iter() {
                let _ = item.set_checked(*m == mode);
            }
        }
    }
}

/// Built before the app state is managed, so everything it shows at first comes in as arguments.
pub fn build(app: &AppHandle, current_mode: Mode, rows: Vec<(String, String, bool)>, active: &str, login_on: bool, pill_on: bool) -> anyhow::Result<TrayIcon> {
    let pause = MenuItemBuilder::with_id("pause", "Pause and unload model").build(app)?;
    let resume = MenuItemBuilder::with_id("resume", "Load model now").build(app)?;
    let mut model_items = Vec::new();
    let mut models_menu = SubmenuBuilder::new(app, "Model");
    for e in model_entries(&rows, active) {
        let item = CheckMenuItemBuilder::with_id(format!("model:{}", e.id), &e.label).checked(e.checked).enabled(e.enabled).build(app)?;
        models_menu = models_menu.item(&item);
        model_items.push((e.id, item));
    }
    let models_menu = models_menu.build()?;
    let _ = MODEL_ITEMS.set(Mutex::new(model_items));
    let login = CheckMenuItemBuilder::with_id("login", "Launch at login").checked(login_on).build(app)?;
    let pill = CheckMenuItemBuilder::with_id("pill", "Show pill").checked(pill_on).build(app)?;
    let _ = TOGGLES.set(Mutex::new((login.clone(), pill.clone())));
    let history = MenuItemBuilder::with_id("history", "Open history").build(app)?;
    let settings = MenuItemBuilder::with_id("settings", "Settings…").build(app)?;
    let quit = MenuItemBuilder::with_id("quit", "Quit PrivateVox").accelerator("CmdOrCtrl+Q").build(app)?;
    let mut mode_items = Vec::new();
    let mut modes = SubmenuBuilder::new(app, "Mode");
    for m in Mode::ALL {
        let item = CheckMenuItemBuilder::with_id(format!("mode:{}", m.id()), m.label()).checked(m == current_mode).build(app)?;
        modes = modes.item(&item);
        mode_items.push((m, item));
    }
    let modes = modes.build()?;
    let _ = MODE_ITEMS.set(Mutex::new(mode_items));
    let menu = MenuBuilder::new(app).item(&pause).item(&resume).item(&models_menu).item(&modes).separator().item(&history).item(&settings).item(&pill).item(&login).separator().item(&quit).build()?;
    let icon = app.default_window_icon().cloned().ok_or_else(|| anyhow::anyhow!("no default icon"))?;
    let tray = TrayIconBuilder::with_id("privatevox")
        .icon(icon)
        .icon_as_template(true)
        .tooltip("PrivateVox")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| {
            let state = app.state::<AppState>();
            match event.id.as_ref() {
                id if id.starts_with("mode:") => {
                    if let Some(mode) = Mode::parse(&id[5..]) {
                        if let Err(e) = crate::llm_commands::set_mode_inner(app, &state, mode) {
                            log::warn!("tray set mode: {e}");
                        }
                    }
                }
                id if id.starts_with("model:") => state.send(Cmd::SwitchModel(id[6..].to_string())),
                "pause" => state.send(Cmd::PauseAndUnload),
                "resume" => state.send(Cmd::Resume),
                "login" => {
                    let on = !state.settings.lock().map(|s| s.launch_at_login).unwrap_or(false);
                    if let Err(e) = crate::commands::set_launch_at_login_inner(app, &state, on) {
                        log::warn!("launch at login: {e}");
                    }
                }
                "pill" => {
                    let on = !state.settings.lock().map(|s| s.show_pill).unwrap_or(true);
                    if let Err(e) = crate::commands::set_show_pill_inner(app, &state, on) {
                        log::warn!("show pill: {e}");
                    }
                }
                "history" | "settings" => {
                    if let Some(w) = app.get_webview_window("panel") {
                        let _ = w.show();
                        let _ = w.set_focus();
                    }
                }
                "quit" => app.exit(0),
                _ => {}
            }
        })
        .build(app)?;
    Ok(tray)
}

pub fn set_status(tray: &TrayIcon, text: &str) {
    let _ = tray.set_tooltip(Some(text));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_entries_check_current_and_disable_missing() {
        let models = vec![("large-v3-turbo".to_string(), "Turbo".to_string(), true), ("large-v3".to_string(), "Large".to_string(), false)];
        let e = model_entries(&models, "large-v3-turbo");
        assert_eq!(e.len(), 2);
        assert_eq!((e[0].id.as_str(), e[0].checked, e[0].enabled), ("large-v3-turbo", true, true));
        assert_eq!((e[1].id.as_str(), e[1].checked, e[1].enabled), ("large-v3", false, false), "not downloaded: shown but greyed");
        assert_eq!(e[1].label, "Large (not downloaded)");
    }
}
