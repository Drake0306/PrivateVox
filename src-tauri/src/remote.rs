//! The client side of host mode (design §8.6): owns the shared `RemoteState`, pushes settings into it, and runs the
//! health loop that decides whether the host is in use.

use privatevox_core::polish::normalize_base_url;
use privatevox_core::settings::Settings;
pub use privatevox_engine::remote::RemoteState;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter};

const HEALTH_PERIOD: Duration = Duration::from_secs(10);
const PROBE_TIMEOUT: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// The name this machine introduces itself with: the setting, else the OS hostname, else "PrivateVox".
pub fn machine_name(s: &Settings) -> String {
    if let Some(n) = s.machine_name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
        return n.to_string();
    }
    let host = std::process::Command::new("hostname").output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).filter(|h| !h.is_empty());
    // "MacBookAir.lan" / "pc.local" → "MacBookAir" / "pc": the domain part says nothing to a person.
    host.map(|h| h.split('.').next().unwrap_or(&h).to_string()).filter(|h| !h.is_empty()).unwrap_or_else(|| "PrivateVox".into())
}

/// Push the remote settings into the live state. Enabling configures the client; disabling clears it.
pub fn apply_settings(state: &RemoteState, s: &Settings) {
    let url = s.remote.base_url.as_deref().and_then(|u| normalize_base_url(u).ok());
    match (s.remote.enabled, url) {
        (true, Some(url)) => {
            let changed = state.client().map(|c| c.base_url() != url).unwrap_or(true);
            state.configure(&s.remote.label, &url, s.remote.token.clone(), &machine_name(s));
            state.enabled.store(true, Ordering::SeqCst);
            if changed {
                state.healthy.store(false, Ordering::SeqCst); // the loop confirms the new host
            }
        }
        _ => {
            state.enabled.store(false, Ordering::SeqCst);
            state.clear();
        }
    }
}

/// True when `now` differs from the last verdict reported, so a flip caused by a failed request (not by this loop)
/// is still announced on the next pass.
pub fn report_transition(last_reported: &mut Option<bool>, now: bool) -> bool {
    if *last_reported == Some(now) {
        return false;
    }
    *last_reported = Some(now);
    true
}

/// Every 10 s while enabled: `GET /v1/status`; flips `healthy` and emits `remote-changed` on a change. Backs off up to 60 s while it fails.
pub fn spawn_health_loop(state: Arc<RemoteState>, app: AppHandle) {
    std::thread::Builder::new()
        .name("privatevox-remote-health".into())
        .spawn(move || {
            let mut failures: u32 = 0;
            let mut last_reported: Option<bool> = Some(false); // a host that is down at launch is not news
            loop {
                let mut wait = HEALTH_PERIOD;
                if state.enabled.load(Ordering::SeqCst) {
                    if let Some(client) = state.client() {
                        match client.status(PROBE_TIMEOUT) {
                            Ok(st) => {
                                failures = 0;
                                *state.last_status.lock().unwrap() = Some(st);
                                *state.last_error.lock().unwrap() = None;
                                state.healthy.store(true, Ordering::SeqCst);
                            }
                            Err(e) => {
                                failures += 1;
                                *state.last_error.lock().unwrap() = Some(e.to_string());
                                state.healthy.store(false, Ordering::SeqCst);
                                wait = (HEALTH_PERIOD * (1 << failures.saturating_sub(1).min(3))).min(MAX_BACKOFF);
                            }
                        }
                        let now = state.healthy.load(Ordering::SeqCst);
                        if report_transition(&mut last_reported, now) {
                            log::info!("host {}: {}", state.label(), if now { "reachable, dictation goes there" } else { "unreachable, dictating locally" });
                            let _ = app.emit("remote-changed", now);
                        }
                    }
                } else {
                    last_reported = Some(false);
                }
                std::thread::sleep(wait);
            }
        })
        .expect("remote health thread");
}

#[cfg(test)]
mod tests {
    use super::*;
    use privatevox_core::settings::Settings;

    #[test]
    fn machine_name_falls_back_to_hostname() {
        let mut s = Settings::default();
        let auto = machine_name(&s);
        assert!(!auto.trim().is_empty());
        s.machine_name = Some("  Roy's Mac ".into());
        assert_eq!(machine_name(&s), "Roy's Mac");
        s.machine_name = None;
        assert!(!machine_name(&s).contains('.'), "domain suffix dropped: {}", machine_name(&s));
    }

    #[test]
    fn apply_settings_configures_and_clears() {
        let st = RemoteState::new();
        let mut s = Settings::default();
        apply_settings(&st, &s);
        assert!(!st.enabled.load(std::sync::atomic::Ordering::SeqCst));
        s.remote.enabled = true;
        s.remote.base_url = Some("192.168.1.20:8765".into());
        s.remote.label = "Big PC".into();
        apply_settings(&st, &s);
        assert!(st.enabled.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(st.label(), "Big PC");
        assert_eq!(st.client().unwrap().base_url(), "http://192.168.1.20:8765");
        s.remote.enabled = false;
        apply_settings(&st, &s);
        assert!(!st.enabled.load(std::sync::atomic::Ordering::SeqCst));
        assert!(st.client().is_none());
    }

    #[test]
    fn transition_is_reported_once_per_flip() {
        let mut last = None;
        assert!(report_transition(&mut last, true), "first verdict is a change");
        assert!(!report_transition(&mut last, true));
        assert!(report_transition(&mut last, false), "a flip from a failed request shows up on the next pass");
        assert!(!report_transition(&mut last, false));
        assert!(report_transition(&mut last, true));
    }
}
