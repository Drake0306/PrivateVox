//! The pipeline coordinator: hotkey → capture → VAD → engine → clean-up → paste → history, on one thread.
//! Generic over the engine, capture source and paste sink so it runs under test without hardware.

use crate::paste::PasteOutcome;
use privatevox_core::appmodes::resolve as resolve_app_mode;
use privatevox_core::audio::{level_db, ClipBuffer, SAMPLE_RATE};
use privatevox_core::chunking::{Chunker, MAX_CHUNK};
use privatevox_core::commands::{strip_trailing, SpokenCommand};
use privatevox_core::dictionary::{correct, initial_prompt};
use privatevox_core::language::{decode_language, settle, LanguageSource};
use privatevox_core::hotkeys::{Effect, Input, Machine};
use privatevox_core::lifecycle::IdleTimer;
use privatevox_core::net::PolishRequest;
use privatevox_core::polish::{Mode, PolishOutcome, PolishStatus};
use privatevox_core::preview::{partial_text, preview_enabled};
use privatevox_engine::remote::RemoteState;
use privatevox_core::settings::Settings;
use privatevox_core::silence::TrailingSilence;
use privatevox_core::store::{NewDictation, Store};
use privatevox_core::textnorm::{normalize, normalize_with};
use privatevox_core::vad::{is_too_short, pad_min, ClipClass, Vad, VadConfig};
use privatevox_engine::protocol::DecodeOptions;
use privatevox_engine::supervisor::{Engine, LoadInfo};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// What the pill shows. Emitted as the Tauri event `pill-state`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PillState {
    Idle { hotkey_hint: String, warning: Option<String> },
    Listening { seconds: f32, level_db: f32, countdown: Option<u32>, loading: bool, mode: String, stop_hint: Option<String>, stopping: bool, partial: Option<String> },
    Transcribing,
    /// Waiting on the LLM (design §4.1); `on` names the host when the polish runs there.
    Polishing { on: Option<String> },
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
            PillState::Polishing { .. } => "polishing",
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
    /// Paste text from history into the front app (no new history row).
    PasteText(String),
    /// Change the active mode (the command layer persists settings; this refreshes the pill hint).
    SetMode(Mode),
    /// Run the LLM again over a history row's raw text with another mode (panel "Re-polish").
    Repolish { id: i64, mode: Mode },
    /// Decode a row's kept clip again, with the given model (panel "Re-run").
    Rerun { id: i64, model: String },
    /// A short message for the pill from a helper thread (shown as an error, then back to idle).
    Notice(String),
    /// macOS Secure Event Input went on or off (design §18.12).
    SecureInput(bool),
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
    /// Host-mode client state: whether chunks and polish go to a host on the LAN (design §8.6).
    pub remote: Arc<RemoteState>,
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
    /// The LLM step (design §4.3 step 5): mode + raw text → text to paste and how it went. Blocks up to the LLM timeout.
    pub polish: Arc<dyn Fn(Mode, &str) -> PolishOutcome + Send + Sync>,
    /// Fired on hotkey down in LLM modes so a cold runtime loads while the user speaks (§4.3 5a).
    pub llm_warm: Arc<dyn Fn() + Send + Sync>,
    /// Fired from the same idle path that unloads Whisper (§4.3 5c).
    pub llm_unload: Arc<dyn Fn() + Send + Sync>,
    /// The panel reloads its rows (after a re-polish).
    pub on_history_changed: Arc<dyn Fn() + Send + Sync>,
    /// Where kept audio clips go (`<data>/audio/<row id>.wav`).
    pub audio_dir: PathBuf,
    /// Sound/visual cue hook; the app gates it on the setting and plays on a helper thread.
    pub cue: Arc<dyn Fn(crate::cues::Cue) + Send + Sync>,
}

/// One chunk's decode request for the per-recording decode worker.
struct ChunkJob {
    index: usize,
    pcm: Vec<f32>,
    /// Last chunk of the dictation: a host closes the session on it.
    final_: bool,
}

struct Recording {
    buffer: Arc<Mutex<ClipBuffer>>,
    level: Arc<Mutex<f32>>,
    started: Instant,
    ticks: u64,
    chunker: Chunker,
    /// Frames (512 samples) already scored; scoring continues from here with a short overlap.
    scored_frames: usize,
    /// One Silero probability per scored frame, used to trim chunks without re-running the VAD.
    probs: Vec<f32>,
    /// Chunks handed to the decode worker so far.
    chunks_sent: usize,
    /// Decode worker (created on the first chunk) and its results by chunk index.
    worker: Option<(Sender<ChunkJob>, std::thread::JoinHandle<()>)>,
    results: Arc<Mutex<std::collections::BTreeMap<usize, String>>>,
    /// The host was in use when this recording started (the row is marked `remote:<label>` unless a fallback happened).
    used_remote: bool,
    /// Set by the first chunk the host failed on; every later chunk of this dictation stays local.
    fell_back: Arc<AtomicBool>,
    /// Set on cancel/pause: the worker stops before its next job and never reloads the model.
    cancelled: Arc<AtomicBool>,
    /// Auto-stop bookkeeping (toggle mode): frames scored for silence so far, and the trailing-silence counter.
    silence_scored: usize,
    silence: TrailingSilence,
    /// Live preview: the latest window text and the partial shown in the pill.
    preview_window: String,
    partial: Option<String>,
}

const FRAME: usize = 512;
/// About one second: enough for Silero's state to settle before the frames we keep.
const SCORE_OVERLAP_FRAMES: usize = 31;

/// Crops a chunk to its speech using the probabilities already scored for it (pre-roll/hangover as in
/// `VadConfig::default`). Returns an empty vector when the chunk has no speech.
fn trim_with_probs(pcm: &[f32], probs: &[f32], chunk_start: usize) -> Vec<f32> {
    let first = chunk_start / FRAME;
    let last = ((chunk_start + pcm.len()) / FRAME).min(probs.len());
    if first >= last {
        return pad_min(pcm.to_vec(), 1.25); // unscored tail: decode as is
    }
    let segs = privatevox_core::chunking::speech_segments(&probs[first..last], FRAME, pcm.len());
    if segs.is_empty() {
        return Vec::new();
    }
    let cfg = VadConfig::default();
    let pre = cfg.pre_roll_ms as usize * SAMPLE_RATE as usize / 1000;
    let post = cfg.hangover_ms as usize * SAMPLE_RATE as usize / 1000;
    let start = segs.first().unwrap().start.saturating_sub(pre);
    let end = (segs.last().unwrap().end + post).min(pcm.len());
    pad_min(pcm[start..end].to_vec(), 1.25)
}

/// Which frames to score this tick: the new ones plus an overlap, or everything the first time.
pub fn score_window(scored_frames: usize, available_frames: usize, overlap_frames: usize) -> std::ops::Range<usize> {
    if available_frames <= scored_frames {
        return scored_frames..scored_frames;
    }
    if scored_frames == 0 {
        return 0..available_frames;
    }
    scored_frames.saturating_sub(overlap_frames)..available_frames
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
    last_paste: Option<privatevox_core::undo::LastPaste>,
    idle_tx: Option<Sender<Cmd>>,
    /// Whether the dictation in flight started with the host in use (for the row's `engine_kind`).
    last_used_remote: bool,
    /// Shared with the recording's chunk worker: a host failure during this dictation.
    fell_back: Arc<AtomicBool>,
    /// macOS Secure Event Input is on: the pill warns that the hold key may not fire.
    secure_input: bool,
    /// The mode of the dictation in flight, resolved once at Start from the per-app table (A4).
    dictation_mode: Mode,
    /// This machine's memory, for the live-preview Auto rule (read once).
    total_memory: u64,
}

/// The hold key as the pill names it ("right ⌥", or a combination), if one is set.
fn ptt_key_label(s: &Settings) -> Option<String> {
    use privatevox_core::settings::ModifierKey;
    match (&s.ptt.modifier, &s.ptt.combo) {
        (Some(ModifierKey::RightOption), _) => Some("right ⌥".to_string()),
        (Some(ModifierKey::LeftOption), _) => Some("left ⌥".to_string()),
        (Some(ModifierKey::RightCommand), _) => Some("right ⌘".to_string()),
        (Some(ModifierKey::RightControl), _) => Some("right ⌃".to_string()),
        (None, Some(c)) => Some(c.clone()),
        (None, None) => None,
    }
}

/// The pill's idle text: which key, and whether to hold or tap it, from the activation setting (§4.1).
fn hotkey_hint(s: &Settings) -> String {
    use privatevox_core::settings::Activation;
    match (s.activation, ptt_key_label(s), &s.toggle.combo) {
        (Activation::PushToTalk, Some(k), _) => format!("Hold {k} to talk"),
        (Activation::HoldOrToggle, Some(k), _) => format!("Hold or tap {k} to talk"),
        (Activation::Toggle, Some(k), _) => format!("Tap {k} to talk"),
        (_, None, Some(t)) => format!("Tap {t} to talk"),
        (_, None, None) => "PrivateVox".into(),
    }
}

/// After a clip was written: record it on the row, or remove it when the row was deleted meanwhile.
pub fn finalize_clip(store: &mut Store, id: i64, path: &std::path::Path) {
    match store.get(id) {
        Ok(Some(_)) => {
            if let Err(e) = store.set_audio_path(id, Some(&path.to_string_lossy())) {
                log::warn!("keep audio: {e}");
            }
        }
        _ => {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Transcribe on the host when it is in use, without holding the local engine lock while waiting on the network
/// (a host served from this same process needs that lock). On a host failure the notice is set and the local engine
/// takes over, loading `model_path` if it has to.
/// `use_remote` is decided once per dictation (at Start) and `fell_back` makes a fallback stick for the rest of it,
/// so a host that blinks mid-dictation never gets half a session.
fn transcribe_routed(remote: &Arc<RemoteState>, engine: &Arc<Mutex<Box<dyn Engine>>>, pcm: &[f32], opts: &DecodeOptions, use_remote: bool, fell_back: &AtomicBool, model_path: Option<&PathBuf>, flash: bool) -> anyhow::Result<privatevox_engine::supervisor::Transcript> {
    if use_remote && !fell_back.load(Ordering::SeqCst) && remote.in_use() {
        match privatevox_engine::remote::RemoteEngine::new(remote.clone(), Duration::from_secs(15)).transcribe(pcm, opts) {
            Ok(t) => return Ok(t),
            Err(e) => {
                log::warn!("remote transcription failed, falling back to local for the rest of this dictation: {e}");
                fell_back.store(true, Ordering::SeqCst);
                remote.set_notice(privatevox_engine::remote::FALLBACK_NOTICE);
            }
        }
    } else if use_remote && !fell_back.load(Ordering::SeqCst) {
        // Started on the host, which has since gone away without a request failing here (health loop): same outcome.
        fell_back.store(true, Ordering::SeqCst);
        remote.set_notice(privatevox_engine::remote::FALLBACK_NOTICE);
    }
    privatevox_engine::routing::with_local_only(|| {
        engine.lock().map_err(|e| anyhow::anyhow!("{e}")).and_then(|mut e| {
            if !e.is_loaded() {
                let path = model_path.ok_or_else(|| anyhow::anyhow!("no local model available"))?;
                e.load(path, flash)?;
            }
            e.transcribe(pcm, opts)
        })
    })
}

/// Polish on the host when it is in use. `None` means the host failed (and is now marked unhealthy): polish locally.
fn remote_polish(remote: &RemoteState, mode: Mode, text: &str, timeout: Duration) -> Option<PolishOutcome> {
    let client = remote.client()?;
    let req = PolishRequest { client: remote.client_name(), session: remote.session(), mode: mode.id().into(), text: text.to_string() };
    let t = Instant::now();
    match client.polish(&req, timeout) {
        Ok(r) => {
            let status = match r.polish_status.as_str() {
                "ok" => PolishStatus::Polished { endpoint_id: format!("host:{}", remote.label()), model: r.polished_by.unwrap_or_default(), ms: t.elapsed().as_millis() as u64 },
                // The client asked for an LLM mode; "none" means the host has polish switched off.
                "none" => PolishStatus::Unpolished { reason: format!("{} has polish off", remote.label()) },
                _ => PolishStatus::Unpolished { reason: format!("{} could not polish", remote.label()) },
            };
            let text = if matches!(status, PolishStatus::Polished { .. }) { r.text } else { text.to_string() };
            Some(PolishOutcome { text, status, notice: None })
        }
        Err(e) => {
            log::warn!("remote polish failed: {e}");
            remote.mark_failed(e.to_string());
            None
        }
    }
}

/// While a tapped recording is latched (like Caps Lock), the pill says how to end it.
fn stop_hint(s: &Settings, stage: &privatevox_core::hotkeys::Stage) -> Option<String> {
    match stage {
        privatevox_core::hotkeys::Stage::Recording { locked: true, .. } => Some(format!("tap {} to stop", ptt_key_label(s).or_else(|| s.toggle.combo.clone()).unwrap_or_else(|| "the key".into()))),
        _ => None,
    }
}

impl Coordinator {
    pub fn new(deps: Deps, settings: Arc<Mutex<Settings>>, store: Arc<Mutex<Store>>, vad: Vad, emit: Box<dyn Fn(PillState) + Send>) -> Self {
        let machine = {
            let s = settings.lock().map(|s| s.clone()).unwrap_or_default();
            Machine::new(s.activation, Duration::from_millis(s.hold_threshold_ms))
        };
        Coordinator { deps, settings, store, vad, emit, recording: None, queued: None, generation: 0, machine, pending_switch: None, last_paste: None, idle_tx: None, last_used_remote: false, fell_back: Arc::new(AtomicBool::new(false)), secure_input: false, dictation_mode: Mode::Clean, total_memory: privatevox_core::memory::system_memory().0 }
    }

    /// Unloads the current model and loads `id` from the models directory. Refused if the file is missing.
    fn switch_model(&mut self, id: &str) {
        let Some(path) = privatevox_core::models::model_file(&self.deps.model_dir, id) else {
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
        let settings = self.settings();
        let warning = self.secure_input.then(|| match settings.toggle.combo.as_deref() {
            Some(c) => format!("Secure input is on (a password field?). The hold key may not fire; use {c}"),
            None => "Secure input is on (a password field?). The hold key may not fire until it is off".to_string(),
        });
        (self.emit)(PillState::Idle { hotkey_hint: hotkey_hint(&settings), warning });
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
                    self.unload_llm();
                    if self.recording.is_none() {
                        self.idle();
                    }
                }
                Cmd::SetMode(mode) => {
                    if let Ok(mut s) = self.settings.lock() {
                        s.mode = mode;
                    }
                    if self.recording.is_none() {
                        self.idle();
                    }
                }
                Cmd::Repolish { id, mode } => self.repolish(id, mode, &tx),
                Cmd::Rerun { id, model } => self.rerun(id, &model, &tx),
                Cmd::SecureInput(on) => {
                    self.secure_input = on;
                    if self.recording.is_none() {
                        self.idle();
                    }
                }
                Cmd::Notice(message) => {
                    if self.recording.is_none() && self.queued.is_none() {
                        self.error(&message);
                        self.schedule_idle(&tx);
                    } else {
                        log::warn!("notice while busy, not shown: {message}");
                    }
                }
                Cmd::SwitchModel(id) => {
                    if self.recording.is_some() || matches!(self.machine.stage(), privatevox_core::hotkeys::Stage::Processing) {
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
                Cmd::PasteText(text) => {
                    let app_name = (self.deps.frontmost)();
                    match self.deps.paste.paste(&text) {
                        Ok(PasteOutcome::Pasted) => {
                            self.last_paste = Some(privatevox_core::undo::LastPaste { text: text.clone(), app: app_name, at: Instant::now() });
                            (self.emit)(PillState::Pasted { preview: format!("Pasted again: {}", first_chars(&text, 50)) });
                        }
                        Ok(PasteOutcome::CopiedOnly(_)) => (self.emit)(PillState::Pasted { preview: format!("Copied (paste blocked): {}", first_chars(&text, 40)) }),
                        Err(e) => self.error(&format!("Paste failed: {e}")),
                    }
                    self.schedule_idle(&tx);
                }
                Cmd::PauseAndUnload => {
                    self.machine.force_idle();
                    self.cancel();
                    if let Ok(mut e) = self.deps.engine.lock() {
                        let _ = e.unload();
                    }
                    self.unload_llm();
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
        // The mode is decided once, from the app in front (per-app table) or the picker; it stays for the dictation.
        let mode = resolve_app_mode(&settings.app_modes, (self.deps.frontmost)().as_deref(), settings.mode);
        self.dictation_mode = mode;
        // A fresh host session per dictation; chunk indexes restart at 0. The host learns the mode and whether a polish call follows.
        self.deps.remote.new_session();
        self.deps.remote.set_dictation(mode.id(), settings.polish_enabled && mode.uses_llm());
        let used_remote = self.deps.remote.in_use();
        self.last_used_remote = used_remote;
        self.fell_back = Arc::new(AtomicBool::new(false));
        let loading = !self.deps.engine.try_lock().map(|e| e.is_loaded()).unwrap_or(false);
        (self.emit)(PillState::Listening { seconds: 0.0, level_db: -100.0, countdown: None, loading, mode: mode.label().to_string(), stop_hint: stop_hint(&settings, self.machine.stage()), stopping: false, partial: None });
        self.preload_model();
        if settings.polish_enabled && mode.uses_llm() {
            let warm = self.deps.llm_warm.clone();
            if let Err(e) = std::thread::Builder::new().name("privatevox-llm-warm".into()).spawn(move || warm()) {
                log::warn!("llm warm thread: {e}");
            }
        }
        match self.deps.capture.start(on_pcm, on_error) {
            Ok(()) => {
                (self.deps.cue)(crate::cues::Cue::Start);
                self.recording = Some(Recording { buffer, level, started: Instant::now(), ticks: 0, chunker: Chunker::new(), scored_frames: 0, probs: Vec::new(), chunks_sent: 0, worker: None, results: Arc::new(Mutex::new(Default::default())), used_remote, fell_back: Arc::new(AtomicBool::new(false)), cancelled: Arc::new(AtomicBool::new(false)), silence_scored: 0, silence: TrailingSilence::new(), preview_window: String::new(), partial: None });
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
            .name("privatevox-preload".into())
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
        let previous = self.settings().model_id;
        if previous != id {
            self.switch_model(id);
            if self.settings().model_id != id {
                return; // the switch reported its own error
            }
        }
        self.benchmark_loaded(id);
        if previous != id {
            self.switch_model(&previous);
        }
    }

    /// Benchmarks whatever model is currently selected and loaded.
    fn benchmark_loaded(&mut self, id: &str) {
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
        // A benchmark measures this machine: never route it to a host.
        let result = privatevox_engine::routing::with_local_only(|| {
            self.deps.engine.lock().map_err(|e| anyhow::anyhow!("{e}")).and_then(|mut e| {
                let mut load_ms = 0u64;
                if !e.is_loaded() {
                    let Some(path) = path.as_ref() else { anyhow::bail!("no model file") };
                    load_ms = e.load(path, flash)?.duration.as_millis() as u64;
                }
                let opts = DecodeOptions { language: Some("en".into()), translate: false, initial_prompt: None, n_threads: 4, final_chunk: false };
                let t = Instant::now();
                e.transcribe(&pcm, &opts)?;
                Ok((t.elapsed(), load_ms))
            })
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
        use privatevox_core::undo::{decide, UndoDecision};
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
        let settings = self.settings();
        let max_clip = settings.max_clip_seconds;
        let mode = self.dictation_mode.label().to_string();
        let stop_hint = stop_hint(&settings, self.machine.stage());
        let latched = matches!(self.machine.stage(), privatevox_core::hotkeys::Stage::Recording { locked: true, .. });
        let Some(rec) = self.recording.as_mut() else {
            self.idle();
            return;
        };
        rec.ticks += 1;
        let seconds = rec.started.elapsed().as_secs_f32();
        let level = rec.level.lock().map(|l| *l).unwrap_or(-100.0);
        // Auto-stop (toggle mode): score the newest audio for silence every half second.
        let mut stopping = false;
        if let (true, Some(limit)) = (latched, settings.auto_stop_silence_secs) {
            if rec.ticks % 5 == 0 {
                let frames_available = rec.buffer.lock().map(|b| b.peek().len() / FRAME).unwrap_or(0);
                if frames_available > rec.silence_scored {
                    let slice: Vec<f32> = rec.buffer.lock().map(|b| b.peek()[rec.silence_scored * FRAME..frames_available * FRAME].to_vec()).unwrap_or_default();
                    if let Ok(probs) = self.vad.score(&slice) {
                        rec.silence.push(&probs, VadConfig::default().threshold);
                    }
                    rec.silence_scored = frames_available;
                }
            }
            let frame_ms = FRAME as f32 / SAMPLE_RATE as f32 * 1000.0;
            let limit = limit as f32;
            stopping = rec.silence.heard_speech() && rec.silence.silent_for(frame_ms) >= (limit - 1.0).max(0.0);
            if rec.silence.due(limit, frame_ms) {
                log::info!("auto-stop: {limit} s of silence after speech");
                self.machine.force_stop();
                self.stop(tx);
                return;
            }
        }
        // Live preview: every 3 s decode the last 6 s of audio if the engine is free, and show finished chunks plus that.
        if rec.ticks % 30 == 0 && preview_enabled(settings.live_preview, self.deps.remote.in_use(), self.total_memory) {
            let window: Vec<f32> = rec.buffer.lock().map(|b| { let p = b.peek(); let n = p.len().min(6 * SAMPLE_RATE as usize); p[p.len() - n..].to_vec() }).unwrap_or_default();
            if window.len() >= SAMPLE_RATE as usize {
                let opts = DecodeOptions { language: settings.spoken_languages.first().cloned(), translate: false, initial_prompt: initial_prompt(&settings.dictionary), n_threads: 4, final_chunk: false };
                let decoded = match self.deps.engine.try_lock() {
                    Ok(mut e) if e.is_loaded() => e.transcribe(&window, &opts).ok().map(|t| t.text),
                    _ => None, // busy loading or decoding a chunk: the preview waits for the next round
                };
                if let Some(text) = decoded {
                    rec.preview_window = normalize(&text);
                    let finished: Vec<String> = rec.results.lock().map(|m| m.values().cloned().collect()).unwrap_or_default();
                    let finished_refs: Vec<&str> = finished.iter().map(|s| s.as_str()).collect();
                    let p = partial_text(&finished_refs, &rec.preview_window, 70);
                    if !p.is_empty() && rec.partial.as_deref() != Some(p.as_str()) {
                        log::info!("preview: {p}");
                    }
                    rec.partial = (!p.is_empty()).then_some(p);
                }
            }
        }
        let partial = rec.partial.clone();
        // While the preload holds the engine lock the model is still loading.
        let loading = !self.deps.engine.try_lock().map(|e| e.is_loaded()).unwrap_or(false);
        (self.emit)(PillState::Listening { seconds, level_db: level, countdown: countdown_for(seconds, max_clip), loading, mode, stop_hint, stopping, partial });
        if rec.ticks % 5 == 0 {
            self.score_and_cut();
        }
        schedule(tx, Cmd::Tick(gen), Duration::from_millis(100));
    }

    /// Every 500 ms while recording: score only the new audio (plus a short overlap) and hand any certain
    /// chunk to the decode worker. Work per tick is bounded whatever the clip length.
    fn score_and_cut(&mut self) {
        let Some(rec) = self.recording.as_mut() else { return };
        let (available_frames, total) = match rec.buffer.lock() {
            Ok(b) => (b.peek().len() / FRAME, b.peek().len()),
            Err(_) => return,
        };
        if total < MAX_CHUNK {
            return; // short clips are decoded in one piece at stop
        }
        let window = score_window(rec.scored_frames, available_frames, SCORE_OVERLAP_FRAMES);
        if window.is_empty() {
            return;
        }
        let slice: Vec<f32> = match rec.buffer.lock() {
            Ok(b) => b.peek()[window.start * FRAME..window.end * FRAME].to_vec(),
            Err(_) => return,
        };
        let probs = match self.vad.score(&slice) {
            Ok(p) => p,
            Err(e) => {
                log::warn!("live vad score failed: {e}");
                return;
            }
        };
        let skip = rec.scored_frames - window.start;
        let new_probs = &probs[skip.min(probs.len())..];
        rec.probs.extend_from_slice(new_probs);
        rec.scored_frames = window.end;
        let ranges = rec.chunker.push(new_probs, FRAME, window.end * FRAME);
        if ranges.is_empty() {
            return;
        }
        let mut trimmed_chunks = Vec::with_capacity(ranges.len());
        for r in ranges {
            let slice: Vec<f32> = match rec.buffer.lock() {
                Ok(b) => b.peek()[r.clone()].to_vec(),
                Err(_) => return,
            };
            trimmed_chunks.push(trim_with_probs(&slice, &rec.probs, r.start));
        }
        for chunk in trimmed_chunks {
            self.send_chunk(chunk, false);
        }
    }

    /// Queues one already-trimmed chunk on the ordered decode worker (started on first use).
    fn send_chunk(&mut self, pcm: Vec<f32>, final_: bool) {
        let settings = self.settings();
        let remote = self.deps.remote.clone();
        let coordinator_fell_back = self.fell_back.clone();
        let Some(rec) = self.recording.as_mut() else { return };
        if rec.worker.is_none() {
            let (jtx, jrx) = std::sync::mpsc::channel::<ChunkJob>();
            let engine = self.deps.engine.clone();
            let remote = remote.clone();
            let use_remote = rec.used_remote;
            rec.fell_back = coordinator_fell_back;
            let fell_back = rec.fell_back.clone();
            let results = rec.results.clone();
            let cancelled = rec.cancelled.clone();
            let idle = self.deps.idle.clone();
            // Chunks decode with the first spoken language fixed; per-chunk detection would cost a pass per chunk (ruling, §18.7).
            let language = settings.spoken_languages.first().cloned();
            let translate = self.dictation_mode.whisper_translate();
            let prompt = initial_prompt(&settings.dictionary);
            let flash = settings.flash_attention;
            let model_path = self.deps.model_path.clone();
            let handle = std::thread::Builder::new()
                .name("privatevox-chunks".into())
                .spawn(move || {
                    for job in jrx {
                        if cancelled.load(Ordering::SeqCst) {
                            break;
                        }
                        // An empty final chunk still has to reach a host so it can close the session: send a sliver of silence.
                        let pcm = if job.pcm.is_empty() && job.final_ && remote.in_use() { vec![0.0f32; 1600] } else { job.pcm };
                        let text = if pcm.is_empty() {
                            String::new()
                        } else {
                            let opts = DecodeOptions { language: language.clone(), translate, initial_prompt: prompt.clone(), n_threads: 4, final_chunk: job.final_ };
                            let cancelled = cancelled.clone();
                            let r = if cancelled.load(Ordering::SeqCst) { Err(anyhow::anyhow!("cancelled")) } else { transcribe_routed(&remote, &engine, &pcm, &opts, use_remote, &fell_back, model_path.as_ref(), flash) };
                            if let Ok(mut t) = idle.lock() {
                                t.touch(Instant::now());
                            }
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
            let _ = jtx.send(ChunkJob { index, pcm, final_ });
        }
    }

    /// LLM unload on a helper thread; the idle path must not wait on the network.
    fn unload_llm(&self) {
        let unload = self.deps.llm_unload.clone();
        if let Err(e) = std::thread::Builder::new().name("privatevox-llm-unload".into()).spawn(move || unload()) {
            log::warn!("llm unload thread: {e}");
        }
    }

    /// Re-run the LLM over a stored row on a helper thread, so dictation stays responsive.
    fn repolish(&mut self, id: i64, mode: Mode, tx: &Sender<Cmd>) {
        if mode.uses_llm() && !self.settings().polish_enabled {
            self.error("Polish is off. Turn it on in Settings → Polish, then re-polish.");
            self.schedule_idle(tx);
            return;
        }
        let (polish, store, changed, tx) = (self.deps.polish.clone(), self.store.clone(), self.deps.on_history_changed.clone(), tx.clone());
        let warm = self.deps.llm_warm.clone();
        let spawned = std::thread::Builder::new().name("privatevox-repolish".into()).spawn(move || {
            let row = match store.lock().map_err(|e| e.to_string()).and_then(|s| s.get(id).map_err(|e| e.to_string())) {
                Ok(Some(r)) => r,
                Ok(None) => return,
                Err(e) => {
                    let _ = tx.send(Cmd::Notice(format!("Re-polish failed: {e}")));
                    return;
                }
            };
            if !mode.uses_llm() {
                if let Ok(mut s) = store.lock() {
                    let _ = s.update_polish(id, mode.id(), None, None, "none");
                }
                changed();
                return;
            }
            // Load the model first so a cold runtime gets the cold-start grace instead of a timeout.
            warm();
            let outcome = polish(mode, &row.raw_text);
            match outcome.status {
                PolishStatus::Polished { endpoint_id, .. } => {
                    if let Ok(mut s) = store.lock() {
                        if let Err(e) = s.update_polish(id, mode.id(), Some(&outcome.text), Some(&endpoint_id), "ok") {
                            let _ = tx.send(Cmd::Notice(format!("Re-polish failed: {e}")));
                            return;
                        }
                    }
                    changed();
                }
                PolishStatus::Unpolished { reason } => {
                    let _ = tx.send(Cmd::Notice(format!("Re-polish failed: {reason}")));
                }
                PolishStatus::NotNeeded => {}
            }
        });
        if let Err(e) = spawned {
            log::warn!("repolish thread: {e}");
        }
    }

    /// Decode a row's kept clip again (optionally with another model), then polish it the way the row was polished.
    fn rerun(&mut self, id: i64, model: &str, tx: &Sender<Cmd>) {
        if self.recording.is_some() {
            self.error("Finish the dictation first, then re-run");
            return;
        }
        let row = match self.store.lock().map_err(|e| e.to_string()).and_then(|s| s.get(id).map_err(|e| e.to_string())) {
            Ok(Some(r)) => r,
            Ok(None) => return,
            Err(e) => {
                self.error(&format!("Re-run failed: {e}"));
                self.schedule_idle(tx);
                return;
            }
        };
        let Some(path) = row.audio_path.as_deref().filter(|p| std::path::Path::new(p).exists()) else {
            self.error("No audio kept for this row (switch on Keep audio clips in Settings)");
            self.schedule_idle(tx);
            return;
        };
        let pcm = match privatevox_core::audio::load_wav_16k(std::path::Path::new(path)) {
            Ok(p) => p,
            Err(e) => {
                self.error(&format!("Re-run failed: {e}"));
                self.schedule_idle(tx);
                return;
            }
        };
        let settings = self.settings();
        let previous_model = settings.model_id.clone();
        if model != previous_model {
            self.switch_model(model);
            if self.settings().model_id != model {
                return; // switch_model reported the problem
            }
        }
        (self.emit)(PillState::Transcribing);
        let settings = self.settings();
        let language = row.language.clone().or_else(|| decode_language(&settings.spoken_languages));
        let mode = Mode::parse(&row.mode).unwrap_or(settings.mode);
        let opts = DecodeOptions { language, translate: mode.whisper_translate(), initial_prompt: initial_prompt(&settings.dictionary), n_threads: 4, final_chunk: true };
        let fell_back = Arc::new(AtomicBool::new(false));
        // Re-run goes wherever dictation goes right now: the host when it is in use, else this machine.
        let use_remote = self.deps.remote.in_use();
        if use_remote {
            self.deps.remote.new_session();
            self.deps.remote.set_dictation(mode.id(), false);
        }
        let text = match transcribe_routed(&self.deps.remote, &self.deps.engine, &pcm, &opts, use_remote, &fell_back, self.deps.model_path.as_ref(), settings.flash_attention) {
            Ok(t) => {
                let english = t.language.as_deref().map(|l| l == "en").unwrap_or(true);
                correct(&normalize_with(&t.text, english, settings.remove_fillers), &settings.dictionary, settings.dictionary_strictness)
            }
            Err(e) => {
                self.error(&format!("Re-run failed: {e}"));
                self.schedule_idle(tx);
                return;
            }
        };
        if text.is_empty() {
            self.error("Nothing heard in the kept clip");
            self.schedule_idle(tx);
            return;
        }
        let polish = if settings.polish_enabled && mode.uses_llm() {
            (self.emit)(PillState::Polishing { on: None });
            Some((self.deps.polish)(mode, &text))
        } else {
            None
        };
        if let Ok(mut st) = self.store.lock() {
            let _ = st.update_raw(id, &text, model);
            let _ = match polish.as_ref().map(|o| &o.status) {
                Some(PolishStatus::Polished { endpoint_id, .. }) => st.update_polish(id, mode.id(), polish.as_ref().map(|o| o.text.as_str()), Some(endpoint_id), "ok"),
                Some(PolishStatus::Unpolished { .. }) => st.update_polish(id, mode.id(), None, None, "unpolished"),
                _ => st.update_polish(id, mode.id(), None, None, "none"),
            };
        }
        (self.deps.on_history_changed)();
        (self.emit)(PillState::Pasted { preview: format!("Re-run done: {}", first_chars(polish.as_ref().map(|o| o.text.as_str()).unwrap_or(&text), 50)) });
        // A re-run is a one-off: the model in daily use stays what it was (like a benchmark).
        if model != previous_model {
            self.switch_model(&previous_model);
        }
        self.schedule_idle(tx);
    }

    fn schedule_idle(&mut self, tx: &Sender<Cmd>) {
        self.generation += 1;
        schedule(tx, Cmd::Tick(self.generation), Duration::from_millis(2000));
        self.pipeline_done(tx);
    }

    /// Tells the activation machine the pipeline drained; a remembered press starts the next recording.
    fn pipeline_done(&mut self, tx: &Sender<Cmd>) {
        if matches!(self.machine.stage(), privatevox_core::hotkeys::Stage::Processing) {
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
        if let Some(rec) = self.recording.take() {
            rec.cancelled.store(true, Ordering::SeqCst);
            drop(rec); // closes the worker's channel; it exits before its next job
            self.deps.capture.stop();
            self.generation += 1;
        }
        self.queued = None;
        let _ = self.deps.remote.take_notice(); // a fallback in the cancelled dictation must not label the next one
        self.idle();
        if let Some(id) = self.pending_switch.take() {
            self.switch_model(&id);
        }
    }

    fn stop(&mut self, tx: &Sender<Cmd>) {
        if self.recording.is_some() {
            (self.deps.cue)(crate::cues::Cue::Stop);
        }
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
        let frames = total / FRAME;
        let scored = self.recording.as_ref().map(|r| r.scored_frames).unwrap_or(0);
        let window = score_window(scored, frames, SCORE_OVERLAP_FRAMES);
        let ranges = {
            let r = self.recording.as_mut().unwrap();
            if !window.is_empty() {
                if let Ok(probs) = self.vad.score(&pcm[window.start * FRAME..window.end * FRAME]) {
                    let skip = scored - window.start;
                    let new_probs = &probs[skip.min(probs.len())..];
                    r.probs.extend_from_slice(new_probs);
                    let _ = r.chunker.push(new_probs, FRAME, window.end * FRAME);
                    r.scored_frames = window.end;
                }
            }
            let mut out = Vec::new();
            // Chunks whose cuts became certain only now, then the remainder.
            out.extend(r.chunker.finish(total));
            out
        };
        let probs_snapshot = self.recording.as_ref().map(|r| r.probs.clone()).unwrap_or_default();
        let n = ranges.len();
        for (i, r) in ranges.into_iter().enumerate() {
            let trimmed = trim_with_probs(&pcm[r.clone()], &probs_snapshot, r.start);
            self.send_chunk(trimmed, i + 1 == n);
        }
        if n == 0 {
            // Everything was already sent while speaking; a host still needs to hear that the dictation ended.
            self.send_chunk(Vec::new(), true);
        }
        let rec = self.recording.take().unwrap();
        let chunks = rec.chunks_sent;
        if let Some((jtx, handle)) = rec.worker {
            drop(jtx);
            let _ = handle.join();
        }
        let texts: Vec<String> = rec.results.lock().map(|m| m.values().cloned().collect()).unwrap_or_default();
        let settings = self.settings();
        let english = settings.spoken_languages.first().map(|l| l == "en").unwrap_or(true);
        let text = correct(&normalize_with(&texts.join(" "), english, settings.remove_fillers), &settings.dictionary, settings.dictionary_strictness);
        if text.is_empty() {
            (self.deps.cue)(crate::cues::Cue::NothingHeard);
            self.error("Nothing heard");
            self.schedule_idle(tx);
            return;
        }
        if let Ok(mut idle) = self.deps.idle.lock() {
            idle.touch(Instant::now());
        }
        let decode_ms = t_total.elapsed().as_millis() as u64;
        // Chunks decode with the first spoken language fixed (Task 2 ruling), so the row says so.
        self.paste_and_store(text, (total as f32 / SAMPLE_RATE as f32 * 1000.0) as u32, serde_json::json!({ "chunks": chunks, "tail_ms": decode_ms, "total_ms": decode_ms, "language_source": "fixed" }), Some(pcm), tx);
    }

    /// Shared tail of both decode paths: polish (when the mode asks for it), paste, store.
    fn paste_and_store(&mut self, raw: String, duration_ms: u32, mut timing: serde_json::Value, audio: Option<Vec<f32>>, tx: &Sender<Cmd>) {
        let settings = self.settings();
        let mode = self.dictation_mode;
        // Spoken commands: only the trailing phrase is looked at (A3).
        let raw = if settings.spoken_commands {
            let (rest, cmd) = strip_trailing(&raw);
            match cmd {
                Some(SpokenCommand::Cancel) => {
                    (self.emit)(PillState::Pasted { preview: "Cancelled (nothing pasted)".into() });
                    self.schedule_idle(tx);
                    return;
                }
                Some(SpokenCommand::DeleteThat) | Some(SpokenCommand::Undo) => {
                    let _ = tx.send(Cmd::Undo);
                    (self.emit)(PillState::Pasted { preview: "Undoing the last paste".into() });
                    self.schedule_idle(tx);
                    return;
                }
                Some(SpokenCommand::NewLine) => format!("{rest}\n"),
                Some(SpokenCommand::NewParagraph) => format!("{rest}\n\n"),
                None => raw,
            }
        } else {
            raw
        };
        if raw.trim().is_empty() {
            self.error("Nothing heard");
            self.schedule_idle(tx);
            return;
        }
        let t_polish = Instant::now();
        let engine_notice = self.deps.remote.take_notice();
        let outcome = if settings.polish_enabled && mode.uses_llm() {
            if self.deps.remote.in_use() {
                (self.emit)(PillState::Polishing { on: Some(self.deps.remote.label()) });
                match remote_polish(&self.deps.remote, mode, &raw, Duration::from_secs(settings.llm_timeout_secs.max(8) + 25)) {
                    Some(o) => o,
                    None => {
                        (self.emit)(PillState::Polishing { on: None });
                        let mut o = (self.deps.polish)(mode, &raw);
                        o.notice = Some("PC unreachable, polished locally".into());
                        o
                    }
                }
            } else {
                (self.emit)(PillState::Polishing { on: None });
                (self.deps.polish)(mode, &raw)
            }
        } else {
            PolishOutcome { text: raw.clone(), status: PolishStatus::NotNeeded, notice: None }
        };
        let engine_kind = if self.last_used_remote && engine_notice.is_none() && !self.fell_back.load(Ordering::SeqCst) { format!("remote:{}", self.deps.remote.label()) } else { "local".to_string() };
        let polish_ms = t_polish.elapsed().as_millis() as u64;
        let (polished_text, polished_by, polish_status) = match &outcome.status {
            PolishStatus::Polished { endpoint_id, .. } => (Some(outcome.text.clone()), Some(endpoint_id.clone()), "ok"),
            PolishStatus::Unpolished { reason } => {
                log::warn!("polish failed, pasting raw: {reason}");
                (None, None, "unpolished")
            }
            PolishStatus::NotNeeded => (None, None, "none"),
        };
        let text = outcome.text;
        let t = Instant::now();
        let app_name = (self.deps.frontmost)();
        let (status, preview) = match self.deps.paste.paste(&text) {
            Ok(PasteOutcome::Pasted) => {
                self.last_paste = Some(privatevox_core::undo::LastPaste { text: text.clone(), app: app_name.clone(), at: Instant::now() });
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
        let preview = match (&outcome.notice, polish_status) {
            (Some(n), _) => format!("{n} · {preview}"),
            (None, "unpolished") => format!("Unpolished · {preview}"),
            _ => preview,
        };
        let preview = match engine_notice {
            Some(n) => format!("{n} · {preview}"),
            None => preview,
        };
        let language = timing.get("language").and_then(|v| v.as_str()).map(|v| v.to_string()).or_else(|| settings.spoken_languages.first().cloned());
        if let Some(obj) = timing.as_object_mut() {
            obj.insert("paste_ms".into(), serde_json::json!(t.elapsed().as_millis() as u64));
            obj.insert("polish_ms".into(), serde_json::json!(polish_ms));
            obj.remove("language");
        }
        let row = NewDictation {
            app_name,
            mode: mode.id().into(),
            model: settings.model_id.clone(),
            language,
            duration_ms,
            raw_text: raw,
            polished_text,
            polished_by,
            polish_status: polish_status.into(),
            status: status.into(),
            engine_kind,
            timing_json: Some(timing.to_string()),
        };
        let inserted = self.store.lock().ok().and_then(|mut store| match store.insert(&row) {
            Ok(id) => Some(id),
            Err(e) => {
                log::error!("history insert failed: {e}");
                None
            }
        });
        if let (true, Some(id), Some(pcm)) = (settings.keep_audio, inserted, audio) {
            // Off the pipeline thread: a 5-minute WAV must never delay the next dictation.
            let path = self.deps.audio_dir.join(format!("{id}.wav"));
            let store = self.store.clone();
            std::thread::Builder::new()
                .name("privatevox-keep-audio".into())
                .spawn(move || match privatevox_core::audio::write_wav_16k(&path, &pcm) {
                    Ok(()) => {
                        if let Ok(mut st) = store.lock() {
                            finalize_clip(&mut st, id, &path);
                        }
                    }
                    Err(e) => log::warn!("keep audio: {e}"),
                })
                .ok();
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
                (self.deps.cue)(crate::cues::Cue::NothingHeard);
                self.error("Microphone is silent");
                self.schedule_idle(tx);
                return;
            }
            Ok(ClipClass::NothingHeard) => {
                (self.deps.cue)(crate::cues::Cue::NothingHeard);
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
        let language = decode_language(&settings.spoken_languages);
        // A short clip is decoded in one go, so this is also the last chunk of its session.
        let opts = DecodeOptions { language, translate: self.dictation_mode.whisper_translate(), initial_prompt: initial_prompt(&settings.dictionary), n_threads: 4, final_chunk: true };
        let t = Instant::now();
        let transcript = transcribe_routed(&self.deps.remote, &self.deps.engine, &speech, &opts, self.last_used_remote, &self.fell_back, self.deps.model_path.as_ref(), settings.flash_attention);
        let mut transcript = match transcript {
            Ok(t) => t,
            Err(e) => {
                self.error(&format!("Transcription failed: {e}"));
                self.schedule_idle(tx);
                return;
            }
        };
        // Several spoken languages: the model detected; if it picked one off the list, decode again with the first.
        let (settled_language, language_source) = settle(&settings.spoken_languages, transcript.language.as_deref());
        // Never on the host path: a second final chunk would corrupt the host's session; the host's pick is recorded.
        if language_source == LanguageSource::Fallback && settings.spoken_languages.len() > 1 && !self.last_used_remote {
            let opts = DecodeOptions { language: settled_language.clone(), ..opts };
            match transcribe_routed(&self.deps.remote, &self.deps.engine, &speech, &opts, self.last_used_remote, &self.fell_back, self.deps.model_path.as_ref(), settings.flash_attention) {
                Ok(t2) => transcript = t2,
                Err(e) => log::warn!("re-run with {:?} failed, keeping the detected decode: {e}", settled_language),
            }
        }
        transcript.language = settled_language.or(transcript.language);
        if let Ok(mut idle) = self.deps.idle.lock() {
            idle.touch(Instant::now());
        }
        let decode_ms = t.elapsed().as_millis() as u64;
        let english = transcript.language.as_deref().or(settings.spoken_languages.first().map(|s| s.as_str())).map(|l| l == "en").unwrap_or(true);
        let text = correct(&normalize_with(&transcript.text, english, settings.remove_fillers), &settings.dictionary, settings.dictionary_strictness);
        if text.is_empty() {
            (self.deps.cue)(crate::cues::Cue::NothingHeard);
            self.error("Nothing heard");
            self.schedule_idle(tx);
            return;
        }
        let timing = serde_json::json!({
            "capture_ms": capture_ms, "vad_ms": vad_ms, "decode_ms": decode_ms,
            "total_ms": t_total.elapsed().as_millis() as u64, "audio_ctx": transcript.audio_ctx, "reran": transcript.reran,
            "language": transcript.language, "language_source": language_source.id(),
        });
        self.paste_and_store(text, duration_ms, timing, Some(speech), tx);
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
    use privatevox_core::net::fake_host::{FakeHost, HostBehaviour};
    use privatevox_core::polish::{Mode, PolishOutcome, PolishStatus};
use privatevox_core::preview::{partial_text, preview_enabled};
    use privatevox_engine::remote::RemoteState;
    use std::sync::atomic::Ordering::SeqCst;
    use privatevox_core::settings::Settings;
    use privatevox_core::store::Store;
    use privatevox_core::vad::Vad;
    use privatevox_engine::protocol::DecodeOptions;
    use privatevox_engine::supervisor::{Engine, LoadInfo, Transcript};
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
        finals: Arc<Mutex<Vec<bool>>>,
        prompts: Arc<Mutex<Vec<Option<String>>>>,
        languages: Arc<Mutex<Vec<Option<String>>>>,
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
        fn transcribe(&mut self, _pcm: &[f32], opts: &DecodeOptions) -> anyhow::Result<Transcript> {
            self.decodes.lock().unwrap().push(std::time::Instant::now());
            self.finals.lock().unwrap().push(opts.final_chunk);
            self.prompts.lock().unwrap().push(opts.initial_prompt.clone());
            self.languages.lock().unwrap().push(opts.language.clone());
            let n = self.decodes.lock().unwrap().len();
            let text = if self.text == "{n}" { n.to_string() } else { self.text.clone() };
            // Auto-detect (no language given) "hears" French; a fixed language is echoed back, like Whisper.
            let language = Some(opts.language.clone().unwrap_or_else(|| "fr".into()));
            Ok(Transcript { text, language, decode_ms: 5, audio_ctx: 768, reran: false })
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
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../crates/privatevox-core/tests/fixtures/hello.wav");
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
        /// Swappable polish behaviour; the default passes text through as "Polished".
        polish_fn: Arc<Mutex<Box<dyn Fn(Mode, &str) -> PolishOutcome + Send>>>,
        polish_calls: Arc<Mutex<Vec<(Mode, String)>>>,
        warms: Arc<Mutex<u32>>,
        llm_unloads: Arc<Mutex<u32>>,
        history_changed: Arc<Mutex<u32>>,
        cues: Arc<Mutex<Vec<crate::cues::Cue>>>,
        tx: std::sync::mpsc::Sender<Cmd>,
        join: std::thread::JoinHandle<()>,
        _dir: tempfile::TempDir,
    }

    fn pass_through() -> Box<dyn Fn(Mode, &str) -> PolishOutcome + Send> {
        Box::new(|_m, raw| PolishOutcome { text: raw.to_string(), status: PolishStatus::Polished { endpoint_id: "fake".into(), model: "m".into(), ms: 1 }, notice: None })
    }

    fn harness(engine: FakeEngine, capture: FakeCapture) -> Harness {
        harness_with(Box::new(engine), capture, RemoteState::new())
    }

    /// Like `harness`, but the fake engine sits behind a `RoutingEngine` driven by `remote`.
    fn harness_remote(engine: FakeEngine, capture: FakeCapture, remote: Arc<RemoteState>) -> Harness {
        let routing = privatevox_engine::routing::RoutingEngine::new(Box::new(engine), privatevox_engine::remote::RemoteEngine::new(remote.clone(), Duration::from_secs(2)), remote.clone(), Arc::new(Mutex::new(None)), true);
        harness_with(Box::new(routing), capture, remote)
    }

    fn harness_with(engine: Box<dyn Engine>, capture: FakeCapture, remote: Arc<RemoteState>) -> Harness {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(Store::open(&dir.path().join("privatevox.sqlite")).unwrap()));
        let mut settings = Settings::default();
        settings.post_release_ms = 0;
        let settings = Arc::new(Mutex::new(settings));
        let vad = Vad::new(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../resources/models/ggml-silero-v5.1.2.bin")).unwrap();
        let states = Arc::new(Mutex::new(Vec::new()));
        let pasted = Arc::new(Mutex::new(Vec::new()));
        let models_dir = dir.path().join("models");
        std::fs::create_dir_all(&models_dir).unwrap();
        for m in privatevox_core::models::catalog() {
            std::fs::write(models_dir.join(m.file), b"stub").unwrap();
        }
        let switched = Arc::new(Mutex::new(Vec::new()));
        let sw = switched.clone();
        let benchmarks: Arc<Mutex<Vec<(String, f32)>>> = Default::default();
        let bm = benchmarks.clone();
        let undos: Arc<Mutex<u32>> = Default::default();
        let frontmost: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(Some("TestApp".into())));
        let fm = frontmost.clone();
        let polish_fn: Arc<Mutex<Box<dyn Fn(Mode, &str) -> PolishOutcome + Send>>> = Arc::new(Mutex::new(pass_through()));
        let polish_calls: Arc<Mutex<Vec<(Mode, String)>>> = Default::default();
        let (pf, pc) = (polish_fn.clone(), polish_calls.clone());
        let warms: Arc<Mutex<u32>> = Default::default();
        let llm_unloads: Arc<Mutex<u32>> = Default::default();
        let history_changed: Arc<Mutex<u32>> = Default::default();
        let (w, u, hc) = (warms.clone(), llm_unloads.clone(), history_changed.clone());
        let cues: Arc<Mutex<Vec<crate::cues::Cue>>> = Default::default();
        let cq = cues.clone();
        let deps = Deps {
            engine: Arc::new(Mutex::new(engine)),
            remote: remote.clone(),
            capture: Box::new(capture),
            paste: Box::new(FakePaste(pasted.clone(), undos.clone())),
            model_path: Some(models_dir.join("ggml-large-v3-turbo-q5_0.bin")),
            model_dir: models_dir,
            frontmost: Box::new(move || fm.lock().unwrap().clone()),
            idle: Arc::new(Mutex::new(privatevox_core::lifecycle::IdleTimer::new(None))),
            on_model_switched: Box::new(move |id, _info| sw.lock().unwrap().push(id.to_string())),
            benchmark_clip: Box::new(|| Ok(vec![0.1f32; 16_000 * 30])),
            on_benchmark: Box::new(move |id, speed, _load_ms| bm.lock().unwrap().push((id.to_string(), speed))),
            polish: Arc::new(move |mode, raw| {
                pc.lock().unwrap().push((mode, raw.to_string()));
                (pf.lock().unwrap())(mode, raw)
            }),
            llm_warm: Arc::new(move || *w.lock().unwrap() += 1),
            llm_unload: Arc::new(move || *u.lock().unwrap() += 1),
            on_history_changed: Arc::new(move || *hc.lock().unwrap() += 1),
            audio_dir: dir.path().join("audio"),
            cue: Arc::new(move |c| cq.lock().unwrap().push(c)),
        };
        let st = states.clone();
        let coord = Coordinator::new(deps, settings.clone(), store.clone(), vad, Box::new(move |s| st.lock().unwrap().push(s)));
        let (tx, rx) = channel();
        let tx2 = tx.clone();
        let join = std::thread::spawn(move || coord.run(rx, tx2));
        let _ = &remote;
        Harness { states, pasted, store, settings, switched, benchmarks, undos, frontmost, polish_fn, polish_calls, warms, llm_unloads, history_changed, cues, tx, join, _dir: dir }
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
        let h = harness(FakeEngine { load_delay: Duration::ZERO, text: "Hello.".into(), loaded: true, load_calls: Default::default(), loads: Default::default(), unloads: Default::default(), decodes: Default::default(), finals: Default::default(), prompts: Default::default(), languages: Default::default() }, FakeCapture { pcm: fixture(), fail: false, die_after_start: false });
        let (states, pasted, count) = run_once(h);
        assert_eq!(pasted, vec!["Hello.".to_string()]);
        assert_eq!(count, 1);
        let k = kinds(&states);
        let pos = |name: &str| k.iter().position(|x| *x == name).unwrap_or_else(|| panic!("{name} missing in {k:?}"));
        assert!(pos("listening") < pos("transcribing") && pos("transcribing") < pos("pasted"), "{k:?}");
    }

    #[test]
    fn pipeline_discards_clips_under_300ms() {
        let h = harness(FakeEngine { load_delay: Duration::ZERO, text: "x".into(), loaded: true, load_calls: Default::default(), loads: Default::default(), unloads: Default::default(), decodes: Default::default(), finals: Default::default(), prompts: Default::default(), languages: Default::default() }, FakeCapture { pcm: vec![0.3; 2_000], fail: false, die_after_start: false });
        let (states, pasted, count) = run_once(h);
        assert!(pasted.is_empty());
        assert_eq!(count, 0);
        assert_eq!(kinds(&states).last().copied(), Some("idle"), "{:?}", kinds(&states));
    }

    #[test]
    fn pipeline_reports_silent_mic() {
        let h = harness(FakeEngine { load_delay: Duration::ZERO, text: "x".into(), loaded: true, load_calls: Default::default(), loads: Default::default(), unloads: Default::default(), decodes: Default::default(), finals: Default::default(), prompts: Default::default(), languages: Default::default() }, FakeCapture { pcm: vec![0.0; 16_000], fail: false, die_after_start: false });
        let (states, pasted, count) = run_once(h);
        assert!(pasted.is_empty());
        assert_eq!(count, 0);
        assert!(states.iter().any(|s| matches!(s, PillState::Error { message } if message == "Microphone is silent")), "{:?}", kinds(&states));
    }

    #[test]
    fn pipeline_queues_clip_until_engine_loaded() {
        let h = harness(FakeEngine { load_delay: Duration::from_millis(300), text: "Hello.".into(), loaded: false, load_calls: Default::default(), loads: Default::default(), unloads: Default::default(), decodes: Default::default(), finals: Default::default(), prompts: Default::default(), languages: Default::default() }, FakeCapture { pcm: fixture(), fail: false, die_after_start: false });
        let (states, pasted, _) = run_once(h);
        assert_eq!(pasted, vec!["Hello.".to_string()]);
        assert!(kinds(&states).contains(&"loading_model"), "{:?}", kinds(&states));
    }

    #[test]
    fn direct_stop_after_hotkey_start_keeps_hotkeys_working() {
        use privatevox_core::hotkeys::{Binding, Input};
        let h = harness(FakeEngine { load_delay: Duration::ZERO, text: "Hello.".into(), loaded: true, load_calls: Default::default(), loads: Default::default(), unloads: Default::default(), decodes: Default::default(), finals: Default::default(), prompts: Default::default(), languages: Default::default() }, FakeCapture { pcm: fixture(), fail: false, die_after_start: false });
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
        let h = harness(FakeEngine { load_delay: Duration::ZERO, text: "x".into(), loaded: true, load_calls: Default::default(), loads: Default::default(), unloads: Default::default(), decodes: Default::default(), finals: Default::default(), prompts: Default::default(), languages: Default::default() }, FakeCapture { pcm: fixture(), fail: false, die_after_start: true });
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
        let h = harness(FakeEngine { load_delay: Duration::from_millis(200), text: "Hello.".into(), loaded: false, load_calls: calls.clone(), loads: Default::default(), unloads: Default::default(), decodes: Default::default(), finals: Default::default(), prompts: Default::default(), languages: Default::default() }, FakeCapture { pcm: fixture(), fail: false, die_after_start: false });
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
        let h = harness(FakeEngine { load_delay: Duration::ZERO, text: "x".into(), loaded: true, load_calls: Default::default(), loads: loads.clone(), unloads: unloads.clone(), decodes: Default::default(), finals: Default::default(), prompts: Default::default(), languages: Default::default() }, FakeCapture { pcm: fixture(), fail: false, die_after_start: false });
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
        let h = harness(FakeEngine { load_delay: Duration::ZERO, text: "Hello.".into(), loaded: true, load_calls: Default::default(), loads: loads.clone(), unloads: Default::default(), decodes: Default::default(), finals: Default::default(), prompts: Default::default(), languages: Default::default() }, FakeCapture { pcm: fixture(), fail: false, die_after_start: false });
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
        let h = harness(FakeEngine { load_delay: Duration::ZERO, text: "x".into(), loaded: true, load_calls: Default::default(), loads: Default::default(), unloads: Default::default(), decodes: Default::default(), finals: Default::default(), prompts: Default::default(), languages: Default::default() }, FakeCapture { pcm: fixture(), fail: false, die_after_start: false });
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
        let h = harness(FakeEngine { load_delay: Duration::ZERO, text: "bench".into(), loaded: true, load_calls: Default::default(), loads: Default::default(), unloads: Default::default(), decodes: Default::default(), finals: Default::default(), prompts: Default::default(), languages: Default::default() }, FakeCapture { pcm: fixture(), fail: false, die_after_start: false });
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
        let h = harness(FakeEngine { load_delay: Duration::ZERO, text: "{n}".into(), loaded: true, load_calls: Default::default(), loads: Default::default(), unloads: Default::default(), decodes: decodes.clone(), finals: Default::default(), prompts: Default::default(), languages: Default::default() }, FakeCapture { pcm: long_fixture(90), fail: false, die_after_start: false });
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
    fn listening_reports_loading_while_model_loads() {
        let h = harness(FakeEngine { load_delay: Duration::from_millis(400), text: "Hello.".into(), loaded: false, load_calls: Default::default(), loads: Default::default(), unloads: Default::default(), decodes: Default::default(), finals: Default::default(), prompts: Default::default(), languages: Default::default() }, FakeCapture { pcm: fixture(), fail: false, die_after_start: false });
        h.tx.send(Cmd::Start).unwrap();
        std::thread::sleep(Duration::from_millis(250)); // a couple of ticks while the preload holds the engine
        h.tx.send(Cmd::Stop).unwrap();
        std::thread::sleep(Duration::from_millis(600));
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
        let states = h.states.lock().unwrap().clone();
        assert!(states.iter().any(|s| matches!(s, PillState::Listening { loading: true, .. })), "{:?}", kinds(&states));
        assert_eq!(h.pasted.lock().unwrap().clone(), vec!["Hello.".to_string()]);
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
        let h = harness(FakeEngine { load_delay: Duration::ZERO, text: "{n}".into(), loaded: true, load_calls: Default::default(), loads: Default::default(), unloads: Default::default(), decodes: Default::default(), finals: Default::default(), prompts: Default::default(), languages: Default::default() }, FakeCapture { pcm: long_fixture(45), fail: false, die_after_start: false });
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
        let h = harness(FakeEngine { load_delay: Duration::ZERO, text: "Hello.".into(), loaded: true, load_calls: Default::default(), loads: Default::default(), unloads: Default::default(), decodes: Default::default(), finals: Default::default(), prompts: Default::default(), languages: Default::default() }, FakeCapture { pcm: fixture(), fail: false, die_after_start: false });
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
        let h = harness(FakeEngine { load_delay: Duration::ZERO, text: "Hello.".into(), loaded: true, load_calls: Default::default(), loads: Default::default(), unloads: Default::default(), decodes: Default::default(), finals: Default::default(), prompts: Default::default(), languages: Default::default() }, FakeCapture { pcm: fixture(), fail: false, die_after_start: false });
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
    fn paste_text_pastes_without_recording() {
        let h = harness(FakeEngine { load_delay: Duration::ZERO, text: "x".into(), loaded: true, load_calls: Default::default(), loads: Default::default(), unloads: Default::default(), decodes: Default::default(), finals: Default::default(), prompts: Default::default(), languages: Default::default() }, FakeCapture { pcm: fixture(), fail: false, die_after_start: false });
        h.tx.send(Cmd::PasteText("from history".into())).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
        assert_eq!(h.pasted.lock().unwrap().clone(), vec!["from history".to_string()]);
        assert_eq!(h.store.lock().unwrap().count().unwrap(), 0, "pasting again does not add a history row");
    }

    #[test]
    fn switch_requested_then_cancelled_is_applied() {
        let loads: Arc<Mutex<Vec<PathBuf>>> = Default::default();
        let h = harness(FakeEngine { load_delay: Duration::ZERO, text: "x".into(), loaded: true, load_calls: Default::default(), loads: loads.clone(), unloads: Default::default(), decodes: Default::default(), finals: Default::default(), prompts: Default::default(), languages: Default::default() }, FakeCapture { pcm: fixture(), fail: false, die_after_start: false });
        h.tx.send(Cmd::Start).unwrap();
        h.tx.send(Cmd::SwitchModel("large-v3".into())).unwrap();
        h.tx.send(Cmd::Cancel).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
        assert!(loads.lock().unwrap().iter().any(|p| p.ends_with("ggml-large-v3-q5_0.bin")), "a switch requested during a cancelled recording must still be applied");
        assert_eq!(h.settings.lock().unwrap().model_id, "large-v3");
    }

    #[test]
    fn pause_during_long_recording_stops_chunk_worker() {
        let loads: Arc<Mutex<Vec<PathBuf>>> = Default::default();
        let unloads: Arc<Mutex<u32>> = Default::default();
        let decodes: Arc<Mutex<Vec<std::time::Instant>>> = Default::default();
        let h = harness(FakeEngine { load_delay: Duration::ZERO, text: "{n}".into(), loaded: true, load_calls: Default::default(), loads: loads.clone(), unloads: unloads.clone(), decodes: decodes.clone(), finals: Default::default(), prompts: Default::default(), languages: Default::default() }, FakeCapture { pcm: long_fixture(150), fail: false, die_after_start: false });
        h.tx.send(Cmd::Start).unwrap();
        std::thread::sleep(Duration::from_millis(700)); // first scoring tick hands chunks to the worker
        h.tx.send(Cmd::PauseAndUnload).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while *unloads.lock().unwrap() == 0 && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(*unloads.lock().unwrap() >= 1, "pause must unload");
        std::thread::sleep(Duration::from_millis(150));
        let decodes_after_pause = decodes.lock().unwrap().len();
        let loads_after_pause = loads.lock().unwrap().len();
        std::thread::sleep(Duration::from_millis(400));
        assert_eq!(loads.lock().unwrap().len(), loads_after_pause, "the chunk worker must not reload the model after a pause");
        assert_eq!(decodes.lock().unwrap().len(), decodes_after_pause, "no further chunk decodes after the pause");
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
        assert!(h.pasted.lock().unwrap().is_empty());
    }

    #[test]
    fn benchmark_restores_previous_model() {
        let loads: Arc<Mutex<Vec<PathBuf>>> = Default::default();
        let h = harness(FakeEngine { load_delay: Duration::ZERO, text: "bench".into(), loaded: true, load_calls: Default::default(), loads: loads.clone(), unloads: Default::default(), decodes: Default::default(), finals: Default::default(), prompts: Default::default(), languages: Default::default() }, FakeCapture { pcm: fixture(), fail: false, die_after_start: false });
        h.tx.send(Cmd::Benchmark("large-v3".into())).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
        assert_eq!(h.benchmarks.lock().unwrap()[0].0, "large-v3");
        assert_eq!(h.settings.lock().unwrap().model_id, "large-v3-turbo", "benchmarking must not change the active model");
        let l = loads.lock().unwrap();
        assert!(l.iter().any(|p| p.ends_with("ggml-large-v3-q5_0.bin")));
        assert!(l.last().unwrap().ends_with("ggml-large-v3-turbo-q5_0.bin"), "previous model reloaded last: {l:?}");
    }

    #[test]
    fn score_window_is_bounded_and_overlapping() {
        // nothing scored yet: score everything available
        assert_eq!(score_window(0, 100, 31), 0..100);
        // 100 frames scored, 120 available: rescore 31 frames of overlap plus the 20 new ones
        assert_eq!(score_window(100, 120, 31), 69..120);
        // no new frames: nothing to do
        assert_eq!(score_window(120, 120, 31), 120..120);
    }

    #[test]
    fn pipeline_reports_capture_error() {
        let h = harness(FakeEngine { load_delay: Duration::ZERO, text: "x".into(), loaded: true, load_calls: Default::default(), loads: Default::default(), unloads: Default::default(), decodes: Default::default(), finals: Default::default(), prompts: Default::default(), languages: Default::default() }, FakeCapture { pcm: vec![], fail: true, die_after_start: false });
        let (states, pasted, count) = run_once_waiting(h, Duration::from_millis(2300));
        assert!(pasted.is_empty());
        assert_eq!(count, 0);
        assert!(states.iter().any(|s| matches!(s, PillState::Error { .. })), "{:?}", kinds(&states));
        assert_eq!(kinds(&states).last().copied(), Some("idle"));
    }

    fn engine_saying(text: &str) -> FakeEngine {
        FakeEngine { load_delay: Duration::ZERO, text: text.into(), loaded: true, load_calls: Default::default(), loads: Default::default(), unloads: Default::default(), decodes: Default::default(), finals: Default::default(), prompts: Default::default(), languages: Default::default() }
    }

    fn capture_ok() -> FakeCapture {
        FakeCapture { pcm: fixture(), fail: false, die_after_start: false }
    }

    fn last_row(h: &Harness) -> privatevox_core::store::Dictation {
        h.store.lock().unwrap().list(1, 0).unwrap().remove(0)
    }

    #[test]
    fn clean_mode_pastes_polished_text_and_stores_both() {
        let h = harness(engine_saying("hello there"), capture_ok());
        *h.polish_fn.lock().unwrap() = Box::new(|_m, _raw| PolishOutcome { text: "Hello there.".into(), status: PolishStatus::Polished { endpoint_id: "ep-1".into(), model: "q".into(), ms: 3 }, notice: None });
        h.tx.send(Cmd::Start).unwrap();
        h.tx.send(Cmd::Stop).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(h.pasted.lock().unwrap().clone(), vec!["Hello there.".to_string()]);
        let row = last_row(&h);
        assert_eq!(row.raw_text, "hello there");
        assert_eq!(row.polished_text.as_deref(), Some("Hello there."));
        assert_eq!(row.polish_status, "ok");
        assert_eq!(row.polished_by.as_deref(), Some("ep-1"));
        assert_eq!(row.mode, "clean");
        assert_eq!(h.polish_calls.lock().unwrap()[0], (Mode::Clean, "hello there".to_string()));
        let k = kinds(&h.states.lock().unwrap());
        assert!(k.contains(&"polishing"), "{k:?}");
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
    }

    #[test]
    fn raw_mode_skips_polish_and_warm() {
        let h = harness(engine_saying("hello"), capture_ok());
        h.settings.lock().unwrap().mode = Mode::Raw;
        h.tx.send(Cmd::Start).unwrap();
        h.tx.send(Cmd::Stop).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(*h.warms.lock().unwrap(), 0);
        assert!(h.polish_calls.lock().unwrap().is_empty());
        let row = last_row(&h);
        assert_eq!(row.polish_status, "none");
        assert_eq!(row.mode, "raw");
        assert!(row.polished_text.is_none());
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
    }

    #[test]
    fn unpolished_outcome_pastes_raw_and_marks_row() {
        let h = harness(engine_saying("raw words"), capture_ok());
        *h.polish_fn.lock().unwrap() = Box::new(|_m, raw| PolishOutcome { text: raw.to_string(), status: PolishStatus::Unpolished { reason: "timed out".into() }, notice: None });
        h.tx.send(Cmd::Start).unwrap();
        h.tx.send(Cmd::Stop).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(h.pasted.lock().unwrap().clone(), vec!["raw words".to_string()]);
        let row = last_row(&h);
        assert_eq!(row.polish_status, "unpolished");
        assert!(row.polished_text.is_none());
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
    }

    #[test]
    fn warm_fires_on_start_in_llm_modes() {
        let h = harness(engine_saying("x"), capture_ok());
        h.tx.send(Cmd::Start).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(*h.warms.lock().unwrap(), 1);
        h.tx.send(Cmd::Cancel).unwrap();
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
    }

    #[test]
    fn idle_unload_also_unloads_llm() {
        let h = harness(engine_saying("x"), capture_ok());
        h.tx.send(Cmd::ModelUnloaded).unwrap();
        h.tx.send(Cmd::PauseAndUnload).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(*h.llm_unloads.lock().unwrap(), 2);
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
    }

    #[test]
    fn notice_prefixes_the_pasted_preview() {
        let h = harness(engine_saying("hi"), capture_ok());
        *h.polish_fn.lock().unwrap() = Box::new(|_m, _raw| PolishOutcome { text: "Hi.".into(), status: PolishStatus::Polished { endpoint_id: "l".into(), model: "m".into(), ms: 1 }, notice: Some("PC unreachable, polished locally".into()) });
        h.tx.send(Cmd::Start).unwrap();
        h.tx.send(Cmd::Stop).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let states = h.states.lock().unwrap().clone();
        let preview = states.iter().rev().find_map(|s| if let PillState::Pasted { preview } = s { Some(preview.clone()) } else { None }).unwrap();
        assert!(preview.starts_with("PC unreachable, polished locally · "), "{preview}");
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
    }

    #[test]
    fn repolish_updates_row_and_notifies() {
        let h = harness(engine_saying("one two"), capture_ok());
        h.tx.send(Cmd::Start).unwrap();
        h.tx.send(Cmd::Stop).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let id = last_row(&h).id;
        *h.polish_fn.lock().unwrap() = Box::new(|m, _raw| PolishOutcome { text: format!("- one\n- two ({})", m.id()), status: PolishStatus::Polished { endpoint_id: "ep-2".into(), model: "m".into(), ms: 1 }, notice: None });
        h.tx.send(Cmd::Repolish { id, mode: Mode::Bullets }).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let row = h.store.lock().unwrap().get(id).unwrap().unwrap();
        assert_eq!(row.mode, "bullets");
        assert_eq!(row.polished_text.as_deref(), Some("- one\n- two (bullets)"));
        assert_eq!(row.polished_by.as_deref(), Some("ep-2"));
        assert_eq!(row.raw_text, "one two");
        assert_eq!(*h.history_changed.lock().unwrap(), 1);
        // A failed re-polish keeps the old text and reports an error.
        *h.polish_fn.lock().unwrap() = Box::new(|_m, raw| PolishOutcome { text: raw.into(), status: PolishStatus::Unpolished { reason: "down".into() }, notice: None });
        h.tx.send(Cmd::Repolish { id, mode: Mode::Email }).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let row = h.store.lock().unwrap().get(id).unwrap().unwrap();
        assert_eq!(row.polished_text.as_deref(), Some("- one\n- two (bullets)"));
        assert_eq!(row.mode, "bullets");
        let states = h.states.lock().unwrap().clone();
        assert!(states.iter().any(|s| matches!(s, PillState::Error { message } if message.starts_with("Re-polish failed"))), "{:?}", kinds(&states));
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
    }

    #[test]
    fn translate_mode_sets_whisper_translate_flag_and_skips_polish() {
        let flags: Arc<Mutex<Vec<bool>>> = Default::default();
        let f = flags.clone();
        let h = harness(engine_saying("hola"), capture_ok());
        h.settings.lock().unwrap().mode = Mode::Translate;
        // The fake engine does not record options; assert through the row and the absence of polish.
        h.tx.send(Cmd::Start).unwrap();
        h.tx.send(Cmd::Stop).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        drop(f);
        assert!(flags.lock().unwrap().is_empty());
        assert!(h.polish_calls.lock().unwrap().is_empty());
        let row = last_row(&h);
        assert_eq!(row.mode, "translate");
        assert_eq!(row.polish_status, "none");
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
    }

    #[test]
    fn fillers_are_removed_before_polish_and_history() {
        let h = harness(engine_saying("Um, so uh hello there"), capture_ok());
        h.tx.send(Cmd::Start).unwrap();
        h.tx.send(Cmd::Stop).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(h.polish_calls.lock().unwrap()[0].1, "So hello there");
        assert_eq!(last_row(&h).raw_text, "So hello there");
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
    }

    #[test]
    fn listening_state_carries_the_mode_label() {
        let h = harness(engine_saying("x"), capture_ok());
        h.settings.lock().unwrap().mode = Mode::Email;
        h.tx.send(Cmd::Start).unwrap();
        std::thread::sleep(Duration::from_millis(150));
        let states = h.states.lock().unwrap().clone();
        assert!(states.iter().any(|s| matches!(s, PillState::Listening { mode, .. } if mode == "Email")), "{:?}", kinds(&states));
        h.tx.send(Cmd::Cancel).unwrap();
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
    }

    #[test]
    fn repolish_warms_the_llm_first() {
        let h = harness(engine_saying("one two"), capture_ok());
        h.settings.lock().unwrap().mode = Mode::Raw;
        h.tx.send(Cmd::Start).unwrap();
        h.tx.send(Cmd::Stop).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(*h.warms.lock().unwrap(), 0, "raw mode never warms");
        let id = last_row(&h).id;
        h.tx.send(Cmd::Repolish { id, mode: Mode::Clean }).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(*h.warms.lock().unwrap(), 1, "a re-polish loads the model before asking it");
        assert_eq!(h.store.lock().unwrap().get(id).unwrap().unwrap().mode, "clean");
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
    }

    #[test]
    fn repolish_with_polish_off_explains_itself() {
        let h = harness(engine_saying("one two"), capture_ok());
        h.settings.lock().unwrap().polish_enabled = false;
        h.tx.send(Cmd::Start).unwrap();
        h.tx.send(Cmd::Stop).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let id = last_row(&h).id;
        h.tx.send(Cmd::Repolish { id, mode: Mode::Email }).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        assert!(h.polish_calls.lock().unwrap().is_empty());
        let states = h.states.lock().unwrap().clone();
        assert!(states.iter().any(|s| matches!(s, PillState::Error { message } if message.contains("Polish is off"))), "{:?}", kinds(&states));
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
    }

    #[test]
    fn hotkey_hint_reflects_the_activation_mode() {
        use privatevox_core::settings::{Activation, HotkeyBinding, ModifierKey};
        let mut s = Settings::default();
        assert_eq!(hotkey_hint(&s), "Hold right ⌥ to talk");
        s.activation = Activation::Toggle;
        assert_eq!(hotkey_hint(&s), "Tap right ⌥ to talk");
        s.activation = Activation::HoldOrToggle;
        assert_eq!(hotkey_hint(&s), "Hold or tap right ⌥ to talk");
        s.ptt = HotkeyBinding { combo: Some("Ctrl+Alt+Space".into()), modifier: None };
        s.activation = Activation::PushToTalk;
        assert_eq!(hotkey_hint(&s), "Hold Ctrl+Alt+Space to talk");
        s.ptt = HotkeyBinding { combo: None, modifier: None };
        assert_eq!(hotkey_hint(&s), "Tap Ctrl+Shift+Space to talk");
        s.toggle = HotkeyBinding { combo: None, modifier: None };
        assert_eq!(hotkey_hint(&s), "PrivateVox");
        s.ptt = HotkeyBinding { combo: None, modifier: Some(ModifierKey::RightCommand) };
        s.activation = Activation::Toggle;
        assert_eq!(hotkey_hint(&s), "Tap right ⌘ to talk");
    }

    #[test]
    fn listening_carries_a_stop_hint_only_while_latched() {
        use privatevox_core::hotkeys::Binding;
        use privatevox_core::settings::Activation;
        // Held key: no stop hint.
        let h = harness(engine_saying("x"), capture_ok());
        h.tx.send(Cmd::Key(Input::Down(Binding::Ptt))).unwrap();
        std::thread::sleep(Duration::from_millis(250));
        let states = h.states.lock().unwrap().clone();
        assert!(states.iter().any(|s| matches!(s, PillState::Listening { stop_hint: None, .. })), "{:?}", kinds(&states));
        assert!(!states.iter().any(|s| matches!(s, PillState::Listening { stop_hint: Some(_), .. })));
        h.tx.send(Cmd::Cancel).unwrap();
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
        // Toggle mode: the recording is latched from the first tap, so the pill says how to stop.
        let h = harness(engine_saying("x"), capture_ok());
        h.settings.lock().unwrap().activation = Activation::Toggle;
        h.tx.send(Cmd::SettingsChanged).unwrap();
        h.tx.send(Cmd::Key(Input::Down(Binding::Ptt))).unwrap();
        h.tx.send(Cmd::Key(Input::Up(Binding::Ptt))).unwrap();
        std::thread::sleep(Duration::from_millis(250));
        let states = h.states.lock().unwrap().clone();
        assert!(states.iter().any(|s| matches!(s, PillState::Listening { stop_hint: Some(t), .. } if t == "tap right ⌥ to stop")), "{:?}", states.iter().filter(|s| matches!(s, PillState::Listening { .. })).last());
        h.tx.send(Cmd::Cancel).unwrap();
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
    }

    fn in_use(h: &FakeHost) -> Arc<RemoteState> {
        let st = RemoteState::new();
        st.configure("PC", &h.base_url, None, "Mac");
        st.enabled.store(true, SeqCst);
        st.healthy.store(true, SeqCst);
        st
    }

    #[test]
    fn final_chunk_flag_set_on_last_chunk() {
        let e = engine_saying("x");
        let finals = e.finals.clone();
        let h = harness(e, capture_ok());
        h.tx.send(Cmd::Start).unwrap();
        h.tx.send(Cmd::Stop).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(finals.lock().unwrap().clone(), vec![true], "a short clip is its own last chunk");
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
    }

    #[test]
    fn remote_dictation_marks_row_skips_local_load_and_polishes_on_the_host() {
        let host = FakeHost::start(HostBehaviour::Ok { text: "hello from pc".into(), polished: "Hello from PC.".into(), token: None });
        let remote = in_use(&host);
        let e = FakeEngine { load_delay: Duration::ZERO, text: "local".into(), loaded: false, load_calls: Default::default(), loads: Default::default(), unloads: Default::default(), decodes: Default::default(), finals: Default::default(), prompts: Default::default(), languages: Default::default() };
        let loads = e.load_calls.clone();
        let h = harness_remote(e, capture_ok(), remote.clone());
        h.tx.send(Cmd::Start).unwrap();
        h.tx.send(Cmd::Stop).unwrap();
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(h.pasted.lock().unwrap().clone(), vec!["Hello from PC.".to_string()]);
        assert_eq!(*loads.lock().unwrap(), 0, "the local model is never loaded while the host is in use");
        assert!(h.polish_calls.lock().unwrap().is_empty(), "local polish is not used");
        let req = host.requests.lock().unwrap()[0].clone();
        assert!(req.final_);
        assert_eq!(req.client, "Mac");
        let pol = host.polishes.lock().unwrap()[0].clone();
        assert_eq!(pol.session, req.session);
        assert_eq!(pol.mode, "clean");
        let row = last_row(&h);
        assert_eq!(row.engine_kind, "remote:PC");
        assert_eq!(row.raw_text, "hello from pc");
        assert_eq!(row.polished_text.as_deref(), Some("Hello from PC."));
        assert_eq!(row.polish_status, "ok");
        let states = h.states.lock().unwrap().clone();
        assert!(states.iter().any(|s| matches!(s, PillState::Polishing { on: Some(l) } if l == "PC")), "{:?}", kinds(&states));
        let preview = states.iter().rev().find_map(|s| if let PillState::Pasted { preview } = s { Some(preview.clone()) } else { None }).unwrap();
        assert!(!preview.contains("unreachable"), "{preview}");
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
    }

    #[test]
    fn fallback_notice_prefixes_preview_and_row_is_local() {
        let host = FakeHost::start(HostBehaviour::Ok { text: "pc".into(), polished: "PC.".into(), token: None });
        let remote = in_use(&host);
        let h = harness_remote(engine_saying("local words"), capture_ok(), remote.clone());
        h.tx.send(Cmd::Start).unwrap();
        std::thread::sleep(Duration::from_millis(150));
        drop(host);
        h.tx.send(Cmd::Stop).unwrap();
        std::thread::sleep(Duration::from_millis(800));
        assert_eq!(h.pasted.lock().unwrap().clone(), vec!["local words".to_string()]);
        let row = last_row(&h);
        assert_eq!(row.engine_kind, "local");
        let states = h.states.lock().unwrap().clone();
        let preview = states.iter().rev().find_map(|s| if let PillState::Pasted { preview } = s { Some(preview.clone()) } else { None }).unwrap();
        assert!(preview.starts_with("PC unreachable, transcribed locally"), "{preview}");
        assert!(!remote.in_use());
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
    }

    #[test]
    fn each_dictation_starts_a_new_host_session() {
        let host = FakeHost::start(HostBehaviour::Ok { text: "a".into(), polished: "A.".into(), token: None });
        let remote = in_use(&host);
        let h = harness_remote(engine_saying("x"), capture_ok(), remote.clone());
        for _ in 0..2 {
            h.tx.send(Cmd::Start).unwrap();
            h.tx.send(Cmd::Stop).unwrap();
            std::thread::sleep(Duration::from_millis(400));
        }
        let reqs = host.requests.lock().unwrap().clone();
        assert_eq!(reqs.len(), 2);
        assert_ne!(reqs[0].session, reqs[1].session);
        assert_eq!((reqs[0].index, reqs[1].index), (0, 0));
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
    }

    /// A real host server with its own engine, store and polisher, for end-to-end tests.
    struct RealHost {
        server: crate::host::HostServer,
        store: Arc<Mutex<Store>>,
        _dir: tempfile::TempDir,
        _llm: privatevox_core::polish::fake_server::FakeLlm,
    }

    fn real_host(text: &str, polished: &str) -> RealHost {
        use privatevox_core::polish::fake_server::{Behaviour, FakeLlm};
        use privatevox_core::polish::{Flavor, HttpLlmClient, Polisher};
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(Store::open(&dir.path().join("host.sqlite")).unwrap()));
        let llm = FakeLlm::start(Behaviour::OpenAi { reply: polished.into() });
        let polisher = Arc::new(Polisher::new(Arc::new(HttpLlmClient::new()), vec![llm.endpoint(Flavor::OpenAi)], Duration::from_secs(2)));
        polisher.check_due(Instant::now());
        let engine: Arc<Mutex<Box<dyn Engine>>> = Arc::new(Mutex::new(Box::new(engine_saying(text))));
        let vad = Arc::new(Mutex::new(Vad::new(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../resources/models/ggml-silero-v5.1.2.bin")).unwrap()));
        let deps = crate::host::HostDeps {
            engine,
            store: store.clone(),
            polisher,
            vad,
            settings: Arc::new(Mutex::new(Settings::default())),
            model_path: Arc::new(Mutex::new(Some(PathBuf::from("/m.bin")))),
            status: Arc::new(|| privatevox_core::net::StatusReply { name: "Host".into(), version: "t".into(), platform: "macos".into(), gpu: privatevox_core::net::GpuInfo { name: "M1".into(), total_bytes: 8_000_000_000, used_bytes: 4_000_000_000 }, whisper: privatevox_core::net::WhisperInfo { model: "large-v3-turbo".into(), loaded: true, resident_bytes: 1 }, polish: privatevox_core::net::PolishInfo { enabled: true, endpoint_label: Some("fake".into()), model: Some("m".into()), resident_bytes: 0 }, clients: 0 }),
            models: Arc::new(|| privatevox_core::net::ModelsReply { models: vec![] }),
            select_model: Arc::new(|_| Ok(())),
        };
        let server = crate::host::HostServer::start("127.0.0.1:0", None, deps).unwrap();
        RealHost { server, store, _dir: dir, _llm: llm }
    }

    #[test]
    fn dictation_through_a_real_host_stores_one_row_on_each_side() {
        let host = real_host("host words", "Host words.");
        let st = RemoteState::new();
        st.configure("PC", &format!("http://127.0.0.1:{}", host.server.port()), None, "Mac");
        st.enabled.store(true, SeqCst);
        st.healthy.store(true, SeqCst);
        let h = harness_remote(engine_saying("unused local"), capture_ok(), st.clone());
        h.tx.send(Cmd::Start).unwrap();
        h.tx.send(Cmd::Stop).unwrap();
        std::thread::sleep(Duration::from_millis(900));
        assert_eq!(h.pasted.lock().unwrap().clone(), vec!["Host words.".to_string()]);
        let client_row = last_row(&h);
        assert_eq!(client_row.engine_kind, "remote:PC");
        assert_eq!(client_row.polished_text.as_deref(), Some("Host words."));
        let host_rows = host.store.lock().unwrap().list(10, 0).unwrap();
        assert_eq!(host_rows.len(), 1, "exactly one host row: {:?}", host_rows.iter().map(|r| (&r.mode, &r.polish_status)).collect::<Vec<_>>());
        assert_eq!(host_rows[0].mode, "clean");
        assert_eq!(host_rows[0].polish_status, "ok");
        assert_eq!(host_rows[0].polished_text.as_deref(), Some("Host words."));
        assert_eq!(host_rows[0].engine_kind, "host");
        assert_eq!(host_rows[0].app_name.as_deref(), Some("Mac"));
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
    }

    #[test]
    fn fallback_is_sticky_within_a_dictation() {
        let host = FakeHost::start(HostBehaviour::Ok { text: "pc".into(), polished: "".into(), token: None });
        let port = host.port();
        let st = in_use(&host);
        let engine: Arc<Mutex<Box<dyn Engine>>> = Arc::new(Mutex::new(Box::new(engine_saying("local"))));
        let fell_back = Arc::new(AtomicBool::new(false));
        drop(host);
        let t = transcribe_routed(&st, &engine, &[0.1; 1600], &DecodeOptions::default(), true, &fell_back, Some(&PathBuf::from("/m.bin")), true).unwrap();
        assert_eq!(t.text, "local");
        assert!(fell_back.load(SeqCst));
        // The host comes back and the loop marks it healthy; this dictation must still stay local.
        let host2 = FakeHost::start_on(HostBehaviour::Ok { text: "pc".into(), polished: "".into(), token: None }, port);
        st.healthy.store(true, SeqCst);
        let t = transcribe_routed(&st, &engine, &[0.1; 1600], &DecodeOptions::default(), true, &fell_back, Some(&PathBuf::from("/m.bin")), true).unwrap();
        assert_eq!(t.text, "local");
        assert!(host2.requests.lock().unwrap().is_empty());
        // A dictation that started local never switches to the host midway either.
        let fresh = Arc::new(AtomicBool::new(false));
        let t = transcribe_routed(&st, &engine, &[0.1; 1600], &DecodeOptions::default(), false, &fresh, Some(&PathBuf::from("/m.bin")), true).unwrap();
        assert_eq!(t.text, "local");
        assert!(host2.requests.lock().unwrap().is_empty());
    }

    #[test]
    fn benchmark_runs_locally_even_when_the_host_is_in_use() {
        let host = FakeHost::start(HostBehaviour::Ok { text: "pc".into(), polished: "".into(), token: None });
        let st = in_use(&host);
        let h = harness_remote(engine_saying("x"), capture_ok(), st.clone());
        h.tx.send(Cmd::Benchmark("large-v3-turbo".into())).unwrap();
        std::thread::sleep(Duration::from_millis(800));
        assert_eq!(h.benchmarks.lock().unwrap().len(), 1, "benchmark completed");
        assert!(host.requests.lock().unwrap().is_empty(), "a benchmark measures this machine, never the host");
        assert!(st.in_use(), "the host was not marked unhealthy by the benchmark");
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
    }

    #[test]
    fn host_with_polish_off_marks_the_row_unpolished() {
        let host = FakeHost::start(HostBehaviour::OkPolishOff { text: "pc text".into() });
        let st = in_use(&host);
        let h = harness_remote(engine_saying("x"), capture_ok(), st.clone());
        h.tx.send(Cmd::Start).unwrap();
        h.tx.send(Cmd::Stop).unwrap();
        std::thread::sleep(Duration::from_millis(500));
        let row = last_row(&h);
        assert_eq!(row.polish_status, "unpolished");
        assert!(row.polished_text.is_none());
        let states = h.states.lock().unwrap().clone();
        let preview = states.iter().rev().find_map(|s| if let PillState::Pasted { preview } = s { Some(preview.clone()) } else { None }).unwrap();
        assert!(preview.starts_with("Unpolished"), "{preview}");
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
    }

    #[test]
    fn cancel_clears_a_pending_fallback_notice() {
        let host = FakeHost::start(HostBehaviour::Ok { text: "pc".into(), polished: "".into(), token: None });
        let st = in_use(&host);
        let h = harness_remote(engine_saying("plain"), capture_ok(), st.clone());
        st.set_notice("PC unreachable, transcribed locally");
        h.tx.send(Cmd::Start).unwrap();
        h.tx.send(Cmd::Cancel).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        assert!(st.take_notice().is_none(), "cancel consumed the notice");
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
    }

    #[test]
    fn dictionary_corrects_transcript_and_prompts_whisper() {
        let e = engine_saying("charge bee works fine");
        let prompts = e.prompts.clone();
        let h = harness(e, capture_ok());
        {
            let mut s = h.settings.lock().unwrap();
            s.mode = Mode::Raw;
            s.dictionary = vec!["ChargeBee".into()];
        }
        h.tx.send(Cmd::Start).unwrap();
        h.tx.send(Cmd::Stop).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(h.pasted.lock().unwrap().clone(), vec!["ChargeBee works fine".to_string()]);
        assert_eq!(prompts.lock().unwrap().clone(), vec![Some("ChargeBee".to_string())]);
        assert_eq!(last_row(&h).raw_text, "ChargeBee works fine");
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
    }

    #[test]
    fn multi_language_rerun_when_detected_is_off_the_list() {
        let e = engine_saying("bonjour");
        let langs = e.languages.clone();
        let h = harness(e, capture_ok());
        {
            let mut s = h.settings.lock().unwrap();
            s.mode = Mode::Raw;
            s.spoken_languages = vec!["en".into(), "de".into()];
        }
        h.tx.send(Cmd::Start).unwrap();
        h.tx.send(Cmd::Stop).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(langs.lock().unwrap().clone(), vec![None, Some("en".to_string())], "auto-detect first, then the first listed language");
        let row = last_row(&h);
        assert_eq!(row.language.as_deref(), Some("en"));
        assert!(row.timing_json.as_deref().unwrap_or("").contains("\"language_source\":\"fallback\""), "{:?}", row.timing_json);
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
    }

    #[test]
    fn single_language_decodes_once_fixed() {
        let e = engine_saying("hello");
        let langs = e.languages.clone();
        let h = harness(e, capture_ok());
        h.settings.lock().unwrap().mode = Mode::Raw;
        h.tx.send(Cmd::Start).unwrap();
        h.tx.send(Cmd::Stop).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(langs.lock().unwrap().clone(), vec![Some("en".to_string())]);
        assert!(last_row(&h).timing_json.as_deref().unwrap_or("").contains("\"language_source\":\"fixed\""));
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
    }

    fn wait_for_clip(h: &Harness, id: i64) -> Option<String> {
        for _ in 0..40 {
            if let Some(p) = h.store.lock().unwrap().get(id).unwrap().unwrap().audio_path {
                return Some(p);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        None
    }

    #[test]
    fn keep_audio_saves_a_clip_next_to_the_row() {
        let h = harness(engine_saying("hello"), capture_ok());
        {
            let mut s = h.settings.lock().unwrap();
            s.mode = Mode::Raw;
            s.keep_audio = true;
        }
        h.tx.send(Cmd::Start).unwrap();
        h.tx.send(Cmd::Stop).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let id = last_row(&h).id;
        let path = wait_for_clip(&h, id).expect("audio_path set");
        assert!(path.ends_with(&format!("{id}.wav")), "{path}");
        let pcm = privatevox_core::audio::load_wav_16k(std::path::Path::new(&path)).unwrap();
        assert!(pcm.len() > 16_000, "the trimmed clip is longer than a second: {}", pcm.len());
        // Off by default: no clip.
        h.settings.lock().unwrap().keep_audio = false;
        h.tx.send(Cmd::Start).unwrap();
        h.tx.send(Cmd::Stop).unwrap();
        std::thread::sleep(Duration::from_millis(400));
        assert!(last_row(&h).audio_path.is_none());
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
    }

    #[test]
    fn rerun_updates_raw_text_from_the_clip() {
        let h = harness(engine_saying("{n}"), capture_ok());
        {
            let mut s = h.settings.lock().unwrap();
            s.mode = Mode::Raw;
            s.keep_audio = true;
        }
        h.tx.send(Cmd::Start).unwrap();
        h.tx.send(Cmd::Stop).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let id = last_row(&h).id;
        assert_eq!(last_row(&h).raw_text, "1");
        wait_for_clip(&h, id).expect("clip saved");
        h.tx.send(Cmd::Rerun { id, model: "large-v3-turbo".into() }).unwrap();
        std::thread::sleep(Duration::from_millis(500));
        let row = h.store.lock().unwrap().get(id).unwrap().unwrap();
        assert_eq!(row.raw_text, "2", "decoded again from the kept clip");
        assert_eq!(*h.history_changed.lock().unwrap(), 1);
        // No clip: a notice, nothing changed.
        h.store.lock().unwrap().set_audio_path(id, None).unwrap();
        h.tx.send(Cmd::Rerun { id, model: "large-v3-turbo".into() }).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(h.store.lock().unwrap().get(id).unwrap().unwrap().raw_text, "2");
        let states = h.states.lock().unwrap().clone();
        assert!(states.iter().any(|s| matches!(s, PillState::Error { message } if message.contains("No audio kept"))), "{:?}", kinds(&states));
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
    }

    #[test]
    fn idle_warning_follows_secure_input_cmd() {
        let h = harness(engine_saying("x"), capture_ok());
        h.tx.send(Cmd::SecureInput(true)).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        let states = h.states.lock().unwrap().clone();
        let last = states.last().unwrap();
        assert!(matches!(last, PillState::Idle { warning: Some(w), .. } if w.contains("Secure input")), "{last:?}");
        h.tx.send(Cmd::SecureInput(false)).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        let states = h.states.lock().unwrap().clone();
        assert!(matches!(states.last().unwrap(), PillState::Idle { warning: None, .. }));
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
    }

    #[test]
    fn no_language_rerun_on_the_host_path() {
        let host = FakeHost::start(HostBehaviour::Ok { text: "host text".into(), polished: "".into(), token: None });
        let st = in_use(&host);
        let h = harness_remote(engine_saying("local"), capture_ok(), st.clone());
        {
            let mut s = h.settings.lock().unwrap();
            s.mode = Mode::Raw;
            s.spoken_languages = vec!["de".into(), "es".into()]; // the host reports "en": off the list
        }
        h.tx.send(Cmd::Start).unwrap();
        h.tx.send(Cmd::Stop).unwrap();
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(host.requests.lock().unwrap().len(), 1, "the host decoded once; no second final chunk");
        let row = last_row(&h);
        assert_eq!(row.raw_text, "host text");
        assert_eq!(row.language.as_deref(), Some("de"));
        assert!(row.timing_json.as_deref().unwrap_or("").contains("\"language_source\":\"fallback\""));
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
    }

    #[test]
    fn rerun_is_refused_while_recording() {
        let h = harness(engine_saying("x"), capture_ok());
        h.tx.send(Cmd::Start).unwrap();
        std::thread::sleep(Duration::from_millis(150));
        h.tx.send(Cmd::Rerun { id: 1, model: "large-v3-turbo".into() }).unwrap();
        std::thread::sleep(Duration::from_millis(150));
        let states = h.states.lock().unwrap().clone();
        assert!(states.iter().any(|s| matches!(s, PillState::Error { message } if message.contains("Finish the dictation"))), "{:?}", kinds(&states));
        assert!(!states.iter().any(|s| matches!(s, PillState::Transcribing)), "the live recording was not interrupted");
        h.tx.send(Cmd::Cancel).unwrap();
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
    }

    #[test]
    fn rerun_records_the_model_used_and_restores_the_previous_model() {
        let e = engine_saying("{n}");
        let loads = e.loads.clone();
        let h = harness(e, capture_ok());
        {
            let mut s = h.settings.lock().unwrap();
            s.mode = Mode::Raw;
            s.keep_audio = true;
        }
        h.tx.send(Cmd::Start).unwrap();
        h.tx.send(Cmd::Stop).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let id = last_row(&h).id;
        wait_for_clip(&h, id).expect("clip saved");
        h.tx.send(Cmd::Rerun { id, model: "large-v3".into() }).unwrap();
        std::thread::sleep(Duration::from_millis(800));
        let row = h.store.lock().unwrap().get(id).unwrap().unwrap();
        assert_eq!(row.model, "large-v3", "the row says which model produced its text");
        assert_eq!(h.settings.lock().unwrap().model_id, "large-v3-turbo", "the active model is restored after the re-run");
        let loaded: Vec<String> = loads.lock().unwrap().iter().map(|p| p.file_name().unwrap().to_string_lossy().to_string()).collect();
        assert!(loaded.iter().any(|f| f.contains("large-v3-q5_0")) && loaded.last().map(|f| f.contains("turbo")).unwrap_or(false), "{loaded:?}");
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
    }

    #[test]
    fn secure_input_warning_names_the_toggle_combo() {
        let h = harness(engine_saying("x"), capture_ok());
        h.settings.lock().unwrap().toggle.combo = Some("Ctrl+Alt+T".into());
        h.tx.send(Cmd::SecureInput(true)).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        let states = h.states.lock().unwrap().clone();
        assert!(matches!(states.last().unwrap(), PillState::Idle { warning: Some(w), .. } if w.contains("Ctrl+Alt+T")), "{:?}", states.last());
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
    }

    fn speech_then_silence(secs: usize) -> Vec<f32> {
        let mut v = fixture();
        v.extend(vec![0.0f32; 16_000 * secs]);
        v
    }

    #[test]
    fn auto_stop_stops_a_latched_recording_after_silence() {
        use privatevox_core::hotkeys::Binding;
        use privatevox_core::settings::Activation;
        let h = harness(engine_saying("hello"), FakeCapture { pcm: speech_then_silence(3), fail: false, die_after_start: false });
        {
            let mut s = h.settings.lock().unwrap();
            s.mode = Mode::Raw;
            s.activation = Activation::Toggle;
        }
        h.tx.send(Cmd::SettingsChanged).unwrap();
        h.tx.send(Cmd::Key(Input::Down(Binding::Ptt))).unwrap();
        h.tx.send(Cmd::Key(Input::Up(Binding::Ptt))).unwrap();
        std::thread::sleep(Duration::from_millis(1800));
        let states = h.states.lock().unwrap().clone();
        assert!(states.iter().any(|s| matches!(s, PillState::Pasted { .. })), "stopped by itself and pasted: {:?}", kinds(&states));
        // (The fake capture hands over all three seconds at once, so the "stopping…" tick is not observable here;
        // the threshold itself is covered by the silence counter's unit tests.)
        assert_eq!(h.store.lock().unwrap().count().unwrap(), 1);
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
    }

    #[test]
    fn auto_stop_ignores_held_and_silent_from_start() {
        use privatevox_core::hotkeys::Binding;
        // Held key: silence after speech must not stop it.
        let h = harness(engine_saying("hello"), FakeCapture { pcm: speech_then_silence(3), fail: false, die_after_start: false });
        h.settings.lock().unwrap().mode = Mode::Raw;
        h.tx.send(Cmd::Key(Input::Down(Binding::Ptt))).unwrap();
        std::thread::sleep(Duration::from_millis(1500));
        let states = h.states.lock().unwrap().clone();
        assert!(!states.iter().any(|s| matches!(s, PillState::Transcribing | PillState::Pasted { .. })), "a held recording is never auto-stopped: {:?}", kinds(&states));
        h.tx.send(Cmd::Cancel).unwrap();
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
        // Latched but nothing said yet: silence from the start must not stop it either.
        use privatevox_core::settings::Activation;
        let h = harness(engine_saying("hello"), FakeCapture { pcm: vec![0.0f32; 16_000 * 4], fail: false, die_after_start: false });
        {
            let mut s = h.settings.lock().unwrap();
            s.mode = Mode::Raw;
            s.activation = Activation::Toggle;
        }
        h.tx.send(Cmd::SettingsChanged).unwrap();
        h.tx.send(Cmd::Key(Input::Down(Binding::Ptt))).unwrap();
        h.tx.send(Cmd::Key(Input::Up(Binding::Ptt))).unwrap();
        std::thread::sleep(Duration::from_millis(1500));
        let states = h.states.lock().unwrap().clone();
        assert!(!states.iter().any(|s| matches!(s, PillState::Transcribing | PillState::Pasted { .. } | PillState::Error { .. })), "{:?}", kinds(&states));
        h.tx.send(Cmd::Cancel).unwrap();
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
    }

    #[test]
    fn auto_stop_never_when_setting_is_none() {
        use privatevox_core::hotkeys::Binding;
        use privatevox_core::settings::Activation;
        let h = harness(engine_saying("hello"), FakeCapture { pcm: speech_then_silence(3), fail: false, die_after_start: false });
        {
            let mut s = h.settings.lock().unwrap();
            s.mode = Mode::Raw;
            s.activation = Activation::Toggle;
            s.auto_stop_silence_secs = None;
        }
        h.tx.send(Cmd::SettingsChanged).unwrap();
        h.tx.send(Cmd::Key(Input::Down(Binding::Ptt))).unwrap();
        h.tx.send(Cmd::Key(Input::Up(Binding::Ptt))).unwrap();
        std::thread::sleep(Duration::from_millis(1500));
        let states = h.states.lock().unwrap().clone();
        assert!(!states.iter().any(|s| matches!(s, PillState::Transcribing | PillState::Pasted { .. })), "{:?}", kinds(&states));
        h.tx.send(Cmd::Cancel).unwrap();
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
    }

    #[test]
    fn mode_resolved_at_start_only() {
        use privatevox_core::appmodes::AppMode;
        let h = harness(engine_saying("dear team hello"), capture_ok());
        {
            let mut s = h.settings.lock().unwrap();
            s.mode = Mode::Clean;
            s.app_modes = vec![AppMode { app: "mail".into(), mode: Mode::Email }];
        }
        *h.frontmost.lock().unwrap() = Some("Mail".into());
        h.tx.send(Cmd::Start).unwrap();
        std::thread::sleep(Duration::from_millis(150));
        *h.frontmost.lock().unwrap() = Some("Safari".into()); // switched apps mid-sentence
        h.tx.send(Cmd::Stop).unwrap();
        std::thread::sleep(Duration::from_millis(400));
        let row = last_row(&h);
        assert_eq!(row.mode, "email", "the mode was decided when the dictation started");
        assert_eq!(h.polish_calls.lock().unwrap()[0].0, Mode::Email);
        let states = h.states.lock().unwrap().clone();
        assert!(states.iter().any(|s| matches!(s, PillState::Listening { mode, .. } if mode == "Email")), "{:?}", kinds(&states));
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
    }

    #[test]
    fn without_a_rule_the_picked_mode_applies() {
        let h = harness(engine_saying("x"), capture_ok());
        h.settings.lock().unwrap().mode = Mode::Bullets;
        *h.frontmost.lock().unwrap() = Some("Mail".into());
        h.tx.send(Cmd::Start).unwrap();
        h.tx.send(Cmd::Stop).unwrap();
        std::thread::sleep(Duration::from_millis(400));
        assert_eq!(last_row(&h).mode, "bullets");
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
    }

    #[test]
    fn cues_fire_on_start_stop_and_nothing_heard() {
        use crate::cues::Cue;
        let h = harness(engine_saying("hello"), capture_ok());
        h.settings.lock().unwrap().mode = Mode::Raw;
        h.tx.send(Cmd::Start).unwrap();
        h.tx.send(Cmd::Stop).unwrap();
        std::thread::sleep(Duration::from_millis(400));
        assert_eq!(h.cues.lock().unwrap().clone(), vec![Cue::Start, Cue::Stop]);
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
        let h = harness(engine_saying("hello"), FakeCapture { pcm: vec![0.0f32; 16_000], fail: false, die_after_start: false });
        h.tx.send(Cmd::Start).unwrap();
        h.tx.send(Cmd::Stop).unwrap();
        std::thread::sleep(Duration::from_millis(400));
        assert_eq!(h.cues.lock().unwrap().clone(), vec![Cue::Start, Cue::Stop, Cue::NothingHeard]);
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
    }

    #[test]
    fn preview_shows_partial_text() {
        use privatevox_core::preview::Preview;
        let e = engine_saying("partial words");
        let decodes = e.decodes.clone();
        let h = harness(e, FakeCapture { pcm: long_fixture(12), fail: false, die_after_start: false });
        {
            let mut s = h.settings.lock().unwrap();
            s.mode = Mode::Raw;
            s.live_preview = Preview::On;
        }
        h.tx.send(Cmd::Start).unwrap();
        std::thread::sleep(Duration::from_millis(3600));
        let states = h.states.lock().unwrap().clone();
        assert!(states.iter().any(|s| matches!(s, PillState::Listening { partial: Some(p), .. } if p.contains("partial words"))), "{:?}", states.iter().filter_map(|s| if let PillState::Listening { partial, .. } = s { partial.clone() } else { None }).collect::<Vec<_>>());
        assert!(decodes.lock().unwrap().len() >= 1, "a preview decode ran");
        h.tx.send(Cmd::Cancel).unwrap();
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
    }

    #[test]
    fn preview_skips_when_engine_busy() {
        use privatevox_core::preview::Preview;
        // The model is not loaded: the preload holds the engine lock for 5 s, so every preview tick must skip.
        let e = FakeEngine { load_delay: Duration::from_secs(5), text: "x".into(), loaded: false, load_calls: Default::default(), loads: Default::default(), unloads: Default::default(), decodes: Default::default(), finals: Default::default(), prompts: Default::default(), languages: Default::default() };
        let decodes = e.decodes.clone();
        let h = harness(e, FakeCapture { pcm: long_fixture(12), fail: false, die_after_start: false });
        {
            let mut s = h.settings.lock().unwrap();
            s.mode = Mode::Raw;
            s.live_preview = Preview::On;
        }
        h.tx.send(Cmd::Start).unwrap();
        std::thread::sleep(Duration::from_millis(3600));
        assert_eq!(decodes.lock().unwrap().len(), 0, "no preview decode while the engine is busy");
        let states = h.states.lock().unwrap().clone();
        assert!(!states.iter().any(|s| matches!(s, PillState::Listening { partial: Some(_), .. })));
        h.tx.send(Cmd::Cancel).unwrap();
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
    }

    #[test]
    fn preview_off_never_decodes_early() {
        use privatevox_core::preview::Preview;
        let e = engine_saying("x");
        let decodes = e.decodes.clone();
        let h = harness(e, FakeCapture { pcm: long_fixture(12), fail: false, die_after_start: false });
        {
            let mut s = h.settings.lock().unwrap();
            s.mode = Mode::Raw;
            s.live_preview = Preview::Off;
        }
        h.tx.send(Cmd::Start).unwrap();
        std::thread::sleep(Duration::from_millis(3600));
        assert_eq!(decodes.lock().unwrap().len(), 0);
        h.tx.send(Cmd::Cancel).unwrap();
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
    }

    #[test]
    fn cancel_alone_pastes_and_stores_nothing() {
        let h = harness(engine_saying("cancel"), capture_ok());
        h.settings.lock().unwrap().mode = Mode::Raw;
        h.tx.send(Cmd::Start).unwrap();
        h.tx.send(Cmd::Stop).unwrap();
        std::thread::sleep(Duration::from_millis(400));
        assert!(h.pasted.lock().unwrap().is_empty());
        assert_eq!(h.store.lock().unwrap().count().unwrap(), 0);
        let states = h.states.lock().unwrap().clone();
        assert!(states.iter().any(|s| matches!(s, PillState::Pasted { preview } if preview.contains("Cancelled"))), "{:?}", kinds(&states));
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
    }

    #[test]
    fn new_line_appends_a_break_and_commands_can_be_switched_off() {
        let h = harness(engine_saying("first line new line"), capture_ok());
        h.settings.lock().unwrap().mode = Mode::Raw;
        h.tx.send(Cmd::Start).unwrap();
        h.tx.send(Cmd::Stop).unwrap();
        std::thread::sleep(Duration::from_millis(400));
        assert_eq!(h.pasted.lock().unwrap().clone(), vec!["first line\n".to_string()]);
        assert_eq!(last_row(&h).raw_text, "first line\n");
        h.settings.lock().unwrap().spoken_commands = false;
        h.tx.send(Cmd::Start).unwrap();
        h.tx.send(Cmd::Stop).unwrap();
        std::thread::sleep(Duration::from_millis(400));
        assert_eq!(h.pasted.lock().unwrap()[1], "first line new line");
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
    }

    #[test]
    fn delete_that_undoes_the_last_paste() {
        let h = harness(engine_saying("delete that"), capture_ok());
        h.settings.lock().unwrap().mode = Mode::Raw;
        h.tx.send(Cmd::Start).unwrap();
        h.tx.send(Cmd::Stop).unwrap();
        std::thread::sleep(Duration::from_millis(400));
        assert!(h.pasted.lock().unwrap().is_empty(), "the command itself is never pasted");
        assert_eq!(h.store.lock().unwrap().count().unwrap(), 0, "and never stored");
        let states = h.states.lock().unwrap().clone();
        assert!(states.iter().any(|s| matches!(s, PillState::Pasted { preview } if preview.contains("Undoing"))), "{:?}", kinds(&states));
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
    }

    #[test]
    fn chunked_rows_record_language_source() {
        let h = harness(engine_saying("{n}"), FakeCapture { pcm: long_fixture(45), fail: false, die_after_start: false });
        h.settings.lock().unwrap().mode = Mode::Raw;
        h.tx.send(Cmd::Start).unwrap();
        std::thread::sleep(Duration::from_millis(1200));
        h.tx.send(Cmd::Stop).unwrap();
        std::thread::sleep(Duration::from_millis(1500));
        let row = last_row(&h);
        assert!(row.timing_json.as_deref().unwrap_or("").contains("\"language_source\":\"fixed\""), "{:?}", row.timing_json);
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
    }

    #[test]
    fn rerun_uses_the_host_when_in_use() {
        let host = FakeHost::start(HostBehaviour::Ok { text: "from host".into(), polished: "".into(), token: None });
        let st = in_use(&host);
        let h = harness_remote(engine_saying("local"), capture_ok(), st.clone());
        {
            let mut s = h.settings.lock().unwrap();
            s.mode = Mode::Raw;
            s.keep_audio = true;
        }
        h.tx.send(Cmd::Start).unwrap();
        h.tx.send(Cmd::Stop).unwrap();
        std::thread::sleep(Duration::from_millis(500));
        let id = last_row(&h).id;
        wait_for_clip(&h, id).expect("clip saved");
        h.tx.send(Cmd::Rerun { id, model: "large-v3-turbo".into() }).unwrap();
        std::thread::sleep(Duration::from_millis(600));
        assert_eq!(host.requests.lock().unwrap().len(), 2, "the re-run went to the host too");
        assert_eq!(h.store.lock().unwrap().get(id).unwrap().unwrap().raw_text, "from host");
        h.tx.send(Cmd::Quit).unwrap();
        h.join.join().unwrap();
    }

    #[test]
    fn clip_finalize_drops_the_file_when_the_row_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(&dir.path().join("privatevox.sqlite")).unwrap();
        let id = store.insert(&NewDictation { app_name: None, mode: "raw".into(), model: "m".into(), language: None, duration_ms: 1, raw_text: "x".into(), polished_text: None, polished_by: None, polish_status: "none".into(), status: "ok".into(), timing_json: None, engine_kind: "local".into() }).unwrap();
        let clip = dir.path().join("audio").join(format!("{id}.wav"));
        privatevox_core::audio::write_wav_16k(&clip, &[0.0; 160]).unwrap();
        store.delete(id).unwrap();
        finalize_clip(&mut store, id, &clip);
        assert!(!clip.exists(), "a clip whose row was deleted meanwhile is removed, not orphaned");
        let id2 = store.insert(&NewDictation { app_name: None, mode: "raw".into(), model: "m".into(), language: None, duration_ms: 1, raw_text: "y".into(), polished_text: None, polished_by: None, polish_status: "none".into(), status: "ok".into(), timing_json: None, engine_kind: "local".into() }).unwrap();
        let clip2 = dir.path().join("audio").join(format!("{id2}.wav"));
        privatevox_core::audio::write_wav_16k(&clip2, &[0.0; 160]).unwrap();
        finalize_clip(&mut store, id2, &clip2);
        assert!(clip2.exists());
        assert_eq!(store.get(id2).unwrap().unwrap().audio_path.as_deref(), Some(clip2.to_string_lossy().as_ref()));
    }
}
