//! LLM endpoint registry: priority order, health, backoff and selection (design §8.1–8.2).

use super::Flavor;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Healthy endpoints are probed again after this long (§8.2: every 10 s).
pub const RECHECK_PERIOD: Duration = Duration::from_secs(10);
/// A successful check stays trusted for this long. Longer than the recheck period, so a probe pass that takes a
/// moment never leaves a fine endpoint looking unhealthy in between.
pub const HEALTH_TTL: Duration = Duration::from_secs(15);

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// Typed in by the user (LAN or a local runtime they added themselves).
    #[default]
    Manual,
    /// One of the probed localhost candidates (§8.1).
    Local,
}

fn default_true() -> bool {
    true
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Endpoint {
    pub id: String,
    pub label: String,
    pub base_url: String,
    /// Model to request; `None` means the first model the server lists.
    pub model: Option<String>,
    pub token: Option<String>,
    pub flavor: Flavor,
    #[serde(default)]
    pub source: Source,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

/// Accepts `host:port`, `http://host:port`, with or without a trailing slash; returns `scheme://host:port`.
pub fn normalize_base_url(input: &str) -> Result<String, String> {
    let s = input.trim().trim_end_matches('/');
    if s.is_empty() {
        return Err("Enter an address like 192.168.1.20:1234".into());
    }
    let (scheme, rest) = match s.split_once("://") {
        Some(("http", r)) => ("http", r),
        Some(("https", r)) => ("https", r),
        Some((other, _)) => return Err(format!("Unsupported scheme {other:?}; use http or https")),
        None => ("http", s),
    };
    let rest = rest.trim_end_matches('/');
    let (host, port) = rest.rsplit_once(':').ok_or_else(|| "Add a port, like 192.168.1.20:1234".to_string())?;
    if host.is_empty() || port.is_empty() || !port.chars().all(|c| c.is_ascii_digit()) {
        return Err(format!("{rest:?} is not host:port"));
    }
    Ok(format!("{scheme}://{host}:{port}"))
}

/// Loopback addresses: an LLM here shares this machine's memory (§9).
pub fn is_local(base_url: &str) -> bool {
    let host = base_url.split("://").nth(1).unwrap_or(base_url);
    let host = host.rsplit_once(':').map(|(h, _)| h).unwrap_or(host);
    matches!(host, "127.0.0.1" | "localhost" | "[::1]" | "::1" | "0.0.0.0")
}

/// FNV-1a over the URL, so an endpoint keeps its id across restarts without storing one.
pub fn endpoint_id(base_url: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in base_url.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("ep-{h:016x}")
}

/// The localhost runtimes probed when auto-detect is on (§8.1), in priority order.
pub fn local_candidates() -> Vec<Endpoint> {
    let mk = |id: &str, label: &str, url: &str, flavor: Flavor| Endpoint {
        id: id.into(),
        label: label.into(),
        base_url: url.into(),
        model: None,
        token: None,
        flavor,
        source: Source::Local,
        enabled: true,
    };
    vec![
        mk("local-ollama", "Ollama (this Mac)", "http://127.0.0.1:11434", Flavor::Ollama),
        mk("local-lmstudio", "LM Studio (this Mac)", "http://127.0.0.1:1234", Flavor::OpenAi),
        mk("local-llamacpp", "llama.cpp server (this Mac)", "http://127.0.0.1:8080", Flavor::OpenAi),
    ]
}

/// User endpoints in their list order (disabled ones included, so the panel can show them), then the local
/// candidates not already present. The registry never selects or probes a disabled endpoint.
pub fn ordered(user: &[Endpoint], auto_local: bool) -> Vec<Endpoint> {
    let mut out: Vec<Endpoint> = user.to_vec();
    if auto_local {
        for c in local_candidates() {
            if !user.iter().any(|u| u.base_url == c.base_url) {
                out.push(c);
            }
        }
    }
    out
}

/// 10 s, 20 s, 40 s, then 60 s for every further failure (§8.2).
pub fn backoff(failures: u32) -> Duration {
    let secs = 10u64.saturating_mul(1u64 << failures.saturating_sub(1).min(6));
    Duration::from_secs(secs.min(60))
}

#[derive(Clone, Debug, Default)]
pub struct Health {
    pub last_ok: Option<Instant>,
    pub last_checked: Option<Instant>,
    pub failures: u32,
    pub next_check: Option<Instant>,
    pub models: Vec<String>,
    pub loaded_models: Vec<String>,
    pub loaded_bytes: u64,
    pub last_error: Option<String>,
}

impl Health {
    pub fn healthy(&self, now: Instant) -> bool {
        self.failures == 0 && self.last_ok.map(|t| now.duration_since(t) <= HEALTH_TTL).unwrap_or(false)
    }
}

pub struct Registry {
    endpoints: Vec<Endpoint>,
    health: HashMap<String, Health>,
}

impl Registry {
    pub fn new(endpoints: Vec<Endpoint>) -> Self {
        Registry { endpoints, health: HashMap::new() }
    }

    /// Swap the endpoint list; health survives for ids that remain.
    pub fn replace(&mut self, endpoints: Vec<Endpoint>) {
        self.health.retain(|id, _| endpoints.iter().any(|e| &e.id == id));
        self.endpoints = endpoints;
    }

    pub fn endpoints(&self) -> &[Endpoint] {
        &self.endpoints
    }

    pub fn health(&self, id: &str) -> Option<&Health> {
        self.health.get(id)
    }

    pub fn mark_ok(&mut self, id: &str, now: Instant, models: Vec<String>, loaded: Vec<String>, loaded_bytes: u64) {
        let h = self.health.entry(id.to_string()).or_default();
        h.last_ok = Some(now);
        h.last_checked = Some(now);
        h.failures = 0;
        h.next_check = Some(now + RECHECK_PERIOD);
        h.models = models;
        h.loaded_models = loaded;
        h.loaded_bytes = loaded_bytes;
        h.last_error = None;
    }

    pub fn mark_failure(&mut self, id: &str, now: Instant, error: String) {
        let h = self.health.entry(id.to_string()).or_default();
        h.last_checked = Some(now);
        h.failures += 1;
        h.next_check = Some(now + backoff(h.failures));
        h.loaded_models.clear();
        h.loaded_bytes = 0;
        h.last_error = Some(error);
    }

    /// Forget resident models after an unload, without touching the health verdict.
    pub fn clear_loaded(&mut self, id: &str) {
        if let Some(h) = self.health.get_mut(id) {
            h.loaded_models.clear();
            h.loaded_bytes = 0;
        }
    }

    /// Never checked, or past its next check time.
    pub fn due_for_check(&self, now: Instant) -> Vec<Endpoint> {
        self.endpoints
            .iter()
            .filter(|e| e.enabled)
            .filter(|e| match self.health.get(&e.id).and_then(|h| h.next_check) {
                None => true,
                Some(t) => t <= now,
            })
            .cloned()
            .collect()
    }

    /// Highest-priority endpoint with a fresh successful check (§8.2 step 1).
    pub fn select(&self, now: Instant) -> Option<Endpoint> {
        self.endpoints.iter().filter(|e| e.enabled).find(|e| self.health.get(&e.id).map(|h| h.healthy(now)).unwrap_or(false)).cloned()
    }

    /// Request-time order: the selected endpoint first, then every other in priority order (failover, §8.2 step 2).
    pub fn candidates(&self, now: Instant) -> Vec<Endpoint> {
        let mut out = Vec::with_capacity(self.endpoints.len());
        if let Some(sel) = self.select(now) {
            out.push(sel);
        }
        for e in self.endpoints.iter().filter(|e| e.enabled) {
            if !out.iter().any(|o| o.id == e.id) {
                out.push(e.clone());
            }
        }
        out
    }

    /// The selected endpoint when it runs on this machine (its model counts in the memory strip).
    pub fn active_local(&self, now: Instant) -> Option<Endpoint> {
        self.select(now).filter(|e| is_local(&e.base_url))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::polish::Flavor;
    use std::time::{Duration, Instant};

    fn lan() -> Endpoint {
        Endpoint { id: "ep-1".into(), label: "PC".into(), base_url: "http://192.168.1.20:1234".into(), model: None, token: None, flavor: Flavor::OpenAi, source: Source::Manual, enabled: true }
    }

    #[test]
    fn normalizes_host_port_forms() {
        assert_eq!(normalize_base_url("192.168.1.20:1234").unwrap(), "http://192.168.1.20:1234");
        assert_eq!(normalize_base_url("http://pc.local:11434/").unwrap(), "http://pc.local:11434");
        assert_eq!(normalize_base_url(" https://pc.local:8443 ").unwrap(), "https://pc.local:8443");
        assert!(normalize_base_url("ftp://x:1").is_err());
        assert!(normalize_base_url("host:abc").is_err());
        assert!(normalize_base_url("").is_err());
        assert!(normalize_base_url("justhost").is_err());
    }

    #[test]
    fn local_detection() {
        assert!(is_local("http://127.0.0.1:11434"));
        assert!(is_local("http://localhost:1234"));
        assert!(is_local("http://[::1]:1234"));
        assert!(!is_local("http://192.168.1.20:1234"));
    }

    #[test]
    fn user_endpoints_come_before_local_candidates() {
        let o = ordered(&[lan()], true);
        assert_eq!(o[0].id, "ep-1");
        assert_eq!(o[1].id, "local-ollama");
        assert_eq!(o[2].id, "local-lmstudio");
        assert_eq!(o[3].id, "local-llamacpp");
        assert_eq!(o.len(), 4);
        assert_eq!(ordered(&[lan()], false).len(), 1);
    }

    #[test]
    fn user_copy_of_a_local_candidate_is_not_duplicated() {
        let mut mine = local_candidates().remove(0);
        mine.model = Some("qwen2.5:3b".into());
        let o = ordered(&[mine], true);
        assert_eq!(o.iter().filter(|e| e.base_url == "http://127.0.0.1:11434").count(), 1);
        assert_eq!(o[0].model.as_deref(), Some("qwen2.5:3b"));
    }

    #[test]
    fn disabled_endpoints_are_listed_but_never_used() {
        let mut e = local_candidates().remove(0);
        e.enabled = false;
        let list = ordered(&[e], false);
        assert_eq!(list.len(), 1, "stays visible so the user can switch it back on");
        let mut r = Registry::new(list);
        let t0 = Instant::now();
        assert!(r.due_for_check(t0).is_empty());
        r.mark_ok("local-ollama", t0, vec![], vec![], 0);
        assert!(r.select(t0).is_none());
        assert!(r.candidates(t0).is_empty());
    }

    #[test]
    fn health_ttl_outlasts_the_recheck_period() {
        assert!(HEALTH_TTL > RECHECK_PERIOD);
    }

    #[test]
    fn endpoint_ids_are_stable_per_url() {
        assert_eq!(endpoint_id("http://a:1"), endpoint_id("http://a:1"));
        assert_ne!(endpoint_id("http://a:1"), endpoint_id("http://a:2"));
        assert!(endpoint_id("http://a:1").starts_with("ep-"));
    }

    #[test]
    fn backoff_doubles_and_caps() {
        assert_eq!(backoff(1).as_secs(), 10);
        assert_eq!(backoff(2).as_secs(), 20);
        assert_eq!(backoff(3).as_secs(), 40);
        assert_eq!(backoff(4).as_secs(), 60);
        assert_eq!(backoff(9).as_secs(), 60);
    }

    #[test]
    fn select_prefers_first_healthy_within_ten_seconds() {
        let mut r = Registry::new(ordered(&[], true));
        let t0 = Instant::now();
        assert!(r.select(t0).is_none());
        r.mark_ok("local-lmstudio", t0, vec!["q".into()], vec![], 0);
        assert_eq!(r.select(t0).unwrap().id, "local-lmstudio");
        r.mark_ok("local-ollama", t0, vec![], vec![], 0);
        assert_eq!(r.select(t0).unwrap().id, "local-ollama");
        assert!(r.select(t0 + HEALTH_TTL + Duration::from_secs(1)).is_none());
    }

    #[test]
    fn failure_marks_unhealthy_and_schedules_backoff() {
        let mut r = Registry::new(local_candidates());
        let t0 = Instant::now();
        r.mark_ok("local-ollama", t0, vec![], vec![], 0);
        r.mark_failure("local-ollama", t0, "timeout".into());
        assert!(r.select(t0).is_none());
        assert!(r.due_for_check(t0).iter().all(|e| e.id != "local-ollama"));
        assert!(r.due_for_check(t0 + Duration::from_secs(10)).iter().any(|e| e.id == "local-ollama"));
        assert_eq!(r.health("local-ollama").unwrap().last_error.as_deref(), Some("timeout"));
        r.mark_ok("local-ollama", t0 + Duration::from_secs(10), vec![], vec![], 0);
        assert_eq!(r.select(t0 + Duration::from_secs(10)).unwrap().id, "local-ollama");
        assert_eq!(r.health("local-ollama").unwrap().failures, 0);
    }

    #[test]
    fn healthy_endpoints_are_rechecked_every_ten_seconds() {
        let mut r = Registry::new(local_candidates());
        let t0 = Instant::now();
        assert_eq!(r.due_for_check(t0).len(), 3);
        r.mark_ok("local-ollama", t0, vec![], vec![], 0);
        assert!(r.due_for_check(t0 + Duration::from_secs(5)).iter().all(|e| e.id != "local-ollama"));
        assert!(r.due_for_check(t0 + Duration::from_secs(10)).iter().any(|e| e.id == "local-ollama"));
    }

    #[test]
    fn replace_keeps_health_of_surviving_ids() {
        let mut r = Registry::new(local_candidates());
        let t0 = Instant::now();
        r.mark_ok("local-ollama", t0, vec!["m".into()], vec![], 0);
        r.replace(vec![local_candidates().remove(0)]);
        assert_eq!(r.health("local-ollama").unwrap().models, vec!["m".to_string()]);
        assert!(r.health("local-lmstudio").is_none());
        assert_eq!(r.endpoints().len(), 1);
    }

    #[test]
    fn candidates_put_selected_first_then_the_rest() {
        let mut r = Registry::new(local_candidates());
        let t0 = Instant::now();
        r.mark_ok("local-lmstudio", t0, vec![], vec![], 0);
        let c = r.candidates(t0);
        assert_eq!(c[0].id, "local-lmstudio");
        assert_eq!(c.len(), 3);
        assert_eq!(c[1].id, "local-ollama");
    }

    #[test]
    fn active_local_only_when_selected_is_local() {
        let mut r = Registry::new(ordered(&[lan()], true));
        let t0 = Instant::now();
        r.mark_ok("ep-1", t0, vec![], vec![], 0);
        assert!(r.active_local(t0).is_none());
        r.mark_failure("ep-1", t0, "x".into());
        r.mark_ok("local-ollama", t0, vec![], vec!["m".into()], 1_000);
        assert_eq!(r.active_local(t0).unwrap().id, "local-ollama");
        assert_eq!(r.health("local-ollama").unwrap().loaded_bytes, 1_000);
    }

    #[test]
    fn settings_defaults_for_endpoints() {
        let s = crate::settings::Settings::default();
        assert!(s.llm_endpoints.is_empty());
        assert!(s.auto_detect_local_llm);
    }
}
