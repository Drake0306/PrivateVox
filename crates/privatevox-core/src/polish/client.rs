//! Blocking HTTP client for LLM endpoints (design §7, §8.2).
//!
//! Blocking on purpose: callers are the pipeline thread, the health thread and command handlers, none of which
//! run inside an async runtime. Never call these from a tokio worker thread.

use super::request::{chat_body, chat_path, clean_output, parse_chat_reply, unload_body, warm_body};
use super::{Endpoint, Flavor};
use serde_json::Value;
use std::time::Duration;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Probe {
    /// Models the server offers.
    pub models: Vec<String>,
    /// Models currently resident in memory (Ollama `/api/ps`, LM Studio `/api/v0/models`).
    pub loaded_models: Vec<String>,
    /// Bytes those resident models take, when the runtime reports it (Ollama only), else 0.
    pub loaded_bytes: u64,
}

pub trait LlmClient: Send + Sync {
    fn probe(&self, ep: &Endpoint, timeout: Duration) -> Result<Probe, String>;
    fn chat(&self, ep: &Endpoint, model: &str, system: &str, user: &str, max_tokens: u32, timeout: Duration) -> Result<String, String>;
    fn warm(&self, ep: &Endpoint, model: &str) -> Result<(), String>;
    fn unload(&self, ep: &Endpoint, model: &str) -> Result<(), String>;
}

pub struct HttpLlmClient {
    http: reqwest::blocking::Client,
}

impl Default for HttpLlmClient {
    fn default() -> Self {
        Self::new()
    }
}

/// A warm-up that takes longer than this is a runtime in trouble; the polish path has its own grace period.
const WARM_TIMEOUT: Duration = Duration::from_secs(30);

/// `reqwest::blocking` must never run on a tokio worker (it parks the worker and panics in debug builds).
fn assert_not_on_async_runtime() {
    debug_assert!(tokio::runtime::Handle::try_current().is_err(), "blocking LLM client called from inside an async runtime; use spawn_blocking or a std thread");
}

impl HttpLlmClient {
    pub fn new() -> Self {
        let http = reqwest::blocking::Client::builder().connect_timeout(Duration::from_secs(1)).build().expect("reqwest client");
        HttpLlmClient { http }
    }

    fn get(&self, ep: &Endpoint, path: &str, timeout: Duration) -> Result<Value, String> {
        assert_not_on_async_runtime();
        let mut req = self.http.get(format!("{}{}", ep.base_url, path)).timeout(timeout);
        if let Some(t) = &ep.token {
            req = req.bearer_auth(t);
        }
        let resp = req.send().map_err(|e| describe(&e))?;
        let status = resp.status();
        let body: Value = resp.json().map_err(|_| format!("{path}: reply was not JSON"))?;
        if !status.is_success() {
            return Err(format!("{path}: HTTP {}", status.as_u16()));
        }
        Ok(body)
    }

    fn post(&self, ep: &Endpoint, path: &str, body: &Value, timeout: Duration) -> Result<Value, String> {
        assert_not_on_async_runtime();
        let mut req = self.http.post(format!("{}{}", ep.base_url, path)).timeout(timeout).json(body);
        if let Some(t) = &ep.token {
            req = req.bearer_auth(t);
        }
        let resp = req.send().map_err(|e| describe(&e))?;
        let status = resp.status();
        let text = resp.text().map_err(|e| describe(&e))?;
        let json: Value = serde_json::from_str(&text).map_err(|_| format!("{path}: HTTP {} with a non-JSON reply", status.as_u16()))?;
        if !status.is_success() {
            let msg = json.pointer("/error/message").and_then(|m| m.as_str()).or_else(|| json.get("error").and_then(|e| e.as_str())).unwrap_or("");
            return Err(format!("{path}: HTTP {} {msg}", status.as_u16()).trim_end().to_string());
        }
        Ok(json)
    }
}

fn describe(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        "timed out".into()
    } else if e.is_connect() {
        "connection refused".into()
    } else {
        e.to_string()
    }
}

fn names(v: &Value, list: &str, field: &str) -> Vec<String> {
    v.get(list).and_then(|m| m.as_array()).map(|a| a.iter().filter_map(|m| m.get(field).and_then(|n| n.as_str()).map(|s| s.to_string())).collect()).unwrap_or_default()
}

impl LlmClient for HttpLlmClient {
    fn probe(&self, ep: &Endpoint, timeout: Duration) -> Result<Probe, String> {
        match ep.flavor {
            Flavor::Ollama => {
                let tags = self.get(ep, "/api/tags", timeout)?;
                let models = names(&tags, "models", "name");
                // Resident models are informational; a failing /api/ps must not mark the endpoint unhealthy.
                let (loaded_models, loaded_bytes) = match self.get(ep, "/api/ps", timeout) {
                    Ok(ps) => {
                        let bytes = ps.get("models").and_then(|m| m.as_array()).map(|a| a.iter().map(|m| m.get("size_vram").and_then(|v| v.as_u64()).filter(|v| *v > 0).or_else(|| m.get("size").and_then(|v| v.as_u64())).unwrap_or(0)).sum()).unwrap_or(0);
                        (names(&ps, "models", "name"), bytes)
                    }
                    Err(_) => (Vec::new(), 0),
                };
                Ok(Probe { models, loaded_models, loaded_bytes })
            }
            Flavor::OpenAi => {
                let list = self.get(ep, "/v1/models", timeout)?;
                let models = names(&list, "data", "id");
                // LM Studio reports load state on its own API; other servers 404 here and we move on.
                let loaded_models = match self.get(ep, "/api/v0/models", timeout) {
                    Ok(v) => v.get("data").and_then(|d| d.as_array()).map(|a| a.iter().filter(|m| m.get("state").and_then(|s| s.as_str()) == Some("loaded")).filter_map(|m| m.get("id").and_then(|i| i.as_str()).map(|s| s.to_string())).collect()).unwrap_or_default(),
                    Err(_) => Vec::new(),
                };
                Ok(Probe { models, loaded_models, loaded_bytes: 0 })
            }
        }
    }

    fn chat(&self, ep: &Endpoint, model: &str, system: &str, user: &str, max_tokens: u32, timeout: Duration) -> Result<String, String> {
        let body = chat_body(ep.flavor, model, system, user, max_tokens);
        let reply = self.post(ep, chat_path(ep.flavor), &body, timeout)?;
        let text = parse_chat_reply(ep.flavor, &reply)?;
        Ok(clean_output(&text))
    }

    fn warm(&self, ep: &Endpoint, model: &str) -> Result<(), String> {
        match warm_body(ep.flavor, model) {
            Some(body) => self.post(ep, "/api/generate", &body, WARM_TIMEOUT).map(|_| ()),
            None => {
                let body = chat_body(ep.flavor, model, "Reply with one word.", "ok", 1);
                self.post(ep, chat_path(ep.flavor), &body, WARM_TIMEOUT).map(|_| ())
            }
        }
    }

    fn unload(&self, ep: &Endpoint, model: &str) -> Result<(), String> {
        match ep.flavor {
            Flavor::Ollama => self.post(ep, "/api/generate", &unload_body(model), Duration::from_secs(10)).map(|_| ()),
            Flavor::OpenAi => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::polish::fake_server::{Behaviour, FakeLlm};
    use crate::polish::{local_candidates, Endpoint, Flavor};
    use std::time::{Duration, Instant};

    #[test]
    fn probes_ollama_models_and_loaded_bytes() {
        let s = FakeLlm::start(Behaviour::Ollama { reply: "x".into(), loaded_bytes: 2_000_000_000 });
        let p = HttpLlmClient::new().probe(&s.endpoint(Flavor::Ollama), Duration::from_secs(1)).unwrap();
        assert_eq!(p.models, vec!["qwen2.5:3b".to_string()]);
        assert_eq!(p.loaded_models, vec!["qwen2.5:3b".to_string()]);
        assert_eq!(p.loaded_bytes, 2_000_000_000);
    }

    #[test]
    fn probes_openai_models() {
        let s = FakeLlm::start(Behaviour::OpenAi { reply: "x".into() });
        let p = HttpLlmClient::new().probe(&s.endpoint(Flavor::OpenAi), Duration::from_secs(1)).unwrap();
        assert_eq!(p.models, vec!["qwen2.5-7b-instruct".to_string()]);
        assert_eq!(p.loaded_bytes, 0);
    }

    #[test]
    fn chat_returns_cleaned_content_for_both_flavors() {
        for (b, f) in [(Behaviour::Ollama { reply: "\"Hello.\"".into(), loaded_bytes: 0 }, Flavor::Ollama), (Behaviour::OpenAi { reply: "Hello.".into() }, Flavor::OpenAi)] {
            let s = FakeLlm::start(b);
            assert_eq!(HttpLlmClient::new().chat(&s.endpoint(f), "m", "sys", "hi", 64, Duration::from_secs(2)).unwrap(), "Hello.");
        }
    }

    #[test]
    fn ollama_chat_sends_keep_alive_and_native_path() {
        let s = FakeLlm::start(Behaviour::Ollama { reply: "x".into(), loaded_bytes: 0 });
        HttpLlmClient::new().chat(&s.endpoint(Flavor::Ollama), "m", "sys", "hi", 64, Duration::from_secs(2)).unwrap();
        assert!(s.hits.lock().unwrap().iter().any(|h| h == "POST /api/chat"));
        assert!(s.last_body().contains("\"num_predict\":64"));
        assert!(s.last_body().contains("\"keep_alive\":-1"));
    }

    #[test]
    fn bearer_token_is_sent_when_set() {
        let s = FakeLlm::start(Behaviour::OpenAi { reply: "x".into() });
        let mut ep = s.endpoint(Flavor::OpenAi);
        ep.token = Some("secret".into());
        HttpLlmClient::new().probe(&ep, Duration::from_secs(1)).unwrap();
        assert_eq!(s.last_auth().as_deref(), Some("Bearer secret"));
    }

    #[test]
    fn server_error_and_bad_json_are_errors() {
        for b in [Behaviour::ServerError, Behaviour::BadJson] {
            let s = FakeLlm::start(b);
            assert!(HttpLlmClient::new().chat(&s.endpoint(Flavor::OpenAi), "m", "s", "u", 64, Duration::from_secs(2)).is_err());
        }
    }

    #[test]
    fn hang_times_out_within_budget() {
        let s = FakeLlm::start(Behaviour::Hang);
        let t = Instant::now();
        assert!(HttpLlmClient::new().chat(&s.endpoint(Flavor::OpenAi), "m", "s", "u", 64, Duration::from_millis(300)).is_err());
        assert!(t.elapsed() < Duration::from_millis(1500), "took {:?}", t.elapsed());
    }

    #[test]
    fn refused_connection_fails_fast() {
        let ep = Endpoint { base_url: "http://127.0.0.1:1".into(), ..local_candidates().remove(0) };
        let t = Instant::now();
        assert!(HttpLlmClient::new().probe(&ep, Duration::from_secs(1)).is_err());
        assert!(t.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn warm_and_unload_hit_ollama_generate() {
        let s = FakeLlm::start(Behaviour::Ollama { reply: "x".into(), loaded_bytes: 0 });
        let c = HttpLlmClient::new();
        c.warm(&s.endpoint(Flavor::Ollama), "m").unwrap();
        assert!(s.last_body().contains("\"keep_alive\":-1"));
        c.unload(&s.endpoint(Flavor::Ollama), "m").unwrap();
        let hits = s.hits.lock().unwrap().clone();
        assert_eq!(hits.iter().filter(|h| *h == "POST /api/generate").count(), 2);
        assert!(s.last_body().contains("\"keep_alive\":0"));
    }

    #[test]
    fn openai_warm_is_a_one_token_chat_and_unload_is_a_noop() {
        let s = FakeLlm::start(Behaviour::OpenAi { reply: "x".into() });
        let c = HttpLlmClient::new();
        c.warm(&s.endpoint(Flavor::OpenAi), "m").unwrap();
        assert!(s.hits.lock().unwrap().iter().any(|h| h == "POST /v1/chat/completions"));
        assert!(s.last_body().contains("\"max_tokens\":1"));
        c.unload(&s.endpoint(Flavor::OpenAi), "m").unwrap();
        assert_eq!(s.hits.lock().unwrap().len(), 1);
    }

    #[test]
    #[should_panic(expected = "blocking LLM client")]
    fn blocking_client_refuses_to_run_on_a_tokio_worker() {
        let s = FakeLlm::start(Behaviour::OpenAi { reply: "x".into() });
        let ep = s.endpoint(Flavor::OpenAi);
        let client = HttpLlmClient::new();
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        rt.block_on(async move {
            let _ = client.probe(&ep, Duration::from_secs(1));
        });
    }
}
