//! LVW desktop app: pill and panel windows, tray, hotkeys, pipeline, lifecycle.

pub mod adapters;
pub mod commands;
pub mod frontmost;
pub mod paste;
pub mod pipeline;
pub mod state;

use crate::adapters::{MicCapture, SystemPaste};
use crate::paste::Paster;
use crate::pipeline::{Cmd, Coordinator, Deps, PillState};
use crate::state::{AppPaths, AppState};
use lvw_core::settings::Settings;
use lvw_core::store::Store;
use lvw_core::vad::Vad;
use lvw_engine::supervisor::WorkerEngine;
use std::process::Command;
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{Emitter, Manager};
use tauri_plugin_log::{Target, TargetKind};

/// Builds and runs the Tauri app. Never returns.
pub fn run() {
    tauri::Builder::default()
        .plugin(
            tauri_plugin_log::Builder::new()
                .targets([Target::new(TargetKind::LogDir { file_name: Some("lvw".into()) }), Target::new(TargetKind::Stdout)])
                .level(log::LevelFilter::Info)
                .build(),
        )
        .invoke_handler(tauri::generate_handler![
            commands::get_settings,
            commands::set_settings,
            commands::list_history,
            commands::search_history,
            commands::delete_history,
            commands::history_count,
            commands::list_input_devices,
            commands::open_panel,
            commands::debug_start,
            commands::debug_stop,
            commands::debug_cancel,
        ])
        .setup(|app| {
            log::info!("LVW {} starting", lvw_core::version());
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            let paths = AppPaths::new(data_dir);

            let settings = Settings::load(&paths.settings()).unwrap_or_else(|e| {
                log::warn!("settings unreadable, using defaults: {e}");
                Settings::default()
            });
            let settings = Arc::new(Mutex::new(settings));
            let store = Arc::new(Mutex::new(Store::open(&paths.db())?));

            let vad_path = app.path().resource_dir()?.join("resources/models/ggml-silero-v5.1.2.bin");
            let vad_path = if vad_path.exists() {
                vad_path
            } else {
                // `cargo tauri dev` serves resources from the source tree.
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../resources/models/ggml-silero-v5.1.2.bin")
            };
            let vad = Vad::new(&vad_path)?;

            let model_id = settings.lock().map(|s| s.model_id.clone()).unwrap_or_default();
            let model_path = paths.model_file(&model_id);
            let model_path = if model_path.exists() {
                Some(model_path)
            } else {
                log::warn!("model {} not present yet", model_path.display());
                None
            };

            let mut worker_cmd = Command::new(std::env::current_exe()?);
            worker_cmd.arg("--engine-worker");
            let engine = WorkerEngine::new(worker_cmd);

            let (post_release, paste_method) = settings
                .lock()
                .map(|s| (s.post_release_ms, s.paste_method))
                .unwrap_or((150, lvw_core::settings::PasteMethod::Clipboard));
            let _ = post_release;
            let paster = Paster::new(paste_method, Duration::from_millis(150), paths.clipboard_backup());

            let deps = Deps {
                engine: Box::new(engine),
                capture: Box::new(MicCapture::new(settings.clone())),
                paste: Box::new(SystemPaste::new(paster)),
                model_path,
                frontmost: Box::new(frontmost::frontmost_app_name),
            };

            let handle = app.handle().clone();
            let emit = Box::new(move |s: PillState| {
                log::info!("pill: {}", serde_json::to_string(&s).unwrap_or_default());
                if let Err(e) = handle.emit("pill-state", &s) {
                    log::warn!("emit pill-state: {e}");
                }
            });
            let coord = Coordinator::new(deps, settings.clone(), store.clone(), vad, emit);
            let (tx, rx) = channel::<Cmd>();
            let tx2 = tx.clone();
            std::thread::Builder::new().name("lvw-pipeline".into()).spawn(move || coord.run(rx, tx2))?;

            // Dev aid: LVW_DEBUG_SCRIPT="start:1000,stop:4000" sends pipeline commands on a schedule.
            if let Ok(script) = std::env::var("LVW_DEBUG_SCRIPT") {
                let tx = tx.clone();
                std::thread::spawn(move || {
                    let t0 = std::time::Instant::now();
                    for step in script.split(',') {
                        let (what, at) = step.split_once(':').unwrap_or((step, "0"));
                        let at = Duration::from_millis(at.parse().unwrap_or(0));
                        if let Some(rem) = at.checked_sub(t0.elapsed()) {
                            std::thread::sleep(rem);
                        }
                        let cmd = match what {
                            "start" => Cmd::Start,
                            "stop" => Cmd::Stop,
                            "cancel" => Cmd::Cancel,
                            _ => continue,
                        };
                        let _ = tx.send(cmd);
                    }
                });
            }
            app.manage(AppState { paths, settings, store, tx: Mutex::new(tx) });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running LVW");
}
