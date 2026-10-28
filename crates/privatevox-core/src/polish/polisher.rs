//! The polish step: mode prompt + endpoint registry + client, with failover, fail-back and the raw fallback (design §4.3 step 5, §8.2).

use super::client::LlmClient;
use super::endpoints::{Endpoint, Health, Registry};
use super::modes::{effective_prompt, Mode};
use super::request::max_tokens_for_mode;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// While a warm-up request is still loading a cold model, a polish may wait this long instead of the configured timeout.
pub const COLD_START_GRACE: Duration = Duration::from_secs(25);

/// Health checks use this timeout (§8.2: 1 s).
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(1);
/// Chat requests get at least this much of the budget; anything less is not worth a round trip.
const MIN_CHAT_BUDGET: Duration = Duration::from_millis(150);

/// Pill notice when the preferred endpoint was skipped and another answered (§8.2 step 2).
pub fn failover_notice(preferred: &Endpoint, used: &Endpoint) -> String {
    if !super::is_local(&preferred.base_url) && super::is_local(&used.base_url) {
        "PC unreachable, polished locally".to_string()
    } else {
        format!("{} unreachable, polished with {}", preferred.label, used.label)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PolishStatus {
    /// Raw or Translate mode, or polish switched off: Whisper's text is pasted as is.
    NotNeeded,
    Polished { endpoint_id: String, model: String, ms: u64 },
    /// Every endpoint failed; the raw transcript was pasted.
    Unpolished { reason: String },
}

#[derive(Clone, Debug, PartialEq)]
pub struct PolishOutcome {
    pub text: String,
    pub status: PolishStatus,
    pub notice: Option<String>,
}

struct Config {
    timeout: Duration,
    prompts: HashMap<String, String>,
    enabled: bool,
}

pub struct Polisher {
    registry: Mutex<Registry>,
    client: Arc<dyn LlmClient>,
    config: Mutex<Config>,
    /// The endpoint whose warm-up is in flight (a cold model is loading there), if any.
    warming: Mutex<Option<String>>,
}

impl Polisher {
    pub fn new(client: Arc<dyn LlmClient>, endpoints: Vec<Endpoint>, timeout: Duration) -> Self {
        Polisher { registry: Mutex::new(Registry::new(endpoints)), client, config: Mutex::new(Config { timeout, prompts: HashMap::new(), enabled: true }), warming: Mutex::new(None) }
    }

    pub fn set_endpoints(&self, endpoints: Vec<Endpoint>) {
        self.registry.lock().unwrap().replace(endpoints);
    }

    pub fn endpoints(&self) -> Vec<Endpoint> {
        self.registry.lock().unwrap().endpoints().to_vec()
    }

    pub fn health_of(&self, id: &str) -> Option<Health> {
        self.registry.lock().unwrap().health(id).cloned()
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.config.lock().unwrap().enabled = enabled;
    }

    pub fn enabled(&self) -> bool {
        self.config.lock().unwrap().enabled
    }

    pub fn set_timeout(&self, timeout: Duration) {
        self.config.lock().unwrap().timeout = timeout;
    }

    pub fn set_prompts(&self, prompts: HashMap<String, String>) {
        self.config.lock().unwrap().prompts = prompts;
    }

    /// The endpoint a polish would use right now, if any is healthy.
    pub fn selected(&self, now: Instant) -> Option<Endpoint> {
        self.registry.lock().unwrap().select(now)
    }

    /// The model to request from `ep`: its configured one, else the first the server listed.
    pub fn active_model(&self, ep: &Endpoint) -> Option<String> {
        ep.model.clone().or_else(|| self.health_of(&ep.id).and_then(|h| h.models.first().cloned()))
    }

    /// Probe every endpoint whose check is due; returns true when any endpoint flipped between reachable and not.
    /// "Reachable" here is the last verdict, not the time-limited `healthy`, so a routine re-check of a fine endpoint is not a change.
    pub fn check_due(&self, now: Instant) -> bool {
        let due = self.registry.lock().unwrap().due_for_check(now);
        let mut changed = false;
        for ep in due {
            let reachable = |h: Option<Health>| h.map(|h| h.failures == 0 && h.last_ok.is_some());
            let before = reachable(self.health_of(&ep.id));
            let result = self.client.probe(&ep, PROBE_TIMEOUT);
            // Stamp the verdict when the probe answered, not when the pass started, so slow probes never eat the TTL.
            let stamped = if now > Instant::now() { now } else { Instant::now() };
            let mut reg = self.registry.lock().unwrap();
            match result {
                Ok(p) => reg.mark_ok(&ep.id, stamped, p.models, p.loaded_models, p.loaded_bytes),
                Err(e) => reg.mark_failure(&ep.id, stamped, e),
            }
            let after = reachable(reg.health(&ep.id).cloned());
            changed |= before != after;
        }
        changed
    }

    /// The endpoint id whose warm-up request is in flight, if any.
    pub fn warming(&self) -> Option<String> {
        self.warming.lock().unwrap().clone()
    }

    pub fn polish(&self, mode: Mode, raw: &str, now: Instant) -> PolishOutcome {
        let (timeout, system, enabled) = {
            let c = self.config.lock().unwrap();
            (c.timeout, effective_prompt(mode, &c.prompts), c.enabled)
        };
        if !enabled || !mode.uses_llm() {
            return PolishOutcome { text: raw.to_string(), status: PolishStatus::NotNeeded, notice: None };
        }
        let (candidates, preferred) = {
            let reg = self.registry.lock().unwrap();
            // Only an endpoint the user added counts as "preferred" for the skipped notice; auto-probed localhost
            // runtimes the user never configured do not earn a complaint when one of them is missing.
            (reg.candidates(now), reg.endpoints().iter().find(|e| e.enabled && e.source == super::Source::Manual).cloned())
        };
        // One budget for the whole dictation, shared by every candidate. It stretches to the cold-start grace only
        // when the first candidate is the one whose model is still loading behind the warm-up.
        let budget = match (self.warming(), candidates.first()) {
            (Some(w), Some(first)) if w == first.id => timeout.max(COLD_START_GRACE),
            _ => timeout,
        };
        let deadline = Instant::now() + budget;
        let max_tokens = max_tokens_for_mode(mode, raw);
        let mut last_error: Option<String> = None;
        for ep in candidates.iter() {
            let Some(model) = self.active_model(ep) else { continue };
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining < MIN_CHAT_BUDGET {
                last_error.get_or_insert_with(|| "LLM timeout".into());
                break;
            }
            let t = Instant::now();
            match self.client.chat(ep, &model, &system, raw, max_tokens, remaining) {
                Ok(text) if !text.trim().is_empty() => {
                    let ms = t.elapsed().as_millis() as u64;
                    let notice = preferred.as_ref().filter(|p| p.id != ep.id).map(|p| failover_notice(p, ep));
                    return PolishOutcome { text, status: PolishStatus::Polished { endpoint_id: ep.id.clone(), model, ms }, notice };
                }
                Ok(_) => {
                    log::warn!("polish: {} returned an empty reply", ep.label);
                    last_error = Some(format!("{} returned an empty reply", ep.label));
                    self.registry.lock().unwrap().mark_failure(&ep.id, now, "empty reply".into());
                }
                Err(e) => {
                    log::warn!("polish: {} failed: {e}", ep.label);
                    last_error = Some(format!("{}: {e}", ep.label));
                    self.registry.lock().unwrap().mark_failure(&ep.id, now, e);
                }
            }
        }
        PolishOutcome { text: raw.to_string(), status: PolishStatus::Unpolished { reason: last_error.unwrap_or_else(|| "no LLM endpoint reachable".into()) }, notice: None }
    }

    /// Warm the selected endpoint's model so a cold runtime loads while the user is still talking (§4.3 5a).
    pub fn warm(&self, now: Instant) {
        if !self.enabled() {
            return;
        }
        let Some(ep) = self.selected(now) else { return };
        let Some(model) = self.active_model(&ep) else { return };
        {
            let mut w = self.warming.lock().unwrap();
            if w.is_some() {
                return; // one in flight is enough; LM Studio serialises requests and would queue the polish behind extras
            }
            *w = Some(ep.id.clone());
        }
        let t = Instant::now();
        let r = self.client.warm(&ep, &model);
        *self.warming.lock().unwrap() = None;
        match r {
            Ok(()) => log::info!("llm warm-up on {} took {:?}", ep.label, t.elapsed()),
            Err(e) => log::warn!("llm warm-up on {} failed: {e}", ep.label),
        }
    }

    /// Unload every model the registry knows to be resident (idle path, §4.3 5c).
    pub fn unload_all(&self) {
        let targets: Vec<(Endpoint, Vec<String>)> = {
            let reg = self.registry.lock().unwrap();
            // Only runtimes on this machine: the Mac's idle timer must not evict a model other tools on the PC may be using.
            reg.endpoints().iter().filter(|e| super::is_local(&e.base_url)).filter_map(|e| reg.health(&e.id).filter(|h| !h.loaded_models.is_empty()).map(|h| (e.clone(), h.loaded_models.clone()))).collect()
        };
        for (ep, models) in targets {
            for m in models {
                if let Err(e) = self.client.unload(&ep, &m) {
                    log::warn!("llm unload of {m} on {} failed: {e}", ep.label);
                }
            }
            self.registry.lock().unwrap().clear_loaded(&ep.id);
        }
    }

    /// Bytes the selected endpoint's loaded model takes on this machine; 0 when it is remote or nothing is selected (§9).
    pub fn resident_local_bytes(&self, now: Instant) -> u64 {
        let reg = self.registry.lock().unwrap();
        reg.active_local(now).and_then(|e| reg.health(&e.id).map(|h| h.loaded_bytes)).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::polish::fake_server::{Behaviour, FakeLlm};
    use crate::polish::{local_candidates, Endpoint, Flavor, HttpLlmClient, LlmClient, Mode};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    fn polisher(eps: Vec<Endpoint>, timeout_ms: u64) -> Polisher {
        Polisher::new(Arc::new(HttpLlmClient::new()), eps, Duration::from_millis(timeout_ms))
    }

    #[test]
    fn raw_mode_never_calls_the_llm() {
        let s = FakeLlm::start(Behaviour::OpenAi { reply: "x".into() });
        let p = polisher(vec![s.endpoint(Flavor::OpenAi)], 2000);
        let o = p.polish(Mode::Raw, "hi", Instant::now());
        assert_eq!(o.status, PolishStatus::NotNeeded);
        assert_eq!(o.text, "hi");
        assert!(s.hits.lock().unwrap().is_empty());
    }

    #[test]
    fn polishes_with_the_first_healthy_endpoint() {
        let s = FakeLlm::start(Behaviour::OpenAi { reply: "Hello there.".into() });
        let p = polisher(vec![s.endpoint(Flavor::OpenAi)], 2000);
        let now = Instant::now();
        p.check_due(now);
        let o = p.polish(Mode::Clean, "hello there", now);
        assert_eq!(o.text, "Hello there.");
        match o.status {
            PolishStatus::Polished { endpoint_id, model, .. } => {
                assert_eq!(endpoint_id, s.endpoint(Flavor::OpenAi).id);
                assert_eq!(model, "qwen2.5-7b-instruct");
            }
            other => panic!("{other:?}"),
        }
        assert!(o.notice.is_none());
        assert!(s.last_body().contains("Reply with only the text to paste"));
    }

    #[test]
    fn falls_back_to_next_endpoint_on_bad_response() {
        let bad = FakeLlm::start(Behaviour::ServerError);
        let good = FakeLlm::start(Behaviour::OpenAi { reply: "ok text".into() });
        let mut bad_ep = bad.endpoint(Flavor::OpenAi);
        bad_ep.model = Some("m".into());
        bad_ep.label = "Bad".into();
        let p = polisher(vec![bad_ep.clone(), good.endpoint(Flavor::OpenAi)], 2000);
        let now = Instant::now();
        p.check_due(now);
        let o = p.polish(Mode::Clean, "x", now);
        assert_eq!(o.text, "ok text");
        assert_eq!(o.notice.as_deref(), Some("Bad unreachable, polished with fake"));
        assert!(p.health_of(&bad_ep.id).unwrap().failures >= 1);
    }

    #[test]
    fn notice_wording_for_a_lan_box_falling_back_to_local() {
        let mut pc = local_candidates().remove(0);
        pc.base_url = "http://192.168.1.20:11434".into();
        pc.label = "PC".into();
        let local = local_candidates().remove(0);
        assert_eq!(failover_notice(&pc, &local), "PC unreachable, polished locally");
        assert_eq!(failover_notice(&local, &pc), "Ollama (this Mac) unreachable, polished with PC");
    }

    #[test]
    fn unload_all_forgets_loaded_models_but_keeps_health() {
        let s = FakeLlm::start(Behaviour::Ollama { reply: "x".into(), loaded_bytes: 10 });
        let p = polisher(vec![s.endpoint(Flavor::Ollama)], 1000);
        let now = Instant::now();
        p.check_due(now);
        p.unload_all();
        let h = p.health_of(&s.endpoint(Flavor::Ollama).id).unwrap();
        assert!(h.loaded_models.is_empty());
        assert_eq!(h.loaded_bytes, 0);
        assert!(h.healthy(now));
        let never = polisher(vec![Endpoint { base_url: "http://127.0.0.1:1".into(), ..local_candidates().remove(1) }], 1000);
        never.unload_all();
        assert!(never.selected(now).is_none(), "unload must not invent health");
    }

    #[test]
    fn unreachable_everything_pastes_raw_and_marks_unpolished() {
        let p = polisher(vec![Endpoint { base_url: "http://127.0.0.1:1".into(), model: Some("m".into()), ..local_candidates().remove(1) }], 1000);
        let o = p.polish(Mode::Clean, "raw words", Instant::now());
        assert_eq!(o.text, "raw words");
        assert!(matches!(o.status, PolishStatus::Unpolished { .. }));
    }

    #[test]
    fn endpoint_without_a_known_model_is_skipped() {
        let p = polisher(vec![Endpoint { base_url: "http://127.0.0.1:1".into(), model: None, ..local_candidates().remove(1) }], 1000);
        let o = p.polish(Mode::Clean, "raw", Instant::now());
        assert_eq!(o.status, PolishStatus::Unpolished { reason: "no LLM endpoint reachable".into() });
    }

    #[test]
    fn empty_llm_output_counts_as_unpolished() {
        let s = FakeLlm::start(Behaviour::OpenAi { reply: "  ".into() });
        let p = polisher(vec![s.endpoint(Flavor::OpenAi)], 2000);
        let now = Instant::now();
        p.check_due(now);
        let o = p.polish(Mode::Clean, "raw", now);
        assert_eq!(o.text, "raw");
        assert!(matches!(o.status, PolishStatus::Unpolished { .. }));
    }

    #[test]
    fn fails_back_once_the_preferred_endpoint_recovers() {
        let pref = FakeLlm::start(Behaviour::Hang);
        let port = pref.port();
        let local = FakeLlm::start(Behaviour::OpenAi { reply: "local".into() });
        let mut pref_ep = pref.endpoint(Flavor::OpenAi);
        pref_ep.model = Some("m".into());
        let p = polisher(vec![pref_ep, local.endpoint(Flavor::OpenAi)], 300);
        let t0 = Instant::now();
        p.check_due(t0);
        assert_eq!(p.polish(Mode::Clean, "a", t0).text, "local");
        drop(pref);
        let _pref2 = FakeLlm::start_on(Behaviour::OpenAi { reply: "pc".into() }, port);
        // The failure was stamped when the 1 s probe gave up, so its 10 s backoff ends a little after t0 + 11 s.
        let t1 = t0 + Duration::from_secs(12);
        p.check_due(t1);
        let o = p.polish(Mode::Clean, "a", t1);
        assert_eq!(o.text, "pc");
        assert!(o.notice.is_none());
    }

    #[test]
    fn disabled_polish_is_not_needed() {
        let s = FakeLlm::start(Behaviour::OpenAi { reply: "x".into() });
        let p = polisher(vec![s.endpoint(Flavor::OpenAi)], 1000);
        p.set_enabled(false);
        assert_eq!(p.polish(Mode::Clean, "r", Instant::now()).status, PolishStatus::NotNeeded);
        assert!(s.hits.lock().unwrap().is_empty());
    }

    #[test]
    fn resident_bytes_only_for_a_local_selected_endpoint() {
        let s = FakeLlm::start(Behaviour::Ollama { reply: "x".into(), loaded_bytes: 3_000_000_000 });
        let p = polisher(vec![s.endpoint(Flavor::Ollama)], 1000);
        let now = Instant::now();
        assert_eq!(p.resident_local_bytes(now), 0);
        p.check_due(now);
        assert_eq!(p.resident_local_bytes(now), 3_000_000_000);
    }

    #[test]
    fn warm_targets_the_selected_endpoint_and_unload_all_hits_loaded_ones() {
        let s = FakeLlm::start(Behaviour::Ollama { reply: "x".into(), loaded_bytes: 10 });
        let p = polisher(vec![s.endpoint(Flavor::Ollama)], 1000);
        let now = Instant::now();
        p.warm(now);
        assert!(s.hits.lock().unwrap().iter().all(|h| !h.starts_with("POST")), "nothing selected yet, no warm");
        p.check_due(now);
        p.warm(now);
        assert!(s.hits.lock().unwrap().iter().any(|h| h == "POST /api/generate"));
        assert!(s.last_body().contains("\"keep_alive\":-1"));
        p.unload_all();
        assert!(s.last_body().contains("\"keep_alive\":0"));
    }

    #[test]
    fn custom_prompt_override_is_used() {
        let s = FakeLlm::start(Behaviour::OpenAi { reply: "x".into() });
        let p = polisher(vec![s.endpoint(Flavor::OpenAi)], 1000);
        let now = Instant::now();
        p.check_due(now);
        let mut prompts = std::collections::HashMap::new();
        prompts.insert("clean".to_string(), "MY PROMPT".to_string());
        p.set_prompts(prompts);
        p.polish(Mode::Clean, "r", now);
        assert!(s.last_body().contains("MY PROMPT"));
    }

    #[test]
    fn registry_replacement_mid_flight_is_safe() {
        let s = FakeLlm::start(Behaviour::OpenAi { reply: "ok".into() });
        let p = Arc::new(polisher(vec![s.endpoint(Flavor::OpenAi)], 2000));
        let now = Instant::now();
        p.check_due(now);
        let p2 = p.clone();
        let h = std::thread::spawn(move || p2.polish(Mode::Clean, "r", now));
        p.set_endpoints(vec![]);
        let o = h.join().unwrap();
        assert!(o.text == "ok" || matches!(o.status, PolishStatus::Unpolished { .. }));
        assert!(p.endpoints().is_empty());
        assert!(matches!(p.polish(Mode::Clean, "r", now).status, PolishStatus::Unpolished { .. }));
    }

    #[test]
    fn check_due_reports_no_change_when_still_reachable() {
        let s = FakeLlm::start(Behaviour::OpenAi { reply: "x".into() });
        let p = polisher(vec![s.endpoint(Flavor::OpenAi)], 1000);
        let t0 = Instant::now();
        assert!(p.check_due(t0), "first probe is a change (unknown → reachable)");
        assert!(!p.check_due(t0 + Duration::from_secs(10)), "routine re-check of a fine endpoint is not a change");
        assert!(!p.check_due(t0 + Duration::from_secs(20)));
    }

    /// A spy client that records the timeout each chat was given and can simulate a slow warm-up.
    struct SpyClient {
        chat_timeouts: Mutex<Vec<Duration>>,
        warm_delay: Duration,
        probe_delay: Duration,
        warms: Mutex<u32>,
        unloads: Mutex<Vec<String>>,
    }
    impl SpyClient {
        fn new(warm_delay: Duration) -> Self {
            SpyClient { chat_timeouts: Mutex::new(vec![]), warm_delay, probe_delay: Duration::ZERO, warms: Mutex::new(0), unloads: Mutex::new(vec![]) }
        }
    }
    impl LlmClient for SpyClient {
        fn probe(&self, _ep: &Endpoint, _t: Duration) -> Result<crate::polish::Probe, String> {
            std::thread::sleep(self.probe_delay);
            Ok(crate::polish::Probe { models: vec!["m".into()], loaded_models: vec!["m".into()], loaded_bytes: 1 })
        }
        fn chat(&self, _ep: &Endpoint, _m: &str, _s: &str, _u: &str, _max_tokens: u32, timeout: Duration) -> Result<String, String> {
            self.chat_timeouts.lock().unwrap().push(timeout);
            Ok("ok".into())
        }
        fn warm(&self, _ep: &Endpoint, _m: &str) -> Result<(), String> {
            *self.warms.lock().unwrap() += 1;
            std::thread::sleep(self.warm_delay);
            Ok(())
        }
        fn unload(&self, ep: &Endpoint, _m: &str) -> Result<(), String> {
            self.unloads.lock().unwrap().push(ep.base_url.clone());
            Ok(())
        }
    }

    #[test]
    fn polish_waits_longer_only_when_the_warming_endpoint_comes_first() {
        let spy = Arc::new(SpyClient::new(Duration::from_millis(400)));
        let p = Arc::new(Polisher::new(spy.clone(), local_candidates(), Duration::from_secs(8)));
        let now = Instant::now();
        p.check_due(now);
        let p2 = p.clone();
        let warm = std::thread::spawn(move || p2.warm(now));
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(p.warming().as_deref(), Some("local-ollama"));
        p.polish(Mode::Clean, "a", now);
        warm.join().unwrap();
        assert!(p.warming().is_none());
        p.polish(Mode::Clean, "a", now);
        let t = spy.chat_timeouts.lock().unwrap().clone();
        assert!(t[0] >= COLD_START_GRACE - Duration::from_millis(50) && t[0] <= COLD_START_GRACE, "{t:?}");
        assert!(t[1] <= Duration::from_secs(8) && t[1] >= Duration::from_millis(7900), "{t:?}");
        // A warm-up on a different endpoint than the first candidate does not stretch the budget.
        let spy2 = Arc::new(SpyClient::new(Duration::from_millis(400)));
        let mut eps = local_candidates();
        eps[0].base_url = "http://127.0.0.1:1".into();
        let p = Arc::new(Polisher::new(spy2.clone(), eps, Duration::from_secs(8)));
        p.check_due(now);
        let p3 = p.clone();
        let mut prefers_second = p.registry.lock().unwrap();
        prefers_second.mark_failure("local-ollama", now, "down".into());
        drop(prefers_second);
        let warm = std::thread::spawn(move || p3.warm(now));
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(p.warming().as_deref(), Some("local-lmstudio"));
        p.polish(Mode::Clean, "a", now);
        warm.join().unwrap();
        let t = spy2.chat_timeouts.lock().unwrap().clone();
        assert!(t[0] >= COLD_START_GRACE - Duration::from_millis(50), "warming endpoint is the first candidate: {t:?}");
    }

    #[test]
    fn warm_is_skipped_while_one_is_in_flight() {
        let spy = Arc::new(SpyClient::new(Duration::from_millis(300)));
        let p = Arc::new(Polisher::new(spy.clone(), local_candidates(), Duration::from_secs(8)));
        let now = Instant::now();
        p.check_due(now);
        let hs: Vec<_> = (0..3).map(|_| { let p = p.clone(); std::thread::spawn(move || p.warm(now)) }).collect();
        for h in hs { h.join().unwrap(); }
        assert_eq!(*spy.warms.lock().unwrap(), 1);
    }

    #[test]
    fn timeout_is_one_budget_across_candidates() {
        let a = FakeLlm::start(Behaviour::Hang);
        let b = FakeLlm::start(Behaviour::Hang);
        let mut ea = a.endpoint(Flavor::OpenAi); ea.model = Some("m".into()); ea.label = "A".into();
        let mut eb = b.endpoint(Flavor::OpenAi); eb.model = Some("m".into()); eb.label = "B".into();
        let p = polisher(vec![ea, eb], 400);
        let t = Instant::now();
        let o = p.polish(Mode::Clean, "x", Instant::now());
        assert!(matches!(o.status, PolishStatus::Unpolished { .. }));
        assert!(t.elapsed() < Duration::from_millis(900), "two hanging candidates must share one 400 ms budget, took {:?}", t.elapsed());
    }

    #[test]
    fn no_notice_when_only_auto_detected_candidates_were_skipped() {
        let lm = FakeLlm::start(Behaviour::OpenAi { reply: "ok".into() });
        let mut eps = local_candidates();
        eps[0].base_url = "http://127.0.0.1:1".into();
        eps[1].base_url = lm.base_url.clone();
        eps[1].id = crate::polish::endpoint_id(&lm.base_url);
        let p = polisher(eps, 1000);
        let now = Instant::now();
        p.check_due(now);
        let o = p.polish(Mode::Clean, "x", now);
        assert_eq!(o.text, "ok");
        assert!(o.notice.is_none(), "{:?}", o.notice);
    }

    #[test]
    fn unload_all_leaves_remote_runtimes_alone() {
        let spy = Arc::new(SpyClient::new(Duration::ZERO));
        let mut pc = local_candidates().remove(0);
        pc.base_url = "http://192.168.1.20:11434".into();
        pc.id = "pc".into();
        let p = Polisher::new(spy.clone(), vec![pc, local_candidates().remove(0)], Duration::from_secs(1));
        p.check_due(Instant::now());
        p.unload_all();
        assert_eq!(spy.unloads.lock().unwrap().clone(), vec!["http://127.0.0.1:11434".to_string()]);
    }

    #[test]
    fn health_is_stamped_after_the_probe_not_before() {
        let spy = Arc::new(SpyClient { probe_delay: Duration::from_millis(300), ..SpyClient::new(Duration::ZERO) });
        let p = Polisher::new(spy, local_candidates(), Duration::from_secs(1));
        let t = Instant::now();
        p.check_due(t);
        let h = p.health_of("local-ollama").unwrap();
        assert!(h.last_ok.unwrap() >= t + Duration::from_millis(250), "last_ok must reflect when the probe answered");
    }
}
