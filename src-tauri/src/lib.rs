//! LVW desktop app: pill and panel windows, tray, hotkeys, pipeline, lifecycle.

pub mod frontmost;
pub mod paste;

use tauri_plugin_log::{Target, TargetKind};

/// Builds and runs the Tauri app. Never returns.
pub fn run() {
    tauri::Builder::default()
        .plugin(
            tauri_plugin_log::Builder::new()
                .targets([
                    Target::new(TargetKind::LogDir { file_name: Some("lvw".into()) }),
                    Target::new(TargetKind::Stdout),
                ])
                .level(log::LevelFilter::Info)
                .build(),
        )
        .setup(|_app| {
            log::info!("LVW {} starting", lvw_core::version());
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running LVW");
}
