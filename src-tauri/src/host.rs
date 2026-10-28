//! Host mode: serve transcription, polish, history and models to other PrivateVox instances on the LAN (design §8.6).
//!
//! A small blocking server (`tiny_http`, four worker threads). Every dictation chunk carries a session id; the host
//! keeps the chunks of a session until the client either sends the final chunk of a mode without an LLM or calls
//! `/v1/polish`, then stores one history row with `engine_kind = "host"` and the client's name as the app.

use crate::lifecycle::SharedEngine;
use crate::llm::SharedPolisher;
use privatevox_core::net::*;
use privatevox_core::polish::{Mode, PolishStatus};
use privatevox_core::settings::Settings;
use privatevox_core::store::{NewDictation, Store};
use privatevox_core::textnorm::normalize_with;
use privatevox_core::vad::{pad_min, ClipClass, Vad, VadConfig};
use privatevox_engine::protocol::DecodeOptions;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};

/// Sessions without traffic for this long are dropped (a client that crashed mid-dictation).
const SESSION_TTL: Duration = Duration::from_secs(10 * 60);
/// A client counts as connected while it sent something in the last minute.
const CLIENT_TTL: Duration = Duration::from_secs(60);
const WORKERS: usize = 4;

pub struct HostDeps {
    pub engine: SharedEngine,
    pub store: Arc<Mutex<Store>>,
    pub polisher: SharedPolisher,
    pub vad: Arc<Mutex<Vad>>,
    pub settings: Arc<Mutex<Settings>>,
    pub model_path: Arc<Mutex<Option<PathBuf>>>,
    pub status: Arc<dyn Fn() -> StatusReply + Send + Sync>,
    pub models: Arc<dyn Fn() -> ModelsReply + Send + Sync>,
    pub select_model: Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>,
}

struct Session {
    client: String,
    mode: String,
    language: Option<String>,
    model: String,
    /// (chunk index, text) in arrival order; joined by index when stored.
    texts: Vec<(u32, String)>,
    samples: usize,
    last: Instant,
}

struct Inner {
    deps: HostDeps,
    token: Option<String>,
    sessions: Mutex<HashMap<String, Session>>,
    clients: Mutex<HashMap<String, Instant>>,
}

pub struct HostServer {
    server: Arc<Server>,
    inner: Arc<Inner>,
    port: u16,
    token: Option<String>,
    stop: Arc<AtomicBool>,
    workers: Vec<std::thread::JoinHandle<()>>,
}

impl HostServer {
    pub fn start(bind: &str, token: Option<String>, deps: HostDeps) -> anyhow::Result<HostServer> {
        let server = Arc::new(Server::http(bind).map_err(|e| anyhow::anyhow!("listen on {bind}: {e}"))?);
        let port = server.server_addr().to_ip().map(|a| a.port()).ok_or_else(|| anyhow::anyhow!("no tcp address"))?;
        let inner = Arc::new(Inner { deps, token: token.filter(|t| !t.trim().is_empty()), sessions: Mutex::new(HashMap::new()), clients: Mutex::new(HashMap::new()) });
        let stop = Arc::new(AtomicBool::new(false));
        let mut workers = Vec::new();
        for i in 0..WORKERS {
            let (srv, inn, st) = (server.clone(), inner.clone(), stop.clone());
            workers.push(
                std::thread::Builder::new()
                    .name(format!("privatevox-host-{i}"))
                    .spawn(move || {
                        while !st.load(Ordering::SeqCst) {
                            match srv.recv() {
                                Ok(req) => inn.handle(req),
                                Err(_) => break,
                            }
                        }
                    })
                    .expect("host worker"),
            );
        }
        log::info!("host mode: serving on {bind} (port {port})");
        let token = inner.token.clone();
        Ok(HostServer { server, inner, port, token, stop, workers })
    }

    pub fn token_matches(&self, token: Option<&str>) -> bool {
        self.token.as_deref() == token.map(str::trim).filter(|t| !t.is_empty())
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// Distinct clients that sent something in the last minute.
    pub fn clients(&self) -> u32 {
        self.inner.client_count()
    }

    pub fn session_count(&self) -> usize {
        self.inner.sessions.lock().map(|s| s.len()).unwrap_or(0)
    }

    #[cfg(test)]
    pub fn age_sessions_for_test(&self, by: Duration) {
        for s in self.inner.sessions.lock().unwrap().values_mut() {
            s.last = s.last.checked_sub(by).unwrap_or(s.last);
        }
    }

    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // `unblock` wakes one thread stuck in `recv` per call; wake every worker.
        for _ in 0..self.workers.len() {
            self.server.unblock();
        }
        for w in self.workers.drain(..) {
            let _ = w.join();
        }
    }
}

impl Drop for HostServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Start or stop the host server to match the settings (called at startup and on every settings save).
pub fn apply_settings(app: &tauri::AppHandle, state: &crate::state::AppState, s: &Settings) {
    let mut slot = match state.host.lock() {
        Ok(h) => h,
        Err(e) => {
            log::warn!("host slot poisoned: {e}");
            return;
        }
    };
    let running_port = slot.as_ref().map(|h| h.port());
    let wanted = s.host.enabled.then_some(s.host.port);
    if running_port == wanted && (wanted.is_none() || slot.as_ref().map(|h| h.token_matches(s.host.token.as_deref())).unwrap_or(false)) {
        return;
    }
    if let Some(h) = slot.take() {
        log::info!("host mode: stopping");
        h.stop();
    }
    if let Some(port) = wanted {
        let deps = deps_from_state(app, state);
        match HostServer::start(&format!("0.0.0.0:{port}"), s.host.token.clone(), deps) {
            Ok(server) => *slot = Some(server),
            Err(e) => log::error!("host mode could not start: {e}"),
        }
    }
}

/// Everything the server needs, borrowed from the running app.
pub fn deps_from_state(app: &tauri::AppHandle, state: &crate::state::AppState) -> HostDeps {
    use tauri::Manager;
    let vad = Vad::new(&state.vad_path).expect("host vad");
    let (settings, store, engine, polisher) = (state.settings.clone(), state.store.clone(), state.engine.clone(), state.polisher.clone());
    let status_app = app.clone();
    let status: Arc<dyn Fn() -> StatusReply + Send + Sync> = Arc::new(move || {
        let st = status_app.state::<crate::state::AppState>();
        host_status(&st)
    });
    let models_app = app.clone();
    let models: Arc<dyn Fn() -> ModelsReply + Send + Sync> = Arc::new(move || {
        let st = models_app.state::<crate::state::AppState>();
        let active = st.settings.lock().map(|s| s.model_id.clone()).unwrap_or_default();
        let dir = st.paths.models_dir();
        ModelsReply {
            models: privatevox_core::models::catalog()
                .into_iter()
                .map(|m| RemoteModel { id: m.id.into(), label: m.label.into(), downloaded: privatevox_core::download::is_present(&m.spec, &dir), active: m.id == active })
                .collect(),
        }
    });
    let select_app = app.clone();
    let select_model: Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync> = Arc::new(move |id| {
        let st = select_app.state::<crate::state::AppState>();
        if !privatevox_core::models::catalog().iter().any(|m| m.id == id) {
            return Err(format!("unknown model {id:?}"));
        }
        st.send(crate::pipeline::Cmd::SwitchModel(id.to_string()));
        Ok(())
    });
    HostDeps { engine, store, polisher, vad: Arc::new(Mutex::new(vad)), settings, model_path: state.model_path_slot.clone(), status, models, select_model }
}

/// The host's specifications as `GET /v1/status` reports them.
pub fn host_status(state: &crate::state::AppState) -> StatusReply {
    let s = state.settings.lock().map(|s| s.clone()).unwrap_or_default();
    let (total, used) = privatevox_core::memory::system_memory();
    let (loaded, resident) = match state.engine.try_lock() {
        Ok(e) => (e.is_loaded(), e.resident_bytes()),
        Err(_) => (true, state.store.lock().ok().and_then(|st| st.model_stats(&s.model_id).ok().flatten()).and_then(|st| st.mem_bytes).unwrap_or(0)),
    };
    let now = Instant::now();
    let selected = state.polisher.selected(now);
    StatusReply {
        name: crate::remote::machine_name(&s),
        version: privatevox_core::version().to_string(),
        platform: std::env::consts::OS.into(),
        gpu: GpuInfo { name: crate::commands::machine_label(), total_bytes: total, used_bytes: used },
        whisper: WhisperInfo { model: s.model_id.clone(), loaded, resident_bytes: resident },
        polish: PolishInfo {
            enabled: state.polisher.enabled(),
            endpoint_label: selected.as_ref().map(|e| e.label.clone()),
            model: selected.as_ref().and_then(|e| state.polisher.active_model(e)),
            resident_bytes: state.polisher.resident_local_bytes(now),
        },
        clients: state.host.try_lock().ok().and_then(|h| h.as_ref().map(|h| h.clients())).unwrap_or(0),
    }
}

/// This machine's IPv4 addresses on real interfaces (what to type on the other machine).
pub fn lan_addresses() -> Vec<String> {
    match local_ip_address::list_afinet_netifas() {
        Ok(list) => list
            .into_iter()
            .filter_map(|(_, ip)| match ip {
                std::net::IpAddr::V4(v4) if !v4.is_loopback() && !v4.is_link_local() => Some(v4.to_string()),
                _ => None,
            })
            .collect(),
        Err(_) => Vec::new(),
    }
}

fn json_header() -> Header {
    Header::from_bytes("Content-Type", "application/json").expect("header")
}

impl Inner {
    fn client_count(&self) -> u32 {
        let now = Instant::now();
        self.clients.lock().map(|c| c.values().filter(|t| now.duration_since(**t) < CLIENT_TTL).count() as u32).unwrap_or(0)
    }

    fn touch_client(&self, name: &str) {
        if let Ok(mut c) = self.clients.lock() {
            c.insert(name.to_string(), Instant::now());
            c.retain(|_, t| t.elapsed() < CLIENT_TTL * 10);
        }
    }

    fn drop_stale_sessions(&self) {
        if let Ok(mut s) = self.sessions.lock() {
            s.retain(|_, v| v.last.elapsed() < SESSION_TTL);
        }
    }

    fn handle(&self, mut req: Request) {
        self.drop_stale_sessions();
        if let Some(t) = &self.token {
            let ok = req.headers().iter().any(|h| h.field.equiv("authorization") && h.value.as_str() == format!("Bearer {t}"));
            if !ok {
                respond(req, 401, &ErrorReply { error: "token required".into() });
                return;
            }
        }
        let mut body = String::new();
        if req.as_reader().read_to_string(&mut body).is_err() {
            respond(req, 400, &ErrorReply { error: "unreadable body".into() });
            return;
        }
        let url = req.url().to_string();
        let (path, query) = url.split_once('?').map(|(p, q)| (p.to_string(), q.to_string())).unwrap_or((url.clone(), String::new()));
        let method = req.method().clone();
        let (status, json) = match (method, path.as_str()) {
            (Method::Get, "/v1/status") => {
                let mut s = (self.deps.status)();
                s.clients = self.client_count();
                (200, serde_json::to_string(&s).unwrap_or_default())
            }
            (Method::Post, "/v1/dictations") => match serde_json::from_str::<DictationRequest>(&body) {
                Ok(d) => self.dictation(d),
                Err(e) => (400, err(format!("bad dictation request: {e}"))),
            },
            (Method::Post, "/v1/polish") => match serde_json::from_str::<PolishRequest>(&body) {
                Ok(p) => self.polish(p),
                Err(e) => (400, err(format!("bad polish request: {e}"))),
            },
            (Method::Get, "/v1/history") => self.history(&query),
            (Method::Get, "/v1/models") => (200, serde_json::to_string(&(self.deps.models)()).unwrap_or_default()),
            (Method::Post, "/v1/models/select") => match serde_json::from_str::<SelectModelRequest>(&body) {
                Ok(s) => match (self.deps.select_model)(&s.id) {
                    Ok(()) => (200, "{}".into()),
                    Err(e) => (400, err(e)),
                },
                Err(e) => (400, err(format!("bad select request: {e}"))),
            },
            _ => (404, err("not found".into())),
        };
        let _ = req.respond(Response::from_string(json).with_status_code(StatusCode(status)).with_header(json_header()));
    }

    fn dictation(&self, d: DictationRequest) -> (u16, String) {
        self.touch_client(&d.client);
        let pcm = match decode_pcm(&d.pcm) {
            Ok(p) => p,
            Err(e) => return (400, err(e)),
        };
        let settings = self.deps.settings.lock().map(|s| s.clone()).unwrap_or_default();
        let model = settings.model_id.clone();
        let speech = match self.deps.vad.lock().map_err(|e| e.to_string()).and_then(|mut v| v.classify(&pcm, &VadConfig::default()).map_err(|e| e.to_string())) {
            Ok(ClipClass::Speech(s)) => Some(pad_min(s, 1.25)),
            Ok(_) => None,
            Err(e) => {
                log::warn!("host vad failed, decoding whole chunk: {e}");
                Some(pcm.clone())
            }
        };
        let t = Instant::now();
        let (text, language) = match speech {
            None => (String::new(), d.language.clone()),
            Some(s) => {
                let opts = DecodeOptions { language: d.language.clone(), translate: d.translate, initial_prompt: None, n_threads: 4, final_chunk: d.final_ };
                // A host does the work itself, even if it is also set up to use a host.
                let r = privatevox_engine::routing::with_local_only(|| {
                    self.deps.engine.lock().map_err(|e| anyhow::anyhow!("{e}")).and_then(|mut e| {
                        if !e.is_loaded() {
                            let path = self.deps.model_path.lock().ok().and_then(|p| p.clone()).ok_or_else(|| anyhow::anyhow!("no model on the host"))?;
                            e.load(&path, settings.flash_attention)?;
                        }
                        e.transcribe(&s, &opts)
                    })
                });
                match r {
                    Ok(tr) => (tr.text, tr.language.or(d.language.clone())),
                    Err(e) => return (500, err(format!("transcription failed: {e}"))),
                }
            }
        };
        let decode_ms = t.elapsed().as_millis() as u64;
        let uses_llm = Mode::parse(&d.mode).map(|m| m.uses_llm()).unwrap_or(false);
        let mut store_now: Option<Session> = None;
        {
            let mut sessions = self.sessions.lock().unwrap();
            let s = sessions.entry(d.session.clone()).or_insert_with(|| Session { client: d.client.clone(), mode: d.mode.clone(), language: language.clone(), model: model.clone(), texts: Vec::new(), samples: 0, last: Instant::now() });
            if !text.is_empty() {
                s.texts.push((d.index, text.clone()));
            }
            s.samples += pcm.len();
            s.last = Instant::now();
            if d.final_ && !uses_llm {
                store_now = sessions.remove(&d.session);
            } else if d.final_ && uses_llm {
                s.mode = d.mode.clone(); // the polish call closes it
            }
        }
        if let Some(s) = store_now {
            self.store_session(s, None, None, "none", &settings);
        }
        (200, serde_json::to_string(&DictationReply { text, language, model, decode_ms }).unwrap_or_default())
    }

    fn polish(&self, p: PolishRequest) -> (u16, String) {
        self.touch_client(&p.client);
        let Some(mode) = Mode::parse(&p.mode) else { return (400, err(format!("unknown mode {:?}", p.mode))) };
        let settings = self.deps.settings.lock().map(|s| s.clone()).unwrap_or_default();
        let t = Instant::now();
        let outcome = self.deps.polisher.polish(mode, &p.text, Instant::now());
        let (status, polished_by, polished_text) = match &outcome.status {
            PolishStatus::Polished { endpoint_id, .. } => ("ok", Some(endpoint_id.clone()), Some(outcome.text.clone())),
            PolishStatus::Unpolished { .. } => ("unpolished", None, None),
            PolishStatus::NotNeeded => ("none", None, None),
        };
        let session = self.sessions.lock().unwrap().remove(&p.session).unwrap_or_else(|| Session { client: p.client.clone(), mode: p.mode.clone(), language: None, model: settings.model_id.clone(), texts: vec![(0, p.text.clone())], samples: 0, last: Instant::now() });
        let session = Session { mode: p.mode.clone(), ..session };
        self.store_session(session, polished_text.as_deref(), polished_by.as_deref(), status, &settings);
        (200, serde_json::to_string(&PolishReply { text: outcome.text, polish_status: status.into(), polished_by, ms: t.elapsed().as_millis() as u64 }).unwrap_or_default())
    }

    fn store_session(&self, mut s: Session, polished_text: Option<&str>, polished_by: Option<&str>, polish_status: &str, settings: &Settings) {
        s.texts.sort_by_key(|(i, _)| *i);
        let joined: Vec<String> = s.texts.into_iter().map(|(_, t)| t).collect();
        let english = s.language.as_deref().map(|l| l == "en").unwrap_or(true);
        let raw = normalize_with(&joined.join(" "), english, settings.remove_fillers);
        if raw.is_empty() {
            return;
        }
        let row = NewDictation {
            app_name: Some(s.client),
            mode: s.mode,
            model: s.model,
            language: s.language,
            duration_ms: (s.samples as f32 / 16.0) as u32,
            raw_text: raw,
            polished_text: polished_text.map(|t| t.to_string()),
            polished_by: polished_by.map(|t| t.to_string()),
            polish_status: polish_status.into(),
            status: "ok".into(),
            timing_json: None,
            engine_kind: "host".into(),
        };
        if let Ok(mut st) = self.deps.store.lock() {
            if let Err(e) = st.insert(&row) {
                log::error!("host history insert failed: {e}");
            }
        }
    }

    fn history(&self, query: &str) -> (u16, String) {
        let mut limit = 100u32;
        let mut offset = 0u32;
        let mut search: Option<String> = None;
        for (k, v) in query.split('&').filter_map(|kv| kv.split_once('=')) {
            match k {
                "limit" => limit = v.parse().unwrap_or(100).min(1000),
                "offset" => offset = v.parse().unwrap_or(0),
                "search" => search = Some(percent_decode(v)),
                _ => {}
            }
        }
        let store = match self.deps.store.lock() {
            Ok(s) => s,
            Err(e) => return (500, err(e.to_string())),
        };
        let rows = match &search {
            Some(q) if !q.trim().is_empty() => store.search(q, limit),
            _ => store.list(limit, offset),
        };
        let total = store.count().unwrap_or(0);
        match rows {
            Ok(rows) => {
                let rows = rows
                    .into_iter()
                    .map(|r| RemoteRow { id: r.id, created_at_ms: r.created_at_ms, app_name: r.app_name, mode: r.mode, model: r.model, duration_ms: r.duration_ms, raw_text: r.raw_text, polished_text: r.polished_text, polish_status: r.polish_status, engine_kind: r.engine_kind })
                    .collect();
                (200, serde_json::to_string(&HistoryReply { rows, total }).unwrap_or_default())
            }
            Err(e) => (500, err(e.to_string())),
        }
    }
}

fn err(message: String) -> String {
    serde_json::to_string(&ErrorReply { error: message }).unwrap_or_default()
}

fn respond<T: serde::Serialize>(req: Request, status: u16, body: &T) {
    let _ = req.respond(Response::from_string(serde_json::to_string(body).unwrap_or_default()).with_status_code(StatusCode(status)).with_header(json_header()));
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() + 0 && i + 2 <= bytes.len() - 1 => {
                if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                    out.push(v);
                    i += 3;
                    continue;
                }
                out.push(b'%');
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use privatevox_core::net::{encode_pcm, ClientError, DictationRequest, HostClient, PolishRequest};
    use privatevox_core::polish::fake_server::{Behaviour, FakeLlm};
    use privatevox_core::polish::{Flavor, HttpLlmClient, Polisher};
    use privatevox_core::settings::Settings;
    use privatevox_core::store::Store;
    use privatevox_core::vad::Vad;
    use privatevox_engine::protocol::DecodeOptions;
    use privatevox_engine::supervisor::{Engine, LoadInfo, Transcript};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    struct HostFakeEngine {
        calls: u32,
        loaded: bool,
        delay: Duration,
        in_flight: Arc<AtomicU32>,
        max_in_flight: Arc<AtomicU32>,
    }
    impl Engine for HostFakeEngine {
        fn load(&mut self, _m: &Path, _f: bool) -> anyhow::Result<LoadInfo> {
            self.loaded = true;
            Ok(LoadInfo { duration: Duration::ZERO, resident_bytes: 1 })
        }
        fn unload(&mut self) -> anyhow::Result<()> {
            self.loaded = false;
            Ok(())
        }
        fn is_loaded(&self) -> bool {
            self.loaded
        }
        fn transcribe(&mut self, _pcm: &[f32], _o: &DecodeOptions) -> anyhow::Result<Transcript> {
            let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_in_flight.fetch_max(now, Ordering::SeqCst);
            std::thread::sleep(self.delay);
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            self.calls += 1;
            Ok(Transcript { text: format!("host:{}", self.calls), language: Some("en".into()), decode_ms: 1, audio_ctx: 768, reran: false })
        }
        fn cancel(&mut self) {}
    }

    struct Rig {
        server: HostServer,
        store: Arc<Mutex<Store>>,
        max_in_flight: Arc<AtomicU32>,
        _dir: tempfile::TempDir,
        _llm: FakeLlm,
    }

    fn rig(token: Option<&str>, delay: Duration) -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(Store::open(&dir.path().join("privatevox.sqlite")).unwrap()));
        let llm = FakeLlm::start(Behaviour::OpenAi { reply: "Polished text.".into() });
        let polisher = Arc::new(Polisher::new(Arc::new(HttpLlmClient::new()), vec![llm.endpoint(Flavor::OpenAi)], Duration::from_secs(2)));
        polisher.check_due(std::time::Instant::now());
        let max_in_flight = Arc::new(AtomicU32::new(0));
        let engine: crate::lifecycle::SharedEngine = Arc::new(Mutex::new(Box::new(HostFakeEngine { calls: 0, loaded: true, delay, in_flight: Arc::new(AtomicU32::new(0)), max_in_flight: max_in_flight.clone() }) as Box<dyn Engine>));
        let vad = Arc::new(Mutex::new(Vad::new(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../resources/models/ggml-silero-v5.1.2.bin")).unwrap()));
        let deps = HostDeps {
            engine,
            store: store.clone(),
            polisher,
            vad,
            settings: Arc::new(Mutex::new(Settings::default())),
            model_path: Arc::new(Mutex::new(Some(PathBuf::from("/m.bin")))),
            status: Arc::new(|| privatevox_core::net::StatusReply {
                name: "Test host".into(),
                version: "0.3.0".into(),
                platform: "macos".into(),
                gpu: privatevox_core::net::GpuInfo { name: "Apple M1".into(), total_bytes: 8_000_000_000, used_bytes: 4_000_000_000 },
                whisper: privatevox_core::net::WhisperInfo { model: "large-v3-turbo".into(), loaded: true, resident_bytes: 700_000_000 },
                polish: privatevox_core::net::PolishInfo { enabled: true, endpoint_label: Some("fake".into()), model: Some("m".into()), resident_bytes: 0 },
                clients: 0,
            }),
            models: Arc::new(|| privatevox_core::net::ModelsReply { models: vec![privatevox_core::net::RemoteModel { id: "large-v3-turbo".into(), label: "turbo".into(), downloaded: true, active: true }] }),
            select_model: Arc::new(|_id| Ok(())),
        };
        let server = HostServer::start("127.0.0.1:0", token.map(|t| t.to_string()), deps).unwrap();
        Rig { server, store, max_in_flight, _dir: dir, _llm: llm }
    }

    fn client(r: &Rig, token: Option<&str>) -> HostClient {
        HostClient::new(&format!("http://127.0.0.1:{}", r.server.port()), token.map(|t| t.to_string()))
    }

    fn speech() -> Vec<f32> {
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../crates/privatevox-core/tests/fixtures/hello.wav");
        let mut r = hound::WavReader::open(p).unwrap();
        let mut pcm: Vec<f32> = vec![0.0; 8000];
        pcm.extend(r.samples::<i16>().map(|s| s.unwrap() as f32 / 32768.0));
        pcm.extend(vec![0.0; 8000]);
        pcm
    }

    fn chunk(session: &str, index: u32, final_: bool, mode: &str, pcm: &[f32]) -> DictationRequest {
        DictationRequest { client: "Mac".into(), session: session.into(), index, final_, pcm: encode_pcm(pcm), language: Some("en".into()), translate: false, mode: mode.into() }
    }

    #[test]
    fn status_and_token() {
        let r = rig(Some("t"), Duration::ZERO);
        assert_eq!(client(&r, None).status(Duration::from_secs(1)).unwrap_err(), ClientError::Unauthorized);
        let s = client(&r, Some("t")).status(Duration::from_secs(1)).unwrap();
        assert_eq!(s.gpu.name, "Apple M1");
        assert_eq!(s.clients, 0, "a status call is not a client");
    }

    #[test]
    fn dictation_stores_one_row_per_session_when_final_without_llm() {
        let r = rig(None, Duration::ZERO);
        let c = client(&r, None);
        let a = c.dictation(&chunk("s", 0, false, "raw", &speech()), Duration::from_secs(5)).unwrap();
        let b = c.dictation(&chunk("s", 1, true, "raw", &speech()), Duration::from_secs(5)).unwrap();
        assert_eq!((a.text.as_str(), b.text.as_str()), ("host:1", "host:2"));
        let rows = r.store.lock().unwrap().list(10, 0).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].raw_text, "host:1 host:2");
        assert_eq!(rows[0].engine_kind, "host");
        assert_eq!(rows[0].app_name.as_deref(), Some("Mac"));
        assert_eq!(rows[0].mode, "raw");
        assert_eq!(rows[0].polish_status, "none");
        assert_eq!(r.server.session_count(), 0, "closed sessions are dropped");
        assert_eq!(c.status(Duration::from_secs(1)).unwrap().clients, 1);
    }

    #[test]
    fn polish_stores_the_row_with_polished_text() {
        let r = rig(None, Duration::ZERO);
        let c = client(&r, None);
        c.dictation(&chunk("s2", 0, true, "clean", &speech()), Duration::from_secs(5)).unwrap();
        assert_eq!(r.store.lock().unwrap().count().unwrap(), 0, "an LLM mode waits for the polish call");
        let p = c.polish(&PolishRequest { client: "Mac".into(), session: "s2".into(), mode: "clean".into(), text: "host:1".into() }, Duration::from_secs(5)).unwrap();
        assert_eq!(p.text, "Polished text.");
        assert_eq!(p.polish_status, "ok");
        let rows = r.store.lock().unwrap().list(10, 0).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].raw_text, "host:1");
        assert_eq!(rows[0].polished_text.as_deref(), Some("Polished text."));
        assert_eq!(rows[0].polish_status, "ok");
        assert_eq!(rows[0].mode, "clean");
    }

    #[test]
    fn bad_body_is_400() {
        let r = rig(None, Duration::ZERO);
        let mut bad = chunk("s", 0, true, "raw", &[]);
        bad.pcm = "!!!".into();
        match client(&r, None).dictation(&bad, Duration::from_secs(2)).unwrap_err() {
            ClientError::Server(m) => assert!(m.contains("base64"), "{m}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn silent_audio_returns_empty_text_and_stores_nothing() {
        let r = rig(None, Duration::ZERO);
        let reply = client(&r, None).dictation(&chunk("s", 0, true, "raw", &vec![0.0f32; 16_000]), Duration::from_secs(5)).unwrap();
        assert_eq!(reply.text, "");
        assert_eq!(r.store.lock().unwrap().count().unwrap(), 0);
    }

    #[test]
    fn concurrent_requests_serialise_on_the_engine() {
        let r = rig(None, Duration::from_millis(150));
        let port = r.server.port();
        let hs: Vec<_> = (0..3)
            .map(|i| {
                std::thread::spawn(move || {
                    let c = HostClient::new(&format!("http://127.0.0.1:{port}"), None);
                    c.dictation(&chunk(&format!("c{i}"), 0, true, "raw", &speech()), Duration::from_secs(10)).unwrap()
                })
            })
            .collect();
        for h in hs {
            h.join().unwrap();
        }
        assert_eq!(r.max_in_flight.load(Ordering::SeqCst), 1);
        assert_eq!(r.store.lock().unwrap().count().unwrap(), 3);
    }

    #[test]
    fn stale_sessions_are_dropped_after_ten_minutes() {
        let r = rig(None, Duration::ZERO);
        let c = client(&r, None);
        c.dictation(&chunk("old", 0, false, "clean", &speech()), Duration::from_secs(5)).unwrap();
        assert_eq!(r.server.session_count(), 1);
        r.server.age_sessions_for_test(Duration::from_secs(11 * 60));
        c.status(Duration::from_secs(1)).unwrap();
        assert_eq!(r.server.session_count(), 0);
    }

    #[test]
    fn history_lists_host_rows_and_models_select() {
        let r = rig(None, Duration::ZERO);
        let c = client(&r, None);
        c.dictation(&chunk("h", 0, true, "raw", &speech()), Duration::from_secs(5)).unwrap();
        let h = c.history(10, 0, None, Duration::from_secs(1)).unwrap();
        assert_eq!(h.rows.len(), 1);
        assert_eq!(h.rows[0].engine_kind, "host");
        assert_eq!(h.total, 1);
        assert!(c.history(10, 0, Some("zzz"), Duration::from_secs(1)).unwrap().rows.is_empty());
        assert!(c.models(Duration::from_secs(1)).unwrap().models[0].active);
        c.select_model("large-v3", Duration::from_secs(1)).unwrap();
    }

    #[test]
    fn lan_addresses_are_not_loopback() {
        for a in lan_addresses() {
            assert!(!a.starts_with("127."), "{a}");
        }
    }
}
