# PrivateVox Panel Workbench, Models, Memory and Chunked Decode Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn the v1 core into the app the design describes on the panel side: three switchable Whisper models with download, delete and benchmark; a live GPU memory strip with Recommended and Best; a history that can be edited, pinned, pasted again and exported; a hotkey picker; undo last paste; and chunked decoding while the user is still speaking.

**Architecture:** Pure logic stays in `privatevox-core` (memory budget, model catalog, chunk planning, undo policy, export formatting) with unit tests. The engine protocol gains resident-memory reporting. The app gains a `models` manager, a `memory` reporter, a chunked decode path in the coordinator, and commands the panel calls. The UI stays plain HTML/JS.

**Tech Stack:** as in the core plan, plus `sysinfo = "0.39.6"`.

**Spec:** `docs/superpowers/specs/2026-10-28-whisper-harness-design.md` §4.4, §4.5, §4.6, §4.2 (undo), §6, §9, §18.6, §18.9; spike results for numbers. API facts: `docs/superpowers/reference/2026-10-28-crate-api-reference.md`. Built on the core plan `docs/superpowers/plans/2026-10-28-privatevox-core-dictation.md` (its Interfaces blocks still hold).

**Out of scope (next plans):** LLM polish, modes, re-polish, LAN endpoints, keep-audio, dictionary, receipt-sequenced paste, secure input, Windows.

## Global Constraints

- Same as the core plan (one engine owner, worker process, decode defaults, data directory, no attribution trailers, pinned versions).
- Model catalog (file, bytes): `ggml-large-v3-turbo-q5_0.bin` 574041195; `ggml-large-v3-q5_0.bin` 1081140203; `ggml-distil-large-v3.bin` 1519521155 (f16, from `distil-whisper/distil-large-v3-ggml`, English only). Model ids: `large-v3-turbo`, `large-v3`, `distil-large-v3`.
- Only one Whisper model resident at a time; switching unloads first (§3).
- Memory strip thresholds: ≤ 80 % good, 80–90 % tight, > 90 % will not fit (§9). Recommended = fastest tier that fits under 80 %; Best = most accurate that fits (`large-v3` > `large-v3-turbo` > `distil-large-v3`).
- Chunking constants (§18.6): Silero threshold 0.5, release 0.35, min speech 250 ms, min silence 100 ms, speech pad 30 ms; chunks 5–40 s aiming at 30 s; a cut is final once 5 s past it is scored; clips ≤ 40 s never cut.
- Undo window: 30 s, same frontmost app as the paste.
- Benchmark clip: 30 s generated with macOS `say` into the data directory once; speed is seconds of audio per second of decode.
- Refresh the memory strip every 5 s while the panel is visible.

## Review Focus

- Switching models while a recording is in progress: the switch waits until the pipeline is idle and the pill says "Loading large-v3…"; the clip is not lost. Pinned in Task 4 by `switch_during_recording_is_deferred`.
- Deleting the active model: refused with a message; deleting an inactive model frees the disk and the catalog shows "not downloaded". Pinned in Task 4 by `delete_active_model_is_refused`.
- A 2-minute dictation: chunks decode while recording; text arrives in order; the clip cap countdown shows at 4:30 and the cap stops at 5:00 cleanly. Pinned in Task 7 by `long_clip_is_decoded_in_order_while_recording` and `cap_countdown_then_stop`.
- Undo after the user switched apps: no keystroke is sent to the wrong app; the pill offers "copy again". Pinned in Task 9 by `undo_refused_when_frontmost_changed`.
- The memory strip when the system is already near full: "will not fit" shows red and Recommended falls back to the smallest tier or "nothing fits". Pinned in Task 2 by `nothing_fits_when_memory_is_exhausted`.

---

### Task 1: Engine reports resident memory

**Files:** Modify `crates/privatevox-engine/src/protocol.rs`, `worker.rs`, `backend.rs`, `supervisor.rs`; `tests/supervisor.rs`.

**Interfaces:** `Response::Loaded { load_ms: u64, resident_bytes: u64 }` (worker RSS delta across the load, via `libc::proc_pidinfo`/`ps -o rss` on macOS); `Engine::load` returns `LoadInfo { duration: Duration, resident_bytes: u64 }`; `WorkerEngine::last_load() -> Option<LoadInfo>`. The fake worker reports `resident_bytes: 123_456_789`.

- [ ] Tests first: `frame_roundtrip_loaded_with_resident_bytes`; supervisor `load_reports_resident_bytes_from_worker` (fake → 123456789). Run, expect RED.
- [ ] Implement; update every `Engine` impl (fakes in app tests). Run all engine and app suites, expect GREEN.
- [ ] Commit: `feat(engine): report resident memory after load`.

### Task 2: Memory budget logic

**Files:** Create `crates/privatevox-core/src/memory.rs`.

**Interfaces:** `pub struct Tier { pub id: &'static str, pub label: &'static str, pub accuracy_rank: u8, pub speed_rank: u8 }` with `pub const TIERS: [Tier; 3]`; `pub struct MemoryInput { pub total: u64, pub used_by_others: u64, pub footprints: HashMap<String, u64> /* measured or estimated bytes per model id */, pub llm_bytes: u64, pub selected: String }`; `pub enum Verdict { Good, Tight, WontFit }`; `pub struct MemoryReport { pub total: u64, pub used_by_others: u64, pub selected_bytes: u64, pub llm_bytes: u64, pub projected: u64, pub percent: u8, pub verdict: Verdict, pub recommended: Option<String>, pub best: Option<String>, pub free_after: u64 }`; `pub fn budget(input: &MemoryInput) -> MemoryReport`; `pub fn estimate_footprint(model_id: &str) -> u64` (turbo 550 MB, large-v3 1.0 GB, distil 1.6 GB until measured); `pub fn system_memory() -> (u64, u64)` via `sysinfo` (total, used).

- [ ] Tests: `recommended_is_turbo_when_it_fits`, `best_is_large_v3_under_80_percent`, `tight_between_80_and_90`, `wont_fit_above_90_and_steps_down`, `nothing_fits_when_memory_is_exhausted`, `measured_footprint_overrides_estimate`. RED → implement → GREEN.
- [ ] Commit: `feat(core): memory budget with recommended and best tiers`.

### Task 3: Model catalog and store rows

**Files:** Create `crates/privatevox-core/src/models.rs`; modify `store.rs` (table `models(id TEXT PRIMARY KEY, downloaded INTEGER, load_ms INTEGER, speed_x REAL, mem_bytes INTEGER, measured_at_ms INTEGER)`), `download.rs` (add `DISTIL_LARGE_V3_F16`).

**Interfaces:** `pub struct ModelInfo { pub id, pub label, pub file, pub bytes, pub quant: &'static str, pub english_only: bool, pub spec: ModelSpec }`; `pub fn catalog() -> Vec<ModelInfo>`; `pub fn model_file(models_dir, id) -> PathBuf`; store: `pub fn model_stats(&self, id) -> Option<ModelStats>`, `pub fn set_model_stats(&mut self, id, stats)`, `pub struct ModelStats { load_ms: Option<u64>, speed_x: Option<f32>, mem_bytes: Option<u64> }`.

- [ ] Tests: `catalog_has_three_tiers_with_spec_sizes`, `model_stats_roundtrip`, `distil_spec_matches_hf`. RED → GREEN. Commit: `feat(core): model catalog and per-model stats`.

### Task 4: Model manager in the app (switch, download, delete, measured footprint)

**Files:** Modify `src-tauri/src/models.rs`, `pipeline.rs` (`Cmd::SwitchModel(String)`, `Cmd::ModelDeleted`), `commands.rs`, `state.rs`.

**Interfaces:** commands `list_models() -> Vec<ModelRow { id, label, downloaded, downloading, active, bytes, stats }>`, `switch_model(id)`, `download_model_by_id(id)`, `delete_model(id) -> Result<(), String>` (refuses the active one), events `model-download` (existing) and `models-changed`. Switching: `Cmd::SwitchModel` is honoured only when `recording.is_none()`; otherwise it is remembered and applied when the pipeline drains; the pill shows `LoadingModel` during the swap; the engine's `LoadInfo.resident_bytes` is written to `models.mem_bytes`.

- [ ] Tests in `pipeline.rs` with fakes: `switch_during_recording_is_deferred`, `switch_unloads_then_loads_and_updates_settings`; in `commands.rs`: `delete_active_model_is_refused`. RED → GREEN. Manual: switch to large-v3 from the panel after downloading it.
- [ ] Commit: `feat: model switching, download and delete from the panel`.

### Task 5: Benchmark

**Files:** Create `src-tauri/src/benchmark.rs`; modify `commands.rs` (`benchmark_model(id)`), `pipeline.rs` (`Cmd::Benchmark(id)` runs on the pipeline thread while idle, emits `benchmark-progress` and writes `load_ms`/`speed_x`).

**Interfaces:** `pub fn ensure_benchmark_clip(dir) -> Result<PathBuf>` (`say -v Samantha -r 175 -o tmp.aiff -f script` + `afconvert` to 16 kHz WAV, cached as `bench_30s.wav`); `pub fn speed_x(audio_seconds: f32, decode: Duration) -> f32`.

- [ ] Tests: `speed_x_is_audio_over_decode`, `ensure_benchmark_clip_is_cached` (`#[ignore]` on CI, runs `say`). RED → GREEN. Manual: Benchmark button fills the model card. Commit: `feat: per-model benchmark`.

### Task 6: Memory strip and model cards in the panel

**Files:** Modify `src-tauri/src/commands.rs` (`memory_report() -> MemoryReport`), `ui/panel.html`, `ui/privatevox.css`.

- [ ] Rust test: `memory_report_uses_measured_footprint_when_present`. Then the UI: a "Models" tab with the strip (bar with other/whisper/llm segments, legend, three tiles), one card per tier (state, size, Download/Delete/Benchmark/Use buttons, measured numbers), and the header model dropdown bound to `switch_model`. Refresh every 5 s while visible. Manual check.
- [ ] Commit: `feat(ui): memory strip and model cards`.

### Task 7: Chunked decode while speaking and the cap countdown

**Files:** Create `crates/privatevox-core/src/chunking.rs`; modify `vad.rs` (`Vad::score(&mut self, pcm) -> Vec<f32>` per-frame probabilities, 512-sample frames), `src-tauri/src/pipeline.rs`.

**Interfaces:** `pub struct Chunker` with `pub fn new() -> Self`, `pub fn push(&mut self, probs: &[f32], frame_samples: usize, samples_so_far: usize) -> Vec<Range<usize>>` (returns the sample ranges whose cuts became certain), `pub fn finish(&mut self, total: usize) -> Option<Range<usize>>`; `pub fn plan_cuts(segments: &[Range<usize>], total: usize) -> Vec<usize>` (Handy's greedy plan: cut at the best pause 5–40 s after the previous cut, prefer long pauses near 30 s, never leave under 5 s, hard cut at 40 s). Pipeline: while recording, every 500 ms score new audio and feed the chunker; each certain range is decoded on a helper thread (engine lock) and its text appended to an ordered list; at stop, the remainder is decoded and all texts joined with a space; `PillState::Listening` gains `countdown: Option<u32>` shown from 4:30.

- [ ] Tests: `plan_cuts_prefers_long_pause_near_30s`, `short_clip_is_never_cut`, `hard_cut_at_40s_without_pause`, `incremental_push_matches_batch`; pipeline `long_clip_is_decoded_in_order_while_recording` (fake engine returns the chunk index as text; synthetic 90 s clip from the fixture with 1 s gaps) and `cap_countdown_then_stop`. RED → GREEN.
- [ ] Commit: `feat: chunked decode while speaking, cap countdown`.

### Task 8: Hotkey picker and live rebinding

**Files:** Modify `ui/panel.html`, `src-tauri/src/commands.rs` (`validate_combo(combo) -> Result<String,String>` using `Shortcut::parse`), `src-tauri/src/hotkeys.rs` (`reinstall` on `SettingsChanged`), `pipeline.rs`.

- [ ] Tests: `validate_combo_accepts_ctrl_shift_space_and_rejects_bare_modifier`. Then the picker: a "Record key" button captures the next keydown (modifiers + key) in JS and shows the combo; a modifier-only select for the hold key. Saving reinstalls bindings without restart. Manual.
- [ ] Commit: `feat(ui): hotkey picker with live rebinding`.

### Task 9: Undo last paste

**Files:** Create `crates/privatevox-core/src/undo.rs`; modify `src-tauri/src/pipeline.rs` (`Cmd::Undo`), `paste.rs` (`KeySender::undo_chord`), `hotkeys.rs` (binding `Undo`, default `Ctrl+Shift+Z`), `settings.rs` (`undo: HotkeyBinding`).

**Interfaces:** `pub struct LastPaste { pub text: String, pub app: Option<String>, pub at: Instant }`; `pub enum UndoDecision { SendUndo, CopyAgain, Nothing }`; `pub fn decide(last: Option<&LastPaste>, frontmost: Option<&str>, now: Instant, window: Duration) -> UndoDecision`.

- [ ] Tests: `undo_within_window_same_app`, `undo_refused_when_frontmost_changed`, `undo_expired_is_copy_again`, `nothing_without_a_paste`. RED → GREEN. Manual: dictate into TextEdit, press Ctrl+Shift+Z.
- [ ] Commit: `feat: undo last paste`.

### Task 10: History workbench: edit, pin, paste again, export

**Files:** Create `crates/privatevox-core/src/export.rs`; modify `commands.rs` (`update_history_text(id, text)`, `pin_history(id, pinned)`, `paste_text(text)` → `Cmd::PasteText`, `export_history(format) -> PathBuf` to `~/Downloads/privatevox-history-YYYYMMDD-HHMM.{md,json}`), `pipeline.rs`, `ui/panel.html`.

**Interfaces:** `pub fn to_markdown(rows: &[Dictation]) -> String` (one `## <time> · <app>` heading per row, metadata line, text), `pub fn to_json(rows) -> String`.

- [ ] Tests: `markdown_has_one_heading_per_row`, `json_roundtrips_rows`; pipeline `paste_text_pastes_without_recording`. RED → GREEN. Then UI: inline edit (Enter saves, Esc cancels), pin toggle sorted first, Paste again, Export buttons. Manual.
- [ ] Commit: `feat: history workbench with edit, pin, paste again and export`.

### Task 11: Docs

- [ ] Update `README.md` "What works today"; add the panel tabs. Commit: `docs: panel, models, memory, chunking`.
