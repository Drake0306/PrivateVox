//! User settings: defaults from the design doc, JSON persistence with atomic writes, validation.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// How the dictation key behaves (design §4.2, §18.10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Activation {
    PushToTalk,
    Toggle,
    HoldOrToggle,
}

/// A single modifier key usable as a hold-to-talk key (needs the low-level key hook).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModifierKey {
    RightOption,
    LeftOption,
    RightCommand,
    RightControl,
}

/// A hotkey: either a key combination for the global-shortcut plugin, or a lone modifier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct HotkeyBinding {
    #[serde(default)]
    pub combo: Option<String>,
    #[serde(default)]
    pub modifier: Option<ModifierKey>,
}

/// How text reaches the frontmost app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PasteMethod {
    Clipboard,
    Type,
}

/// Serve transcription and polish to other PrivateVox instances on the LAN (design §8.6).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct HostSettings {
    pub enabled: bool,
    pub port: u16,
    pub token: Option<String>,
}

impl Default for HostSettings {
    fn default() -> Self {
        HostSettings { enabled: false, port: crate::net::DEFAULT_HOST_PORT, token: None }
    }
}

/// Send dictation to a host on the LAN instead of running Whisper and the LLM here (design §8.6).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct RemoteSettings {
    pub enabled: bool,
    /// `host:port` or `http://host:port`.
    pub base_url: Option<String>,
    pub token: Option<String>,
    /// How the host is named in the pill, the strip and the history marker.
    pub label: String,
}

impl Default for RemoteSettings {
    fn default() -> Self {
        RemoteSettings { enabled: false, base_url: None, token: None, label: "PC".into() }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub activation: Activation,
    pub ptt: HotkeyBinding,
    pub toggle: HotkeyBinding,
    /// Undo the last paste (sends the app's undo keystroke within the undo window).
    pub undo: HotkeyBinding,
    pub hold_threshold_ms: u64,
    pub microphone: Option<String>,
    pub spoken_languages: Vec<String>,
    pub model_id: String,
    /// `None` means never unload.
    pub idle_unload_minutes: Option<u32>,
    pub post_release_ms: u64,
    pub mic_lazy_close_secs: u64,
    pub max_clip_seconds: u32,
    pub paste_method: PasteMethod,
    pub flash_attention: bool,
    /// Last dragged pill position in physical pixels; `None` means bottom-centre above the Dock.
    pub pill_position: Option<(i32, i32)>,
    /// Remove filler words ("uh", "hmm"; "um" only in English) before saving and before the LLM (design §18.9).
    pub remove_fillers: bool,
    /// Active polish mode (design §7). Raw and Translate skip the LLM.
    pub mode: crate::polish::Mode,
    /// Per-mode prompt overrides keyed by mode id; empty or missing means the default prompt.
    pub mode_prompts: HashMap<String, String>,
    /// Seconds to wait for the LLM before pasting the raw transcript (spec default 8).
    pub llm_timeout_secs: u64,
    /// Master switch for the LLM step; off pastes Whisper's text in every mode.
    pub polish_enabled: bool,
    /// User-added LLM endpoints in priority order (LAN boxes first, §8.2).
    pub llm_endpoints: Vec<crate::polish::Endpoint>,
    /// Probe the localhost runtimes (Ollama, LM Studio, llama.cpp) after the user's list.
    pub auto_detect_local_llm: bool,
    pub host: HostSettings,
    pub remote: RemoteSettings,
    /// How this machine introduces itself to a host; `None` means the OS hostname.
    pub machine_name: Option<String>,
    /// Names and jargon Whisper should spell right (design §18.8).
    pub dictionary: Vec<String>,
    /// Fuzzy-correction threshold (normalised edit distance); 0 switches the safety net off.
    pub dictionary_strictness: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            activation: Activation::PushToTalk,
            ptt: HotkeyBinding { combo: None, modifier: Some(ModifierKey::RightOption) },
            toggle: HotkeyBinding { combo: Some("Ctrl+Shift+Space".to_string()), modifier: None },
            undo: HotkeyBinding { combo: Some("Ctrl+Shift+Z".to_string()), modifier: None },
            hold_threshold_ms: 300,
            microphone: None,
            spoken_languages: vec!["en".to_string()],
            model_id: "large-v3-turbo".to_string(),
            idle_unload_minutes: Some(10),
            post_release_ms: 150,
            mic_lazy_close_secs: 60,
            max_clip_seconds: 300,
            paste_method: PasteMethod::Clipboard,
            flash_attention: true,
            pill_position: None,
            remove_fillers: true,
            mode: crate::polish::Mode::Clean,
            mode_prompts: HashMap::new(),
            llm_timeout_secs: 8,
            polish_enabled: true,
            llm_endpoints: Vec::new(),
            auto_detect_local_llm: true,
            host: HostSettings::default(),
            remote: RemoteSettings::default(),
            machine_name: None,
            dictionary: Vec::new(),
            dictionary_strictness: 0.18,
        }
    }
}

impl Settings {
    /// Loads settings from `path`; a missing file yields the defaults. Unknown keys are ignored.
    pub fn load(path: &Path) -> Result<Settings> {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).with_context(|| format!("parse {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
            Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
        }
    }

    /// Writes to a sibling temp file and renames it over `path`, so a crash never leaves a half-written file.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, path).with_context(|| format!("rename to {}", path.display()))?;
        Ok(())
    }

    /// Range checks for values the UI lets the user type.
    pub fn validate(&self) -> std::result::Result<(), String> {
        if let Some(m) = self.idle_unload_minutes {
            if !(1..=240).contains(&m) {
                return Err("Idle unload must be between 1 and 240 minutes, or Never".into());
            }
        }
        if !(100..=2000).contains(&self.hold_threshold_ms) {
            return Err("Hold threshold must be between 100 and 2000 ms".into());
        }
        if !(10..=600).contains(&self.max_clip_seconds) {
            return Err("Maximum clip length must be between 10 and 600 seconds".into());
        }
        if self.spoken_languages.is_empty() {
            return Err("Pick at least one spoken language".into());
        }
        if self.remote.enabled {
            let url = self.remote.base_url.as_deref().map(str::trim).filter(|u| !u.is_empty()).ok_or_else(|| "Enter the host's address, like 192.168.1.20:8765".to_string())?;
            crate::polish::normalize_base_url(url).map_err(|e| format!("Host address: {e}"))?;
        }
        if self.host.enabled && self.host.port < 1024 {
            return Err("Host port must be 1024 or higher".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp() -> (tempfile::TempDir, PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("settings.json");
        (d, p)
    }

    #[test]
    fn default_matches_spec() {
        let s = Settings::default();
        assert!(matches!(s.activation, Activation::PushToTalk));
        assert_eq!(s.ptt.combo, None);
        assert!(matches!(s.ptt.modifier, Some(ModifierKey::RightOption)));
        assert_eq!(s.toggle.combo.as_deref(), Some("Ctrl+Shift+Space"));
        assert_eq!(s.toggle.modifier, None);
        assert_eq!(s.undo.combo.as_deref(), Some("Ctrl+Shift+Z"));
        assert_eq!(s.hold_threshold_ms, 300);
        assert_eq!(s.microphone, None);
        assert_eq!(s.spoken_languages, vec!["en".to_string()]);
        assert_eq!(s.model_id, "large-v3-turbo");
        assert_eq!(s.idle_unload_minutes, Some(10));
        assert_eq!(s.post_release_ms, 150);
        assert_eq!(s.mic_lazy_close_secs, 60);
        assert_eq!(s.max_clip_seconds, 300);
        assert!(matches!(s.paste_method, PasteMethod::Clipboard));
        assert!(s.flash_attention);
        assert_eq!(s.pill_position, None);
    }

    #[test]
    fn load_missing_file_returns_default() {
        let (_d, p) = tmp();
        let s = Settings::load(&p).unwrap();
        assert_eq!(s, Settings::default());
    }

    #[test]
    fn save_then_load_roundtrips() {
        let (_d, p) = tmp();
        let mut s = Settings::default();
        s.idle_unload_minutes = None;
        s.spoken_languages = vec!["en".into(), "hi".into()];
        s.save(&p).unwrap();
        assert!(!p.with_extension("json.tmp").exists(), "temp file must be renamed away");
        assert_eq!(Settings::load(&p).unwrap(), s);
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let (_d, p) = tmp();
        std::fs::write(&p, r#"{"model_id":"large-v3","future_field":123}"#).unwrap();
        let s = Settings::load(&p).unwrap();
        assert_eq!(s.model_id, "large-v3");
        assert_eq!(s.hold_threshold_ms, 300, "missing fields take defaults");
    }

    #[test]
    fn validate_rejects_idle_minutes_out_of_range() {
        let mut s = Settings::default();
        s.idle_unload_minutes = Some(0);
        assert!(s.validate().is_err());
        s.idle_unload_minutes = Some(241);
        assert!(s.validate().is_err());
        s.idle_unload_minutes = Some(1);
        assert!(s.validate().is_ok());
        s.idle_unload_minutes = Some(240);
        assert!(s.validate().is_ok());
        s.hold_threshold_ms = 50;
        assert!(s.validate().is_err());
        s.hold_threshold_ms = 300;
        s.max_clip_seconds = 5;
        assert!(s.validate().is_err());
    }

    #[test]
    fn host_and_remote_defaults_are_off() {
        let s = Settings::default();
        assert!(!s.host.enabled);
        assert_eq!(s.host.port, 8765);
        assert!(s.host.token.is_none());
        assert!(!s.remote.enabled);
        assert!(s.remote.base_url.is_none());
        assert_eq!(s.remote.label, "PC");
        assert!(s.machine_name.is_none());
    }

    #[test]
    fn validate_rejects_bad_remote_url_when_enabled() {
        let mut s = Settings::default();
        s.remote.enabled = true;
        assert!(s.validate().unwrap_err().contains("address"));
        s.remote.base_url = Some("192.168.1.20:8765".into());
        assert!(s.validate().is_ok());
        s.host.enabled = true;
        s.host.port = 80;
        assert!(s.validate().unwrap_err().contains("port"));
        s.host.port = 8765;
        assert!(s.validate().is_ok());
    }
}
