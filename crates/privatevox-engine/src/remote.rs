//! `RemoteEngine`: the `Engine` trait over a PrivateVox host's HTTP API (design §8.6).

use crate::protocol::DecodeOptions;
use crate::supervisor::{Engine, LoadInfo, Transcript};
use anyhow::Result;
use privatevox_core::net::{encode_pcm, DictationRequest, HostClient, StatusReply};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Shared between the routing engine, the health loop, the polish closure and the panel: whether a host is
/// configured, whether it answered recently, and the current dictation session.
pub struct RemoteState {
    pub enabled: AtomicBool,
    pub healthy: AtomicBool,
    pub label: Mutex<String>,
    pub client: Mutex<Option<Arc<HostClient>>>,
    pub client_name: Mutex<String>,
    pub session: Mutex<String>,
    pub next_index: AtomicU32,
    pub last_status: Mutex<Option<StatusReply>>,
    pub last_error: Mutex<Option<String>>,
    notice: Mutex<Option<String>>,
    session_counter: AtomicU64,
    /// Mode id of the dictation in flight and whether the client will ask the host to polish it.
    mode: Mutex<String>,
    polish: AtomicBool,
}

pub const FALLBACK_NOTICE: &str = "PC unreachable, transcribed locally";

impl RemoteState {
    pub fn new() -> Arc<Self> {
        Arc::new(RemoteState {
            enabled: AtomicBool::new(false),
            healthy: AtomicBool::new(false),
            label: Mutex::new("PC".into()),
            client: Mutex::new(None),
            client_name: Mutex::new("PrivateVox".into()),
            session: Mutex::new(String::new()),
            next_index: AtomicU32::new(0),
            last_status: Mutex::new(None),
            last_error: Mutex::new(None),
            notice: Mutex::new(None),
            session_counter: AtomicU64::new(0),
            mode: Mutex::new("raw".into()),
            polish: AtomicBool::new(false),
        })
    }

    /// Enabled, answered recently, and has somewhere to send to.
    pub fn in_use(&self) -> bool {
        self.enabled.load(Ordering::SeqCst) && self.healthy.load(Ordering::SeqCst) && self.client.lock().map(|c| c.is_some()).unwrap_or(false)
    }

    pub fn configure(&self, label: &str, base_url: &str, token: Option<String>, client_name: &str) {
        *self.label.lock().unwrap() = label.to_string();
        *self.client.lock().unwrap() = Some(Arc::new(HostClient::new(base_url, token)));
        *self.client_name.lock().unwrap() = client_name.to_string();
    }

    pub fn clear(&self) {
        *self.client.lock().unwrap() = None;
        self.healthy.store(false, Ordering::SeqCst);
        *self.last_status.lock().unwrap() = None;
    }

    pub fn client(&self) -> Option<Arc<HostClient>> {
        self.client.lock().unwrap().clone()
    }

    pub fn label(&self) -> String {
        self.label.lock().unwrap().clone()
    }

    pub fn client_name(&self) -> String {
        self.client_name.lock().unwrap().clone()
    }

    /// Start a new dictation: fresh session id, chunk index back to 0.
    pub fn new_session(&self) -> String {
        let n = self.session_counter.fetch_add(1, Ordering::SeqCst);
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
        let id = format!("{t:x}-{n}");
        *self.session.lock().unwrap() = id.clone();
        self.next_index.store(0, Ordering::SeqCst);
        id
    }

    pub fn session(&self) -> String {
        self.session.lock().unwrap().clone()
    }

    /// What the dictation in flight is for: its mode, and whether `/v1/polish` will follow the final chunk.
    pub fn set_dictation(&self, mode_id: &str, polish: bool) {
        *self.mode.lock().unwrap() = mode_id.to_string();
        self.polish.store(polish, Ordering::SeqCst);
    }

    pub fn dictation_mode(&self) -> String {
        self.mode.lock().unwrap().clone()
    }

    pub fn dictation_polish(&self) -> bool {
        self.polish.load(Ordering::SeqCst)
    }

    pub fn set_notice(&self, text: &str) {
        *self.notice.lock().unwrap() = Some(text.to_string());
    }

    /// The pending pill notice, consumed on read.
    pub fn take_notice(&self) -> Option<String> {
        self.notice.lock().unwrap().take()
    }

    pub fn mark_failed(&self, error: String) {
        self.healthy.store(false, Ordering::SeqCst);
        *self.last_error.lock().unwrap() = Some(error);
    }
}

pub struct RemoteEngine {
    state: Arc<RemoteState>,
    timeout: Duration,
}

impl RemoteEngine {
    pub fn new(state: Arc<RemoteState>, timeout: Duration) -> Self {
        RemoteEngine { state, timeout }
    }
}

impl Engine for RemoteEngine {
    fn load(&mut self, _model: &std::path::Path, _flash_attn: bool) -> Result<LoadInfo> {
        if self.state.in_use() {
            Ok(LoadInfo { duration: Duration::ZERO, resident_bytes: 0 })
        } else {
            anyhow::bail!("host not in use")
        }
    }

    fn unload(&mut self) -> Result<()> {
        Ok(())
    }

    fn is_loaded(&self) -> bool {
        self.state.in_use()
    }

    fn transcribe(&mut self, pcm: &[f32], opts: &DecodeOptions) -> Result<Transcript> {
        let Some(client) = self.state.client() else { anyhow::bail!("host not configured") };
        let req = DictationRequest {
            client: self.state.client_name(),
            session: self.state.session(),
            index: self.state.next_index.fetch_add(1, Ordering::SeqCst),
            final_: opts.final_chunk,
            pcm: encode_pcm(pcm),
            language: opts.language.clone(),
            translate: opts.translate,
            mode: self.state.dictation_mode(),
            polish: self.state.dictation_polish(),
            initial_prompt: opts.initial_prompt.clone(),
        };
        let clip_secs = pcm.len() as f32 / 16_000.0;
        let timeout = self.timeout.max(crate::policy::deadline_for(clip_secs));
        let t = Instant::now();
        match client.dictation(&req, timeout) {
            Ok(r) => Ok(Transcript { text: r.text, language: r.language, decode_ms: t.elapsed().as_millis() as u64, audio_ctx: 0, reran: false }),
            Err(e) => {
                self.state.mark_failed(e.to_string());
                anyhow::bail!("host: {e}")
            }
        }
    }

    fn cancel(&mut self) {}
}
