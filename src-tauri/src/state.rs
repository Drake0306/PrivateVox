//! Shared app state handed to Tauri commands.

use crate::pipeline::Cmd;
use lvw_core::settings::Settings;
use lvw_core::store::Store;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

/// Where LVW keeps its files (Tauri `app_data_dir`, e.g. `~/Library/Application Support/com.drake0306.lvw`).
#[derive(Debug, Clone)]
pub struct AppPaths {
    pub data_dir: PathBuf,
}

impl AppPaths {
    pub fn new(data_dir: PathBuf) -> Self {
        AppPaths { data_dir }
    }
    pub fn settings(&self) -> PathBuf {
        self.data_dir.join("settings.json")
    }
    pub fn db(&self) -> PathBuf {
        self.data_dir.join("lvw.sqlite")
    }
    pub fn lock(&self) -> PathBuf {
        self.data_dir.join("lvw.lock")
    }
    pub fn clipboard_backup(&self) -> PathBuf {
        self.data_dir.join("clipboard.bak")
    }
    pub fn models_dir(&self) -> PathBuf {
        self.data_dir.join("models")
    }
    /// Model file for a model id, e.g. `large-v3-turbo` → `models/ggml-large-v3-turbo-q5_0.bin`.
    pub fn model_file(&self, model_id: &str) -> PathBuf {
        self.models_dir().join(format!("ggml-{model_id}-q5_0.bin"))
    }
}

pub struct AppState {
    pub paths: AppPaths,
    pub settings: Arc<Mutex<Settings>>,
    pub store: Arc<Mutex<Store>>,
    pub tx: Mutex<Sender<Cmd>>,
    /// The previous run ended without removing its lock file.
    pub stale_lock: bool,
}

impl AppState {
    pub fn send(&self, cmd: Cmd) {
        if let Ok(tx) = self.tx.lock() {
            let _ = tx.send(cmd);
        }
    }
}
