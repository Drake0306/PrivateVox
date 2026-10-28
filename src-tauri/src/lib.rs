//! LVW desktop app: pill and panel windows, tray, hotkeys, pipeline, lifecycle.

pub mod adapters;
pub mod commands;
pub mod frontmost;
pub mod hotkeys;
pub mod laststate;
pub mod lifecycle;
pub mod models;
pub mod paste;
pub mod pipeline;
pub mod state;
pub mod tray;

use crate::adapters::{MicCapture, SystemPaste};
use crate::lifecycle::SharedEngine;
use crate::paste::Paster;
use crate::pipeline::{Cmd, Coordinator, Deps, PillState};
use crate::state::{AppPaths, AppState};
use lvw_core::lifecycle::{IdleTimer, LockFile};
use lvw_core::settings::Settings;
use lvw_core::store::Store;
use lvw_core::vad::Vad;
use lvw_engine::supervisor::{Engine, PidHandle, WorkerEngine};
use std::process::Command;
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{Emitter, Manager, RunEvent};
use tauri_plugin_log::{Target, TargetKind};

#[cfg(target_os = "macos")]
tauri_nspanel::tauri_panel! {
    panel!(PillPanel {
        config: {
            can_become_key_window: false,
            is_floating_panel: true,
            becomes_key_only_if_needed: true,
            hides_on_deactivate: false
        }
    })
}

/// Builds and runs the Tauri app. Never returns.
pub fn run() {
    let lock: Arc<Mutex<Option<LockFile>>> = Arc::new(Mutex::new(None));
    let engine_slot: Arc<Mutex<Option<SharedEngine>>> = Arc::new(Mutex::new(None));
    let pid_slot: Arc<Mutex<Option<PidHandle>>> = Arc::new(Mutex::new(None));
    let (lock_for_exit, engine_for_exit, pid_for_exit) = (lock.clone(), engine_slot.clone(), pid_slot.clone());

    let mut builder = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(w) = app.get_webview_window("panel") {
                let _ = w.show();
                let _ = w.set_focus();
            }
        }))
        .plugin(
            tauri_plugin_log::Builder::new()
                .targets([Target::new(TargetKind::LogDir { file_name: Some("lvw".into()) }), Target::new(TargetKind::Stdout)])
                .level(log::LevelFilter::Info)
                .build(),
        )
        .plugin(tauri_plugin_global_shortcut::Builder::new().build());
    #[cfg(target_os = "macos")]
    {
        builder = builder.plugin(tauri_nspanel::init());
    }

    let app = builder
        .invoke_handler(tauri::generate_handler![
            commands::get_settings,
            commands::set_settings,
            commands::list_history,
            commands::search_history,
            commands::delete_history,
            commands::history_count,
            commands::list_input_devices,
            commands::open_panel,
            commands::current_pill_state,
            commands::open_privacy,
            commands::permissions,
            commands::download_model,
            commands::debug_start,
            commands::debug_stop,
            commands::debug_cancel,
        ])
        .setup(move |app| {
            log::info!("LVW {} starting", lvw_core::version());
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            let paths = AppPaths::new(data_dir);

            let (lockfile, stale) = LockFile::acquire(&paths.lock())?;
            if stale {
                log::warn!("last session ended unexpectedly (stale lock file)");
            }
            *lock.lock().unwrap() = Some(lockfile);

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
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../resources/models/ggml-silero-v5.1.2.bin")
            };
            let vad = Vad::new(&vad_path)?;

            let current = settings.lock().map(|s| s.clone()).unwrap_or_default();
            let model_path = paths.model_file(&current.model_id);
            let deps_model_missing = !model_path.exists();
            let model_path = if deps_model_missing {
                log::warn!("model {} not present yet; downloading", model_path.display());
                None
            } else {
                Some(model_path)
            };

            let mut worker_cmd = Command::new(std::env::current_exe()?);
            worker_cmd.arg("--engine-worker");
            let worker_engine = WorkerEngine::new(worker_cmd);
            *pid_slot.lock().unwrap() = Some(worker_engine.pid_handle());
            let engine: SharedEngine = Arc::new(Mutex::new(Box::new(worker_engine) as Box<dyn Engine>));
            *engine_slot.lock().unwrap() = Some(engine.clone());

            match paste::restore_backup_on_launch(&paths.clipboard_backup()) {
                Ok(true) => log::info!("restored the clipboard from an interrupted paste"),
                Ok(false) => {}
                Err(e) => log::warn!("clipboard restore at launch failed: {e}"),
            }

            let idle = Arc::new(Mutex::new(IdleTimer::new(current.idle_unload_minutes)));
            let paster = Paster::new(current.paste_method, Duration::from_millis(150), paths.clipboard_backup());
            let deps = Deps {
                engine: engine.clone(),
                capture: Box::new(MicCapture::new(settings.clone())),
                paste: Box::new(SystemPaste::new(paster)),
                model_path,
                frontmost: Box::new(frontmost::frontmost_app_name),
                idle: idle.clone(),
            };

            let last_state = Arc::new(laststate::LastState::default());
            let handle = app.handle().clone();
            let recorder = last_state.clone();
            let emit = Box::new(move |s: PillState| {
                if !matches!(s, PillState::Listening { .. }) {
                    log::info!("pill: {}", serde_json::to_string(&s).unwrap_or_default());
                }
                recorder.record(s.clone());
                if let Err(e) = handle.emit("pill-state", &s) {
                    log::warn!("emit pill-state: {e}");
                }
            });
            let coord = Coordinator::new(deps, settings.clone(), store.clone(), vad, emit);
            let (tx, rx) = channel::<Cmd>();
            let tx2 = tx.clone();
            std::thread::Builder::new().name("lvw-pipeline".into()).spawn(move || coord.run(rx, tx2))?;

            // Pill placement: where it was last dragged if still on screen, else bottom-centre above the Dock.
            if let Some(pill) = app.get_webview_window("pill") {
                let pill_size = pill.outer_size().map(|s| (s.width, s.height)).unwrap_or((280, 48));
                let monitor = pill.current_monitor().ok().flatten().or_else(|| pill.primary_monitor().ok().flatten());
                if let Some(m) = monitor {
                    let wa = m.work_area();
                    let area = lvw_core::placement::Rect { x: wa.position.x, y: wa.position.y, w: wa.size.width, h: wa.size.height };
                    let saved = current.pill_position.and_then(|p| lvw_core::placement::clamp_to_area(p, &area, pill_size));
                    let (x, y) = saved.unwrap_or_else(|| lvw_core::placement::default_pill_position(&area, pill_size, 12));
                    if let Err(e) = pill.set_position(tauri::PhysicalPosition::new(x, y)) {
                        log::warn!("pill position: {e}");
                    }
                    log::info!("pill placed at ({x}, {y}) in work area {area:?}");
                }
            }

            // Pill window: non-activating floating panel on every space.
            #[cfg(target_os = "macos")]
            if let Some(pill) = app.get_webview_window("pill") {
                use tauri_nspanel::WebviewWindowExt;
                match pill.to_panel::<PillPanel>() {
                    Ok(panel) => {
                        use tauri_nspanel::{CollectionBehavior, PanelLevel, StyleMask};
                        panel.set_level(PanelLevel::Floating.value());
                        let _ = panel.add_style_mask(StyleMask::empty().nonactivating_panel().value());
                        panel.set_collection_behavior(CollectionBehavior::new().can_join_all_spaces().full_screen_auxiliary().value());
                        log::info!("pill converted to a non-activating panel");
                    }
                    Err(e) => log::warn!("pill panel conversion failed: {e}"),
                }
            }

            let tray = tray::build(app.handle())?;
            let tray_for_idle = tray.clone();
            lifecycle::spawn_idle_watch(engine.clone(), idle.clone(), tx.clone(), Box::new(move |remaining| {
                let text = match remaining {
                    Some(d) => format!("LVW · unloads in {} min", (d.as_secs() + 59) / 60),
                    None => "LVW".to_string(),
                };
                tray::set_status(&tray_for_idle, &text);
            }));

            if let Err(e) = hotkeys::install(app.handle(), &current, tx.clone()) {
                log::error!("hotkeys: {e}");
            }

            let exit_handle = app.handle().clone();
            let _ = ctrlc::set_handler(move || exit_handle.exit(0));

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
            if deps_model_missing {
                let h = app.handle().clone();
                let h2 = h.clone();
                models::ensure_model(h, &current.model_id, &paths.models_dir(), tx.clone(), move |s| {
                    let _ = h2.emit("pill-state", &s);
                });
            }
            app.manage(AppState { paths, settings, store, tx: Mutex::new(tx), stale_lock: stale, last_state });
            Ok(())
        })
        .on_window_event(|window, event| {
            // The panel hides instead of closing; LVW lives in the tray.
            if window.label() == "panel" {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
            // Remember where the pill was dragged.
            if window.label() == "pill" {
                if let tauri::WindowEvent::Moved(pos) = event {
                    let state = window.state::<AppState>();
                    let settings_path = state.paths.settings();
                    let changed = match state.settings.lock() {
                        Ok(mut s) if s.pill_position != Some((pos.x, pos.y)) => {
                            s.pill_position = Some((pos.x, pos.y));
                            Some(s.clone())
                        }
                        _ => None,
                    };
                    if let Some(snapshot) = changed {
                        if let Err(e) = snapshot.save(&settings_path) {
                            log::warn!("save pill position: {e}");
                        }
                    }
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building LVW");

    app.run(move |app, event| match event {
        RunEvent::ExitRequested { code, api, .. } => {
            if code.is_none() {
                // The last window closed; LVW lives in the tray.
                api.prevent_exit();
            }
        }
        RunEvent::Exit => {
            log::info!("exit: unloading engine");
            let engine = engine_for_exit.lock().ok().and_then(|e| e.clone());
            if let Some(engine) = engine {
                if lifecycle::shutdown(engine, Duration::from_secs(2)) == lifecycle::ShutdownOutcome::TimedOut {
                    if let Some(h) = pid_for_exit.lock().ok().and_then(|p| p.clone()) {
                        if h.kill() {
                            log::warn!("exit: killed a busy engine worker");
                        }
                    }
                }
            }
            if let Ok(mut l) = lock_for_exit.lock() {
                l.take();
            }
            let _ = app;
        }
        _ => {}
    });
}
