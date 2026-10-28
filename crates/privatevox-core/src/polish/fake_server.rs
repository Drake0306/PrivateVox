//! A tiny in-process HTTP server that imitates Ollama and OpenAI-compatible runtimes for tests.

use super::{endpoint_id, Endpoint, Flavor, Source};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone, Debug)]
pub enum Behaviour {
    /// Ollama: `/api/tags`, `/api/ps` (reports `loaded_bytes` when non-zero), `/api/chat`, `/api/generate`.
    Ollama { reply: String, loaded_bytes: u64 },
    /// OpenAI-compatible: `/v1/models`, `/v1/chat/completions`.
    OpenAi { reply: String },
    /// Every request answers 200 with a body that is not JSON.
    BadJson,
    /// Every request answers 500 with an error object.
    ServerError,
    /// Accepts the request and never replies.
    Hang,
}

pub struct FakeLlm {
    pub base_url: String,
    /// "METHOD path" per request, in order.
    pub hits: Arc<Mutex<Vec<String>>>,
    bodies: Arc<Mutex<Vec<String>>>,
    auth: Arc<Mutex<Option<String>>>,
    port: u16,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl FakeLlm {
    pub fn start(behaviour: Behaviour) -> FakeLlm {
        Self::start_on(behaviour, 0)
    }

    /// Bind a specific port (0 = any free port), so a test can "restart" a server at the same address.
    pub fn start_on(behaviour: Behaviour, port: u16) -> FakeLlm {
        let listener = TcpListener::bind(("127.0.0.1", port)).expect("bind fake llm");
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let hits = Arc::new(Mutex::new(Vec::new()));
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let auth = Arc::new(Mutex::new(None));
        let (h2, b2, s2, a2) = (hits.clone(), bodies.clone(), stop.clone(), auth.clone());
        let handle = std::thread::spawn(move || {
            while !s2.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let (b, h, bo, st, au) = (behaviour.clone(), h2.clone(), b2.clone(), s2.clone(), a2.clone());
                        std::thread::spawn(move || serve(stream, b, h, bo, au, st));
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(5)),
                    Err(_) => break,
                }
            }
        });
        FakeLlm { base_url: format!("http://127.0.0.1:{port}"), hits, bodies, auth, port, stop, handle: Some(handle) }
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// The `Authorization` header of the last request, if any.
    pub fn last_auth(&self) -> Option<String> {
        self.auth.lock().unwrap().clone()
    }

    pub fn last_body(&self) -> String {
        self.bodies.lock().unwrap().last().cloned().unwrap_or_default()
    }

    pub fn endpoint(&self, flavor: Flavor) -> Endpoint {
        Endpoint { id: endpoint_id(&self.base_url), label: "fake".into(), base_url: self.base_url.clone(), model: None, token: None, flavor, source: Source::Manual, enabled: true }
    }
}

impl Drop for FakeLlm {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn respond(stream: &mut TcpStream, status: &str, body: &str) {
    let _ = write!(stream, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
    let _ = stream.flush();
}

fn serve(mut stream: TcpStream, behaviour: Behaviour, hits: Arc<Mutex<Vec<String>>>, bodies: Arc<Mutex<Vec<String>>>, auth: Arc<Mutex<Option<String>>>, stop: Arc<AtomicBool>) {
    // On macOS an accepted socket inherits the listener's non-blocking flag; reads must block here.
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        match stream.read(&mut chunk) {
            Ok(0) => return,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    break i + 4;
                }
            }
            Err(_) => return,
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = head.lines();
    let request_line = lines.next().unwrap_or_default().to_string();
    *auth.lock().unwrap() = head.lines().filter_map(|l| l.split_once(':')).find(|(k, _)| k.eq_ignore_ascii_case("authorization")).map(|(_, v)| v.trim().to_string());
    let content_length: usize = lines
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse().ok())
        .unwrap_or(0);
    while buf.len() < head_end + content_length {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
    let body = String::from_utf8_lossy(&buf[head_end..(head_end + content_length).min(buf.len())]).to_string();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or_default().to_string();
    hits.lock().unwrap().push(format!("{method} {path}"));
    bodies.lock().unwrap().push(body);

    match behaviour {
        Behaviour::Hang => {
            while !stop.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        Behaviour::BadJson => respond(&mut stream, "200 OK", "not json{"),
        Behaviour::ServerError => respond(&mut stream, "500 Internal Server Error", r#"{"error":{"message":"boom"}}"#),
        Behaviour::Ollama { reply, loaded_bytes } => match (method.as_str(), path.as_str()) {
            ("GET", "/api/tags") => respond(&mut stream, "200 OK", r#"{"models":[{"name":"qwen2.5:3b","size":1900000000}]}"#),
            ("GET", "/api/ps") => {
                let b = if loaded_bytes > 0 { format!(r#"{{"models":[{{"name":"qwen2.5:3b","size":{loaded_bytes},"size_vram":{loaded_bytes}}}]}}"#) } else { r#"{"models":[]}"#.to_string() };
                respond(&mut stream, "200 OK", &b)
            }
            ("GET", "/v1/models") => respond(&mut stream, "200 OK", r#"{"data":[{"id":"qwen2.5:3b"}]}"#),
            ("POST", "/api/chat") => {
                let b = serde_json::json!({ "model": "qwen2.5:3b", "message": { "role": "assistant", "content": reply }, "done": true });
                respond(&mut stream, "200 OK", &b.to_string())
            }
            ("POST", "/api/generate") => respond(&mut stream, "200 OK", r#"{"done":true}"#),
            _ => respond(&mut stream, "404 Not Found", r#"{"error":"not found"}"#),
        },
        Behaviour::OpenAi { reply } => match (method.as_str(), path.as_str()) {
            ("GET", "/v1/models") => respond(&mut stream, "200 OK", r#"{"data":[{"id":"qwen2.5-7b-instruct","object":"model"}]}"#),
            ("POST", "/v1/chat/completions") => {
                let b = serde_json::json!({ "choices": [{ "index": 0, "message": { "role": "assistant", "content": reply }, "finish_reason": "stop" }] });
                respond(&mut stream, "200 OK", &b.to_string())
            }
            _ => respond(&mut stream, "404 Not Found", r#"{"error":{"message":"not found"}}"#),
        },
    }
}
