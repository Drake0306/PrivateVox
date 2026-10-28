//! The pipeline coordinator: hotkey → capture → VAD → engine → clean-up → paste → history, on one thread.
//! Generic over the engine, capture source and paste sink so it runs under test without hardware.

use crate::paste::PasteOutcome;
use lvw_core::audio::{level_db, ClipBuffer, SAMPLE_RATE};
use lvw_core::chunking::{Chunker, MAX_CHUNK};
use lvw_core::hotkeys::{Effect, Input, Machine};
use lvw_core::lifecycle::IdleTimer;
use lvw_core::settings::Settings;
use lvw_core::store::{NewDictation, Store};
use lvw_core::textnorm::normalize;
use lvw_core::vad::{is_too_short, pad_min, ClipClass, Vad, VadConfig};
use lvw_engine::protocol::DecodeOptions;
use lvw_engine::supervisor::{Engine, LoadInfo};
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
    Listening { seconds: f32, level_db: f32, countdown: Option<u32> },
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
    /// A hotkey edge, fed through the activation machine.
    Key(Input),
    /// The idle timer unloaded the model.
    ModelUnloaded,
    /// Tray: unload now and stay paused until the next dictation.
    PauseAndUnload,
    /// The input stream failed while recording.
    CaptureError(String),
    /// Switch the resident Whisper model (deferred while a recording is in progress).
    SwitchModel(String),
    /// Benchmark a model: switches to it if needed, decodes the fixed clip, reports speed.
    Benchmark(String),
    /// Undo the last paste (hotkey).
    Undo,
    Quit,
}

pub trait CaptureSource: Send {
    /// `on_error` is called if the stream dies mid-recording (device unplugged, permission revoked).
    fn start(&mut self, on_pcm: Box<dyn FnMut(&[f32]) + Send>, on_error: Box<dyn FnMut(String) + Send>) -> anyhow::Result<()>;
    fn stop(&mut self);
}

pub trait PasteSink: Send {
    fn paste(&mut self, text: &str) -> anyhow::Result<PasteOutcome>;
    /// Sends the front app's undo keystroke.
    fn undo(&mut self) -> anyhow::Result<()>;
    /// Puts text on the clipboard without pasting.
    fn copy_only(&mut self, text: &str) -> anyhow::Result<()>;
}

pub struct Deps {
    pub engine: Arc<Mutex<Box<dyn Engine>>>,
    pub capture: Box<dyn CaptureSource>,
    pub paste: Box<dyn PasteSink>,
    pub model_path: Option<PathBuf>,
    /// Where model files live; switching resolves ids against the catalog here.
    pub model_dir: PathBuf,
    pub frontmost: Box<dyn Fn() -> Option<String> + Send>,
    pub idle: Arc<Mutex<IdleTimer>>,
    /// Called after a successful switch with the new id and its load info (persist settings, record stats).
    pub on_model_switched: Box<dyn Fn(&str, LoadInfo) + Send>,
    /// Produces the benchmark clip (16 kHz mono).
    pub benchmark_clip: Box<dyn Fn() -> anyhow::Result<Vec<f32>> + Send>,
    /// Called with (model id, speed_x, load_ms) after a benchmark.
    pub on_benchmark: Box<dyn Fn(&str, f32, u64) + Send>,
}

/// One chunk's decode request for the per-recording decode worker.
struct ChunkJob {
    index: usize,
    pcm: Vec<f32>,
}

struct Recording {
    buffer: Arc<Mutex<ClipBuffer>>,
    level: Arc<Mutex<f32>>,
    started: Instant,
    ticks: u64,
    chunker: Chunker,
    /// Chunks handed to the decode worker so far.
    chunks_sent: usize,
    /// Decode worker (created on the first chunk) and its results by chunk index.
    worker: Option<(Sender<ChunkJob>, std::thread::JoinHandle<()>)>,
    results: Arc<Mutex<std::collections::BTreeMap<usize, String>>>,
}

/// Seconds left before the clip cap, shown from 30 s out.
pub fn countdown_for(elapsed_s: f32, max_s: u32) -> Option<u32> {
    let remaining = max_s as f32 - elapsed_s;
    if remaining <= 30.0 {
        Some(remaining.max(0.0).ceil() as u32)
    } else {
        None
    }
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
    machine: Machine,
    /// A switch requested while recording; applied once the pipeline drains.
    pending_switch: Option<String>,
    last_paste: Option<lvw_core::undo::LastPaste>,
    idle_tx: Option<Sender<Cmd>>,
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
        let machine = {
            let s = settings.lock().map(|s| s.clone()).unwrap_or_default();
            Machine::new(s.activation, Duration::from_millis(s.hold_threshold_ms))
        };
        Coordinator { deps, settings, store, vad, emit, recording: None, queued: None, generation: 0, machine, pending_switch: None, last_paste: None, idle_tx: None }
    }

    /// Unloads the current model and loads `id` from the models directory. Refused if the file is missing.
    fn switch_model(&mut self, id: &str) {
        let Some(path) = lvw_core::models::model_file(&self.deps.model_dir, id) else {
            self.error(&format!("Unknown model {id}"));
            return;
        };
        if !path.exists() {
            self.error(&format!("Model {id} is not downloaded yet"));
            return;
        }
        (self.emit)(PillState::LoadingModel { progress: None });
        let flash = self.settings().flash_attention;
        let result = self.deps.engine.lock().map_err(|e| anyhow::anyhow!("{e}")).and_then(|mut e| {
            if e.is_loaded() {
                e.unload()?;
            }
            e.load(&path, flash)
        });
        match result {
            Ok(info) => {
                self.deps.model_path = Some(path);
                if let Ok(mut s) = self.settings.lock() {
                    s.model_id = id.to_string();
                }
                if let Ok(mut t) = self.deps.idle.lock() {
                    t.touch(Instant::now());
                }
                (self.deps.on_model_switched)(id, info);
                log::info!("switched to {id} in {:?} ({} MB resident)", info.duration, info.resident_bytes / 1_000_000);
                self.idle();
            }
            Err(e) => {
                self.error(&format!("Model {id} failed to load: {e}"));
            }
        }
    }

    fn apply(&mut self, effect: Effect, tx: &Sender<Cmd>) {
        match effect {
            Effect::StartRecording => self.start(tx),
            Effect::StopRecording => self.stop(tx),
            Effect::CancelRecording => self.cancel(),
            Effect::Nothing => {}
        }
    }

    fn machine_input(&mut self, input: Input, tx: &Sender<Cmd>) {
        let effect = self.machine.on(input, Instant::now());
        self.apply(effect, tx);
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
        self.idle_tx = Some(tx.clone());
        self.idle();
        while let Ok(cmd) = rx.recv() {
            match cmd {
                Cmd::Start => self.start(&tx),
                Cmd::Stop => {
                    // A stop that did not come through the key: keep the machine in step.
                    self.machine.force_stop();
                    self.stop(&tx);
                }
                Cmd::Cancel => {
                    self.machine.force_idle();
                    self.cancel();
                }
                Cmd::CaptureError(msg) => {
                    if self.recording.take().is_some() {
                        self.deps.capture.stop();
                        self.generation += 1;
                    }
                    self.machine.force_idle();
                    self.error(&format!("Microphone stopped: {msg}"));
                    self.schedule_idle(&tx);
                }
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
                    let s = self.settings();
                    self.machine.set_activation(s.activation, Duration::from_millis(s.hold_threshold_ms));
                    if let Ok(mut t) = self.deps.idle.lock() {
                        t.set_minutes(s.idle_unload_minutes, Instant::now());
                    }
                    if self.recording.is_none() {
                        self.idle();
                    }
                }
                Cmd::Key(input) => self.machine_input(input, &tx),
                Cmd::ModelUnloaded => {
                    if self.recording.is_none() {
                        self.idle();
                    }
                }
                Cmd::SwitchModel(id) => {
                    if self.recording.is_some() || matches!(self.machine.stage(), lvw_core::hotkeys::Stage::Processing) {
                        log::info!("model switch to {id} deferred until the pipeline is idle");
                        self.pending_switch = Some(id);
                    } else {
                        self.switch_model(&id);
                    }
                }
                Cmd::Benchmark(id) => {
                    if self.recording.is_some() {
                        self.error("Finish the current dictation first");
                    } else {
                        self.benchmark(&id);
                    }
                }
                Cmd::Undo => self.undo(),
                Cmd::PauseAndUnload => {
                    self.machine.force_idle();
                    self.cancel();
                    if let Ok(mut e) = self.deps.engine.lock() {
                        let _ = e.unload();
                    }
                    if let Ok(mut t) = self.deps.idle.lock() {
                        t.clear();
                    }
                    self.idle();
                }
                Cmd::Quit => break,
            }
        }
        if self.recording.is_some() {
            self.deps.capture.stop();
        }
        if let Ok(mut e) = self.deps.engine.lock() {
            let _ = e.unload();
        }
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
        let err_tx = tx.clone();
        let on_error: Box<dyn FnMut(String) + Send> = Box::new(move |msg| {
            let _ = err_tx.send(Cmd::CaptureError(msg));
        });
        (self.emit)(PillState::Listening { seconds: 0.0, level_db: -100.0, countdown: None });
        self.preload_model();
        match self.deps.capture.start(on_pcm, on_error) {
            Ok(()) => {
                self.recording = Some(Recording { buffer, level, started: Instant::now(), ticks: 0, chunker: Chunker::new(), chunks_sent: 0, worker: None, results: Arc::new(Mutex::new(Default::default())) });
                self.generation += 1;
                let _ = self.machine.on(Input::StartResult(true), Instant::now());
                schedule(tx, Cmd::Tick(self.generation), Duration::from_millis(100));
            }
            Err(e) => {
                let _ = self.machine.on(Input::StartResult(false), Instant::now());
                self.error(&format!("Microphone unavailable: {e}"));
                self.schedule_idle(tx);
            }
        }
    }

    /// Starts loading the model on a helper thread while the user is still speaking (spec §4.3 step 1).
    /// The helper holds the engine lock for the load, so the decode simply waits on it.
    fn preload_model(&mut self) {
        let Some(path) = self.deps.model_path.clone() else { return };
        let engine = self.deps.engine.clone();
        let flash = self.settings().flash_attention;
        let idle = self.deps.idle.clone();
        std::thread::Builder::new()
            .name("lvw-preload".into())
            .spawn(move || {
                if let Ok(mut e) = engine.lock() {
                    if !e.is_loaded() {
                        match e.load(&path, flash) {
                            Ok(d) => {
                                log::info!("model preloaded in {:?} ({} MB resident)", d.duration, d.resident_bytes / 1_000_000);
                                if let Ok(mut t) = idle.lock() {
                                    t.touch(Instant::now());
                                }
                            }
                            Err(err) => log::warn!("preload failed: {err}"),
                        }
                    }
                }
            })
            .ok();
    }

    /// Switches to `id` if needed, then decodes the benchmark clip once and reports seconds-per-second.
    fn benchmark(&mut self, id: &str) {
        let active = self.settings().model_id;
        if active != id {
            self.switch_model(id);
            if self.settings().model_id != id {
                return; // the switch reported its own error
            }
        }
        let pcm = match (self.deps.benchmark_clip)() {
            Ok(p) => p,
            Err(e) => {
                self.error(&format!("Benchmark clip unavailable: {e}"));
                return;
            }
        };
        (self.emit)(PillState::Transcribing);
        let seconds = pcm.len() as f32 / SAMPLE_RATE as f32;
        let flash = self.settings().flash_attention;
        let path = self.deps.model_path.clone();
        let result = self.deps.engine.lock().map_err(|e| anyhow::anyhow!("{e}")).and_then(|mut e| {
            let mut load_ms = 0u64;
            if !e.is_loaded() {
                let Some(path) = path.as_ref() else { anyhow::bail!("no model file") };
                load_ms = e.load(path, flash)?.duration.as_millis() as u64;
            }
            let opts = DecodeOptions { language: Some("en".into()), translate: false, initial_prompt: None, n_threads: 4 };
            let t = Instant::now();
            e.transcribe(&pcm, &opts)?;
            Ok((t.elapsed(), load_ms))
        });
        match result {
            Ok((decode, load_ms)) => {
                let speed = crate::benchmark::speed_x(seconds, decode);
                log::info!("benchmark {id}: {seconds:.1} s of audio in {:?} = {speed:.1}x", decode);
                (self.deps.on_benchmark)(id, speed, load_ms);
                if let Ok(mut t) = self.deps.idle.lock() {
                    t.touch(Instant::now());
                }
                self.idle();
            }
            Err(e) => self.error(&format!("Benchmark failed: {e}")),
        }
    }

    /// Undo last paste: the app's undo keystroke if the same app is still in front within 30 s, otherwise
    /// the text goes back on the clipboard so nothing is typed into the wrong window (design §4.2).
    fn undo(&mut self) {
        use lvw_core::undo::{decide, UndoDecision};
        let front = (self.deps.frontmost)();
        match decide(self.last_paste.as_ref(), front.as_deref(), Instant::now(), Duration::from_secs(30)) {
            UndoDecision::SendUndo => match self.deps.paste.undo() {
                Ok(()) => {
                    let text = self.last_paste.take().map(|l| l.text).unwrap_or_default();
                    (self.emit)(PillState::Pasted { preview: format!("Undone: {}", first_chars(&text, 40)) });
                }
                Err(e) => self.error(&format!("Undo failed: {e}")),
            },
            UndoDecision::CopyAgain => {
                let text = self.last_paste.as_ref().map(|l| l.text.clone()).unwrap_or_default();
                match self.deps.paste.copy_only(&text) {
                    Ok(()) => (self.emit)(PillState::Pasted { preview: format!("Copied again (different app in front): {}", first_chars(&text, 30)) }),
                    Err(e) => self.error(&format!("Copy failed: {e}")),
                }
            }
            UndoDecision::Nothing => self.error("Nothing to undo"),
        }
        self.generation += 1;
        let gen = self.generation;
        if let Some(tx) = self.idle_tx.clone() {
            schedule(&tx, Cmd::Tick(gen), Duration::from_millis(2000));
        }
    }

    fn tick(&mut self, gen: u64, tx: &Sender<Cmd>) {
        if gen != self.generation {
            return;
        }
        let max_clip = self.settings().max_clip_seconds;
        let Some(rec) = self.recording.as_mut() else {
            self.idle();
            return;
        };
        rec.ticks += 1;
        let seconds = rec.started.elapsed().as_secs_f32();
        let level = rec.level.lock().map(|l| *l).unwrap_or(-100.0);
        (self.emit)(PillState::Listening { seconds, level_db: level, countdown: countdown_for(seconds, max_clip) });
        if rec.ticks % 5 == 0 {
            self.score_and_cut();
        }
        schedule(tx, Cmd::Tick(gen), Duration::from_millis(100));
    }

    /// Every 500 ms while recording: rescore the audio so far and hand any certain chunk to the decode worker.
    fn score_and_cut(&mut self) {
        let Some(rec) = self.recording.as_mut() else { return };
        let pcm: Vec<f32> = match rec.buffer.lock() {
            Ok(b) => b.peek().to_vec(),
            Err(_) => return,
        };
        if pcm.len() < MAX_CHUNK {
            return; // short clips are decoded in one piece at stop
        }
        let frames = pcm.len() / 512;
        let probs = match self.vad.score(&pcm[..frames * 512]) {
            Ok(p) => p,
            Err(e) => {
                log::warn!("live vad score failed: {e}");
                return;
            }
        };
        let ranges = rec.chunker.rescore(probs, 512, frames * 512);
        for r in ranges {
            let slice = pcm[r.clone()].to_vec();
            self.send_chunk(slice);
        }
    }

    /// Queues one chunk on the ordered decode worker (started on first use).
    fn send_chunk(&mut self, pcm: Vec<f32>) {
        let settings = self.settings();
        let cls = self.vad.classify(&pcm, &VadConfig::default());
        let pcm = match cls {
            Ok(ClipClass::Speech(sp)) => pad_min(sp, 1.25),
            Ok(_) => Vec::new(), // silence: nothing to decode, keeps the index
            Err(_) => pcm,
        };
        let Some(rec) = self.recording.as_mut() else { return };
        if rec.worker.is_none() {
            let (jtx, jrx) = std::sync::mpsc::channel::<ChunkJob>();
            let engine = self.deps.engine.clone();
            let results = rec.results.clone();
            let language = if settings.spoken_languages.len() == 1 { settings.spoken_languages.first().cloned() } else { None };
            let flash = settings.flash_attention;
            let model_path = self.deps.model_path.clone();
            let handle = std::thread::Builder::new()
                .name("lvw-chunks".into())
                .spawn(move || {
                    for job in jrx {
                        let text = if job.pcm.is_empty() {
                            String::new()
                        } else {
                            let opts = DecodeOptions { language: language.clone(), translate: false, initial_prompt: None, n_threads: 4 };
                            let r = engine.lock().map_err(|e| anyhow::anyhow!("{e}")).and_then(|mut e| {
                                if !e.is_loaded() {
                                    if let Some(p) = model_path.as_ref() {
                                        e.load(p, flash)?;
                                    }
                                }
                                e.transcribe(&job.pcm, &opts)
                            });
                            match r {
                                Ok(t) => normalize(&t.text),
                                Err(e) => {
                                    log::warn!("chunk {} failed: {e}", job.index);
                                    String::new()
                                }
                            }
                        };
                        if let Ok(mut m) = results.lock() {
                            m.insert(job.index, text);
                        }
                    }
                })
                .expect("chunk worker");
            rec.worker = Some((jtx, handle));
        }
        let index = rec.chunks_sent;
        rec.chunks_sent += 1;
        if let Some((jtx, _)) = rec.worker.as_ref() {
            let _ = jtx.send(ChunkJob { index, pcm });
        }
    }

    fn schedule_idle(&mut self, tx: &Sender<Cmd>) {
        self.generation += 1;
        schedule(tx, Cmd::Tick(self.generation), Duration::from_millis(2000));
        self.pipeline_done(tx);
    }

    /// Tells the activation machine the pipeline drained; a remembered press starts the next recording.
    fn pipeline_done(&mut self, tx: &Sender<Cmd>) {
        if matches!(self.machine.stage(), lvw_core::hotkeys::Stage::Processing) {
            let effect = self.machine.on(Input::PipelineDone, Instant::now());
            self.apply(effect, tx);
        }
        if self.recording.is_none() {
            if let Some(id) = self.pending_switch.take() {
                self.switch_model(&id);
            }
        }
    }

    fn cancel(&mut self) {
        if self.recording.take().is_some() {
            self.deps.capture.stop();
        }
        self.queued = None;
        self.idle();
    }

    fn stop(&mut self, tx: &Sender<Cmd>) {
        let Some(rec) = self.recording.take() else {
            self.pipeline_done(tx);
            return;
        };
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
            self.pipeline_done(tx);
            return;
        }
        if pcm.len() > MAX_CHUNK || rec.chunks_sent > 0 {
            self.finish_chunked(rec, pcm, tx);
        } else {
            self.decode_and_paste(pcm, tx);
        }
    }

    /// Long clip: decode what is left in order, wait for the worker, join the texts, paste and store.
    fn finish_chunked(&mut self, rec: Recording, pcm: Vec<f32>, tx: &Sender<Cmd>) {
        let t_total = Instant::now();
        (self.emit)(PillState::Transcribing);
        self.recording = Some(rec);
        // Make sure everything is scored, then plan the remainder.
        let total = pcm.len();
        let frames = total / 512;
        let ranges = match self.vad.score(&pcm[..frames * 512]) {
            Ok(probs) => {
                let r = self.recording.as_mut().unwrap();
                let mut out = r.chunker.rescore(probs, 512, frames * 512);
                out.extend(r.chunker.finish(total));
                out
            }
            Err(_) => {
                let r = self.recording.as_mut().unwrap();
                r.chunker.finish(total)
            }
        };
        for r in ranges {
            let slice = pcm[r].to_vec();
            self.send_chunk(slice);
        }
        let rec = self.recording.take().unwrap();
        let chunks = rec.chunks_sent;
        if let Some((jtx, handle)) = rec.worker {
            drop(jtx);
            let _ = handle.join();
        }
        let texts: Vec<String> = rec.results.lock().map(|m| m.values().cloned().collect()).unwrap_or_default();
        let text = normalize(&texts.join(" "));
        if text.is_empty() {
            self.error("Nothing heard");
            self.schedule_idle(tx);
            return;
        }
        if let Ok(mut idle) = self.deps.idle.lock() {
            idle.touch(Instant::now());
        }
        let decode_ms = t_total.elapsed().as_millis() as u64;
        self.paste_and_store(text, (total as f32 / SAMPLE_RATE as f32 * 1000.0) as u32, serde_json::json!({ "chunks": chunks, "tail_ms": decode_ms, "total_ms": decode_ms }), tx);
    }

    /// Shared tail of both decode paths.
    fn paste_and_store(&mut self, text: String, duration_ms: u32, mut timing: serde_json::Value, tx: &Sender<Cmd>) {
        let settings = self.settings();
        let t = Instant::now();
        let app_name = (self.deps.frontmost)();
        let (status, preview) = match self.deps.paste.paste(&text) {
            Ok(PasteOutcome::Pasted) => {
                self.last_paste = Some(lvw_core::undo::LastPaste { text: text.clone(), app: app_name.clone(), at: Instant::now() });
                ("ok", format!("Pasted: {}", first_chars(&text, 60)))
            }
            Ok(PasteOutcome::CopiedOnly(reason)) => {
                log::warn!("paste fell back to clipboard: {reason}");
                ("copied_only", format!("Copied (paste blocked): {}", first_chars(&text, 40)))
            }
            Err(e) => {
                log::warn!("paste failed: {e}");
                ("paste_failed", format!("Paste failed: {}", first_chars(&text, 40)))
            }
        };
        let language = timing.get("language").and_then(|v| v.as_str()).map(|v| v.to_string()).or_else(|| settings.spoken_languages.first().cloned());
        if let Some(obj) = timing.as_object_mut() {
            obj.insert("paste_ms".into(), serde_json::json!(t.elapsed().as_millis() as u64));
            obj.remove("language");
        }
        let row = NewDictation {
            app_name,
            mode: "raw".into(),
            model: settings.model_id.clone(),
            language,
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

    /// Short clip (40 s or less): trim, decode in one run, paste, store.
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

        // A preload in progress holds the lock: treat that as "not loaded yet" so the pill says so while we wait.
        let loaded = self.deps.engine.try_lock().map(|e| e.is_loaded()).unwrap_or(false);
        if !loaded {
            let Some(path) = self.deps.model_path.clone() else {
                (self.emit)(PillState::LoadingModel { progress: None });
                self.queued = Some(speech);
                return;
            };
            (self.emit)(PillState::LoadingModel { progress: None });
            let load = self.deps.engine.lock().map_err(|e| anyhow::anyhow!("{e}")).and_then(|mut e| if e.is_loaded() { Ok(LoadInfo { duration: Duration::ZERO, resident_bytes: 0 }) } else { e.load(&path, settings.flash_attention) });
            if let Err(e) = load {
                self.error(&format!("Model failed to load: {e}"));
                self.schedule_idle(tx);
                return;
            }
            if let Ok(mut idle) = self.deps.idle.lock() {
                idle.touch(Instant::now());
            }
        }

        (self.emit)(PillState::Transcribing);
        let language = if settings.spoken_languages.len() == 1 { settings.spoken_languages.first().cloned() } else { None };
        let opts = DecodeOptions { language, translate: false, initial_prompt: None, n_threads: 4 };
        let t = Instant::now();
        let transcript = self.deps.engine.lock().map_err(|e| anyhow::anyhow!("{e}")).and_then(|mut e| e.transcribe(&speech, &opts));
        let transcript = match transcript {
            Ok(t) => t,
            Err(e) => {
                self.error(&format!("Transcription failed: {e}"));
                self.schedule_idle(tx);
                return;
            }
        };
        if let Ok(mut idle) = self.deps.idle.lock() {
            idle.touch(Instant::now());
        }
        let decode_ms = t.elapsed().as_millis() as u64;
        let text = normalize(&transcript.text);
        if text.is_empty() {
            self.error("Nothing heard");
            self.schedule_idle(tx);
            return;
        }
        let timing = serde_json::json!({
            "capture_ms": capture_ms, "vad_ms": vad_ms, "decode_ms": decode_ms,
            "total_ms": t_total.elapsed().as_millis() as u64, "audio_ctx": transcript.audio_ctx, "reran": transcript.reran,
            "language": transcript.language,
        });
        self.paste_and_store(text, duration_ms, timing, tx);
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
    use lvw_engine::supervisor::{Engine, LoadInfo, Transcript};
    use std::path::{Path, PathBuf};
    use std::sync::mpsc::channel;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    struct FakeEngine {
        load_delay: Duration,
        text: String,
        loaded: bool,
        load_calls: Arc<Mutex<u32>>,
        loads: Arc<Mutex<Vec<PathBuf>>>,
        unloads: Arc<Mutex<u32>>,
        decodes: Arc<Mutex<Vec<std::time::Instant>>>,
    }
    impl Engine for FakeEngine {
        fn load(&mut self, model: &Path, _flash: bool) -> anyhow::Result<LoadInfo> {
            *self.load_calls.lock().unwrap() += 1;
            self.loads.lock().unwrap().push(model.to_path_buf());
            std::thread::sleep(self.load_delay);
            self.loaded = true;
            Ok(LoadInfo { duration: self.load_delay, resident_bytes: 1 })
        }
        fn unload(&mut self) -> anyhow::Result<()> {
            *self.unloads.lock().unwrap() += 1;
            self.loaded = false;
            Ok(())
        }
        fn is_loaded(&self) -> bool {
            self.loaded
        }
        fn transcribe(&mut self, _pcm: &[f32], _opts: &DecodeOptions) -> anyhow::Result<Transcript> {
            self.decodes.lock().unwrap().push(std::time::Instant::now());
            let n = self.decodes.lock().unwrap().len();
            let text = if self.text == "{n}" { n.to_string() } else { self.text.clone() };
            Ok(Transcript { text, language: Some("en".into()), decode_ms: 5, audio_ctx: 768, reran: false })
        }
        fn cancel(&mut self) {}
    }

    struct FakeCapture {
        pcm: Vec<f32>,
        fail: bool,
        die_after_start: bool,
    }
    impl CaptureSource for FakeCapture {
        fn start(&mut self, mut on_pcm: Box<dyn FnMut(&[f32]) + Send>, mut on_error: Box<dyn FnMut(String) + Send>) -> anyhow::Result<()> {
            if self.fail {
                anyhow::bail!("microphone access denied");
            }
            on_pcm(&self.pcm);
            if self.die_after_start {
                on_error("device disconnected".into());
            }
            Ok(())
        }
        fn stop(&mut self) {}
    }

    struct FakePaste(Arc<Mutex<Vec<String>>>, Arc<Mutex<u32>>);
    impl PasteSink for FakePaste {
        fn paste(&mut self, text: &str) -> anyhow::Result<crate::paste::PasteOutcome> {
            self.0.lock().unwrap().push(text.to_string());
            Ok(crate::paste::PasteOutcome::Pasted)
        }
        fn undo(&mut self) -> anyhow::Result<()> {
            *self.1.lock().unwrap() += 1;
            Ok(())
        }
        fn copy_only(&mut self, text: &str) -> anyhow::Result<()> {
            self.0.lock().unwrap().push(format!("copy:{text}"));
            Ok(())
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
        settings: Arc<Mutex<Settings>>,
        switched: Arc<Mutex<Vec<String>>>,
        benchmarks: Arc<Mutex<Vec<(String, f32)>>>,
        undos: Arc<Mutex<u32>>,
        frontmost: Arc<Mutex<Option<String>>>,
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
        let models_dir = dir.path().join("models");
        std::fs::create_dir_all(&models_dir).unwrap();
        for m in lvw_core::models::catalog() {
            std::fs::write(models_dir.join(m.file), b"stub").unwrap();
        }
        let switched = Arc::new(Mutex::new(Vec::new()));
        let sw = switched.clone();
        let benchmarks: Arc<Mutex<Vec<(String, f32)>>> = Default::default();
        let bm = benchmarks.clone();
        let undos: Arc<Mutex<u32>> = Default::default();
        let frontmost: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(Some("TestApp".into())));
        let fm = frontmost.clone();
        let deps = Deps {
            engine: Arc::new(Mutex::new(Box::new(engine) as Box<dyn Engine>)),
            capture: Box::new(capture),
            paste: Box::new(FakePaste(pasted.clone(), undos.clone())),
            model_path: Some(models_dir.join("ggml-large-v3-turbo-q5_0.bin")),
            model_dir: models_dir,
            frontmost: Box::new(move || fm.lock().unwrap().clone()),
            idle: Arc::new(Mutex::new(lvw_core::lifecycle::IdleTimer::new(None))),
            on_model_switched: Box::new(move |id, _info| sw.lock().unwrap().push(id.to_string())),
            benchmark_clip: Box::new(|| Ok(vec![0.1f32; 16_000 * 30])),
            on_benchmark: Box::new(move |id, speed, _load_ms| bm.lock().unwrap().push((id.to_string(), speed))),
        };
        let st = states.clone();
        let coord = Coordinator::new(deps, settings.clone(), store.clone(), vad, Box::new(move |s| st.lock().unwrap().push(s)));
        let (tx, rx) = channel();
        let tx2 = tx.clone();
        let join = std::thread::spawn(move || coord.run(rx, tx2));
        Harness { states, pasted, store, settings, switched, benchmarks, undos, frontmost, tx, join, _dir: dir }
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
        let h = harness(FakeEngine { load_delay: Duration::ZERO, text: "Hello.".into(), loaded: true, load_calls: Default::default(), loads: Default::default(), unloads: Default::default(), decodes: Default::default() }, FakeCapture { pcm: fixture(), fail: false, die_after_start: false });
        let (states, pasted, count) = run_once(h);
        assert_eq!(pasted, vec!["Hello.".to_string()]);
        assert_eq!(count, 1);
        let k = kinds(&states);
        let pos = |name: &str| k.iter().position(|x| *x == name).unwrap_or_else(|| panic!("{name} missing in {k:?}"));
        assert!(pos("listening") < pos("transcribing") && pos("transcribing") < pos("pasted"), "{k:?}");
    }

    #[test]
    fn pipeline_discards_clips_under_300ms() {
        let h = harness(FakeEngine { load_delay: Duration::ZERO, text: "x".into(), loaded: true, load_calls: Default::default(), loads: Default::default(), unloads: Default::default(), decodes: Default::default() }, FakeCapture { pcm: vec![0.3; 2_000], fail: false, die_after_start: false });
        let (states, pasted, count) = run_once(h);
        assert!(pasted.is_empty());
        assert_eq!(count, 0);
        assert_eq!(kinds(&states).last().copied(), Some("idle"), "{:?}", kinds(&states));
    }

    #[test]
    fn pipeline_reports_silent_mic() {
        let h = harness(FakeEngine { load_delay: Duration::ZERO, text: "x".into(), loaded: true, load_calls: Default::default(), loads: Default::default(), unloads: Default::default(), decodes: Default::default() }, FakeCapture { pcm: vec![0.0; 16_000], fail: false, die_after_start: false });
        let (states, pasted, count) = run_once(h);
        assert!(pasted.is_empty());
        assert_eq!(count, 0);
        assert!(states.iter().any(|s| matches!(s, PillState::Error { message } if message == "Microphone is silent")), "{:?}", kinds(&states));
    }

    #[test]
    fn pipeline_queues_clip_until_engine_loaded() {
        let h = harness(FakeEngine { load_delay: Duration::from_millis(300), text: "Hello.".into(), loaded: false, load_calls: Default::default(), loads: Default::default(), unloads: Default::default(), decodes: Default::default() }, FakeCapture { pcm: fixture(), fail: false, die_after_start: false });
        let (states, pasted, _) = run_once(h);
        assert_eq!(pasted, vec!["Hello.".to_string()]);
        assert!(kinds(&states).contains(&"loading_model"), "{:?}", kinds(&states));
    }

    #[test]
    fn direct_stop_after_hotkey_start_keeps_hotkeys_working() {
        use lvw_core::hotkeys::{Binding, Input};
        let h = harness(FakeEngine { load_delay: Duration::ZERO, text: "Hello.".into(), loaded: true, load_calls: Default::default(), loads: Default::default(), unloads: Default::default(), decodes: Default::default() }, FakeCapture { pcm: fixture(), fail: false, die_after_start: false });
        h.tx.send(Cmd::Key(Input::Down(Binding::Ptt))).unwrap();
        h.tx.send(Cmd::Stop).unwrap(); // what the clip cap, the tray or the debug button sends
        h.tx.send(Cmd::Key(Input::Up(Binding::Ptt))).unwrap();
        h.tx.send(Cmd::Key(Input::Down(Binding::Ptt))).unwrap();
        h.tx.send(Cmd::Key(Input::Up(Binding::Ptt))).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
        let pasted = h.pasted.lock().unwrap().clone();
        assert_eq!(pasted.len(), 2, "second hotkey press must record again; states {:?}", kinds(&h.states.lock().unwrap()));
    }

    #[test]
    fn pipeline_reports_device_loss_mid_recording() {
        let h = harness(FakeEngine { load_delay: Duration::ZERO, text: "x".into(), loaded: true, load_calls: Default::default(), loads: Default::default(), unloads: Default::default(), decodes: Default::default() }, FakeCapture { pcm: fixture(), fail: false, die_after_start: true });
        h.tx.send(Cmd::Start).unwrap();
        std::thread::sleep(Duration::from_millis(2300));
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
        let states = h.states.lock().unwrap().clone();
        assert!(states.iter().any(|s| matches!(s, PillState::Error { message } if message.starts_with("Microphone stopped"))), "{:?}", kinds(&states));
        assert_eq!(kinds(&states).last().copied(), Some("idle"));
        assert!(h.pasted.lock().unwrap().is_empty());
    }

    #[test]
    fn pipeline_starts_model_load_while_recording() {
        let calls = Arc::new(Mutex::new(0u32));
        let h = harness(FakeEngine { load_delay: Duration::from_millis(200), text: "Hello.".into(), loaded: false, load_calls: calls.clone(), loads: Default::default(), unloads: Default::default(), decodes: Default::default() }, FakeCapture { pcm: fixture(), fail: false, die_after_start: false });
        h.tx.send(Cmd::Start).unwrap();
        std::thread::sleep(Duration::from_millis(80));
        assert_eq!(*calls.lock().unwrap(), 1, "load must begin at Start, not after Stop");
        h.tx.send(Cmd::Stop).unwrap();
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
        assert_eq!(h.pasted.lock().unwrap().clone(), vec!["Hello.".to_string()]);
        assert_eq!(*calls.lock().unwrap(), 1, "the decode must reuse the preloaded model");
    }

    #[test]
    fn switch_unloads_then_loads_and_updates_settings() {
        let loads: Arc<Mutex<Vec<PathBuf>>> = Default::default();
        let unloads: Arc<Mutex<u32>> = Default::default();
        let h = harness(FakeEngine { load_delay: Duration::ZERO, text: "x".into(), loaded: true, load_calls: Default::default(), loads: loads.clone(), unloads: unloads.clone(), decodes: Default::default() }, FakeCapture { pcm: fixture(), fail: false, die_after_start: false });
        h.tx.send(Cmd::SwitchModel("large-v3".into())).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
        assert_eq!(*unloads.lock().unwrap() >= 1, true, "old model unloaded first");
        let last = loads.lock().unwrap().last().cloned().unwrap();
        assert!(last.ends_with("ggml-large-v3-q5_0.bin"), "{last:?}");
        assert_eq!(h.settings.lock().unwrap().model_id, "large-v3");
        assert_eq!(h.switched.lock().unwrap().clone(), vec!["large-v3".to_string()]);
        assert!(kinds(&h.states.lock().unwrap()).contains(&"loading_model"));
    }

    #[test]
    fn switch_during_recording_is_deferred() {
        let loads: Arc<Mutex<Vec<PathBuf>>> = Default::default();
        let h = harness(FakeEngine { load_delay: Duration::ZERO, text: "Hello.".into(), loaded: true, load_calls: Default::default(), loads: loads.clone(), unloads: Default::default(), decodes: Default::default() }, FakeCapture { pcm: fixture(), fail: false, die_after_start: false });
        h.tx.send(Cmd::Start).unwrap();
        h.tx.send(Cmd::SwitchModel("large-v3".into())).unwrap();
        std::thread::sleep(Duration::from_millis(150));
        assert!(loads.lock().unwrap().iter().all(|p| !p.ends_with("ggml-large-v3-q5_0.bin")), "switch must wait while recording");
        h.tx.send(Cmd::Stop).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
        assert_eq!(h.pasted.lock().unwrap().clone(), vec!["Hello.".to_string()], "the clip is not lost");
        assert!(loads.lock().unwrap().iter().any(|p| p.ends_with("ggml-large-v3-q5_0.bin")), "switch applied after the pipeline drained");
        assert_eq!(h.settings.lock().unwrap().model_id, "large-v3");
    }

    #[test]
    fn switch_to_missing_model_is_refused() {
        let h = harness(FakeEngine { load_delay: Duration::ZERO, text: "x".into(), loaded: true, load_calls: Default::default(), loads: Default::default(), unloads: Default::default(), decodes: Default::default() }, FakeCapture { pcm: fixture(), fail: false, die_after_start: false });
        std::fs::remove_file(h._dir.path().join("models/ggml-distil-large-v3.bin")).unwrap();
        h.tx.send(Cmd::SwitchModel("distil-large-v3".into())).unwrap();
        std::thread::sleep(Duration::from_millis(150));
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
        assert_eq!(h.settings.lock().unwrap().model_id, "large-v3-turbo");
        assert!(h.states.lock().unwrap().iter().any(|s| matches!(s, PillState::Error { message } if message.contains("not downloaded"))));
    }

    #[test]
    fn benchmark_records_speed_for_active_model() {
        let h = harness(FakeEngine { load_delay: Duration::ZERO, text: "bench".into(), loaded: true, load_calls: Default::default(), loads: Default::default(), unloads: Default::default(), decodes: Default::default() }, FakeCapture { pcm: fixture(), fail: false, die_after_start: false });
        h.tx.send(Cmd::Benchmark("large-v3-turbo".into())).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
        let b = h.benchmarks.lock().unwrap().clone();
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].0, "large-v3-turbo");
        assert!(b[0].1 > 0.0, "speed_x must be positive, got {}", b[0].1);
    }

    /// 90 s: the one-word fixture followed by 1 s of silence, repeated.
    fn long_fixture(seconds: usize) -> Vec<f32> {
        let unit = {
            let mut u = fixture();
            u.extend(vec![0.0f32; 16_000]);
            u
        };
        let mut out = Vec::new();
        while out.len() < seconds * 16_000 {
            out.extend_from_slice(&unit);
        }
        out.truncate(seconds * 16_000);
        out
    }

    #[test]
    fn long_clip_is_decoded_in_order_while_recording() {
        let decodes: Arc<Mutex<Vec<std::time::Instant>>> = Default::default();
        let h = harness(FakeEngine { load_delay: Duration::ZERO, text: "{n}".into(), loaded: true, load_calls: Default::default(), loads: Default::default(), unloads: Default::default(), decodes: decodes.clone() }, FakeCapture { pcm: long_fixture(90), fail: false, die_after_start: false });
        h.tx.send(Cmd::Start).unwrap();
        std::thread::sleep(Duration::from_millis(1500)); // a few scoring ticks
        let stop_at = std::time::Instant::now();
        h.tx.send(Cmd::Stop).unwrap();
        std::thread::sleep(Duration::from_millis(500));
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
        let pasted = h.pasted.lock().unwrap().clone();
        assert_eq!(pasted.len(), 1);
        let n = pasted[0].split_whitespace().count();
        assert!(n >= 2, "90 s must decode as several chunks, got {:?}", pasted[0]);
        let expected: Vec<String> = (1..=n).map(|i| i.to_string()).collect();
        assert_eq!(pasted[0], expected.join(" "), "chunk texts must be joined in order");
        assert!(decodes.lock().unwrap().iter().any(|t| *t < stop_at), "at least one chunk decoded before the key came up");
    }

    #[test]
    fn countdown_shows_in_the_last_30_seconds() {
        assert_eq!(countdown_for(10.0, 300), None);
        assert_eq!(countdown_for(269.0, 300), None);
        assert_eq!(countdown_for(270.0, 300), Some(30));
        assert_eq!(countdown_for(299.4, 300), Some(1));
        assert_eq!(countdown_for(305.0, 300), Some(0));
    }

    #[test]
    fn cap_countdown_then_stop() {
        let h = harness(FakeEngine { load_delay: Duration::ZERO, text: "{n}".into(), loaded: true, load_calls: Default::default(), loads: Default::default(), unloads: Default::default(), decodes: Default::default() }, FakeCapture { pcm: long_fixture(45), fail: false, die_after_start: false });
        h.settings.lock().unwrap().max_clip_seconds = 40;
        h.tx.send(Cmd::Start).unwrap(); // the capture overfills the 40 s buffer at once → auto-stop
        std::thread::sleep(Duration::from_millis(800));
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
        let pasted = h.pasted.lock().unwrap().clone();
        assert_eq!(pasted.len(), 1, "the cap must stop and transcribe, not drop the clip; states {:?}", kinds(&h.states.lock().unwrap()));
    }

    #[test]
    fn undo_after_paste_sends_undo() {
        let h = harness(FakeEngine { load_delay: Duration::ZERO, text: "Hello.".into(), loaded: true, load_calls: Default::default(), loads: Default::default(), unloads: Default::default(), decodes: Default::default() }, FakeCapture { pcm: fixture(), fail: false, die_after_start: false });
        h.tx.send(Cmd::Start).unwrap();
        h.tx.send(Cmd::Stop).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        h.tx.send(Cmd::Undo).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
        assert_eq!(*h.undos.lock().unwrap(), 1);
    }

    #[test]
    fn undo_refused_when_frontmost_changed_copies_again() {
        let h = harness(FakeEngine { load_delay: Duration::ZERO, text: "Hello.".into(), loaded: true, load_calls: Default::default(), loads: Default::default(), unloads: Default::default(), decodes: Default::default() }, FakeCapture { pcm: fixture(), fail: false, die_after_start: false });
        h.tx.send(Cmd::Start).unwrap();
        h.tx.send(Cmd::Stop).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        *h.frontmost.lock().unwrap() = Some("Other".into());
        h.tx.send(Cmd::Undo).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
        assert_eq!(*h.undos.lock().unwrap(), 0, "no keystroke into the wrong app");
        let pasted = h.pasted.lock().unwrap().clone();
        assert_eq!(pasted, vec!["Hello.".to_string(), "copy:Hello.".to_string()]);
        assert!(h.states.lock().unwrap().iter().any(|s| matches!(s, PillState::Pasted { preview } if preview.contains("Copied again"))));
    }

    #[test]
    fn pipeline_reports_capture_error() {
        let h = harness(FakeEngine { load_delay: Duration::ZERO, text: "x".into(), loaded: true, load_calls: Default::default(), loads: Default::default(), unloads: Default::default(), decodes: Default::default() }, FakeCapture { pcm: vec![], fail: true, die_after_start: false });
        let (states, pasted, count) = run_once_waiting(h, Duration::from_millis(2300));
        assert!(pasted.is_empty());
        assert_eq!(count, 0);
        assert!(states.iter().any(|s| matches!(s, PillState::Error { .. })), "{:?}", kinds(&states));
        assert_eq!(kinds(&states).last().copied(), Some("idle"));
    }
}
