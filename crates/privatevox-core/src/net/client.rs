//! Blocking client for a PrivateVox host (design §8.6). Same rule as the LLM client: never on an async runtime thread.

use super::protocol::*;
use std::fmt;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq)]
pub enum ClientError {
    Unreachable(String),
    Timeout,
    Unauthorized,
    /// A 4xx/5xx with the host's own message.
    Server(String),
    BadReply(String),
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ClientError::Unreachable(e) => write!(f, "unreachable: {e}"),
            ClientError::Timeout => write!(f, "timed out"),
            ClientError::Unauthorized => write!(f, "token rejected"),
            ClientError::Server(m) => write!(f, "host error: {m}"),
            ClientError::BadReply(m) => write!(f, "bad reply: {m}"),
        }
    }
}

impl std::error::Error for ClientError {}

pub struct HostClient {
    base_url: String,
    token: Option<String>,
    http: reqwest::blocking::Client,
}

/// `reqwest::blocking` parks the thread; on a tokio worker that is a deadlock in release and a panic in debug.
pub(crate) fn assert_blocking_context() {
    debug_assert!(tokio::runtime::Handle::try_current().is_err(), "blocking host client called from inside an async runtime; use spawn_blocking or a std thread");
}

impl HostClient {
    pub fn new(base_url: &str, token: Option<String>) -> Self {
        let http = reqwest::blocking::Client::builder().connect_timeout(Duration::from_secs(1)).build().expect("reqwest client");
        HostClient { base_url: base_url.trim_end_matches('/').to_string(), token: token.filter(|t| !t.trim().is_empty()), http }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    fn send(&self, req: reqwest::blocking::RequestBuilder) -> Result<String, ClientError> {
        assert_blocking_context();
        let req = match &self.token {
            Some(t) => req.bearer_auth(t),
            None => req,
        };
        let resp = req.send().map_err(|e| if e.is_timeout() { ClientError::Timeout } else { ClientError::Unreachable(if e.is_connect() { "connection refused".into() } else { e.to_string() }) })?;
        let status = resp.status();
        let text = resp.text().map_err(|e| if e.is_timeout() { ClientError::Timeout } else { ClientError::BadReply(e.to_string()) })?;
        if status.as_u16() == 401 {
            return Err(ClientError::Unauthorized);
        }
        if !status.is_success() {
            let msg = serde_json::from_str::<ErrorReply>(&text).map(|e| e.error).unwrap_or_else(|_| format!("HTTP {}", status.as_u16()));
            return Err(ClientError::Server(msg));
        }
        Ok(text)
    }

    fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str, timeout: Duration) -> Result<T, ClientError> {
        let text = self.send(self.http.get(format!("{}{}", self.base_url, path)).timeout(timeout))?;
        serde_json::from_str(&text).map_err(|e| ClientError::BadReply(format!("{path}: {e}")))
    }

    fn post_json<B: serde::Serialize, T: serde::de::DeserializeOwned>(&self, path: &str, body: &B, timeout: Duration) -> Result<T, ClientError> {
        let text = self.send(self.http.post(format!("{}{}", self.base_url, path)).timeout(timeout).json(body))?;
        serde_json::from_str(&text).map_err(|e| ClientError::BadReply(format!("{path}: {e}")))
    }

    pub fn status(&self, timeout: Duration) -> Result<StatusReply, ClientError> {
        self.get_json("/v1/status", timeout)
    }

    pub fn dictation(&self, req: &DictationRequest, timeout: Duration) -> Result<DictationReply, ClientError> {
        self.post_json("/v1/dictations", req, timeout)
    }

    pub fn polish(&self, req: &PolishRequest, timeout: Duration) -> Result<PolishReply, ClientError> {
        self.post_json("/v1/polish", req, timeout)
    }

    pub fn history(&self, limit: u32, offset: u32, search: Option<&str>, timeout: Duration) -> Result<HistoryReply, ClientError> {
        let mut path = format!("/v1/history?limit={limit}&offset={offset}");
        if let Some(q) = search.map(str::trim).filter(|q| !q.is_empty()) {
            path.push_str("&search=");
            path.push_str(&percent_encode(q));
        }
        self.get_json(&path, timeout)
    }

    pub fn models(&self, timeout: Duration) -> Result<ModelsReply, ClientError> {
        self.get_json("/v1/models", timeout)
    }

    pub fn select_model(&self, id: &str, timeout: Duration) -> Result<(), ClientError> {
        let _: serde_json::Value = self.post_json("/v1/models/select", &SelectModelRequest { id: id.to_string() }, timeout)?;
        Ok(())
    }
}

fn percent_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::fake_host::{FakeHost, HostBehaviour};
    use crate::net::protocol::{encode_pcm, DictationRequest};
    use std::time::Duration;

    fn ok_host() -> FakeHost {
        FakeHost::start(HostBehaviour::Ok { text: "hello there".into(), polished: "Hello there.".into(), token: None })
    }

    #[test]
    fn status_round_trip() {
        let h = ok_host();
        let s = HostClient::new(&h.base_url, None).status(Duration::from_secs(1)).unwrap();
        assert_eq!(s.gpu.name, "Fake RX 9070");
        assert_eq!(s.gpu.total_bytes, 16_000_000_000);
        assert!(h.hits.lock().unwrap().contains(&"GET /v1/status".to_string()));
    }

    #[test]
    fn dictation_sends_pcm_and_returns_text() {
        let h = ok_host();
        let req = DictationRequest { client: "Mac".into(), session: "s".into(), index: 0, final_: true, pcm: encode_pcm(&[0.1; 1600]), language: Some("en".into()), translate: false, mode: "clean".into(), polish: false };
        let r = HostClient::new(&h.base_url, None).dictation(&req, Duration::from_secs(2)).unwrap();
        assert_eq!(r.text, "hello there");
        let got = h.requests.lock().unwrap()[0].clone();
        assert_eq!(got.index, 0);
        assert!(got.final_);
        assert_eq!(got.pcm, req.pcm);
    }

    #[test]
    fn polish_round_trip() {
        let h = ok_host();
        let r = HostClient::new(&h.base_url, None).polish(&PolishRequest { client: "Mac".into(), session: "s".into(), mode: "clean".into(), text: "hello there".into() }, Duration::from_secs(2)).unwrap();
        assert_eq!(r.text, "Hello there.");
        assert_eq!(r.polish_status, "ok");
        assert_eq!(h.polishes.lock().unwrap()[0].text, "hello there");
    }

    #[test]
    fn token_mismatch_is_unauthorized() {
        let h = FakeHost::start(HostBehaviour::Ok { text: "".into(), polished: "".into(), token: Some("s3cret".into()) });
        assert_eq!(HostClient::new(&h.base_url, None).status(Duration::from_secs(1)).unwrap_err(), ClientError::Unauthorized);
        assert_eq!(HostClient::new(&h.base_url, Some("wrong".into())).status(Duration::from_secs(1)).unwrap_err(), ClientError::Unauthorized);
        assert!(HostClient::new(&h.base_url, Some("s3cret".into())).status(Duration::from_secs(1)).is_ok());
    }

    #[test]
    fn hang_times_out_and_refused_is_unreachable() {
        let h = FakeHost::start(HostBehaviour::Hang);
        assert_eq!(HostClient::new(&h.base_url, None).status(Duration::from_millis(300)).unwrap_err(), ClientError::Timeout);
        assert!(matches!(HostClient::new("http://127.0.0.1:1", None).status(Duration::from_secs(1)).unwrap_err(), ClientError::Unreachable(_)));
    }

    #[test]
    fn server_error_carries_the_message() {
        let h = FakeHost::start(HostBehaviour::ServerError);
        assert!(matches!(HostClient::new(&h.base_url, None).status(Duration::from_secs(1)).unwrap_err(), ClientError::Server(m) if m.contains("boom")));
    }

    #[test]
    fn history_and_models_parse() {
        let h = ok_host();
        let c = HostClient::new(&h.base_url, None);
        let hist = c.history(10, 0, None, Duration::from_secs(1)).unwrap();
        assert_eq!(hist.rows.len(), 2);
        assert_eq!(hist.total, 2);
        c.history(10, 0, Some("sec ond"), Duration::from_secs(1)).unwrap();
        assert!(h.hits.lock().unwrap().iter().any(|x| x.contains("search=sec%20ond")), "{:?}", h.hits.lock().unwrap());
        assert!(c.models(Duration::from_secs(1)).unwrap().models.iter().any(|m| m.active));
        c.select_model("large-v3", Duration::from_secs(1)).unwrap();
        assert!(h.hits.lock().unwrap().iter().any(|x| x == "POST /v1/models/select"));
    }

    #[test]
    fn error_display_is_human() {
        assert_eq!(ClientError::Timeout.to_string(), "timed out");
        assert_eq!(ClientError::Unauthorized.to_string(), "token rejected");
        assert_eq!(ClientError::Unreachable("connection refused".into()).to_string(), "unreachable: connection refused");
    }
}
