//! Real implementations of the pipeline's capture and paste traits.

use crate::paste::{EnigoSender, KeySender, PasteOutcome, Paster};
use crate::pipeline::{CaptureSource, PasteSink};
use privatevox_core::audio::Capture;
use privatevox_core::settings::Settings;
use std::sync::{Arc, Mutex};

/// Opens the microphone from the current settings on each start.
pub struct MicCapture {
    settings: Arc<Mutex<Settings>>,
    stream: Option<Capture>,
}

impl MicCapture {
    pub fn new(settings: Arc<Mutex<Settings>>) -> Self {
        MicCapture { settings, stream: None }
    }
}

impl CaptureSource for MicCapture {
    fn start(&mut self, on_pcm: Box<dyn FnMut(&[f32]) + Send>, on_error: Box<dyn FnMut(String) + Send>) -> anyhow::Result<()> {
        let device = self.settings.lock().ok().and_then(|s| s.microphone.clone());
        self.stream = Some(Capture::start_with_errors(device.as_deref(), on_pcm, on_error)?);
        Ok(())
    }

    fn stop(&mut self) {
        if let Some(s) = self.stream.take() {
            s.stop();
        }
    }
}

/// Pastes through enigo; creates the sender lazily so a missing Accessibility grant is reported per paste.
pub struct SystemPaste {
    paster: Paster,
    sender: Option<EnigoSender>,
}

impl SystemPaste {
    pub fn new(paster: Paster) -> Self {
        SystemPaste { paster, sender: None }
    }
}

impl PasteSink for SystemPaste {
    fn paste(&mut self, text: &str, method: privatevox_core::settings::PasteMethod) -> anyhow::Result<PasteOutcome> {
        // The coordinator picks the method per paste (per-app table, else the live setting).
        self.paster.method = method;
        // Re-check on every paste: a revoked Accessibility grant must not turn into a silent "pasted".
        if matches!(method, privatevox_core::settings::PasteMethod::Clipboard) && !crate::paste::accessibility_trusted(false) {
            let mut cb = arboard::Clipboard::new()?;
            cb.set_text(text.to_string())?;
            return Ok(PasteOutcome::CopiedOnly("Accessibility not granted".into()));
        }
        if self.sender.is_none() {
            match EnigoSender::new() {
                Ok(s) => self.sender = Some(s),
                Err(e) => {
                    // Leave the text on the clipboard so nothing is lost.
                    let mut cb = arboard::Clipboard::new()?;
                    cb.set_text(text.to_string())?;
                    return Ok(PasteOutcome::CopiedOnly(format!("keystrokes unavailable: {e}")));
                }
            }
        }
        let sender = self.sender.as_mut().expect("sender");
        self.paster.paste(text, sender)
    }

    fn undo(&mut self) -> anyhow::Result<()> {
        if !crate::paste::accessibility_trusted(false) {
            anyhow::bail!("Accessibility not granted");
        }
        if self.sender.is_none() {
            self.sender = Some(EnigoSender::new()?);
        }
        self.sender.as_mut().expect("sender").undo_chord()
    }

    fn copy_only(&mut self, text: &str) -> anyhow::Result<()> {
        let mut cb = arboard::Clipboard::new()?;
        cb.set_text(text.to_string())?;
        Ok(())
    }
}

/// Convenience for tests and the debug command: a sender that never presses keys.
pub struct NoKeys;
impl KeySender for NoKeys {
    fn paste_chord(&mut self) -> anyhow::Result<()> {
        anyhow::bail!("no key sender")
    }
    fn type_text(&mut self, _text: &str) -> anyhow::Result<()> {
        anyhow::bail!("no key sender")
    }
}
