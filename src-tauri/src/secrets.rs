//! Tokens out of settings.json (design §8.5): the host token, the remote host's token and every LLM endpoint token
//! are kept in the OS keychain (macOS Keychain, Windows Credential Manager) and the file carries `@keychain` in
//! their place. In memory the settings always hold the real values; only the file on disk is redacted.
//! Linux has no store here yet: tokens stay in the file, as before.

use privatevox_core::settings::Settings;
use std::path::Path;

/// The placeholder written to settings.json where a token was moved to the keychain.
pub const PLACEHOLDER: &str = "@keychain";

/// Keychain service name; entries are keyed `host`, `remote`, `endpoint:<id>`.
pub const SERVICE: &str = "com.drake0306.privatevox";

pub trait SecretStore: Send + Sync {
    fn get(&self, key: &str) -> Option<String>;
    fn set(&self, key: &str, value: &str) -> anyhow::Result<()>;
    fn delete(&self, key: &str) -> anyhow::Result<()>;
}

/// In-memory store for tests and as the fallback where no keychain is wired.
#[derive(Default)]
pub struct MemoryStore(std::sync::Mutex<std::collections::HashMap<String, String>>);

impl SecretStore for MemoryStore {
    fn get(&self, key: &str) -> Option<String> {
        self.0.lock().ok()?.get(key).cloned()
    }
    fn set(&self, key: &str, value: &str) -> anyhow::Result<()> {
        self.0.lock().map_err(|_| anyhow::anyhow!("poisoned"))?.insert(key.into(), value.into());
        Ok(())
    }
    fn delete(&self, key: &str) -> anyhow::Result<()> {
        self.0.lock().map_err(|_| anyhow::anyhow!("poisoned"))?.remove(key);
        Ok(())
    }
}

/// A store that keeps nothing: `stash` leaves the tokens in the file (Linux today).
pub struct NoStore;

impl SecretStore for NoStore {
    fn get(&self, _key: &str) -> Option<String> {
        None
    }
    fn set(&self, _key: &str, _value: &str) -> anyhow::Result<()> {
        anyhow::bail!("no keychain on this platform")
    }
    fn delete(&self, _key: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

/// The OS keychain through the `keyring` crate.
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub struct KeyringStore;

#[cfg(any(target_os = "macos", target_os = "windows"))]
impl SecretStore for KeyringStore {
    fn get(&self, key: &str) -> Option<String> {
        keyring::Entry::new(SERVICE, key).ok()?.get_password().ok()
    }
    fn set(&self, key: &str, value: &str) -> anyhow::Result<()> {
        keyring::Entry::new(SERVICE, key)?.set_password(value)?;
        Ok(())
    }
    fn delete(&self, key: &str) -> anyhow::Result<()> {
        match keyring::Entry::new(SERVICE, key)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

/// The store for this platform.
pub fn platform_store() -> Box<dyn SecretStore> {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        Box::new(KeyringStore)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Box::new(NoStore)
    }
}

fn slots(settings: &mut Settings) -> Vec<(String, &mut Option<String>)> {
    let mut v: Vec<(String, &mut Option<String>)> = vec![("host".to_string(), &mut settings.host.token), ("remote".to_string(), &mut settings.remote.token)];
    for e in settings.llm_endpoints.iter_mut() {
        v.push((format!("endpoint:{}", e.id), &mut e.token));
    }
    v
}

/// Moves every token into `store` and leaves `@keychain` in its place. A token the store refuses stays in the
/// struct (and so in the file) with a warning, so a broken keychain never loses a token. An empty or cleared slot
/// removes its entry.
pub fn stash(settings: &mut Settings, store: &dyn SecretStore) {
    for (key, slot) in slots(settings) {
        match slot.as_deref().map(str::trim) {
            Some(PLACEHOLDER) => {}
            Some(value) if !value.is_empty() => match store.set(&key, value) {
                Ok(()) => *slot = Some(PLACEHOLDER.to_string()),
                Err(e) => log::warn!("keychain: {key} stays in settings.json: {e}"),
            },
            _ => {
                let _ = store.delete(&key);
                *slot = None;
            }
        }
    }
}

/// Replaces every `@keychain` placeholder with the stored token; a placeholder with no entry becomes `None`.
pub fn unstash(settings: &mut Settings, store: &dyn SecretStore) {
    for (key, slot) in slots(settings) {
        if slot.as_deref().map(str::trim) == Some(PLACEHOLDER) {
            *slot = store.get(&key).filter(|v| !v.is_empty());
            if slot.is_none() {
                log::warn!("keychain: no entry for {key}; the token must be entered again");
            }
        }
    }
}

/// Writes `settings` to `path` with the tokens stashed; the caller's copy keeps the real values.
pub fn save(settings: &Settings, path: &Path, store: &dyn SecretStore) -> anyhow::Result<()> {
    let mut redacted = settings.clone();
    stash(&mut redacted, store);
    redacted.save(path)
}

/// Loads `path` and fills the tokens back in from the store.
pub fn load(path: &Path, store: &dyn SecretStore) -> anyhow::Result<Settings> {
    let mut s = Settings::load(path)?;
    unstash(&mut s, store);
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use privatevox_core::polish::{Endpoint, Flavor, Source};

    fn with_tokens() -> Settings {
        let mut s = Settings::default();
        s.host.token = Some("host-secret".into());
        s.remote.token = Some("remote-secret".into());
        s.llm_endpoints = vec![
            Endpoint { id: "ep-1".into(), label: "PC".into(), base_url: "http://192.168.1.20:1234".into(), model: None, token: Some("ep-secret".into()), flavor: Flavor::OpenAi, source: Source::Manual, enabled: true },
            Endpoint { id: "ep-2".into(), label: "Local".into(), base_url: "http://127.0.0.1:11434".into(), model: None, token: None, flavor: Flavor::Ollama, source: Source::Manual, enabled: true },
        ];
        s
    }

    #[test]
    fn tokens_leave_settings_json_and_come_back() {
        let store = MemoryStore::default();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let original = with_tokens();
        save(&original, &path, &store).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        for secret in ["host-secret", "remote-secret", "ep-secret"] {
            assert!(!text.contains(secret), "{secret} must not be on disk: {text}");
        }
        assert_eq!(text.matches(PLACEHOLDER).count(), 3, "three placeholders: {text}");
        assert_eq!(store.get("host").as_deref(), Some("host-secret"));
        assert_eq!(store.get("remote").as_deref(), Some("remote-secret"));
        assert_eq!(store.get("endpoint:ep-1").as_deref(), Some("ep-secret"));
        assert!(store.get("endpoint:ep-2").is_none(), "an endpoint without a token gets no entry");

        let back = load(&path, &store).unwrap();
        assert_eq!(back, original, "the in-memory settings are whole again");

        // Clearing a token removes its entry and leaves nothing behind.
        let mut cleared = original.clone();
        cleared.host.token = None;
        save(&cleared, &path, &store).unwrap();
        assert!(store.get("host").is_none());
        assert_eq!(load(&path, &store).unwrap().host.token, None);

        // A placeholder whose entry vanished (new machine, keychain reset) loads as no token rather than "@keychain".
        store.delete("remote").unwrap();
        assert_eq!(load(&path, &store).unwrap().remote.token, None);
    }

    #[test]
    fn a_store_that_refuses_keeps_the_token_in_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let original = with_tokens();
        save(&original, &path, &NoStore).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("host-secret"), "no keychain: the token stays where it was");
        assert!(!text.contains(PLACEHOLDER));
        assert_eq!(load(&path, &NoStore).unwrap(), original);
    }

    #[test]
    fn stash_is_idempotent_on_already_stashed_settings() {
        let store = MemoryStore::default();
        let mut s = with_tokens();
        stash(&mut s, &store);
        let once = s.clone();
        stash(&mut s, &store);
        assert_eq!(s, once);
        assert_eq!(store.get("host").as_deref(), Some("host-secret"), "a second stash never overwrites the entry with the placeholder");
    }
}
