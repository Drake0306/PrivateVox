//! Paste into the frontmost app: clipboard save, Cmd+V, restore after a short delay (design §4.3 step 6).
//! The receipt-sequenced restore from §18.11 replaces the fixed delay in a later plan.

use anyhow::{Context, Result};
use lvw_core::settings::PasteMethod;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Sends the paste chord or types text. Abstracted so the pipeline is testable without Accessibility.
pub trait KeySender: Send {
    fn paste_chord(&mut self) -> Result<()>;
    fn type_text(&mut self, text: &str) -> Result<()>;
}

/// Real key sender through enigo. On macOS the paste chord uses the raw key code for V (9) so it works
/// on every keyboard layout (enigo's Unicode lookup falls back to key code 0, which is A).
pub struct EnigoSender {
    enigo: enigo::Enigo,
}

impl EnigoSender {
    pub fn new() -> Result<Self> {
        let settings = enigo::Settings { open_prompt_to_get_permissions: false, ..Default::default() };
        let enigo = enigo::Enigo::new(&settings).map_err(|e| anyhow::anyhow!("enigo: {e:?}"))?;
        Ok(EnigoSender { enigo })
    }
}

impl KeySender for EnigoSender {
    fn paste_chord(&mut self) -> Result<()> {
        use enigo::{Direction, Key, Keyboard};
        #[cfg(target_os = "macos")]
        {
            self.enigo.key(Key::Meta, Direction::Press).map_err(|e| anyhow::anyhow!("{e:?}"))?;
            let r = self.enigo.key(Key::Other(9), Direction::Click);
            let _ = self.enigo.key(Key::Meta, Direction::Release);
            r.map_err(|e| anyhow::anyhow!("{e:?}"))?;
        }
        #[cfg(not(target_os = "macos"))]
        {
            self.enigo.key(Key::Control, Direction::Press).map_err(|e| anyhow::anyhow!("{e:?}"))?;
            let r = self.enigo.key(Key::Unicode('v'), Direction::Click);
            let _ = self.enigo.key(Key::Control, Direction::Release);
            r.map_err(|e| anyhow::anyhow!("{e:?}"))?;
        }
        Ok(())
    }

    fn type_text(&mut self, text: &str) -> Result<()> {
        use enigo::Keyboard;
        self.enigo.text(text).map_err(|e| anyhow::anyhow!("{e:?}"))
    }
}

/// The previous clipboard text, persisted so a crash mid-paste can still restore it on the next launch.
pub struct ClipboardBackup;

impl ClipboardBackup {
    pub fn save(path: &Path, text: Option<&str>) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        match text {
            Some(t) => std::fs::write(path, t).context("write clipboard backup")?,
            None => {
                let _ = std::fs::remove_file(path);
            }
        }
        Ok(())
    }

    /// Returns the pending backup (if any) and removes the file.
    pub fn restore_pending(path: &Path) -> Result<Option<String>> {
        match std::fs::read_to_string(path) {
            Ok(t) => {
                let _ = std::fs::remove_file(path);
                Ok(Some(t))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).context("read clipboard backup"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum PasteOutcome {
    Pasted,
    /// The chord could not be sent; the text was left on the clipboard. Carries the reason.
    CopiedOnly(String),
}

pub struct Paster {
    pub method: PasteMethod,
    pub restore_delay: Duration,
    backup_path: PathBuf,
}

impl Paster {
    pub fn new(method: PasteMethod, restore_delay: Duration, backup_path: PathBuf) -> Self {
        Paster { method, restore_delay, backup_path }
    }

    pub fn paste<S: KeySender>(&mut self, text: &str, sender: &mut S) -> Result<PasteOutcome> {
        if matches!(self.method, PasteMethod::Type) {
            return match sender.type_text(text) {
                Ok(()) => Ok(PasteOutcome::Pasted),
                Err(e) => {
                    let mut cb = arboard::Clipboard::new().context("clipboard")?;
                    cb.set_text(text.to_string()).context("set clipboard")?;
                    Ok(PasteOutcome::CopiedOnly(e.to_string()))
                }
            };
        }
        let mut cb = arboard::Clipboard::new().context("clipboard")?;
        let previous = cb.get_text().ok();
        ClipboardBackup::save(&self.backup_path, previous.as_deref())?;
        cb.set_text(text.to_string()).context("set clipboard")?;
        match sender.paste_chord() {
            Ok(()) => {
                std::thread::sleep(self.restore_delay);
                if let Some(prev) = previous {
                    // Only restore if the clipboard still holds our text; a newer user copy wins.
                    if cb.get_text().ok().as_deref() == Some(text) {
                        let _ = cb.set_text(prev);
                    }
                }
                let _ = std::fs::remove_file(&self.backup_path);
                Ok(PasteOutcome::Pasted)
            }
            Err(e) => Ok(PasteOutcome::CopiedOnly(e.to_string())),
        }
    }
}

/// Whether this process may send keystrokes (macOS Accessibility). `prompt` opens the system dialog.
#[cfg(target_os = "macos")]
pub fn accessibility_trusted(prompt: bool) -> bool {
    use std::ffi::c_void;
    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXIsProcessTrusted() -> bool;
        fn AXIsProcessTrustedWithOptions(options: *const c_void) -> bool;
    }
    if !prompt {
        return unsafe { AXIsProcessTrusted() };
    }
    use objc2_foundation::{ns_string, NSDictionary, NSNumber, NSString};
    let key: &NSString = ns_string!("AXTrustedCheckOptionPrompt");
    let value = NSNumber::new_bool(true);
    let dict = NSDictionary::from_slices::<NSString>(&[key], &[&*value]);
    let ptr: *const NSDictionary<NSString, NSNumber> = &*dict;
    unsafe { AXIsProcessTrustedWithOptions(ptr as *const c_void) }
}

#[cfg(not(target_os = "macos"))]
pub fn accessibility_trusted(_prompt: bool) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use lvw_core::settings::PasteMethod;
    use std::time::Duration;

    struct FakeSender {
        chords: usize,
        typed: Vec<String>,
        fail: bool,
    }

    impl KeySender for FakeSender {
        fn paste_chord(&mut self) -> anyhow::Result<()> {
            if self.fail {
                anyhow::bail!("not trusted for accessibility");
            }
            self.chords += 1;
            Ok(())
        }
        fn type_text(&mut self, text: &str) -> anyhow::Result<()> {
            self.typed.push(text.to_string());
            Ok(())
        }
    }

    fn clipboard_text() -> Option<String> {
        arboard::Clipboard::new().ok().and_then(|mut c| c.get_text().ok())
    }

    fn set_clipboard(text: &str) {
        arboard::Clipboard::new().unwrap().set_text(text.to_string()).unwrap();
    }

    #[test]
    #[ignore = "touches the system clipboard"]
    fn paste_sets_clipboard_sends_chord_and_restores() {
        let d = tempfile::tempdir().unwrap();
        set_clipboard("old");
        let mut p = Paster::new(PasteMethod::Clipboard, Duration::from_millis(10), d.path().join("clipboard.bak"));
        let mut s = FakeSender { chords: 0, typed: vec![], fail: false };
        let out = p.paste("new", &mut s).unwrap();
        assert!(matches!(out, PasteOutcome::Pasted));
        assert_eq!(s.chords, 1);
        assert_eq!(clipboard_text().as_deref(), Some("old"));
        assert!(!d.path().join("clipboard.bak").exists(), "backup removed after restore");
    }

    #[test]
    #[ignore = "touches the system clipboard"]
    fn paste_without_permission_keeps_text_on_clipboard() {
        let d = tempfile::tempdir().unwrap();
        set_clipboard("old");
        let mut p = Paster::new(PasteMethod::Clipboard, Duration::from_millis(10), d.path().join("clipboard.bak"));
        let mut s = FakeSender { chords: 0, typed: vec![], fail: true };
        let out = p.paste("new", &mut s).unwrap();
        assert!(matches!(out, PasteOutcome::CopiedOnly(_)));
        assert_eq!(clipboard_text().as_deref(), Some("new"));
        assert!(d.path().join("clipboard.bak").exists(), "backup kept so the old clipboard can be restored later");
    }

    #[test]
    fn backup_roundtrip_and_delete() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("clipboard.bak");
        ClipboardBackup::save(&p, Some("stash")).unwrap();
        assert_eq!(ClipboardBackup::restore_pending(&p).unwrap().as_deref(), Some("stash"));
        assert!(!p.exists());
        assert_eq!(ClipboardBackup::restore_pending(&p).unwrap(), None);
        ClipboardBackup::save(&p, None).unwrap();
        assert_eq!(ClipboardBackup::restore_pending(&p).unwrap(), None);
    }

    #[test]
    fn type_method_types_instead_of_pasting() {
        let d = tempfile::tempdir().unwrap();
        let mut p = Paster::new(PasteMethod::Type, Duration::from_millis(1), d.path().join("clipboard.bak"));
        let mut s = FakeSender { chords: 0, typed: vec![], fail: false };
        assert!(matches!(p.paste("typed text", &mut s).unwrap(), PasteOutcome::Pasted));
        assert_eq!(s.typed, vec!["typed text".to_string()]);
        assert_eq!(s.chords, 0);
    }
}
