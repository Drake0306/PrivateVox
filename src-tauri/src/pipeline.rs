//! The pipeline coordinator: hotkey → capture → VAD → engine → clean-up → paste → history, on one thread.
//! Generic over the engine, capture source and paste sink so it runs under test without hardware.

use crate::paste::PasteOutcome;
use lvw_core::audio::{level_db, ClipBuffer, SAMPLE_RATE};
use lvw_core::settings::Settings;
use lvw_core::store::{NewDictation, Store};
use lvw_core::textnorm::normalize;
use lvw_core::vad::{is_too_short, pad_min, ClipClass, Vad, VadConfig};
use lvw_engine::protocol::DecodeOptions;
use lvw_engine::supervisor::Engine;
use serde::Serialize;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// What the pill shows. Emitted as the Tauri event `pill-state`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PillState {
    Idle { hotkey_hint: String },
    Listening { seconds: f32, level_db: f32 },
    Transcribing,
    LoadingModel { progress: Option<f32> },
    Pasted { preview: String },
    Error { message: String },
}

impl PillState {
    pub fn kind(&self) -> &'static str {
        match self {
            PillState::Idle { .. } => "idle",
            PillState::Listening { .. } => "listening",
            PillState::Transcribing => "transcribing",
            PillState::LoadingModel { .. } => "loading_model",
            PillState::Pasted { .. } => "pasted",
            PillState::Error { .. } => "error",
        }
    }
}

/// Commands the coordinator thread processes in order.
#[derive(Debug)]
pub enum Cmd {
    Start,
    Stop,
    Cancel,
    /// The default model file is now on disk.
    ModelReady(PathBuf),
    /// Periodic tick while recording (level meter, cap) and after a paste (back to idle).
    Tick(u64),
    /// Settings changed; re-read what the pipeline caches.
    SettingsChanged,
    Quit,
}

pub trait CaptureSource: Send {
    fn start(&mut self, on_pcm: Box<dyn FnMut(&[f32]) + Send>) -> anyhow::Result<()>;
    fn stop(&mut self);
}

pub trait PasteSink: Send {
    fn paste(&mut self, text: &str) -> anyhow::Result<PasteOutcome>;
}

pub struct Deps {
    pub engine: Box<dyn Engine>,
    pub capture: Box<dyn CaptureSource>,
    pub paste: Box<dyn PasteSink>,
    pub model_path: Option<PathBuf>,
    pub frontmost: Box<dyn Fn() -> Option<String> + Send>,
}

struct Recording {
    buffer: Arc<Mutex<ClipBuffer>>,
    level: Arc<Mutex<f32>>,
    started: Instant,
}

pub struct Coordinator {
    deps: Deps,
    settings: Arc<Mutex<Settings>>,
    store: Arc<Mutex<Store>>,
    vad: Vad,
    emit: Box<dyn Fn(PillState) + Send>,
    recording: Option<Recording>,
    /// Clip waiting for the model file to appear.
    queued: Option<Vec<f32>>,
    generation: u64,
}

fn hotkey_hint(s: &Settings) -> String {
    match (&s.ptt.modifier, &s.ptt.combo) {
        (Some(lvw_core::settings::ModifierKey::RightOption), _) => "Hold right ⌥ to talk".into(),
        (Some(lvw_core::settings::ModifierKey::LeftOption), _) => "Hold left ⌥ to talk".into(),
        (Some(lvw_core::settings::ModifierKey::RightCommand), _) => "Hold right ⌘ to talk".into(),
        (Some(lvw_core::settings::ModifierKey::RightControl), _) => "Hold right ⌃ to talk".into(),
        (None, Some(c)) => format!("Hold {c} to talk"),
        (None, None) => "LVW".into(),
    }
}

impl Coordinator {
    pub fn new(deps: Deps, settings: Arc<Mutex<Settings>>, store: Arc<Mutex<Store>>, vad: Vad, emit: Box<dyn Fn(PillState) + Send>) -> Self {
        Coordinator { deps, settings, store, vad, emit, recording: None, queued: None, generation: 0 }
    }

    fn settings(&self) -> Settings {
        self.settings.lock().map(|s| s.clone()).unwrap_or_default()
    }

    fn idle(&self) {
        (self.emit)(PillState::Idle { hotkey_hint: hotkey_hint(&self.settings()) });
    }

    fn error(&self, message: &str) {
        log::warn!("pipeline: {message}");
        (self.emit)(PillState::Error { message: message.to_string() });
    }

    /// Runs until `Cmd::Quit`. `tx` lets the coordinator schedule its own ticks.
    pub fn run(mut self, rx: Receiver<Cmd>, tx: Sender<Cmd>) {
        self.idle();
        while let Ok(cmd) = rx.recv() {
            match cmd {
                Cmd::Start => self.start(&tx),
                Cmd::Stop => self.stop(&tx),
                Cmd::Cancel => self.cancel(),
                Cmd::ModelReady(p) => {
                    self.deps.model_path = Some(p);
                    if let Some(pcm) = self.queued.take() {
                        self.decode_and_paste(pcm, &tx);
                    } else {
                        self.idle();
                    }
                }
                Cmd::Tick(gen) => self.tick(gen, &tx),
                Cmd::SettingsChanged => {
                    if self.recording.is_none() {
                        self.idle();
                    }
                }
                Cmd::Quit => break,
            }
        }
        if self.recording.is_some() {
            self.deps.capture.stop();
        }
        let _ = self.deps.engine.unload();
    }

    fn start(&mut self, tx: &Sender<Cmd>) {
        if self.recording.is_some() {
            return;
        }
        let settings = self.settings();
        let buffer = Arc::new(Mutex::new(ClipBuffer::new(settings.max_clip_seconds)));
        let level = Arc::new(Mutex::new(-100.0f32));
        let (b, l, t) = (buffer.clone(), level.clone(), tx.clone());
        let on_pcm: Box<dyn FnMut(&[f32]) + Send> = Box::new(move |pcm| {
            if let Ok(mut lv) = l.lock() {
                *lv = level_db(pcm);
            }
            let full = match b.lock() {
                Ok(mut buf) => !buf.push(pcm),
                Err(_) => false,
            };
            if full {
                let _ = t.send(Cmd::Stop);
            }
        });
        (self.emit)(PillState::Listening { seconds: 0.0, level_db: -100.0 });
        match self.deps.capture.start(on_pcm) {
            Ok(()) => {
                self.recording = Some(Recording { buffer, level, started: Instant::now() });
                self.generation += 1;
                schedule(tx, Cmd::Tick(self.generation), Duration::from_millis(100));
            }
            Err(e) => {
                self.error(&format!("Microphone unavailable: {e}"));
                self.schedule_idle(tx);
            }
        }
    }

    fn tick(&mut self, gen: u64, tx: &Sender<Cmd>) {
        if gen != self.generation {
            return;
        }
        match &self.recording {
            Some(rec) => {
                let seconds = rec.started.elapsed().as_secs_f32();
                let level = rec.level.lock().map(|l| *l).unwrap_or(-100.0);
                (self.emit)(PillState::Listening { seconds, level_db: level });
                schedule(tx, Cmd::Tick(gen), Duration::from_millis(100));
            }
            None => self.idle(),
        }
    }

    fn schedule_idle(&mut self, tx: &Sender<Cmd>) {
        self.generation += 1;
        schedule(tx, Cmd::Tick(self.generation), Duration::from_millis(2000));
    }

    fn cancel(&mut self) {
        if self.recording.take().is_some() {
            self.deps.capture.stop();
        }
        self.queued = None;
        self.idle();
    }

    fn stop(&mut self, tx: &Sender<Cmd>) {
        let Some(rec) = self.recording.take() else { return };
        let settings = self.settings();
        if settings.post_release_ms > 0 {
            std::thread::sleep(Duration::from_millis(settings.post_release_ms));
        }
        self.deps.capture.stop();
        self.generation += 1;
        let pcm = rec.buffer.lock().map(|mut b| b.take()).unwrap_or_default();
        if is_too_short(&pcm, 300) {
            log::info!("pipeline: clip under 300 ms discarded");
            self.idle();
            return;
        }
        self.decode_and_paste(pcm, tx);
    }

    fn decode_and_paste(&mut self, pcm: Vec<f32>, tx: &Sender<Cmd>) {
        let t_total = Instant::now();
        let settings = self.settings();
        let capture_ms = (pcm.len() as f32 / SAMPLE_RATE as f32 * 1000.0) as u64;

        let t = Instant::now();
        let speech = match self.vad.classify(&pcm, &VadConfig::default()) {
            Ok(ClipClass::Silent) => {
                self.error("Microphone is silent");
                self.schedule_idle(tx);
                return;
            }
            Ok(ClipClass::NothingHeard) => {
                self.error("Nothing heard");
                self.schedule_idle(tx);
                return;
            }
            Ok(ClipClass::Speech(s)) => s,
            Err(e) => {
                log::warn!("vad failed, decoding the whole clip: {e}");
                pcm.clone()
            }
        };
        let vad_ms = t.elapsed().as_millis() as u64;
        let speech = pad_min(speech, 1.25);
        let duration_ms = (speech.len() as f32 / SAMPLE_RATE as f32 * 1000.0) as u32;

        if !self.deps.engine.is_loaded() {
            let Some(path) = self.deps.model_path.clone() else {
                (self.emit)(PillState::LoadingModel { progress: None });
                self.queued = Some(speech);
                return;
            };
            (self.emit)(PillState::LoadingModel { progress: None });
            if let Err(e) = self.deps.engine.load(&path, settings.flash_attention) {
                self.error(&format!("Model failed to load: {e}"));
                self.schedule_idle(tx);
                return;
            }
        }

        (self.emit)(PillState::Transcribing);
        let language = if settings.spoken_languages.len() == 1 { settings.spoken_languages.first().cloned() } else { None };
        let opts = DecodeOptions { language, translate: false, initial_prompt: None, n_threads: 4 };
        let t = Instant::now();
        let transcript = match self.deps.engine.transcribe(&speech, &opts) {
            Ok(t) => t,
            Err(e) => {
                self.error(&format!("Transcription failed: {e}"));
                self.schedule_idle(tx);
                return;
            }
        };
        let decode_ms = t.elapsed().as_millis() as u64;
        let text = normalize(&transcript.text);
        if text.is_empty() {
            self.error("Nothing heard");
            self.schedule_idle(tx);
            return;
        }

        let t = Instant::now();
        let (status, preview) = match self.deps.paste.paste(&text) {
            Ok(PasteOutcome::Pasted) => ("ok", format!("Pasted: {}", first_chars(&text, 60))),
            Ok(PasteOutcome::CopiedOnly(reason)) => {
                log::warn!("paste fell back to clipboard: {reason}");
                ("copied_only", format!("Copied (paste blocked): {}", first_chars(&text, 40)))
            }
            Err(e) => {
                log::warn!("paste failed: {e}");
                ("paste_failed", format!("Paste failed: {}", first_chars(&text, 40)))
            }
        };
        let paste_ms = t.elapsed().as_millis() as u64;

        let timing = serde_json::json!({
            "capture_ms": capture_ms, "vad_ms": vad_ms, "decode_ms": decode_ms, "paste_ms": paste_ms,
            "total_ms": t_total.elapsed().as_millis() as u64, "audio_ctx": transcript.audio_ctx, "reran": transcript.reran,
        });
        let row = NewDictation {
            app_name: (self.deps.frontmost)(),
            mode: "raw".into(),
            model: settings.model_id.clone(),
            language: transcript.language.clone(),
            duration_ms,
            raw_text: text.clone(),
            polished_text: None,
            status: status.into(),
            timing_json: Some(timing.to_string()),
        };
        if let Ok(mut store) = self.store.lock() {
            if let Err(e) = store.insert(&row) {
                log::error!("history insert failed: {e}");
            }
        }
        (self.emit)(PillState::Pasted { preview });
        self.schedule_idle(tx);
    }
}

fn first_chars(s: &str, n: usize) -> String {
    let mut out: String = s.chars().take(n).collect();
    if s.chars().count() > n {
        out.push('…');
    }
    out
}

fn schedule(tx: &Sender<Cmd>, cmd: Cmd, after: Duration) {
    let tx = tx.clone();
    std::thread::spawn(move || {
        std::thread::sleep(after);
        let _ = tx.send(cmd);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use lvw_core::settings::Settings;
    use lvw_core::store::Store;
    use lvw_core::vad::Vad;
    use lvw_engine::protocol::DecodeOptions;
    use lvw_engine::supervisor::{Engine, Transcript};
    use std::path::{Path, PathBuf};
    use std::sync::mpsc::channel;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    struct FakeEngine {
        load_delay: Duration,
        text: String,
        loaded: bool,
    }
    impl Engine for FakeEngine {
        fn load(&mut self, _model: &Path, _flash: bool) -> anyhow::Result<Duration> {
            std::thread::sleep(self.load_delay);
            self.loaded = true;
            Ok(self.load_delay)
        }
        fn unload(&mut self) -> anyhow::Result<()> {
            self.loaded = false;
            Ok(())
        }
        fn is_loaded(&self) -> bool {
            self.loaded
        }
        fn transcribe(&mut self, _pcm: &[f32], _opts: &DecodeOptions) -> anyhow::Result<Transcript> {
            Ok(Transcript { text: self.text.clone(), language: Some("en".into()), decode_ms: 5, audio_ctx: 768, reran: false })
        }
        fn cancel(&mut self) {}
    }

    struct FakeCapture {
        pcm: Vec<f32>,
        fail: bool,
    }
    impl CaptureSource for FakeCapture {
        fn start(&mut self, mut on_pcm: Box<dyn FnMut(&[f32]) + Send>) -> anyhow::Result<()> {
            if self.fail {
                anyhow::bail!("microphone access denied");
            }
            on_pcm(&self.pcm);
            Ok(())
        }
        fn stop(&mut self) {}
    }

    struct FakePaste(Arc<Mutex<Vec<String>>>);
    impl PasteSink for FakePaste {
        fn paste(&mut self, text: &str) -> anyhow::Result<crate::paste::PasteOutcome> {
            self.0.lock().unwrap().push(text.to_string());
            Ok(crate::paste::PasteOutcome::Pasted)
        }
    }

    fn fixture() -> Vec<f32> {
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../crates/lvw-core/tests/fixtures/hello.wav");
        let mut r = hound::WavReader::open(p).unwrap();
        let mut pcm: Vec<f32> = vec![0.0; 8000];
        pcm.extend(r.samples::<i16>().map(|s| s.unwrap() as f32 / 32768.0));
        pcm.extend(vec![0.0; 8000]);
        pcm
    }

    struct Harness {
        states: Arc<Mutex<Vec<PillState>>>,
        pasted: Arc<Mutex<Vec<String>>>,
        store: Arc<Mutex<Store>>,
        tx: std::sync::mpsc::Sender<Cmd>,
        join: std::thread::JoinHandle<()>,
        _dir: tempfile::TempDir,
    }

    fn harness(engine: FakeEngine, capture: FakeCapture) -> Harness {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(Store::open(&dir.path().join("lvw.sqlite")).unwrap()));
        let mut settings = Settings::default();
        settings.post_release_ms = 0;
        let settings = Arc::new(Mutex::new(settings));
        let vad = Vad::new(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../resources/models/ggml-silero-v5.1.2.bin")).unwrap();
        let states = Arc::new(Mutex::new(Vec::new()));
        let pasted = Arc::new(Mutex::new(Vec::new()));
        let deps = Deps {
            engine: Box::new(engine),
            capture: Box::new(capture),
            paste: Box::new(FakePaste(pasted.clone())),
            model_path: Some(PathBuf::from("/fake/model.bin")),
            frontmost: Box::new(|| Some("TestApp".into())),
        };
        let st = states.clone();
        let coord = Coordinator::new(deps, settings, store.clone(), vad, Box::new(move |s| st.lock().unwrap().push(s)));
        let (tx, rx) = channel();
        let tx2 = tx.clone();
        let join = std::thread::spawn(move || coord.run(rx, tx2));
        Harness { states, pasted, store, tx, join, _dir: dir }
    }

    fn kinds(states: &[PillState]) -> Vec<&'static str> {
        states.iter().map(|s| s.kind()).collect()
    }

    fn run_once(h: Harness) -> (Vec<PillState>, Vec<String>, u32) {
        run_once_waiting(h, Duration::ZERO)
    }

    /// `settle` lets the 2 s "back to idle" timer fire before quitting.
    fn run_once_waiting(h: Harness, settle: Duration) -> (Vec<PillState>, Vec<String>, u32) {
        h.tx.send(Cmd::Start).unwrap();
        h.tx.send(Cmd::Stop).unwrap();
        std::thread::sleep(settle);
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
        let states = h.states.lock().unwrap().clone();
        let pasted = h.pasted.lock().unwrap().clone();
        let count = h.store.lock().unwrap().count().unwrap();
        (states, pasted, count)
    }

    #[test]
    fn pipeline_records_transcribes_pastes_and_stores() {
        let h = harness(FakeEngine { load_delay: Duration::ZERO, text: "Hello.".into(), loaded: true }, FakeCapture { pcm: fixture(), fail: false });
        let (states, pasted, count) = run_once(h);
        assert_eq!(pasted, vec!["Hello.".to_string()]);
        assert_eq!(count, 1);
        let k = kinds(&states);
        let pos = |name: &str| k.iter().position(|x| *x == name).unwrap_or_else(|| panic!("{name} missing in {k:?}"));
        assert!(pos("listening") < pos("transcribing") && pos("transcribing") < pos("pasted"), "{k:?}");
    }

    #[test]
    fn pipeline_discards_clips_under_300ms() {
        let h = harness(FakeEngine { load_delay: Duration::ZERO, text: "x".into(), loaded: true }, FakeCapture { pcm: vec![0.3; 2_000], fail: false });
        let (states, pasted, count) = run_once(h);
        assert!(pasted.is_empty());
        assert_eq!(count, 0);
        assert_eq!(kinds(&states).last().copied(), Some("idle"), "{:?}", kinds(&states));
    }

    #[test]
    fn pipeline_reports_silent_mic() {
        let h = harness(FakeEngine { load_delay: Duration::ZERO, text: "x".into(), loaded: true }, FakeCapture { pcm: vec![0.0; 16_000], fail: false });
        let (states, pasted, count) = run_once(h);
        assert!(pasted.is_empty());
        assert_eq!(count, 0);
        assert!(states.iter().any(|s| matches!(s, PillState::Error { message } if message == "Microphone is silent")), "{:?}", kinds(&states));
    }

    #[test]
    fn pipeline_queues_clip_until_engine_loaded() {
        let h = harness(FakeEngine { load_delay: Duration::from_millis(300), text: "Hello.".into(), loaded: false }, FakeCapture { pcm: fixture(), fail: false });
        let (states, pasted, _) = run_once(h);
        assert_eq!(pasted, vec!["Hello.".to_string()]);
        assert!(kinds(&states).contains(&"loading_model"), "{:?}", kinds(&states));
    }

    #[test]
    fn pipeline_reports_capture_error() {
        let h = harness(FakeEngine { load_delay: Duration::ZERO, text: "x".into(), loaded: true }, FakeCapture { pcm: vec![], fail: true });
        let (states, pasted, count) = run_once_waiting(h, Duration::from_millis(2300));
        assert!(pasted.is_empty());
        assert_eq!(count, 0);
        assert!(states.iter().any(|s| matches!(s, PillState::Error { .. })), "{:?}", kinds(&states));
        assert_eq!(kinds(&states).last().copied(), Some("idle"));
    }
}
