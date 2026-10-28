//! Panel commands for modes, prompts, LLM endpoints and re-polish (design §4.4, §4.5, §8).

use crate::pipeline::Cmd;
use crate::state::AppState;
use privatevox_core::polish::{default_prompt, effective_prompt, endpoint_id, is_local, normalize_base_url, Endpoint, Flavor, HttpLlmClient, LlmClient, Mode, Source};
use privatevox_core::settings::Settings;
use std::time::{Duration, Instant};
use tauri::State;

#[derive(serde::Serialize)]
pub struct ModeInfo {
    pub id: String,
    pub label: String,
    pub uses_llm: bool,
    pub prompt: String,
    pub default_prompt: String,
    pub is_default: bool,
    pub active: bool,
}

#[derive(serde::Serialize, Clone)]
pub struct EndpointView {
    pub id: String,
    pub label: String,
    pub base_url: String,
    pub model: Option<String>,
    pub flavor: Flavor,
    pub source: Source,
    pub enabled: bool,
    pub healthy: bool,
    pub checked: bool,
    pub last_error: Option<String>,
    pub models: Vec<String>,
    pub loaded_models: Vec<String>,
    pub loaded_bytes: u64,
    pub is_local: bool,
    pub selected: bool,
}

#[derive(serde::Serialize)]
pub struct LlmStatus {
    pub enabled: bool,
    pub selected: Option<EndpointView>,
    pub mode: String,
}

pub fn unreachable_message(url: &str) -> String {
    format!("Nothing answered at {url}. Is the server running and set to serve on the network?")
}

/// Move `id` one place up (towards priority 1) or down. Returns false when nothing moved.
pub fn move_in(list: &mut [Endpoint], id: &str, up: bool) -> bool {
    let Some(i) = list.iter().position(|e| e.id == id) else { return false };
    if up && i > 0 {
        list.swap(i, i - 1);
        true
    } else if !up && i + 1 < list.len() {
        list.swap(i, i + 1);
        true
    } else {
        false
    }
}

/// Move `id` to the top of the list. Returns false when it is not in the list.
pub fn prefer(list: &mut Vec<Endpoint>, id: &str) -> bool {
    let Some(i) = list.iter().position(|e| e.id == id) else { return false };
    let e = list.remove(i);
    list.insert(0, e);
    true
}

/// Prefer `id`; when it is one of the probed local candidates rather than a user entry, adopt it into the list first.
pub fn prefer_or_adopt(list: &mut Vec<Endpoint>, id: &str, candidates: &[Endpoint]) -> bool {
    if prefer(list, id) {
        return true;
    }
    let Some(c) = candidates.iter().find(|c| c.id == id) else { return false };
    let mut adopted = c.clone();
    adopted.source = Source::Manual;
    list.insert(0, adopted);
    true
}

fn edit_settings(state: &AppState, f: impl FnOnce(&mut Settings)) -> Result<Settings, String> {
    let mut s = state.settings.lock().map(|s| s.clone()).map_err(|e| e.to_string())?;
    f(&mut s);
    crate::commands::apply_settings(&state.settings, &state.paths.settings(), s.clone())?;
    crate::llm::apply_settings(&state.polisher, &s);
    Ok(s)
}

fn view(state: &AppState, ep: &Endpoint, selected: Option<&str>, now: Instant) -> EndpointView {
    let h = state.polisher.health_of(&ep.id);
    EndpointView {
        id: ep.id.clone(),
        label: ep.label.clone(),
        base_url: ep.base_url.clone(),
        model: ep.model.clone(),
        flavor: ep.flavor,
        source: ep.source,
        enabled: ep.enabled,
        healthy: h.as_ref().map(|h| h.healthy(now)).unwrap_or(false),
        checked: h.as_ref().map(|h| h.last_checked.is_some()).unwrap_or(false),
        last_error: h.as_ref().and_then(|h| h.last_error.clone()),
        models: h.as_ref().map(|h| h.models.clone()).unwrap_or_default(),
        loaded_models: h.as_ref().map(|h| h.loaded_models.clone()).unwrap_or_default(),
        loaded_bytes: h.as_ref().map(|h| h.loaded_bytes).unwrap_or(0),
        is_local: is_local(&ep.base_url),
        selected: selected == Some(ep.id.as_str()),
    }
}

#[tauri::command]
pub fn list_modes(state: State<'_, AppState>) -> Result<Vec<ModeInfo>, String> {
    let s = state.settings.lock().map(|s| s.clone()).map_err(|e| e.to_string())?;
    Ok(Mode::ALL
        .into_iter()
        .map(|m| {
            let prompt = effective_prompt(m, &s.mode_prompts);
            ModeInfo { id: m.id().into(), label: m.label().into(), uses_llm: m.uses_llm(), is_default: prompt == default_prompt(m), prompt, default_prompt: default_prompt(m).into(), active: s.mode == m }
        })
        .collect())
}

/// Shared by the panel command and the tray menu.
pub fn set_mode_inner(state: &AppState, mode: Mode) -> Result<(), String> {
    edit_settings(state, |s| s.mode = mode)?;
    state.send(Cmd::SetMode(mode));
    crate::tray::sync_mode(mode);
    Ok(())
}

#[tauri::command]
pub fn set_mode(state: State<'_, AppState>, id: String) -> Result<(), String> {
    let mode = Mode::parse(&id).ok_or_else(|| format!("Unknown mode {id:?}"))?;
    set_mode_inner(&state, mode)
}

/// `None` or an empty prompt resets the mode to its default prompt.
#[tauri::command]
pub fn set_mode_prompt(state: State<'_, AppState>, id: String, prompt: Option<String>) -> Result<(), String> {
    let mode = Mode::parse(&id).ok_or_else(|| format!("Unknown mode {id:?}"))?;
    if !mode.uses_llm() {
        return Err(format!("{} has no prompt; it does not use the LLM", mode.label()));
    }
    edit_settings(&state, |s| match prompt.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
        Some(p) => {
            s.mode_prompts.insert(mode.id().into(), p.to_string());
        }
        None => {
            s.mode_prompts.remove(mode.id());
        }
    })?;
    Ok(())
}

#[tauri::command]
pub fn list_llm_endpoints(state: State<'_, AppState>) -> Vec<EndpointView> {
    let now = Instant::now();
    let selected = state.polisher.selected(now).map(|e| e.id);
    state.polisher.endpoints().iter().map(|e| view(&state, e, selected.as_deref(), now)).collect()
}

#[tauri::command]
pub fn llm_status(state: State<'_, AppState>) -> Result<LlmStatus, String> {
    let now = Instant::now();
    let selected = state.polisher.selected(now);
    let mode = state.settings.lock().map(|s| s.mode.label().to_string()).map_err(|e| e.to_string())?;
    Ok(LlmStatus { enabled: state.polisher.enabled(), selected: selected.as_ref().map(|e| view(&state, e, Some(&e.id), now)), mode })
}

/// Normalise the address, find out which API it speaks (Ollama first, then OpenAI-compatible), and add it at the top.
#[tauri::command]
pub async fn add_llm_endpoint(state: State<'_, AppState>, input: String, label: Option<String>) -> Result<EndpointView, String> {
    let base_url = normalize_base_url(&input)?;
    if state.polisher.endpoints().iter().any(|e| e.base_url == base_url && e.source == Source::Manual) {
        return Err(format!("{base_url} is already in the list"));
    }
    let url = base_url.clone();
    let probed = tauri::async_runtime::spawn_blocking(move || {
        let client = HttpLlmClient::new();
        let timeout = Duration::from_secs(2);
        for flavor in [Flavor::Ollama, Flavor::OpenAi] {
            let ep = Endpoint { id: endpoint_id(&url), label: String::new(), base_url: url.clone(), model: None, token: None, flavor, source: Source::Manual, enabled: true };
            if let Ok(p) = client.probe(&ep, timeout) {
                return Some((flavor, p.models));
            }
        }
        None
    })
    .await
    .map_err(|e| e.to_string())?;
    let Some((flavor, models)) = probed else { return Err(unreachable_message(&base_url)) };
    let label = label.map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).unwrap_or_else(|| {
        let host = base_url.split("://").nth(1).unwrap_or(&base_url).rsplit_once(':').map(|(h, _)| h.to_string()).unwrap_or(base_url.clone());
        if is_local(&base_url) { format!("{} (this machine)", if flavor == Flavor::Ollama { "Ollama" } else { "Local server" }) } else { format!("{host} ({})", if flavor == Flavor::Ollama { "Ollama" } else { "LAN" }) }
    });
    let ep = Endpoint { id: endpoint_id(&base_url), label, base_url: base_url.clone(), model: models.first().cloned(), token: None, flavor, source: Source::Manual, enabled: true };
    let ep2 = ep.clone();
    edit_settings(&state, |s| {
        s.llm_endpoints.retain(|e| e.base_url != base_url);
        s.llm_endpoints.insert(0, ep2);
        s.polish_enabled = true;
    })?;
    // Mark it healthy right away so the next dictation can use it without waiting for the 10 s loop.
    // The blocking client must not run on this tokio worker.
    let p = state.polisher.clone();
    tauri::async_runtime::spawn_blocking(move || p.check_due(Instant::now())).await.map_err(|e| e.to_string())?;
    let now = Instant::now();
    let selected = state.polisher.selected(now).map(|e| e.id);
    Ok(view(&state, &ep, selected.as_deref(), now))
}

#[tauri::command]
pub fn remove_llm_endpoint(state: State<'_, AppState>, id: String) -> Result<(), String> {
    edit_settings(&state, |s| s.llm_endpoints.retain(|e| e.id != id))?;
    Ok(())
}

#[tauri::command]
pub fn set_llm_endpoint(state: State<'_, AppState>, id: String, model: Option<String>, enabled: Option<bool>, label: Option<String>) -> Result<(), String> {
    let candidates = privatevox_core::polish::local_candidates();
    edit_settings(&state, |s| {
        if !s.llm_endpoints.iter().any(|e| e.id == id) {
            if let Some(c) = candidates.iter().find(|c| c.id == id) {
                let mut adopted = c.clone();
                adopted.source = Source::Manual;
                s.llm_endpoints.push(adopted);
            }
        }
        if let Some(e) = s.llm_endpoints.iter_mut().find(|e| e.id == id) {
            if let Some(m) = model {
                e.model = Some(m).filter(|m| !m.trim().is_empty());
            }
            if let Some(en) = enabled {
                e.enabled = en;
            }
            if let Some(l) = label.map(|l| l.trim().to_string()).filter(|l| !l.is_empty()) {
                e.label = l;
            }
        }
    })?;
    Ok(())
}

#[tauri::command]
pub fn move_llm_endpoint(state: State<'_, AppState>, id: String, up: bool) -> Result<bool, String> {
    let mut moved = false;
    edit_settings(&state, |s| moved = move_in(&mut s.llm_endpoints, &id, up))?;
    Ok(moved)
}

/// The memory strip's selector: make `id` priority 1 (adopting a local candidate if needed) and switch polish on.
#[tauri::command]
pub fn set_llm_endpoint_preferred(state: State<'_, AppState>, id: String) -> Result<(), String> {
    let candidates = privatevox_core::polish::local_candidates();
    let mut ok = false;
    edit_settings(&state, |s| {
        ok = prefer_or_adopt(&mut s.llm_endpoints, &id, &candidates);
        if ok {
            s.polish_enabled = true;
            if let Some(e) = s.llm_endpoints.iter_mut().find(|e| e.id == id) {
                e.enabled = true;
            }
        }
    })?;
    if ok {
        let p = state.polisher.clone();
        std::thread::spawn(move || p.check_due(Instant::now()));
        Ok(())
    } else {
        Err(format!("Unknown endpoint {id:?}"))
    }
}

/// Shared by the panel command and the debug script hook.
pub fn set_polish_enabled_inner(state: &AppState, enabled: bool) -> Result<(), String> {
    edit_settings(state, |s| s.polish_enabled = enabled)?;
    Ok(())
}

#[tauri::command]
pub fn set_polish_enabled(state: State<'_, AppState>, enabled: bool) -> Result<(), String> {
    set_polish_enabled_inner(&state, enabled)
}

#[tauri::command]
pub fn set_llm_timeout(state: State<'_, AppState>, secs: u64) -> Result<(), String> {
    if !(1..=60).contains(&secs) {
        return Err("Timeout must be between 1 and 60 seconds".into());
    }
    edit_settings(&state, |s| s.llm_timeout_secs = secs)?;
    Ok(())
}

#[tauri::command]
pub fn set_remove_fillers(state: State<'_, AppState>, enabled: bool) -> Result<(), String> {
    edit_settings(&state, |s| s.remove_fillers = enabled)?;
    Ok(())
}

#[tauri::command]
pub fn repolish_history(state: State<'_, AppState>, id: i64, mode: String) -> Result<(), String> {
    let mode = Mode::parse(&mode).ok_or_else(|| format!("Unknown mode {mode:?}"))?;
    state.send(Cmd::Repolish { id, mode });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use privatevox_core::polish::{local_candidates, Endpoint};

    fn ep(id: &str) -> Endpoint {
        Endpoint { id: id.into(), base_url: format!("http://10.0.0.{id}:1"), ..local_candidates().remove(0) }
    }
    fn ids(v: &[Endpoint]) -> Vec<&str> {
        v.iter().map(|e| e.id.as_str()).collect()
    }

    #[test]
    fn add_endpoint_error_copy_names_the_url() {
        assert_eq!(unreachable_message("http://10.0.0.5:1234"), "Nothing answered at http://10.0.0.5:1234. Is the server running and set to serve on the network?");
    }

    #[test]
    fn move_endpoint_reorders_user_list() {
        let mut v = vec![ep("a"), ep("b"), ep("c")];
        assert!(move_in(&mut v, "c", true));
        assert_eq!(ids(&v), ["a", "c", "b"]);
        assert!(!move_in(&mut v, "a", true), "already first");
        assert_eq!(ids(&v), ["a", "c", "b"]);
        assert!(move_in(&mut v, "a", false));
        assert_eq!(ids(&v), ["c", "a", "b"]);
        assert!(!move_in(&mut v, "zzz", true), "unknown id");
    }

    #[test]
    fn prefer_moves_to_the_top() {
        let mut v = vec![ep("a"), ep("b"), ep("c")];
        assert!(prefer(&mut v, "c"));
        assert_eq!(ids(&v), ["c", "a", "b"]);
        assert!(!prefer(&mut v, "nope"));
    }

    #[test]
    fn preferring_a_local_candidate_adds_it_to_the_user_list() {
        let mut v = vec![ep("a")];
        let cand = local_candidates().remove(1);
        assert!(prefer_or_adopt(&mut v, &cand.id, &local_candidates()));
        assert_eq!(ids(&v), [cand.id.as_str(), "a"]);
        assert_eq!(v[0].source, privatevox_core::polish::Source::Manual);
    }
}
