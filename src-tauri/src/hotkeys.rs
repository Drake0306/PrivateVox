//! Global hotkeys: key combinations through the Tauri global-shortcut plugin, lone modifiers through a
//! low-level key hook (rdev). Both feed the activation machine on the pipeline thread.

use crate::pipeline::Cmd;
use lvw_core::hotkeys::{Binding, Input};
use lvw_core::settings::{HotkeyBinding, ModifierKey, Settings};
use std::sync::mpsc::Sender;
use tauri::AppHandle;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

fn modifier_key(m: ModifierKey) -> rdev::Key {
    match m {
        ModifierKey::RightOption => rdev::Key::AltGr,
        ModifierKey::LeftOption => rdev::Key::Alt,
        ModifierKey::RightCommand => rdev::Key::MetaRight,
        ModifierKey::RightControl => rdev::Key::ControlRight,
    }
}

/// Whether this process can see keyboard events (macOS Input Monitoring). `prompt` asks the system to show the dialog.
#[cfg(target_os = "macos")]
pub fn input_monitoring_granted(prompt: bool) -> bool {
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGPreflightListenEventAccess() -> bool;
        fn CGRequestListenEventAccess() -> bool;
    }
    unsafe {
        if CGPreflightListenEventAccess() {
            true
        } else if prompt {
            CGRequestListenEventAccess()
        } else {
            false
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub fn input_monitoring_granted(_prompt: bool) -> bool {
    true
}

fn register_combo(app: &AppHandle, combo: &str, binding: Binding, tx: Sender<Cmd>) -> anyhow::Result<()> {
    let shortcut: Shortcut = combo.parse().map_err(|e| anyhow::anyhow!("hotkey {combo:?}: {e}"))?;
    app.global_shortcut()
        .on_shortcut(shortcut, move |_app, _sc, event| {
            let input = match event.state {
                ShortcutState::Pressed => Input::Down(binding),
                ShortcutState::Released => Input::Up(binding),
            };
            let _ = tx.send(Cmd::Key(input));
        })
        .map_err(|e| anyhow::anyhow!("register {combo:?}: {e}"))?;
    log::info!("hotkey registered: {combo} -> {binding:?}");
    Ok(())
}

fn bind(app: &AppHandle, b: &HotkeyBinding, binding: Binding, tx: &Sender<Cmd>) -> anyhow::Result<()> {
    if let Some(c) = &b.combo {
        register_combo(app, c, binding, tx.clone())?;
    }
    Ok(())
}

/// The lone-modifier bindings from the settings, in the form the key hook matches on.
pub fn modifier_mapping(settings: &Settings) -> Vec<(rdev::Key, Binding)> {
    let mut m = Vec::new();
    if let Some(k) = settings.ptt.modifier {
        m.push((modifier_key(k), Binding::Ptt));
    }
    if let Some(k) = settings.toggle.modifier {
        m.push((modifier_key(k), Binding::Toggle));
    }
    m
}

/// Registers every binding from the settings. Lone modifiers go through one rdev listener thread whose
/// mapping is replaced on every install, so a removed modifier stops firing.
pub fn install(app: &AppHandle, settings: &Settings, tx: Sender<Cmd>) -> anyhow::Result<()> {
    let _ = app.global_shortcut().unregister_all();
    bind(app, &settings.ptt, Binding::Ptt, &tx)?;
    bind(app, &settings.toggle, Binding::Toggle, &tx)?;
    let modifiers = modifier_mapping(settings);
    if let Some(c) = &settings.undo.combo {
        let shortcut: Shortcut = c.parse().map_err(|e| anyhow::anyhow!("undo hotkey {c:?}: {e}"))?;
        let utx = tx.clone();
        app.global_shortcut()
            .on_shortcut(shortcut, move |_app, _sc, event| {
                if event.state == ShortcutState::Pressed {
                    let _ = utx.send(Cmd::Undo);
                }
            })
            .map_err(|e| anyhow::anyhow!("register undo {c:?}: {e}"))?;
        log::info!("hotkey registered: {c} -> Undo");
    }
    set_modifier_mapping(modifiers.clone());
    if !modifiers.is_empty() {
        if !input_monitoring_granted(true) {
            log::warn!("Input Monitoring not granted: the modifier-only hotkey will not fire until it is");
        }
        start_modifier_listener(tx);
    }
    Ok(())
}

use std::sync::OnceLock;
static MAPPING: OnceLock<std::sync::Mutex<Vec<(rdev::Key, Binding)>>> = OnceLock::new();
static LISTENER_STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn set_modifier_mapping(modifiers: Vec<(rdev::Key, Binding)>) {
    let slot = MAPPING.get_or_init(|| std::sync::Mutex::new(Vec::new()));
    if let Ok(mut v) = slot.lock() {
        *v = modifiers;
    }
}

fn current_mapping() -> Vec<(rdev::Key, Binding)> {
    MAPPING.get().and_then(|m| m.lock().ok().map(|v| v.clone())).unwrap_or_default()
}

fn start_modifier_listener(tx: Sender<Cmd>) {
    if LISTENER_STARTED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return; // the listener thread reads the current mapping on every event
    }
    std::env::set_var("KEYBOARD_ONLY", "1");
    std::thread::Builder::new()
        .name("lvw-keyhook".into())
        .spawn(move || {
            let mut down: Vec<rdev::Key> = Vec::new();
            let result = rdev::listen(move |ev| {
                let mapping = current_mapping();
                match ev.event_type {
                    rdev::EventType::KeyPress(k) => {
                        if let Some((_, b)) = mapping.iter().find(|(mk, _)| *mk == k) {
                            if !down.contains(&k) {
                                down.push(k);
                                let _ = tx.send(Cmd::Key(Input::Down(*b)));
                            }
                        }
                    }
                    rdev::EventType::KeyRelease(k) => {
                        if let Some((_, b)) = mapping.iter().find(|(mk, _)| *mk == k) {
                            down.retain(|d| *d != k);
                            let _ = tx.send(Cmd::Key(Input::Up(*b)));
                        }
                    }
                    _ => {}
                }
            });
            if let Err(e) = result {
                log::error!("key hook stopped: {e:?}");
            }
        })
        .expect("key hook thread");
}

#[cfg(test)]
mod tests {
    use super::*;
    use lvw_core::settings::{HotkeyBinding, ModifierKey, Settings};

    #[test]
    fn modifier_mapping_is_empty_when_hold_key_is_a_combo() {
        let mut s = Settings::default();
        assert_eq!(modifier_mapping(&s), vec![(rdev::Key::AltGr, Binding::Ptt)]);
        s.ptt = HotkeyBinding { combo: Some("Ctrl+Alt+Space".into()), modifier: None };
        assert!(modifier_mapping(&s).is_empty(), "switching to a combo must clear the old Right Option entry");
        s.toggle = HotkeyBinding { combo: None, modifier: Some(ModifierKey::RightCommand) };
        assert_eq!(modifier_mapping(&s), vec![(rdev::Key::MetaRight, Binding::Toggle)]);
    }
}
