//! Shared app state handed to Tauri commands.

use crate::pipeline::Cmd;
use privatevox_core::settings::Settings;
use privatevox_core::store::Store;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

/// Where PrivateVox keeps its files (Tauri `app_data_dir`, e.g. `~/Library/Application Support/com.drake0306.privatevox`).
#[derive(Debug, Clone)]
pub struct AppPaths {
    pub data_dir: PathBuf,
    /// Tauri `app_log_dir`: the log file and the crash reports (§10).
    pub log_dir: PathBuf,
}

impl AppPaths {
    pub fn new(data_dir: PathBuf, log_dir: PathBuf) -> Self {
        AppPaths { data_dir, log_dir }
    }
    pub fn settings(&self) -> PathBuf {
        self.data_dir.join("settings.json")
    }
    pub fn db(&self) -> PathBuf {
        self.data_dir.join("privatevox.sqlite")
    }
    pub fn lock(&self) -> PathBuf {
        self.data_dir.join("privatevox.lock")
    }
    pub fn clipboard_backup(&self) -> PathBuf {
        self.data_dir.join("clipboard.bak")
    }
    pub fn models_dir(&self) -> PathBuf {
        self.data_dir.join("models")
    }
    /// Model file for a catalog id, e.g. `large-v3-turbo` → `models/ggml-large-v3-turbo-q5_0.bin`. Unknown ids
    /// map to a q5_0 name so the caller sees "not present" rather than panicking.
    pub fn model_file(&self, model_id: &str) -> PathBuf {
        privatevox_core::models::model_file(&self.models_dir(), model_id).unwrap_or_else(|| self.models_dir().join(format!("ggml-{model_id}-q5_0.bin")))
    }
}

pub struct AppState {
    pub paths: AppPaths,
    pub settings: Arc<Mutex<Settings>>,
    pub store: Arc<Mutex<Store>>,
    pub tx: Mutex<Sender<Cmd>>,
    /// The previous run ended without removing its lock file.
    pub stale_lock: bool,
    pub last_state: Arc<crate::laststate::LastState>,
    /// Model ids with a download in progress.
    pub downloading: Mutex<std::collections::HashSet<String>>,
    pub engine: crate::lifecycle::SharedEngine,
    pub polisher: crate::llm::SharedPolisher,
    pub remote: Arc<privatevox_engine::remote::RemoteState>,
    pub host: Mutex<Option<crate::host::HostServer>>,
    pub vad_path: PathBuf,
    /// The local model the routing engine falls back to; written on every load.
    pub model_path_slot: Arc<Mutex<Option<PathBuf>>>,
}

impl AppState {
    pub fn send(&self, cmd: Cmd) {
        if let Ok(tx) = self.tx.lock() {
            let _ = tx.send(cmd);
        }
    }
}
