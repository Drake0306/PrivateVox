//! macOS Secure Event Input detection (design §18.12): while a password field (or Terminal's Secure Keyboard Entry)
//! holds secure input, event-tap hotkeys such as the modifier-only hold key may not fire. We poll and warn.
//! The module is empty on other platforms; the poller is only started on macOS.
#![cfg(target_os = "macos")]

use crate::pipeline::Cmd;
use std::sync::mpsc::Sender;
use std::time::Duration;

pub fn secure_input_on() -> bool {
    #[link(name = "Carbon", kind = "framework")]
    extern "C" {
        fn IsSecureEventInputEnabled() -> bool;
    }
    unsafe { IsSecureEventInputEnabled() }
}

/// Polls every 2 s and reports changes to the coordinator, which shows or clears the pill warning.
pub fn spawn_watch(tx: Sender<Cmd>) {
    std::thread::Builder::new()
        .name("privatevox-secure-input".into())
        .spawn(move || {
            let mut last = false;
            loop {
                let now = secure_input_on();
                if now != last {
                    last = now;
                    log::info!("secure input {}", if now { "on: the hold key may not fire" } else { "off" });
                    if tx.send(Cmd::SecureInput(now)).is_err() {
                        break;
                    }
                }
                std::thread::sleep(Duration::from_secs(2));
            }
        })
        .expect("secure input thread");
}
