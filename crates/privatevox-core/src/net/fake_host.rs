//! An in-process stand-in for a PrivateVox host, for client, routing and pipeline tests.

use super::minihttp::{read_request, respond};
use super::protocol::*;
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone, Debug)]
pub enum HostBehaviour {
    /// Answers every route; `text` for dictations, `polished` for polish; requires `token` when set.
    Ok { text: String, polished: String, token: Option<String> },
    /// Accepts and never replies.
    Hang,
    /// 500 with `{"error":"boom"}` on every route.
    ServerError,
}

pub struct FakeHost {
    pub base_url: String,
    /// "METHOD path" per request.
    pub hits: Arc<Mutex<Vec<String>>>,
    /// Every dictation request received, in order.
    pub requests: Arc<Mutex<Vec<DictationRequest>>>,
    /// Every polish request received, in order.
    pub polishes: Arc<Mutex<Vec<PolishRequest>>>,
    port: u16,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl FakeHost {
    pub fn start(behaviour: HostBehaviour) -> FakeHost {
        Self::start_on(behaviour, 0)
    }

    pub fn start_on(behaviour: HostBehaviour, port: u16) -> FakeHost {
        let listener = TcpListener::bind(("127.0.0.1", port)).expect("bind fake host");
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let hits: Arc<Mutex<Vec<String>>> = Default::default();
        let requests: Arc<Mutex<Vec<DictationRequest>>> = Default::default();
        let polishes: Arc<Mutex<Vec<PolishRequest>>> = Default::default();
        let stop = Arc::new(AtomicBool::new(false));
        let (h2, r2, p2, s2) = (hits.clone(), requests.clone(), polishes.clone(), stop.clone());
        let handle = std::thread::spawn(move || {
            while !s2.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let (b, h, r, p, st) = (behaviour.clone(), h2.clone(), r2.clone(), p2.clone(), s2.clone());
                        std::thread::spawn(move || {
                            let Some(req) = read_request(&mut stream) else { return };
                            h.lock().unwrap().push(format!("{} {}", req.method, req.path));
                            match b {
                                HostBehaviour::Hang => {
                                    while !st.load(Ordering::SeqCst) {
                                        std::thread::sleep(Duration::from_millis(10));
                                    }
                                }
                                HostBehaviour::ServerError => respond(&mut stream, "500 Internal Server Error", r#"{"error":"boom"}"#),
                                HostBehaviour::Ok { text, polished, token } => {
                                    if let Some(t) = token {
                                        if req.header("authorization") != Some(&format!("Bearer {t}")) {
                                            respond(&mut stream, "401 Unauthorized", r#"{"error":"token required"}"#);
                                            return;
                                        }
                                    }
                                    let path = req.path.split('?').next().unwrap_or("");
                                    let body = match (req.method.as_str(), path) {
                                        ("GET", "/v1/status") => serde_json::to_string(&StatusReply {
                                            name: "Fake PC".into(),
                                            version: "0.3.0".into(),
                                            platform: "windows".into(),
                                            gpu: GpuInfo { name: "Fake RX 9070".into(), total_bytes: 16_000_000_000, used_bytes: 5_000_000_000 },
                                            whisper: WhisperInfo { model: "large-v3-turbo".into(), loaded: true, resident_bytes: 1_300_000_000 },
                                            polish: PolishInfo { enabled: true, endpoint_label: Some("LM Studio".into()), model: Some("qwen2.5-7b".into()), resident_bytes: 2_000_000_000 },
                                            clients: 1,
                                        })
                                        .unwrap(),
                                        ("POST", "/v1/dictations") => match serde_json::from_str::<DictationRequest>(&req.body) {
                                            Ok(d) => {
                                                r.lock().unwrap().push(d);
                                                serde_json::to_string(&DictationReply { text: text.clone(), language: Some("en".into()), model: "large-v3-turbo".into(), decode_ms: 7 }).unwrap()
                                            }
                                            Err(e) => {
                                                respond(&mut stream, "400 Bad Request", &serde_json::to_string(&ErrorReply { error: e.to_string() }).unwrap());
                                                return;
                                            }
                                        },
                                        ("POST", "/v1/polish") => match serde_json::from_str::<PolishRequest>(&req.body) {
                                            Ok(pr) => {
                                                p.lock().unwrap().push(pr);
                                                serde_json::to_string(&PolishReply { text: polished.clone(), polish_status: "ok".into(), polished_by: Some("pc-llm".into()), ms: 5 }).unwrap()
                                            }
                                            Err(e) => {
                                                respond(&mut stream, "400 Bad Request", &serde_json::to_string(&ErrorReply { error: e.to_string() }).unwrap());
                                                return;
                                            }
                                        },
                                        ("GET", "/v1/history") => serde_json::to_string(&HistoryReply {
                                            rows: vec![
                                                RemoteRow { id: 2, created_at_ms: 2, app_name: Some("Mac".into()), mode: "clean".into(), model: "large-v3-turbo".into(), duration_ms: 1500, raw_text: "second".into(), polished_text: Some("Second.".into()), polish_status: "ok".into(), engine_kind: "host".into() },
                                                RemoteRow { id: 1, created_at_ms: 1, app_name: Some("Mac".into()), mode: "raw".into(), model: "large-v3-turbo".into(), duration_ms: 900, raw_text: "first".into(), polished_text: None, polish_status: "none".into(), engine_kind: "host".into() },
                                            ],
                                            total: 2,
                                        })
                                        .unwrap(),
                                        ("GET", "/v1/models") => serde_json::to_string(&ModelsReply {
                                            models: vec![
                                                RemoteModel { id: "large-v3-turbo".into(), label: "turbo".into(), downloaded: true, active: true },
                                                RemoteModel { id: "large-v3".into(), label: "large-v3".into(), downloaded: true, active: false },
                                            ],
                                        })
                                        .unwrap(),
                                        ("POST", "/v1/models/select") => "{}".into(),
                                        _ => {
                                            respond(&mut stream, "404 Not Found", r#"{"error":"not found"}"#);
                                            return;
                                        }
                                    };
                                    respond(&mut stream, "200 OK", &body);
                                }
                            }
                        });
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(5)),
                    Err(_) => break,
                }
            }
        });
        FakeHost { base_url: format!("http://127.0.0.1:{port}"), hits, requests, polishes, port, stop, handle: Some(handle) }
    }

    pub fn port(&self) -> u16 {
        self.port
    }
}

impl Drop for FakeHost {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}
