//! Panel commands for host mode and the remote interface (design §8.6).

use crate::state::AppState;
use privatevox_core::net::{ClientError, HistoryReply, HostClient, ModelsReply, StatusReply};
use privatevox_core::polish::normalize_base_url;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tauri::{AppHandle, State};

#[derive(serde::Serialize)]
pub struct HostStatus {
    pub enabled: bool,
    pub running: bool,
    pub port: u16,
    pub addresses: Vec<String>,
    pub clients: u32,
    pub token_set: bool,
}

#[derive(serde::Serialize)]
pub struct RemoteStatus {
    pub enabled: bool,
    pub label: String,
    pub base_url: Option<String>,
    pub token_set: bool,
    pub healthy: bool,
    pub in_use: bool,
    pub last_error: Option<String>,
    pub status: Option<StatusReply>,
    pub machine_name: String,
}

#[derive(serde::Serialize)]
pub struct NetworkStatus {
    pub host: HostStatus,
    pub remote: RemoteStatus,
}

/// Human copy for a failed host test (the panel shows it inline).
pub fn test_host_error(url: &str, e: &ClientError) -> String {
    match e {
        ClientError::Unreachable(_) => format!("Nothing answered at {url}. Is PrivateVox running there with 'Act as host' on?"),
        ClientError::Timeout => format!("{url} did not answer in time"),
        ClientError::Unauthorized => "The host wants a token; enter the one set on the PC".to_string(),
        ClientError::Server(m) => format!("{url} answered with an error: {m}"),
        ClientError::BadReply(m) => format!("{url} is not a PrivateVox host ({m})"),
    }
}

#[tauri::command]
pub fn network_status(state: State<'_, AppState>) -> Result<NetworkStatus, String> {
    let s = state.settings.lock().map(|s| s.clone()).map_err(|e| e.to_string())?;
    let (running, port, clients) = match state.host.try_lock() {
        Ok(h) => match h.as_ref() {
            Some(h) => (true, h.port(), h.clients()),
            None => (false, s.host.port, 0),
        },
        Err(_) => (s.host.enabled, s.host.port, 0),
    };
    let r = &state.remote;
    Ok(NetworkStatus {
        host: HostStatus { enabled: s.host.enabled, running, port, addresses: crate::host::lan_addresses(), clients, token_set: s.host.token.as_deref().map(|t| !t.trim().is_empty()).unwrap_or(false) },
        remote: RemoteStatus {
            enabled: s.remote.enabled,
            label: s.remote.label.clone(),
            base_url: s.remote.base_url.clone(),
            token_set: s.remote.token.as_deref().map(|t| !t.trim().is_empty()).unwrap_or(false),
            healthy: r.healthy.load(Ordering::SeqCst),
            in_use: r.in_use(),
            last_error: r.last_error.lock().ok().and_then(|e| e.clone()),
            status: r.last_status.lock().ok().and_then(|s| s.clone()),
            machine_name: crate::remote::machine_name(&s),
        },
    })
}

/// Probe a host before saving it. Runs the blocking client off the async runtime.
#[tauri::command]
pub async fn test_host(url: String, token: Option<String>) -> Result<StatusReply, String> {
    let base = normalize_base_url(&url).map_err(|e| format!("Host address: {e}"))?;
    let b2 = base.clone();
    let r = tauri::async_runtime::spawn_blocking(move || HostClient::new(&b2, token).status(Duration::from_secs(3))).await.map_err(|e| e.to_string())?;
    r.map_err(|e| test_host_error(&base, &e))
}

#[tauri::command]
pub fn set_remote(app: AppHandle, state: State<'_, AppState>, enabled: bool, base_url: Option<String>, token: Option<String>, label: Option<String>) -> Result<(), String> {
    let mut s = state.settings.lock().map(|s| s.clone()).map_err(|e| e.to_string())?;
    s.remote.enabled = enabled;
    if let Some(u) = base_url {
        s.remote.base_url = Some(u).filter(|u| !u.trim().is_empty());
    }
    if let Some(t) = token {
        s.remote.token = Some(t).filter(|t| !t.trim().is_empty());
    }
    if let Some(l) = label.map(|l| l.trim().to_string()).filter(|l| !l.is_empty()) {
        s.remote.label = l;
    }
    crate::commands::apply_settings(&state.settings, &state.paths.settings(), s.clone())?;
    crate::remote::apply_settings(&state.remote, &s);
    crate::host::apply_settings(&app, &state, &s);
    // Confirm the host right away rather than waiting for the loop.
    let st = state.remote.clone();
    std::thread::spawn(move || {
        if let Some(c) = st.client() {
            match c.status(Duration::from_secs(2)) {
                Ok(reply) => {
                    *st.last_status.lock().unwrap() = Some(reply);
                    st.healthy.store(true, Ordering::SeqCst);
                }
                Err(e) => *st.last_error.lock().unwrap() = Some(e.to_string()),
            }
        }
    });
    Ok(())
}

#[tauri::command]
pub fn set_host(app: AppHandle, state: State<'_, AppState>, enabled: bool, port: Option<u16>, token: Option<String>) -> Result<(), String> {
    let mut s = state.settings.lock().map(|s| s.clone()).map_err(|e| e.to_string())?;
    s.host.enabled = enabled;
    if let Some(p) = port {
        s.host.port = p;
    }
    if let Some(t) = token {
        s.host.token = Some(t).filter(|t| !t.trim().is_empty());
    }
    crate::commands::apply_settings(&state.settings, &state.paths.settings(), s.clone())?;
    crate::host::apply_settings(&app, &state, &s);
    let running = state.host.lock().map(|h| h.is_some()).unwrap_or(false);
    if enabled && !running {
        return Err(format!("Could not listen on port {}. Is another program using it?", s.host.port));
    }
    Ok(())
}

#[tauri::command]
pub fn set_machine_name(state: State<'_, AppState>, name: String) -> Result<(), String> {
    let mut s = state.settings.lock().map(|s| s.clone()).map_err(|e| e.to_string())?;
    s.machine_name = Some(name).map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
    crate::commands::apply_settings(&state.settings, &state.paths.settings(), s.clone())?;
    crate::remote::apply_settings(&state.remote, &s);
    Ok(())
}

/// Hosts advertising on this network right now (this machine excluded). Takes about two seconds.
#[tauri::command]
pub async fn found_hosts() -> Result<Vec<crate::discovery::FoundHost>, String> {
    let own = crate::host::lan_addresses();
    tauri::async_runtime::spawn_blocking(move || crate::discovery::browse(Duration::from_secs(2)).map(|f| crate::discovery::without_self(f, &own)))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

/// The host's history, live. Read-only on this side.
#[tauri::command]
pub async fn remote_history(state: State<'_, AppState>, limit: u32, offset: u32, search: Option<String>) -> Result<HistoryReply, String> {
    let Some(client) = state.remote.client() else { return Err("No host configured".into()) };
    let label = state.remote.label();
    tauri::async_runtime::spawn_blocking(move || client.history(limit.min(500), offset, search.as_deref(), Duration::from_secs(5)))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("{label}: {e}"))
}

#[tauri::command]
pub async fn remote_models(state: State<'_, AppState>) -> Result<ModelsReply, String> {
    let Some(client) = state.remote.client() else { return Err("No host configured".into()) };
    tauri::async_runtime::spawn_blocking(move || client.models(Duration::from_secs(3))).await.map_err(|e| e.to_string())?.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn select_remote_model(state: State<'_, AppState>, id: String) -> Result<(), String> {
    let Some(client) = state.remote.client() else { return Err("No host configured".into()) };
    tauri::async_runtime::spawn_blocking(move || client.select_model(&id, Duration::from_secs(3))).await.map_err(|e| e.to_string())?.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use privatevox_core::net::ClientError;

    #[test]
    fn status_explains_a_rejected_token() {
        assert_eq!(test_host_error("http://10.0.0.5:8765", &ClientError::Unauthorized), "The host wants a token; enter the one set on the PC");
        assert_eq!(test_host_error("http://10.0.0.5:8765", &ClientError::Timeout), "http://10.0.0.5:8765 did not answer in time");
        assert!(test_host_error("http://10.0.0.5:8765", &ClientError::Server("boom".into())).contains("boom"));
    }

    #[test]
    fn unreachable_copy_names_the_url() {
        assert_eq!(test_host_error("http://10.0.0.5:8765", &ClientError::Unreachable("connection refused".into())), "Nothing answered at http://10.0.0.5:8765. Is PrivateVox running there with 'Act as host' on?");
    }
}
