//! App-side owner of the LLM polish step: builds the shared `Polisher` from settings and runs the health loop (design §8.2).

use privatevox_core::polish::{ordered, Endpoint, HttpLlmClient, Polisher};
use privatevox_core::settings::Settings;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};

pub type SharedPolisher = Arc<Polisher>;

/// Health checks run this often while the app is awake (§8.2).
const HEALTH_PERIOD: Duration = Duration::from_secs(10);

/// User endpoints in priority order, then the localhost candidates when auto-detect is on.
pub fn endpoints_from_settings(s: &Settings) -> Vec<Endpoint> {
    ordered(&s.llm_endpoints, s.auto_detect_local_llm)
}

pub fn build(settings: &Settings) -> SharedPolisher {
    let p = Polisher::new(Arc::new(HttpLlmClient::new()), endpoints_from_settings(settings), Duration::from_secs(settings.llm_timeout_secs));
    apply_settings(&p, settings);
    Arc::new(p)
}

/// Push every polish-related setting into the live polisher (called at start and on each settings save).
pub fn apply_settings(p: &Polisher, s: &Settings) {
    p.set_endpoints(endpoints_from_settings(s));
    p.set_timeout(Duration::from_secs(s.llm_timeout_secs.clamp(1, 60)));
    p.set_prompts(s.mode_prompts.clone());
    p.set_enabled(s.polish_enabled);
}

/// Probes due endpoints every 10 s; emits `llm-changed` whenever any endpoint flips healthy/unhealthy.
pub fn spawn_health_loop(polisher: SharedPolisher, app: AppHandle) {
    std::thread::Builder::new()
        .name("privatevox-llm-health".into())
        .spawn(move || loop {
            let changed = polisher.check_due(Instant::now());
            if changed {
                if let Some(ep) = polisher.selected(Instant::now()) {
                    log::info!("polish LLM: using {} ({})", ep.label, ep.base_url);
                } else {
                    log::info!("polish LLM: no endpoint reachable");
                }
                let _ = app.emit("llm-changed", ());
            }
            std::thread::sleep(HEALTH_PERIOD);
        })
        .expect("llm health thread");
}
