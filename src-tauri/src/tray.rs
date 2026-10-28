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

pub fn build(app: &AppHandle, current_mode: Mode) -> anyhow::Result<TrayIcon> {
    let pause = MenuItemBuilder::with_id("pause", "Pause and unload model").build(app)?;
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
    let menu = MenuBuilder::new(app).item(&pause).item(&modes).separator().item(&history).item(&settings).separator().item(&quit).build()?;
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
                "pause" => state.send(Cmd::PauseAndUnload),
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
