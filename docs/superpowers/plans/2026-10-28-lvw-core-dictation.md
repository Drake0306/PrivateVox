# LVW Core Dictation (macOS) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A working macOS build of LVW where holding a key records speech, Whisper (large-v3-turbo on Metal, in a child worker process) transcribes it, the text is pasted into the frontmost app, and every dictation is saved to a local history visible in a panel.

**Architecture:** A Cargo workspace with three members. `crates/lvw-core` is pure logic with no Tauri dependency (settings, audio conversion, VAD trimming, hotkey state machine, text clean-up, SQLite store, lock file, idle timer). `crates/lvw-engine` holds the `Engine` trait, the whisper-rs backend, the length-prefixed pipe protocol, the worker loop and the `WorkerEngine` supervisor. `src-tauri` is the Tauri 2 app: pill and panel windows (plain HTML in `ui/`), tray, hotkey wiring, paste, the pipeline coordinator and lifecycle. The app binary doubles as the worker when started with `--engine-worker`.

**Tech Stack:** Rust 1.93, Tauri 2.12.4 (plugins: single-instance 2.5.4, log 2.10.1, global-shortcut 2.4.1), tauri-nspanel 2.1.1, whisper-rs 0.16.0 (`metal`), cpal 0.18.2, rubato 5.0.1, arboard 3.6.1, enigo 0.6.1, objc2-app-kit 0.3.2, rusqlite 0.40.2 (`bundled`), postcard 1.1.3, serde/serde_json, rdev (rustdesk-org fork), reqwest 0.13.5, env_logger 0.11, log 0.4. Plain HTML/CSS/JS UI, no bundler (`withGlobalTauri`).

**Spec:** `docs/superpowers/specs/2026-10-28-whisper-harness-design.md` (§4.1–4.5, §4.6 minimal, §5, §6, §10, §18.1, §18.3–18.5, §18.9–18.11 fixed-delay variant, §18.13) and `docs/superpowers/spikes/2026-10-28-engine-spike-results.md` (decode defaults, audio-context policy). API facts: `docs/superpowers/reference/2026-10-28-crate-api-reference.md`.

**Out of scope for this plan (later plans):** LLM polish and modes, LAN endpoints, GPU memory strip, model switching UI beyond turbo, per-model benchmark, chunked decode while speaking, dictionary, undo last paste, receipt-sequenced clipboard restore, secure-input detection, keep-audio, export, Windows build, packaging/notarisation.

## Global Constraints

- Product name `LVW`, bundle identifier `com.drake0306.lvw`, licence MIT (spec header).
- Audio inside the app is always 16 kHz mono f32 in [-1, 1] (§4.3).
- One engine owner: only `WorkerEngine` loads, swaps or frees a model; quit, idle timer and Pause all call `WorkerEngine::unload` (§5, §18.1).
- Native Whisper code runs only in the worker process; the worker exits when its stdin reaches EOF (§18.1).
- Decode defaults (spike decision): greedy `best_of 1`, flash attention on, language fixed, `no_context`, `suppress_blank`, `suppress_nst`, no timestamps, 4 threads, `audio_ctx = clamp(round_up_64(1500 × (seconds + 2) / 30), 768, 1500)`, repetition guard re-runs once at 1500.
- Clips under 300 ms are discarded; clips under 1.25 s are zero-padded to 1.25 s; maximum clip 5 minutes with auto-stop (§4.3, §18.5).
- VAD smoothing constants: pre-roll 450 ms, onset 60 ms, hangover 450 ms; Silero thresholds 0.5, min speech 250 ms, min silence 100 ms, speech pad 30 ms (§18.4, §18.6).
- Hotkey defaults: push-to-talk Right Option (single modifier, via rdev `Key::AltGr`), toggle `Ctrl+Shift+Space` (plugin); hold-or-toggle threshold 300 ms (§4.2, §18.10).
- Paste: save clipboard to disk, set text, send Cmd+V as `Key::Meta` press, `Key::Other(9)` click, `Key::Meta` release, restore after 150 ms (§4.3 step 6; receipt-sequenced restore comes in a later plan).
- SQLite in WAL mode, one transaction per dictation (§6, §10).
- Idle unload: 1–240 minutes or Never, default 10, takes effect without restart (§4.5).
- Data directory: Tauri `app_data_dir()` (`~/Library/Application Support/com.drake0306.lvw/`): `settings.json`, `lvw.sqlite`, `lvw.lock`, `clipboard.bak`, `models/`.
- Crate versions pinned as listed in the Tech Stack; no `*` or caret-only latest.
- No commit attribution trailers.

## Review Focus

- A dictation while the model is still loading (first launch, or after idle unload): recording must start at once and the decode must wait for the load, never drop the clip. Pinned in Task 10 by `pipeline_queues_clip_until_engine_loaded`.
- The microphone permission denied or the default input device vanishing mid-session: the pill shows the error and the machine returns to idle instead of staying stuck in Listening. Pinned in Task 6 by `optimistic_start_rolls_back_when_capture_fails` and Task 10 by `pipeline_reports_capture_error`.
- The worker process crashing during a decode (simulated by killing it): the supervisor returns an error for that clip, the next clip works after an automatic respawn, and no second worker is ever alive. Pinned in Task 5 by `supervisor_respawns_after_worker_killed`.
- A paste into an app that does not accept Cmd+V, or when Accessibility is not granted: the text is still in history and on the clipboard, and the pill says so. Pinned in Task 7 by `paste_without_permission_keeps_text_on_clipboard` (unit level with an injected key sender).
- Quit while a decode is in flight: the app exits within 2 s, the worker is gone, the lock file is removed. Pinned in Task 11 by `exit_path_completes_with_worker_busy` (uses a fake slow engine).

---

### Task 1: Workspace scaffold and a visible pill

**Files:**
- Create: `Cargo.toml` (workspace), `crates/lvw-core/Cargo.toml`, `crates/lvw-core/src/lib.rs`, `crates/lvw-engine/Cargo.toml`, `crates/lvw-engine/src/lib.rs`
- Create: `src-tauri/Cargo.toml`, `src-tauri/build.rs`, `src-tauri/tauri.conf.json`, `src-tauri/capabilities/default.json`, `src-tauri/src/main.rs`, `src-tauri/src/lib.rs`, `src-tauri/icons/` (generated by `cargo tauri icon` from a placeholder PNG), `src-tauri/Info.plist`
- Create: `ui/pill.html`, `ui/panel.html`, `ui/lvw.css`, `ui/lvw.js`
- Modify: `.gitignore` (add `target/`, `src-tauri/gen/`)

**Interfaces:**
- Produces: workspace members `lvw-core`, `lvw-engine`, `lvw` (the app, package name `lvw`). `tauri.conf.json` windows: `pill` (label `pill`, 280×48, `decorations false`, `transparent true`, `alwaysOnTop true`, `visibleOnAllWorkspaces true`, `skipTaskbar true`, `resizable false`, `focus false`, `shadow false`, `url pill.html`) and `panel` (label `panel`, 560×640, `visible false`, `url panel.html`). `app.withGlobalTauri true`, `build.frontendDist "../ui"`, no `devUrl`. Identifier `com.drake0306.lvw`, product name `LVW`. `Info.plist` with `NSMicrophoneUsageDescription`.
- Produces: `ui/lvw.js` exposing `window.lvw = { invoke, listen }` thin wrappers over `window.__TAURI__.core.invoke` and `window.__TAURI__.event.listen`.

- [ ] **Step 1: Write a smoke test in `crates/lvw-core/src/lib.rs`**: `pub fn version() -> &'static str` returning `env!("CARGO_PKG_VERSION")`, with test `version_is_semver` asserting it contains two dots.
- [ ] **Step 2: Run it**: `cargo test -p lvw-core`. Expected: FAIL to compile (no crate yet). Then create the workspace and crate so it PASSES (1 test).
- [ ] **Step 3: Scaffold the Tauri app** with the files above. `main.rs`: `fn main() { if std::env::args().nth(1).as_deref() == Some("--engine-worker") { std::process::exit(lvw_engine::worker::run_worker()); } lvw::run(); }` (the worker fn arrives in Task 5; stub it as `pub fn run_worker() -> i32 { 0 }` in `lvw-engine` now). `lib.rs`: `pub fn run()` builds `tauri::Builder::default()` with `tauri_plugin_log` (LogDir `lvw`, Stdout) and runs. `pill.html` is a dark rounded pill (`<div class="pill" data-tauri-drag-region>● LVW</div>`) styled by `lvw.css`.
- [ ] **Step 4: Run it**: `cargo tauri dev` from `src-tauri`. Expected: the pill appears, floats above other windows, can be dragged by its body; the log file appears under `~/Library/Logs/com.drake0306.lvw/`.
- [ ] **Step 5: Commit.**

```bash
git add Cargo.toml Cargo.lock crates src-tauri ui .gitignore
git commit -m "feat: workspace, Tauri app skeleton with floating pill"
```

### Task 2: Settings model and persistence

**Files:**
- Create: `crates/lvw-core/src/settings.rs`
- Modify: `crates/lvw-core/src/lib.rs` (`pub mod settings;`)
- Test: inline `#[cfg(test)]` in `settings.rs`

**Interfaces:**
- Produces: `pub enum Activation { PushToTalk, Toggle, HoldOrToggle }`, `pub enum ModifierKey { RightOption, LeftOption, RightCommand, RightControl }`, `pub struct HotkeyBinding { pub combo: Option<String>, pub modifier: Option<ModifierKey> }`, `pub enum PasteMethod { Clipboard, Type }`, `pub struct Settings { pub activation: Activation, pub ptt: HotkeyBinding, pub toggle: HotkeyBinding, pub hold_threshold_ms: u64, pub microphone: Option<String>, pub spoken_languages: Vec<String>, pub model_id: String, pub idle_unload_minutes: Option<u32>, pub post_release_ms: u64, pub mic_lazy_close_secs: u64, pub max_clip_seconds: u32, pub paste_method: PasteMethod, pub flash_attention: bool }` with `impl Default` (PushToTalk; ptt `modifier: Some(RightOption)`; toggle `combo: Some("Ctrl+Shift+Space")`; 300; None; `["en"]`; `"large-v3-turbo"`; `Some(10)`; 150; 60; 300; Clipboard; true). `Settings::load(path: &Path) -> Result<Settings>` (missing file → default; unknown fields ignored via `#[serde(default)]`), `Settings::save(&self, path: &Path) -> Result<()>` (write to `path.with_extension("json.tmp")` then rename), `Settings::validate(&self) -> Result<(), String>` (idle minutes within 1..=240 when Some; hold threshold 100..=2000; max clip 10..=600).

- [ ] **Step 1: Write failing tests**: `default_matches_spec` (asserts every default above), `load_missing_file_returns_default`, `save_then_load_roundtrips` (temp dir), `unknown_fields_are_ignored` (JSON with an extra key loads), `validate_rejects_idle_minutes_out_of_range` (0 and 241 fail, 1 and 240 pass).
- [ ] **Step 2: Run**: `cargo test -p lvw-core settings`. Expected: FAIL, module missing.
- [ ] **Step 3: Implement `settings.rs`** per the Interfaces block (serde derive, `anyhow`, `tempfile` as dev-dependency).
- [ ] **Step 4: Run**: `cargo test -p lvw-core settings`. Expected: 5 passed.
- [ ] **Step 5: Commit.** `git add crates/lvw-core && git commit -m "feat(core): settings model with defaults, load/save, validation"`

### Task 3: Audio conversion and capture

**Files:**
- Create: `crates/lvw-core/src/audio.rs`
- Modify: `crates/lvw-core/Cargo.toml` (cpal 0.18.2, rubato 5.0.1), `lib.rs`
- Test: inline tests; one `#[ignore]` device test

**Interfaces:**
- Produces: `pub const SAMPLE_RATE: u32 = 16_000;` `pub fn mixdown(interleaved: &[f32], channels: usize) -> Vec<f32>`; `pub struct Resampler16k` with `pub fn new(input_rate: u32) -> anyhow::Result<Self>` (identity when `input_rate == 16_000`, otherwise `rubato::Fft::<f32>::new(input_rate as usize, 16_000, 1024, 1, FixedSync::Input)`), `pub fn push(&mut self, mono: &[f32]) -> Vec<f32>` (buffers to the resampler's chunk size), `pub fn flush(&mut self) -> Vec<f32>`; `pub struct ClipBuffer` with `pub fn new(max_seconds: u32) -> Self`, `pub fn push(&mut self, pcm: &[f32]) -> bool` (false once the cap is hit; extra samples dropped), `pub fn seconds(&self) -> f32`, `pub fn take(&mut self) -> Vec<f32>`; `pub fn level_db(frame: &[f32]) -> f32` (RMS in dBFS, -100 floor); `pub struct Capture` with `pub fn start(device_name: Option<&str>, on_pcm16k: Box<dyn FnMut(&[f32]) + Send>) -> anyhow::Result<Capture>` (opens cpal default input config, mixes down, resamples, calls back with 16 kHz mono chunks) and `pub fn stop(self)`; `pub fn input_devices() -> Vec<String>`.

- [ ] **Step 1: Write failing tests**: `mixdown_averages_channels` (`[1.0, 0.0, 0.5, 0.5]`, 2 ch → `[0.5, 0.5]`), `resampler_48k_to_16k_keeps_duration` (1 s of 440 Hz at 48 kHz → between 15 900 and 16 100 samples after push+flush), `resampler_preserves_tone` (zero crossings of the output within 5% of 880 per second), `identity_resampler_passes_through`, `clip_buffer_caps_at_max_seconds` (cap 1 s; pushing 1.5 s returns false and `seconds()` is 1.0), `level_db_of_silence_is_floor`, and `#[ignore] capture_from_default_device_delivers_16k_audio` (starts, sleeps 500 ms, expects ≥ 6000 samples).
- [ ] **Step 2: Run**: `cargo test -p lvw-core audio`. Expected: FAIL, module missing.
- [ ] **Step 3: Implement `audio.rs`.** Inside the cpal callback do mixdown then resample; use `build_input_stream::<f32, _, _>` with the device's default config and `BufferSize::Default`; call `stream.play()`. Keep the `Stream` inside `Capture`.
- [ ] **Step 4: Run**: `cargo test -p lvw-core audio` then `cargo test -p lvw-core audio -- --ignored` (grants the mic prompt once). Expected: 6 passed; ignored test passes with the mic allowed.
- [ ] **Step 5: Commit.** `git commit -am "feat(core): audio mixdown, 16 kHz resampler, clip buffer, cpal capture"`

### Task 4: VAD trimming and clip classification

**Files:**
- Create: `crates/lvw-core/src/vad.rs`, `resources/models/ggml-silero-v5.1.2.bin` (from `https://huggingface.co/ggml-org/whisper-vad/resolve/main/ggml-silero-v5.1.2.bin`, under 1 MB; commit it with its MIT notice in `resources/models/README.md`), `crates/lvw-core/tests/fixtures/hello.wav` (copy of `spike/engine-bench/clips/clip_short.wav`, 23 KB)
- Modify: `crates/lvw-core/Cargo.toml` (whisper-rs 0.16.0 with `metal`, hound dev-dep), `lib.rs`

**Interfaces:**
- Produces: `pub struct VadConfig { pub threshold: f32, pub pre_roll_ms: u32, pub onset_ms: u32, pub hangover_ms: u32, pub min_speech_ms: u32, pub min_silence_ms: u32, pub speech_pad_ms: u32 }` with `Default` = 0.5 / 450 / 60 / 450 / 250 / 100 / 30; `pub enum ClipClass { Silent, NothingHeard, Speech(Vec<f32>) }`; `pub struct Vad` with `pub fn new(model_path: &Path) -> anyhow::Result<Vad>` (wraps `whisper_rs::WhisperVadContext`), `pub fn classify(&mut self, pcm: &[f32], cfg: &VadConfig) -> anyhow::Result<ClipClass>` (all samples `|x| < 1e-6` → `Silent`; `segments_from_samples` with the config → no segments → `NothingHeard`; otherwise `Speech(slice)` from `first.start − pre_roll` to `last.end + hangover`, clamped; segment timestamps are centiseconds, 160 samples each); `pub fn pad_min(pcm: Vec<f32>, min_seconds: f32) -> Vec<f32>`; `pub fn is_too_short(pcm: &[f32], min_ms: u32) -> bool`.

- [ ] **Step 1: Write failing tests**: `digital_silence_is_silent` (1 s of zeros), `low_noise_is_nothing_heard` (1 s of ±0.002 white noise), `speech_fixture_is_speech_and_trimmed` (load `hello.wav`, pad front and back with 1 s of zeros, classify → `Speech(v)` with `v.len()` between 0.5 s and 2.0 s of samples), `pad_min_extends_to_1_25_s` (100 samples → 20 000), `too_short_under_300_ms`.
- [ ] **Step 2: Run**: `cargo test -p lvw-core vad`. Expected: FAIL, module missing.
- [ ] **Step 3: Implement `vad.rs`** per Interfaces; `WhisperVadParams` setters: `set_threshold`, `set_min_speech_duration`, `set_min_silence_duration`, `set_speech_pad`.
- [ ] **Step 4: Run**: `cargo test -p lvw-core vad`. Expected: 5 passed.
- [ ] **Step 5: Commit.** `git add resources crates && git commit -m "feat(core): Silero VAD trimming and clip classification"`

### Task 5: Engine protocol, whisper backend, worker and supervisor

**Files:**
- Create: `crates/lvw-engine/src/protocol.rs`, `src/backend.rs`, `src/worker.rs`, `src/supervisor.rs`, `src/policy.rs`, `src/bin/lvw-worker.rs`
- Modify: `crates/lvw-engine/Cargo.toml` (whisper-rs 0.16.0 `metal`, postcard 1.1.3 with `use-std`, serde, anyhow, log, env_logger, lvw-core path dep), `src/lib.rs`
- Test: inline tests; `crates/lvw-engine/tests/worker_integration.rs` (`#[ignore]`, needs `LVW_TEST_MODEL` env var pointing at `ggml-large-v3-turbo-q5_0.bin`)

**Interfaces:**
- `protocol.rs` produces: `pub struct DecodeOptions { pub language: Option<String>, pub translate: bool, pub initial_prompt: Option<String>, pub n_threads: i32 }`; `pub enum Request { Ping, Load { model_path: PathBuf, flash_attn: bool, use_gpu: bool }, Unload, Transcribe { id: u64, pcm: Vec<f32>, opts: DecodeOptions }, Shutdown }`; `pub enum Response { Pong, Loaded { load_ms: u64 }, Unloaded, Transcript { id: u64, text: String, language: Option<String>, decode_ms: u64, audio_ctx: i32, reran: bool }, Error { id: Option<u64>, message: String } }`; `pub fn write_frame<W: Write, T: Serialize>(w: &mut W, msg: &T) -> io::Result<()>` (u32 little-endian length + postcard bytes, then flush); `pub fn read_frame<R: Read, T: DeserializeOwned>(r: &mut R) -> io::Result<Option<T>>` (`Ok(None)` on EOF at a frame boundary).
- `policy.rs` produces: `pub fn audio_ctx_for(seconds: f32) -> i32` (the Global Constraints formula); `pub fn deadline_for(seconds: f32) -> Duration` (15 s + 3 × seconds).
- `backend.rs` produces: `pub struct WhisperBackend` with `pub fn load(path: &Path, flash_attn: bool, use_gpu: bool) -> anyhow::Result<(Self, Duration)>`, `pub fn transcribe(&self, pcm: &[f32], opts: &DecodeOptions) -> anyhow::Result<Decoded>` where `pub struct Decoded { pub text: String, pub language: Option<String>, pub audio_ctx: i32, pub reran: bool }`; applies the decode defaults, `audio_ctx_for(pcm.len() as f32 / 16000.0)`, and the repetition guard using `lvw_core::textnorm::has_repetition_loop(&text, 4)` (Task 9 provides it; until then, implement the guard with a local helper and swap in Task 9).
- `worker.rs` produces: `pub fn run_worker() -> i32` (env_logger to stderr; loop `read_frame` on stdin; on `Ok(None)` or error: `std::mem::forget` the backend and `std::process::exit(0)`; `Shutdown` → same; responses via `write_frame` on a locked stdout).
- `supervisor.rs` produces: `pub trait Engine: Send { fn load(&mut self, model: &Path, flash_attn: bool) -> anyhow::Result<Duration>; fn unload(&mut self) -> anyhow::Result<()>; fn is_loaded(&self) -> bool; fn transcribe(&mut self, pcm: &[f32], opts: &DecodeOptions) -> anyhow::Result<Transcript>; fn cancel(&mut self); }`; `pub struct Transcript { pub text: String, pub language: Option<String>, pub decode_ms: u64, pub audio_ctx: i32, pub reran: bool }`; `pub struct WorkerEngine` with `pub fn new(worker: Command) -> Self` (the command to spawn, e.g. `Command::new(current_exe()).arg("--engine-worker")`), implementing `Engine`: spawns on first `load`, pipes stdin/stdout, a reader thread for stderr keeping the last 50 lines in `pub fn last_stderr(&self) -> Vec<String>`; `transcribe` writes the request and waits up to `deadline_for(seconds)`; on timeout or a dead child it kills the child, records `pub fn last_failure(&self) -> Option<String>`, sets state to unloaded-with-model-path and returns `Err`; the next `load` or `transcribe` respawns and reloads the remembered model; `cancel` kills the child the same way; `Drop` drops stdin (worker exits) then `kill`+`wait` after 2 s.

- [ ] **Step 1: Write failing tests in `protocol.rs` and `policy.rs`**: `frame_roundtrip_through_vec` (Request::Transcribe with 3 samples), `read_frame_returns_none_at_eof`, `audio_ctx_policy_matches_spike` (0.7→768, 7.5→768, 13.0→768, 20.0→1152, 28.0→1500, 40.0→1500), `deadline_scales_with_clip` (10 s → 45 s; 300 s → 915 s).
- [ ] **Step 2: Run**: `cargo test -p lvw-engine`. Expected: FAIL, modules missing. Implement `protocol.rs` and `policy.rs`. Run again. Expected: 4 passed.
- [ ] **Step 3: Write the supervisor tests** in `supervisor.rs` using a fake worker command `Command::new(env!("CARGO_BIN_EXE_lvw-worker")).env("LVW_WORKER_FAKE", "1")` where the worker, when that env var is set, answers `Load` with `Loaded{load_ms:1}` without touching whisper, answers `Transcribe` with the text `"fake"` after sleeping `LVW_WORKER_SLEEP_MS` (default 0), and exits on EOF: `supervisor_loads_and_transcribes_through_fake_worker`, `supervisor_times_out_and_respawns` (sleep 3000, patched deadline 500 ms via `WorkerEngine::with_deadline_override`), `supervisor_respawns_after_worker_killed` (kill the child via `child_pid()`, next transcribe succeeds, exactly one worker process alive, checked with `pgrep -f lvw-worker | wc -l` equal to 1), `dropping_supervisor_ends_worker` (after drop, `try_wait` on the pid via `kill -0` fails within 2 s).
- [ ] **Step 4: Run**: `cargo test -p lvw-engine supervisor`. Expected: FAIL, types missing.
- [ ] **Step 5: Implement `backend.rs`, `worker.rs` (with the fake mode), `supervisor.rs`, `bin/lvw-worker.rs` (`fn main() { std::process::exit(lvw_engine::worker::run_worker()) }`).**
- [ ] **Step 6: Run**: `cargo test -p lvw-engine`. Expected: all pass (8). Then the real-model integration test `worker_integration.rs`: `transcribes_hello_fixture` loads `LVW_TEST_MODEL`, sends the `hello.wav` fixture padded to 1.25 s, expects text containing "hello" (case-insensitive) and `audio_ctx == 768`. Run: `LVW_TEST_MODEL=~/.cache/lvw-spike/models/ggml-large-v3-turbo-q5_0.bin cargo test -p lvw-engine --test worker_integration -- --ignored`. Expected: 1 passed, decode under 3 s.
- [ ] **Step 7: Commit.** `git add crates/lvw-engine && git commit -m "feat(engine): pipe protocol, whisper-rs backend, worker process and supervisor"`

### Task 6: Hotkey state machine

**Files:**
- Create: `crates/lvw-core/src/hotkeys.rs`
- Modify: `lib.rs`

**Interfaces:**
- Produces: `pub enum Binding { Ptt, Toggle }`; `pub enum Input { Down(Binding), Up(Binding), Cancel, PipelineDone, StartResult(bool) }`; `pub enum Effect { StartRecording, StopRecording, CancelRecording, Nothing }`; `pub enum Stage { Idle, Recording { locked: bool, since: Instant }, Processing }`; `pub struct Machine` with `pub fn new(activation: Activation, hold_threshold: Duration) -> Self`, `pub fn on(&mut self, input: Input, now: Instant) -> Effect`, `pub fn stage(&self) -> &Stage`. Rules (from §18.10): a `Down` in `Idle` → `StartRecording` and `Recording` (optimistic); `StartResult(false)` → back to `Idle`; PushToTalk: any `Up(Ptt)` while recording → `StopRecording` and `Processing`; Toggle: `Up` ignored, next `Down` → `StopRecording`; HoldOrToggle: `Up` before the threshold locks (`locked = true`, `Nothing`), `Up` at or after the threshold → `StopRecording`; while `Processing`, a `Down` is remembered and `PipelineDone` returns `StartRecording` for it; two `Down`s while processing cancel each other; `Cancel` while recording → `CancelRecording`; `Cancel` while processing clears any remembered press.

- [ ] **Step 1: Write failing tests** with an explicit `Instant` clock: `ptt_hold_and_release_records_once`, `toggle_tap_tap_starts_then_stops`, `hold_or_toggle_short_tap_locks_until_next_press`, `hold_or_toggle_long_hold_stops_on_release` (350 ms), `press_while_processing_is_remembered_until_pipeline_done`, `two_presses_while_processing_cancel_out`, `optimistic_start_rolls_back_when_capture_fails`, `cancel_while_recording_cancels`.
- [ ] **Step 2: Run**: `cargo test -p lvw-core hotkeys`. Expected: FAIL.
- [ ] **Step 3: Implement `hotkeys.rs`.**
- [ ] **Step 4: Run**: Expected: 8 passed.
- [ ] **Step 5: Commit.** `git commit -am "feat(core): activation state machine (ptt, toggle, hold-or-toggle)"`

### Task 7: Paste, clipboard backup and frontmost app (macOS)

**Files:**
- Create: `src-tauri/src/paste.rs`, `src-tauri/src/frontmost.rs`
- Modify: `src-tauri/Cargo.toml` (arboard 3.6.1, enigo 0.6.1, objc2 0.6.5, objc2-foundation 0.3.2, objc2-app-kit 0.3.2 with `NSWorkspace`, `NSRunningApplication`)
- Test: inline unit tests with an injected key sender; manual steps for the real paste

**Interfaces:**
- Produces: `pub trait KeySender { fn paste_chord(&mut self) -> anyhow::Result<()>; fn type_text(&mut self, text: &str) -> anyhow::Result<()>; }` with `pub struct EnigoSender` (Cmd+V as `Key::Meta` press, `Key::Other(9)` click, `Key::Meta` release); `pub struct ClipboardBackup` with `pub fn save(path: &Path, text: Option<&str>) -> anyhow::Result<()>` and `pub fn restore_pending(path: &Path) -> anyhow::Result<Option<String>>` (returns and deletes); `pub struct Paster<S: KeySender> { pub method: PasteMethod, pub restore_delay: Duration, backup_path: PathBuf }` with `pub fn paste(&mut self, text: &str, sender: &mut S) -> anyhow::Result<PasteOutcome>` where `pub enum PasteOutcome { Pasted, CopiedOnly(String) }` (sets clipboard, sends the chord, sleeps `restore_delay`, restores; if the chord fails the clipboard is left holding the text and `CopiedOnly(reason)` is returned); `pub fn accessibility_trusted(prompt: bool) -> bool` (via `AXIsProcessTrustedWithOptions`); `frontmost.rs`: `pub fn frontmost_app_name() -> Option<String>` via `NSWorkspace::sharedWorkspace().frontmostApplication().localizedName()`.

- [ ] **Step 1: Write failing tests** with a `FakeSender` (records calls; configurable failure): `paste_sets_clipboard_sends_chord_and_restores` (seed clipboard "old", paste "new", after the call the fake saw one chord and the clipboard is "old" again; uses a 10 ms delay), `paste_without_permission_keeps_text_on_clipboard` (sender fails → `CopiedOnly`, clipboard is "new", backup file still present), `backup_roundtrip_and_delete`. These touch the real system clipboard, so mark them `#[ignore]` and run them explicitly.
- [ ] **Step 2: Run**: `cargo test -p lvw paste -- --ignored`. Expected: FAIL.
- [ ] **Step 3: Implement `paste.rs` and `frontmost.rs`.**
- [ ] **Step 4: Run**: Expected: 3 passed. Manual: a tiny `#[tauri::command] debug_paste(text)` invoked from the panel's devtools console pastes into TextEdit with Accessibility granted, and returns `CopiedOnly` when it is not.
- [ ] **Step 5: Commit.** `git commit -am "feat(app): clipboard paste with disk backup, frontmost app name"`

### Task 8: History store

**Files:**
- Create: `crates/lvw-core/src/store.rs`
- Modify: `crates/lvw-core/Cargo.toml` (rusqlite 0.40.2 `bundled`), `lib.rs`

**Interfaces:**
- Produces: `pub struct NewDictation { pub app_name: Option<String>, pub mode: String, pub model: String, pub language: Option<String>, pub duration_ms: u32, pub raw_text: String, pub polished_text: Option<String>, pub status: String, pub timing_json: Option<String> }`; `pub struct Dictation { pub id: i64, pub created_at_ms: i64, ..same fields.., pub pinned: bool }`; `pub struct Store` with `pub fn open(path: &Path) -> anyhow::Result<Store>` (WAL, synchronous NORMAL, busy timeout 5 s, `CREATE TABLE IF NOT EXISTS dictations(...)` per §6 plus `settings(key TEXT PRIMARY KEY, value_json TEXT)` and `schema_version`), `pub fn insert(&mut self, d: &NewDictation) -> anyhow::Result<i64>` (one transaction), `pub fn list(&self, limit: u32, offset: u32) -> anyhow::Result<Vec<Dictation>>` (newest first, excludes soft-deleted), `pub fn search(&self, query: &str, limit: u32) -> anyhow::Result<Vec<Dictation>>` (`LIKE %q%` on raw and polished, case-insensitive), `pub fn update_text(&mut self, id: i64, polished_text: &str) -> anyhow::Result<()>`, `pub fn delete(&mut self, id: i64) -> anyhow::Result<()>` (sets `deleted_at`), `pub fn set_pinned(&mut self, id: i64, pinned: bool)`, `pub fn count(&self) -> anyhow::Result<u32>`.

- [ ] **Step 1: Write failing tests** on a temp file DB: `open_creates_schema_in_wal_mode` (`PRAGMA journal_mode` returns `wal`), `insert_then_list_newest_first`, `search_matches_case_insensitively`, `update_text_changes_polished`, `delete_hides_row_but_keeps_it` (count via raw SQL still 1), `reopen_keeps_rows`.
- [ ] **Step 2: Run**: `cargo test -p lvw-core store`. Expected: FAIL.
- [ ] **Step 3: Implement `store.rs`.**
- [ ] **Step 4: Run**: Expected: 6 passed.
- [ ] **Step 5: Commit.** `git commit -am "feat(core): SQLite history store in WAL mode"`

### Task 9: Text clean-up

**Files:**
- Create: `crates/lvw-core/src/textnorm.rs`
- Modify: `lib.rs`; `crates/lvw-engine/src/backend.rs` (swap the local repetition helper for `lvw_core::textnorm::has_repetition_loop`)

**Interfaces:**
- Produces: `pub fn collapse_stutters(text: &str) -> String` (three or more consecutive identical words, case-insensitive, become one), `pub fn has_repetition_loop(text: &str, n: usize) -> bool`, `pub fn normalize(text: &str) -> String` (collapse stutters, collapse runs of whitespace, trim).

- [ ] **Step 1: Write failing tests**: `collapses_three_or_more_repeats` ("I I I think" → "I think"; "no no" unchanged), `repetition_loop_detected_at_four` ("Hello. Hello. Hello. Hello." true at n=4, "Hello. Hello." false), `normalize_trims_and_collapses_spaces`.
- [ ] **Step 2: Run**: `cargo test -p lvw-core textnorm`. Expected: FAIL.
- [ ] **Step 3: Implement; update `backend.rs` to use it.**
- [ ] **Step 4: Run**: `cargo test -p lvw-core textnorm && cargo test -p lvw-engine`. Expected: all pass.
- [ ] **Step 5: Commit.** `git commit -am "feat(core): stutter collapse and repetition-loop guard"`

### Task 10: Pipeline coordinator and pill states

**Files:**
- Create: `src-tauri/src/pipeline.rs`, `src-tauri/src/state.rs`, `src-tauri/src/commands.rs`
- Modify: `src-tauri/src/lib.rs`, `ui/pill.html`, `ui/lvw.js`, `ui/lvw.css`
- Test: `src-tauri/src/pipeline.rs` inline tests with fakes (the coordinator is generic over `Engine`, a `CaptureSource` trait and a `Paster` trait so it runs without hardware)

**Interfaces:**
- Produces: `pub enum PillState { Idle { hotkey_hint: String }, Listening { seconds: f32, level_db: f32 }, Transcribing, LoadingModel, Pasted { preview: String }, Error { message: String } }` (serialised as `{ "state": "idle", ... }` and emitted as the Tauri event `pill-state`); `pub enum Cmd { Start, Stop, Cancel, ModelReady, Quit }`; `pub struct Coordinator<E: Engine, C: CaptureSource, P: PasteSink>` with `pub fn new(deps: Deps<E, C, P>, settings: Arc<Mutex<Settings>>, store: Arc<Mutex<Store>>, vad: Vad, emit: Box<dyn Fn(PillState) + Send>) -> Self` and `pub fn run(self, rx: Receiver<Cmd>)` on its own thread; `pub trait CaptureSource { fn start(&mut self, on_pcm: Box<dyn FnMut(&[f32]) + Send>) -> anyhow::Result<()>; fn stop(&mut self); }`; `pub trait PasteSink { fn paste(&mut self, text: &str) -> anyhow::Result<PasteOutcome>; }`. Stop handling: wait `post_release_ms`, stop capture, `ClipBuffer::take`, discard under 300 ms, `Vad::classify` (Silent → `Error("Microphone is silent")`, NothingHeard → `Error("Nothing heard")`), `pad_min` 1.25 s, if engine not loaded show `LoadingModel` and load (model path from `models/<id>.bin`), `transcribe` with `DecodeOptions { language: first spoken language, n_threads: 4, .. }`, `normalize`, paste, insert history row with `timing_json` (`{"capture_ms","vad_ms","decode_ms","paste_ms"}`), emit `Pasted { preview: first 60 chars }`, then `Idle` after 2 s. Max clip: a timer stops the recording at `max_clip_seconds`. `commands.rs`: `#[tauri::command] get_settings`, `set_settings(settings)` (validate, save, apply idle minutes), `list_history(limit, offset)`, `search_history(query)`, `delete_history(id)`, `list_input_devices`, `pill_hint`.

- [ ] **Step 1: Write failing tests** with `FakeEngine` (configurable load delay and text), `FakeCapture` (feeds a given PCM on start), `FakePaste` (records text): `pipeline_records_transcribes_pastes_and_stores` (fixture `hello.wav` → paste called with text, store count 1, states seen in order Listening, Transcribing, Pasted), `pipeline_discards_clips_under_300ms`, `pipeline_reports_silent_mic` (zeros → `Error("Microphone is silent")`, store count 0), `pipeline_queues_clip_until_engine_loaded` (engine load delay 300 ms; Stop arrives before load completes; transcript still produced; `LoadingModel` state seen), `pipeline_reports_capture_error` (FakeCapture start fails → `Error`, machine idle).
- [ ] **Step 2: Run**: `cargo test -p lvw pipeline`. Expected: FAIL.
- [ ] **Step 3: Implement `pipeline.rs`, `state.rs` (AppState with `Arc<Mutex<..>>` fields and the `Sender<Cmd>`), `commands.rs`; implement `CaptureSource` for a wrapper around `lvw_core::audio::Capture` and `PasteSink` for `Paster<EnigoSender>`; bundle `resources/models/ggml-silero-v5.1.2.bin` via `bundle.resources` in `tauri.conf.json` and resolve it with `app.path().resource_dir()` to build `Vad`; wire real `Capture`, `WorkerEngine` (`Command::new(std::env::current_exe()?).arg("--engine-worker")`), `Paster<EnigoSender>` in `lib.rs`; `pill.html` renders each state (dot, bars, timer, spinner, check + preview, warning) from the `pill-state` event; `pill.html` click opens the panel window.**
- [ ] **Step 4: Run**: `cargo test -p lvw pipeline`. Expected: 5 passed. Manual: `cargo tauri dev`, trigger Start/Stop via a temporary `debug_start`/`debug_stop` command from the panel devtools, speak, see the pill cycle and text pasted into TextEdit.
- [ ] **Step 5: Commit.** `git commit -am "feat(app): pipeline coordinator, pill states, history and settings commands"`

### Task 11: Hotkey wiring, tray, lifecycle and idle unload

**Files:**
- Create: `src-tauri/src/hotkeys.rs`, `src-tauri/src/tray.rs`, `src-tauri/src/lifecycle.rs`, `crates/lvw-core/src/lifecycle.rs`
- Modify: `src-tauri/Cargo.toml` (tauri `tray-icon`, `image-png`, `macos-private-api`; tauri-plugin-global-shortcut 2.4.1; tauri-plugin-single-instance 2.5.4; tauri-nspanel 2.1.1; rdev rustdesk fork; ctrlc 3), `src-tauri/src/lib.rs`, `capabilities/default.json`

**Interfaces:**
- `lvw-core::lifecycle` produces: `pub struct LockFile` with `pub fn acquire(path: &Path) -> anyhow::Result<(LockFile, bool)>` (bool = a stale lock existed, meaning the last run ended uncleanly; writes the pid; `Drop` removes the file) and `pub fn release(self)`; `pub struct IdleTimer` with `pub fn new(minutes: Option<u32>) -> Self`, `pub fn set_minutes(&mut self, minutes: Option<u32>, now: Instant)`, `pub fn touch(&mut self, now: Instant)`, `pub fn due(&self, now: Instant) -> bool`, `pub fn remaining(&self, now: Instant) -> Option<Duration>`.
- `src-tauri::hotkeys` produces: `pub fn install(app: &AppHandle, settings: &Settings, tx: Sender<HotkeyEvent>) -> anyhow::Result<()>` where `pub enum HotkeyEvent { Down(Binding), Up(Binding) }`: combos through `tauri_plugin_global_shortcut` (`ShortcutState::Pressed`/`Released`), single modifiers through an `rdev::listen` thread mapping `Key::AltGr` → RightOption etc. (`KEYBOARD_ONLY=1`), feeding `Machine` (Task 6) on the coordinator thread.
- `src-tauri::tray` produces: `pub fn build(app: &AppHandle) -> anyhow::Result<TrayIcon>` with items `Pause and unload`, `Resume`, `Open history`, `Settings`, separator, `Quit LVW`, and `pub fn set_status(tray: &TrayIcon, text: &str)` (tooltip, e.g. "Unloads in 7 min").
- `src-tauri::lifecycle` produces: `pub fn install(app: &AppHandle, state: AppState)` which: acquires the lock file (emits a one-line notice to the panel if stale), registers `ctrlc` for SIGINT/SIGTERM, handles `RunEvent::ExitRequested`/`Exit` by running `shutdown(state)` = cancel in-flight, `engine.unload()` (which drops the worker), store flush, lock release, within a 2 s deadline then `process::exit`; `prevent_exit` when the last window closes (tray app); the idle timer tick (every 15 s) calls `engine.unload()` when due and updates the tray; pill becomes `Idle` again. `ActivationPolicy::Accessory` so LVW has no Dock icon; the pill window is converted with `to_panel::<PillPanel>()` (non-activating, all spaces, full-screen auxiliary).

- [ ] **Step 1: Write failing tests** in `lvw-core::lifecycle`: `lock_file_reports_stale_on_second_acquire_without_release`, `lock_file_removed_on_drop`, `idle_timer_due_after_minutes` (fake `Instant`s), `idle_timer_never_when_none`, `idle_timer_set_minutes_reschedules`. In `src-tauri`: `exit_path_completes_with_worker_busy` (fake engine whose `transcribe` sleeps 5 s; `shutdown` returns within 2.5 s and `unload` was called).
- [ ] **Step 2: Run**: `cargo test -p lvw-core lifecycle && cargo test -p lvw lifecycle`. Expected: FAIL.
- [ ] **Step 3: Implement** the four modules and wire them in `lib.rs` (single-instance plugin first; nspanel plugin; global-shortcut plugin; tray; hotkeys; lifecycle). Add `global-shortcut:allow-register`, `allow-unregister`, `core:window:allow-show/hide/set-focus` to the capability.
- [ ] **Step 4: Run**: tests pass (6). Manual: Right Option hold records and pastes; Ctrl+Shift+Space toggles; Input Monitoring and Accessibility prompts appear once; Quit from the tray exits within 2 s and `pgrep -f engine-worker` finds nothing; killing the app with `kill -9` leaves no worker within 2 s; relaunch shows the stale-lock notice; idle unload set to 1 minute frees the model (log line) and the next hotkey shows `LoadingModel` then works.
- [ ] **Step 5: Commit.** `git commit -am "feat(app): hotkeys, tray, lock file, exit path, idle unload, non-activating pill panel"`

### Task 12: Model download on first run

**Files:**
- Create: `src-tauri/src/models.rs`, `crates/lvw-core/src/download.rs`
- Modify: `crates/lvw-core/Cargo.toml` (reqwest 0.13.5 `stream`, futures-util 0.3, sha2 0.10), `src-tauri/src/lib.rs`, `src-tauri/src/commands.rs`, `ui/panel.html`

**Interfaces:**
- `lvw-core::download` produces: `pub struct ModelSpec { pub id: &'static str, pub file: &'static str, pub url: &'static str, pub bytes: u64, pub sha256: Option<&'static str> }` with `pub const TURBO_Q5_0: ModelSpec` (`large-v3-turbo`, `ggml-large-v3-turbo-q5_0.bin`, `https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q5_0.bin`, 574041195, None); `pub async fn download(spec: &ModelSpec, dir: &Path, progress: impl FnMut(u64, u64)) -> anyhow::Result<PathBuf>` (resumes a `.part` file with a `Range` header, renames on completion, verifies size); `pub fn is_present(spec: &ModelSpec, dir: &Path) -> bool`.
- `src-tauri::models` produces: `pub async fn ensure_default_model(app: &AppHandle) -> anyhow::Result<PathBuf>` emitting `model-download` events `{ received, total }`; called at startup; the pill shows `LoadingModel` with a percentage during the download.

- [ ] **Step 1: Write failing tests**: `download_resumes_partial_file` and `download_verifies_size` against a local HTTP server from `tiny_http` (dev-dependency) serving a 1 MB body with Range support; `is_present_checks_size`.
- [ ] **Step 2: Run**: `cargo test -p lvw-core download`. Expected: FAIL.
- [ ] **Step 3: Implement**; the panel shows a progress bar on the `model-download` event; the first-run copy in the panel explains the three permissions (Microphone, Input Monitoring, Accessibility) with "Open System Settings" buttons (`open x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone` etc. via `tauri_plugin_opener` 2.5 or `std::process::Command::new("open")`).
- [ ] **Step 4: Run**: tests pass (3). Manual: delete `models/`, launch, watch the download reach 100% then the first dictation work.
- [ ] **Step 5: Commit.** `git commit -am "feat: first-run model download with resume and progress"`

### Task 13: Panel: history list and settings

**Files:**
- Modify: `ui/panel.html`, `ui/lvw.js`, `ui/lvw.css`, `src-tauri/src/commands.rs`

**Interfaces:**
- Consumes the commands from Task 10 and 12. Produces the panel UI: header with search, model dropdown (shows `large-v3-turbo` only in this plan, disabled), stats (count); rows with time, app, duration, text, buttons Copy (via `navigator.clipboard.writeText`) and Delete; a Settings section with activation mode, hotkey display, microphone select, idle minutes (number input 1–240 plus "Never" checkbox), post-release ms, paste method; Save calls `set_settings`, errors from `validate` shown inline.

- [ ] **Step 1: Write the one Rust test this task adds**: `set_settings_rejects_invalid_and_keeps_old` (command-level: invalid idle minutes → `Err`, stored settings unchanged).
- [ ] **Step 2: Run**: `cargo test -p lvw commands`. Expected: FAIL.
- [ ] **Step 3: Implement the command guard and the panel UI.**
- [ ] **Step 4: Run**: test passes. Manual: dictate three times, open the panel from the pill, see three rows, copy one, delete one, change idle minutes to 2 and confirm the tray tooltip countdown changes without restart.
- [ ] **Step 5: Commit.** `git commit -am "feat(ui): history panel and settings"`

### Task 14: README and dev notes

**Files:**
- Create: `README.md` (what LVW is, status, build on macOS: Rust, `cargo install tauri-cli`, `cargo tauri dev`; permissions; where data lives; licence), `docs/dev/permissions.md` (how to reset TCC grants with `tccutil reset Microphone com.drake0306.lvw` etc. for retesting)

- [ ] **Step 1: Write both files.**
- [ ] **Step 2: Verify**: a fresh clone builds with the README steps (`cargo build` from the root succeeds).
- [ ] **Step 3: Commit.** `git add README.md docs/dev && git commit -m "docs: README and permission reset notes"`
