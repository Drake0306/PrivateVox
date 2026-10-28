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

fn bind(app: &AppHandle, b: &HotkeyBinding, binding: Binding, tx: &Sender<Cmd>, modifiers: &mut Vec<(rdev::Key, Binding)>) -> anyhow::Result<()> {
    if let Some(m) = b.modifier {
        modifiers.push((modifier_key(m), binding));
    }
    if let Some(c) = &b.combo {
        register_combo(app, c, binding, tx.clone())?;
    }
    Ok(())
}

/// Registers every binding from the settings. Lone modifiers start one rdev listener thread.
pub fn install(app: &AppHandle, settings: &Settings, tx: Sender<Cmd>) -> anyhow::Result<()> {
    let _ = app.global_shortcut().unregister_all();
    let mut modifiers: Vec<(rdev::Key, Binding)> = Vec::new();
    bind(app, &settings.ptt, Binding::Ptt, &tx, &mut modifiers)?;
    bind(app, &settings.toggle, Binding::Toggle, &tx, &mut modifiers)?;
    if !modifiers.is_empty() {
        if !input_monitoring_granted(true) {
            log::warn!("Input Monitoring not granted: the modifier-only hotkey will not fire until it is");
        }
        start_modifier_listener(modifiers, tx);
    }
    Ok(())
}

fn start_modifier_listener(modifiers: Vec<(rdev::Key, Binding)>, tx: Sender<Cmd>) {
    use std::sync::OnceLock;
    static STARTED: OnceLock<std::sync::Mutex<Vec<(rdev::Key, Binding)>>> = OnceLock::new();
    let slot = STARTED.get_or_init(|| std::sync::Mutex::new(Vec::new()));
    let first = slot.lock().map(|v| v.is_empty()).unwrap_or(false);
    if let Ok(mut v) = slot.lock() {
        *v = modifiers;
    }
    if !first {
        return; // the listener thread reads the current mapping on every event
    }
    std::env::set_var("KEYBOARD_ONLY", "1");
    std::thread::Builder::new()
        .name("lvw-keyhook".into())
        .spawn(move || {
            let mut down: Vec<rdev::Key> = Vec::new();
            let result = rdev::listen(move |ev| {
                let mapping = STARTED.get().and_then(|m| m.lock().ok().map(|v| v.clone())).unwrap_or_default();
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
