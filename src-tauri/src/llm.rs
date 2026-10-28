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
    let p = Arc::new(Polisher::new(Arc::new(HttpLlmClient::new()), endpoints_from_settings(settings), Duration::from_secs(settings.llm_timeout_secs)));
    let _ = SHARED.set(p.clone());
    apply_settings_inner(&p, settings, false);
    p
}

/// Push every polish-related setting into the live polisher (called at start and on each settings save).
/// Turning polish off unloads every local model we keep alive; any other change unloads models no longer in use.
/// Network work happens on a helper thread so a settings save never waits on a runtime.
pub fn apply_settings(p: &Polisher, s: &Settings) {
    apply_settings_inner(p, s, true)
}

/// `user_change` is false at startup: settings are pushed in, but no model is loaded until the user asks for
/// something (a key press warms it; toggling or switching in the panel loads it).
fn apply_settings_inner(p: &Polisher, s: &Settings, user_change: bool) {
    let was_enabled = p.enabled();
    p.set_endpoints(endpoints_from_settings(s));
    p.set_timeout(Duration::from_secs(s.llm_timeout_secs.clamp(1, 60)));
    p.set_prompts(s.mode_prompts.clone());
    p.set_enabled(s.polish_enabled);
    let turned_off = was_enabled && !s.polish_enabled;
    if let Some(p) = SHARED.get() {
        let p = p.clone();
        let spawned = std::thread::Builder::new().name("privatevox-llm-apply".into()).spawn(move || {
            // Probe first: right after launch the registry may not know yet what is resident.
            p.recheck_soon();
            p.check_due(Instant::now());
            if turned_off {
                log::info!("polish switched off: unloading local LLM models");
                p.unload_all();
            } else {
                p.unload_unused(Instant::now());
                // Polish is on after a user change: load the model now so the strip shows it and the first dictation is quick.
                if user_change && s.polish_enabled {
                    p.warm(Instant::now());
                }
            }
            p.recheck_soon();
            p.check_due(Instant::now());
        });
        if let Err(e) = spawned {
            log::warn!("llm apply thread: {e}");
        }
    }
}

/// The one polisher the app owns, so `apply_settings` can hand it to helper threads.
static SHARED: std::sync::OnceLock<SharedPolisher> = std::sync::OnceLock::new();

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
