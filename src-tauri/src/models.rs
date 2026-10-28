//! First-run model download with progress events (design §4.6, §13 step 5).

use crate::pipeline::{Cmd, PillState};
use lvw_core::download::{download_blocking, is_present, ModelSpec, TURBO_Q5_0};
use serde::Serialize;
use std::path::Path;
use std::sync::mpsc::Sender;
use tauri::{AppHandle, Emitter, Manager};

#[derive(Debug, Clone, Serialize)]
pub struct DownloadProgress {
    pub model: String,
    pub received: u64,
    pub total: u64,
    pub done: bool,
    pub error: Option<String>,
}

pub fn spec_for(model_id: &str) -> Option<ModelSpec> {
    lvw_core::models::info(model_id).map(|m| m.spec)
}

/// Downloads the model for `model_id` into `models_dir` on a background thread, emitting `model-download`
/// events and pill progress, then tells the pipeline the model is ready.
pub fn ensure_model(app: AppHandle, model_id: &str, models_dir: &Path, tx: Sender<Cmd>, activate: bool, emit_pill: impl Fn(PillState) + Send + 'static) {
    let Some(spec) = spec_for(model_id) else {
        log::error!("unknown model id {model_id}");
        return;
    };
    let finish = {
        let app = app.clone();
        let id = model_id.to_string();
        move || {
            if let Some(state) = app.try_state::<crate::state::AppState>() {
                if let Ok(mut d) = state.downloading.lock() {
                    d.remove(&id);
                }
            }
            let _ = app.emit("models-changed", ());
        }
    };
    if is_present(&spec, models_dir) {
        if activate {
            let _ = tx.send(Cmd::ModelReady(models_dir.join(spec.file)));
        }
        finish();
        return;
    }
    let dir = models_dir.to_path_buf();
    std::thread::Builder::new()
        .name("lvw-download".into())
        .spawn(move || {
            log::info!("downloading {} ({} MB)", spec.file, spec.bytes / 1_000_000);
            let mut last_pct = -1i64;
            let app2 = app.clone();
            let model = spec.id.to_string();
            let result = download_blocking(&spec, &dir, |received, total| {
                let pct = (received * 100 / total.max(1)) as i64;
                if pct != last_pct {
                    last_pct = pct;
                    emit_pill(PillState::LoadingModel { progress: Some(received as f32 / total.max(1) as f32) });
                    let _ = app2.emit("model-download", DownloadProgress { model: model.clone(), received, total, done: false, error: None });
                }
            });
            match result {
                Ok(path) => {
                    let _ = app.emit("model-download", DownloadProgress { model: spec.id.into(), received: spec.bytes, total: spec.bytes, done: true, error: None });
                    if activate {
                        let _ = tx.send(Cmd::ModelReady(path));
                    } else {
                        emit_pill(PillState::Idle { hotkey_hint: String::new() });
                    }
                }
                Err(e) => {
                    log::error!("model download failed: {e:#}");
                    let _ = app.emit("model-download", DownloadProgress { model: spec.id.into(), received: 0, total: spec.bytes, done: false, error: Some(e.to_string()) });
                    emit_pill(PillState::Error { message: "Model download failed; open the panel to retry".into() });
                }
            }
            finish();
        })
        .expect("download thread");
}
