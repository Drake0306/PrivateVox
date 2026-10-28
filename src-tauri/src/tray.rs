//! Menu bar icon and menu (design §4.7).

use crate::pipeline::Cmd;
use crate::state::AppState;
use tauri::menu::{MenuBuilder, MenuItemBuilder};
use tauri::tray::{TrayIcon, TrayIconBuilder};
use tauri::{AppHandle, Manager};

pub fn build(app: &AppHandle) -> anyhow::Result<TrayIcon> {
    let pause = MenuItemBuilder::with_id("pause", "Pause and unload model").build(app)?;
    let history = MenuItemBuilder::with_id("history", "Open history").build(app)?;
    let settings = MenuItemBuilder::with_id("settings", "Settings…").build(app)?;
    let quit = MenuItemBuilder::with_id("quit", "Quit LVW").accelerator("CmdOrCtrl+Q").build(app)?;
    let menu = MenuBuilder::new(app).item(&pause).separator().item(&history).item(&settings).separator().item(&quit).build()?;
    let icon = app.default_window_icon().cloned().ok_or_else(|| anyhow::anyhow!("no default icon"))?;
    let tray = TrayIconBuilder::with_id("lvw")
        .icon(icon)
        .icon_as_template(true)
        .tooltip("LVW")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| {
            let state = app.state::<AppState>();
            match event.id.as_ref() {
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
