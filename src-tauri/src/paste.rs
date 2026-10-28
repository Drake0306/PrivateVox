//! Paste into the frontmost app: clipboard save, Cmd+V, restore (design §4.3 step 6).
//! On macOS the restore is receipt-sequenced (§18.11): the transcript is published as a lazy pasteboard promise and
//! the old clipboard comes back only after the target app actually read it. Elsewhere a fixed delay still applies.

use anyhow::{Context, Result};
use privatevox_core::settings::PasteMethod;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Sends the paste chord or types text. Abstracted so the pipeline is testable without Accessibility.
pub trait KeySender: Send {
    fn paste_chord(&mut self) -> Result<()>;
    fn type_text(&mut self, text: &str) -> Result<()>;
    /// Cmd+Z / Ctrl+Z.
    fn undo_chord(&mut self) -> Result<()> {
        anyhow::bail!("undo not supported by this sender")
    }
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

    fn undo_chord(&mut self) -> Result<()> {
        use enigo::{Direction, Key, Keyboard};
        #[cfg(target_os = "macos")]
        {
            self.enigo.key(Key::Meta, Direction::Press).map_err(|e| anyhow::anyhow!("{e:?}"))?;
            // Layout-aware: on QWERTZ the physical Z key is Y, so look the character up instead of using key code 6.
            let r = self.enigo.key(Key::Unicode('z'), Direction::Click);
            let _ = self.enigo.key(Key::Meta, Direction::Release);
            r.map_err(|e| anyhow::anyhow!("{e:?}"))?;
        }
        #[cfg(not(target_os = "macos"))]
        {
            self.enigo.key(Key::Control, Direction::Press).map_err(|e| anyhow::anyhow!("{e:?}"))?;
            let r = self.enigo.key(Key::Unicode('z'), Direction::Click);
            let _ = self.enigo.key(Key::Control, Direction::Release);
            r.map_err(|e| anyhow::anyhow!("{e:?}"))?;
        }
        Ok(())
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

/// What to do with the old clipboard right now (design §18.11).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Wait,
    Restore,
    /// A newer user copy is on the pasteboard: never clobber it.
    Leave,
    /// Nothing read the pasteboard before the deadline: the chord was swallowed. The transcript stays on the
    /// clipboard (§18.11's failure mode), so the user can paste it by hand.
    TimedOut,
}

/// Pasteboard types the promise declares: the string, plus the markers clipboard managers honour to skip it.
pub fn promised_types() -> Vec<&'static str> {
    vec!["public.utf8-plain-text", "org.nspasteboard.ConcealedType", "org.nspasteboard.TransientType"]
}

/// `read_after_chord`: when the target app read our promise (after the chord). Restore after `quiet` has passed
/// since that read, or at the `deadline` when nothing read it (the chord was swallowed); never when the change count
/// moved past ours (the user copied something meanwhile).
pub fn restore_decision(read_after_chord: Option<Instant>, now: Instant, quiet: Duration, deadline: Instant, change_count_now: i64, change_count_ours: i64) -> Decision {
    if change_count_now != change_count_ours {
        return Decision::Leave;
    }
    match read_after_chord {
        Some(t) if now.duration_since(t) >= quiet => Decision::Restore,
        Some(_) => Decision::Wait,
        None if now >= deadline => Decision::TimedOut,
        None => Decision::Wait,
    }
}

/// How long to wait after the app's read before restoring (some apps read twice).
pub const RESTORE_QUIET: Duration = Duration::from_millis(100);
/// Give up waiting for a read after this long; failure mode: the transcript stays on the clipboard.
pub const RESTORE_TIMEOUT: Duration = Duration::from_secs(3);

#[cfg(target_os = "macos")]
mod macos_receipt {
    //! The transcript as a lazy pasteboard promise: AppKit asks our owner object for the string when an app reads
    //! the pasteboard, which is the receipt that the paste happened.

    use super::{restore_decision, Decision, KeySender, PasteOutcome, RESTORE_QUIET, RESTORE_TIMEOUT};
    use anyhow::Result;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, NSObjectProtocol};
    use objc2::{define_class, msg_send, AnyThread, DefinedClass};
    use objc2_app_kit::{NSPasteboard, NSPasteboardType, NSPasteboardTypeString};
    use objc2_foundation::{NSArray, NSObject, NSString};
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    #[derive(Default)]
    pub struct Receipt {
        pub read_at: Option<Instant>,
        pub reads: u32,
    }

    pub struct PromiseIvars {
        text: String,
        receipt: Arc<Mutex<Receipt>>,
    }

    define_class!(
        // SAFETY: NSObject has no subclassing requirements and PromiseOwner does not implement Drop.
        #[unsafe(super(NSObject))]
        #[name = "PVXPasteboardPromise"]
        #[ivars = PromiseIvars]
        pub struct PromiseOwner;

        impl PromiseOwner {
            #[unsafe(method(pasteboard:provideDataForType:))]
            fn provide(&self, pasteboard: &NSPasteboard, data_type: &NSPasteboardType) {
                // The marker types carry no content; a read of the string type is the receipt.
                if data_type.to_string() == "public.utf8-plain-text" {
                    let text = NSString::from_str(&self.ivars().text);
                    let _ = pasteboard.setString_forType(&text, data_type);
                    if let Ok(mut r) = self.ivars().receipt.lock() {
                        r.read_at = Some(Instant::now());
                        r.reads += 1;
                    }
                } else {
                    let _ = pasteboard.setString_forType(&NSString::from_str(""), data_type);
                }
            }
        }

        unsafe impl NSObjectProtocol for PromiseOwner {}
    );

    impl PromiseOwner {
        fn new(text: &str, receipt: Arc<Mutex<Receipt>>) -> Retained<Self> {
            let this = Self::alloc().set_ivars(PromiseIvars { text: text.to_string(), receipt });
            unsafe { msg_send![super(this), init] }
        }
    }

    fn put_string(pb: &NSPasteboard, text: &str) -> i64 {
        let types = NSArray::from_slice(&[unsafe { NSPasteboardTypeString }]);
        let count = unsafe { pb.declareTypes_owner(&types, None) };
        let _ = pb.setString_forType(&NSString::from_str(text), unsafe { NSPasteboardTypeString });
        count as i64
    }

    /// Publish `text` as a promise, send the chord, wait for the receipt, restore `previous` (design §18.11).
    pub fn paste_with_receipt<S: KeySender>(text: &str, previous: Option<&str>, sender: &mut S) -> Result<PasteOutcome> {
        let pb = NSPasteboard::generalPasteboard();
        let receipt = Arc::new(Mutex::new(Receipt::default()));
        let owner = PromiseOwner::new(text, receipt.clone());
        let type_strings: Vec<Retained<NSString>> = super::promised_types().iter().map(|t| NSString::from_str(t)).collect();
        let type_refs: Vec<&NSString> = type_strings.iter().map(|t| t.as_ref()).collect();
        let types = NSArray::from_slice(&type_refs);
        let ours = unsafe { pb.declareTypes_owner(&types, Some(owner.as_ref() as &AnyObject)) } as i64;
        let chord_at = Instant::now();
        if let Err(e) = sender.paste_chord() {
            // No chord went out: make the transcript a real string so it survives our owner, and leave it there.
            put_string(&pb, text);
            return Ok(PasteOutcome::CopiedOnly(e.to_string()));
        }
        let deadline = chord_at + RESTORE_TIMEOUT;
        loop {
            let now = Instant::now();
            let read_after = receipt.lock().ok().and_then(|r| r.read_at).filter(|t| *t >= chord_at);
            match restore_decision(read_after, now, RESTORE_QUIET, deadline, pb.changeCount() as i64, ours) {
                Decision::Wait => std::thread::sleep(std::time::Duration::from_millis(20)),
                Decision::Restore => {
                    match previous {
                        Some(prev) => {
                            put_string(&pb, prev);
                        }
                        None => {
                            // Nothing to restore; keep the transcript readable after our owner goes away.
                            put_string(&pb, text);
                        }
                    }
                    break;
                }
                Decision::Leave => {
                    log::info!("paste: the pasteboard changed under us; leaving it alone");
                    break;
                }
                Decision::TimedOut => {
                    put_string(&pb, text);
                    log::warn!("paste: nothing read the pasteboard within {:?}; the transcript stays on the clipboard", RESTORE_TIMEOUT);
                    drop(owner);
                    return Ok(PasteOutcome::CopiedOnly(format!("nothing read the pasteboard within {} s", RESTORE_TIMEOUT.as_secs())));
                }
            }
        }
        drop(owner);
        Ok(PasteOutcome::Pasted)
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
        #[cfg(target_os = "macos")]
        {
            drop(cb);
            let outcome = macos_receipt::paste_with_receipt(text, previous.as_deref(), sender)?;
            let _ = std::fs::remove_file(&self.backup_path);
            return Ok(outcome);
        }
        #[allow(unreachable_code)]
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
            Err(e) => {
                // The transcript stays on the clipboard on purpose; nothing is pending any more.
                let _ = std::fs::remove_file(&self.backup_path);
                Ok(PasteOutcome::CopiedOnly(e.to_string()))
            }
        }
    }
}

/// At launch: if a paste was interrupted by a crash, put the previous clipboard back. Returns whether it did.
pub fn restore_backup_on_launch(path: &Path) -> Result<bool> {
    match ClipboardBackup::restore_pending(path)? {
        Some(prev) => {
            let mut cb = arboard::Clipboard::new().context("clipboard")?;
            cb.set_text(prev).context("restore clipboard")?;
            Ok(true)
        }
        None => Ok(false),
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
    use privatevox_core::settings::PasteMethod;
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
        assert!(!d.path().join("clipboard.bak").exists(), "no restore is pending once paste() has returned");
    }

    #[test]
    #[ignore = "touches the system clipboard"]
    fn launch_restore_puts_previous_clipboard_back() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("clipboard.bak");
        set_clipboard("transcript left by a crash");
        ClipboardBackup::save(&p, Some("what was there before")).unwrap();
        assert!(restore_backup_on_launch(&p).unwrap());
        assert_eq!(clipboard_text().as_deref(), Some("what was there before"));
        assert!(!p.exists());
        assert!(!restore_backup_on_launch(&p).unwrap());
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

    #[test]
    fn restores_after_read_plus_quiet() {
        use std::time::{Duration, Instant};
        let t0 = Instant::now();
        let quiet = Duration::from_millis(100);
        let deadline = t0 + Duration::from_secs(3);
        assert_eq!(restore_decision(Some(t0), t0 + Duration::from_millis(150), quiet, deadline, 7, 7), Decision::Restore);
    }

    #[test]
    fn waits_while_read_is_fresh_or_absent_before_the_deadline() {
        use std::time::{Duration, Instant};
        let t0 = Instant::now();
        let quiet = Duration::from_millis(100);
        let deadline = t0 + Duration::from_secs(3);
        assert_eq!(restore_decision(Some(t0), t0 + Duration::from_millis(50), quiet, deadline, 7, 7), Decision::Wait, "Chromium reads twice; wait out the quiet period");
        assert_eq!(restore_decision(None, t0 + Duration::from_millis(500), quiet, deadline, 7, 7), Decision::Wait);
    }

    #[test]
    fn timeout_leaves_the_transcript_when_nothing_read() {
        use std::time::{Duration, Instant};
        let t0 = Instant::now();
        let deadline = t0 + Duration::from_secs(3);
        assert_eq!(restore_decision(None, deadline, Duration::from_millis(100), deadline, 7, 7), Decision::TimedOut, "the chord was swallowed: the transcript stays on the clipboard, never stale content pasted (§18.11)");
    }

    #[test]
    fn concealment_marker_types_are_declared_with_the_string() {
        let t = promised_types();
        assert!(t.contains(&"org.nspasteboard.ConcealedType"));
        assert!(t.contains(&"org.nspasteboard.TransientType"));
    }

    #[test]
    fn never_restores_after_a_user_copy() {
        use std::time::{Duration, Instant};
        let t0 = Instant::now();
        let deadline = t0 + Duration::from_secs(3);
        assert_eq!(restore_decision(Some(t0), t0 + Duration::from_millis(500), Duration::from_millis(100), deadline, 9, 7), Decision::Leave);
        assert_eq!(restore_decision(None, deadline, Duration::from_millis(100), deadline, 9, 7), Decision::Leave);
    }
}
