//! Real implementations of the pipeline's capture and paste traits.

use crate::paste::{EnigoSender, KeySender, PasteOutcome, Paster};
use crate::pipeline::{CaptureSource, PasteSink};
use privatevox_core::audio::Capture;
use privatevox_core::settings::Settings;
use privatevox_core::warmmic::{Start, Stop, WarmMic};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

type PcmSink = Arc<Mutex<Option<Box<dyn FnMut(&[f32]) + Send>>>>;
type ErrSink = Arc<Mutex<Option<Box<dyn FnMut(String) + Send>>>>;

/// The open stream and its timing state behind one lock, so the close timer and a new start never race.
struct MicInner {
    stream: Option<Capture>,
    /// The device the open stream was opened with; a changed setting means a fresh open.
    device: Option<String>,
    warm: WarmMic,
}

/// Opens the microphone from the current settings. After a stop the stream stays open with delivery paused for
/// `mic_warm_secs` (design §18.3); a start inside that window reuses it, otherwise a timer closes it.
pub struct MicCapture {
    settings: Arc<Mutex<Settings>>,
    inner: Arc<Mutex<MicInner>>,
    /// Where the open stream delivers; `None` while no dictation or mic test is running.
    pcm: PcmSink,
    err: ErrSink,
    /// Bumped on every stop; a timer whose generation is stale does nothing.
    timer_gen: Arc<AtomicU64>,
    /// Set from the stream's error callback: the open stream is dead and must not be kept warm or reused.
    broken: Arc<AtomicBool>,
}

impl MicCapture {
    pub fn new(settings: Arc<Mutex<Settings>>) -> Self {
        MicCapture {
            settings,
            inner: Arc::new(Mutex::new(MicInner { stream: None, device: None, warm: WarmMic::new() })),
            pcm: Arc::new(Mutex::new(None)),
            err: Arc::new(Mutex::new(None)),
            timer_gen: Arc::new(AtomicU64::new(0)),
            broken: Arc::new(AtomicBool::new(false)),
        }
    }

    fn close(inner: &mut MicInner, why: &str) {
        if let Some(s) = inner.stream.take() {
            s.stop();
            log::info!("mic: closed ({why})");
        }
        inner.warm.mark_closed();
    }
}

impl CaptureSource for MicCapture {
    fn start(&mut self, on_pcm: Box<dyn FnMut(&[f32]) + Send>, on_error: Box<dyn FnMut(String) + Send>) -> anyhow::Result<()> {
        let device = self.settings.lock().ok().and_then(|s| s.microphone.clone());
        let mut inner = self.inner.lock().map_err(|_| anyhow::anyhow!("mic state poisoned"))?;
        if inner.stream.is_some() && inner.device != device {
            Self::close(&mut inner, "input device changed");
        }
        if inner.stream.is_some() && self.broken.load(Ordering::SeqCst) {
            Self::close(&mut inner, "stream had failed");
        }
        let reuse = matches!(inner.warm.on_start(), Start::Reuse) && inner.stream.is_some();
        if reuse {
            log::info!("mic: reused warm stream");
        } else {
            let (pcm, err, broken) = (self.pcm.clone(), self.err.clone(), self.broken.clone());
            let forward_pcm: Box<dyn FnMut(&[f32]) + Send> = Box::new(move |data| {
                if let Ok(mut sink) = pcm.lock() {
                    if let Some(f) = sink.as_mut() {
                        f(data);
                    }
                }
            });
            let forward_err: Box<dyn FnMut(String) + Send> = Box::new(move |msg| {
                // A fatal stream error: whoever stops or starts next must open a fresh stream, never reuse this one.
                broken.store(true, Ordering::SeqCst);
                if let Ok(mut sink) = err.lock() {
                    if let Some(f) = sink.as_mut() {
                        f(msg);
                    }
                }
            });
            match Capture::start_with_errors(device.as_deref(), forward_pcm, forward_err) {
                Ok(c) => {
                    self.broken.store(false, Ordering::SeqCst);
                    inner.stream = Some(c);
                    inner.device = device;
                }
                Err(e) => {
                    inner.warm.mark_closed();
                    return Err(e);
                }
            }
        }
        drop(inner);
        if let Ok(mut s) = self.pcm.lock() {
            *s = Some(on_pcm);
        }
        if let Ok(mut s) = self.err.lock() {
            *s = Some(on_error);
        }
        Ok(())
    }

    fn stop(&mut self) {
        // Pause delivery first: the stream keeps running but nobody receives frames.
        if let Ok(mut s) = self.pcm.lock() {
            *s = None;
        }
        if let Ok(mut s) = self.err.lock() {
            *s = None;
        }
        let lazy = Duration::from_secs(self.settings.lock().map(|s| s.mic_warm_secs).unwrap_or(0));
        let Ok(mut inner) = self.inner.lock() else { return };
        if self.broken.load(Ordering::SeqCst) {
            // The stream reported a fatal error during this run: close it now rather than keep a dead stream warm.
            Self::close(&mut inner, "stream failed");
            return;
        }
        match inner.warm.on_stop(Instant::now(), lazy) {
            Stop::Close => Self::close(&mut inner, "lazy close off"),
            Stop::Keep => {
                let gen = self.timer_gen.fetch_add(1, Ordering::SeqCst) + 1;
                let (shared, tg) = (self.inner.clone(), self.timer_gen.clone());
                let spawned = std::thread::Builder::new().name("privatevox-mic-close".into()).spawn(move || {
                    std::thread::sleep(lazy);
                    if tg.load(Ordering::SeqCst) != gen {
                        return; // a later stop re-armed the window
                    }
                    if let Ok(mut inner) = shared.lock() {
                        if inner.warm.due(Instant::now()) {
                            Self::close(&mut inner, &format!("{} s idle", lazy.as_secs()));
                        }
                    }
                });
                if spawned.is_err() {
                    Self::close(&mut inner, "no timer thread");
                }
            }
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
        // Windows (§12): keystrokes into a window of an elevated process are dropped by UIPI, typed or chorded. Leave
        // the text on the clipboard and say so, instead of a silent "pasted" that never arrived.
        if crate::elevation::foreground_blocks_paste() {
            let mut cb = arboard::Clipboard::new()?;
            cb.set_text(text.to_string())?;
            return Ok(PasteOutcome::CopiedOnly("the app in front runs as administrator".into()));
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
