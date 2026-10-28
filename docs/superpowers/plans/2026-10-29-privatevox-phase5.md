# PrivateVox Phase 5 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The approved phase 5 set from the proposal: auto-stop on silence, per-app default modes, sound and visual cues, live preview while speaking, spoken end-of-dictation commands, the deferred-minors cleanup, and an MLX measurement.

**Architecture:** Pure logic in core (`silence` trailing-silence counter, `appmodes` matcher, `commands` trailing-phrase matcher, preview window selection); the pipeline threads them through `tick`, `start` and the normalisation step; cues are a tiny `cues.rs` in the app crate; the pill learns a second line for the partial; Settings gets an "Dictation behaviour" group (auto-stop, cues, preview, commands) and an "Apps" table.

**Tech Stack:** Rust 2021; macOS `afplay` for cues (no new crate); existing VAD scores; Tauri events; vanilla JS.

**Spec:** `docs/superpowers/specs/2026-10-28-whisper-harness-design.md` §13 v2 and `docs/superpowers/plans/2026-10-29-privatevox-phase5-proposal.md` (A1–A6, B1).

## Global Constraints

- Auto-stop applies to latched (toggle) recordings only; default 2 s of trailing silence; "Never" possible; the pill shows "stopping…" during the last second. (A2)
- Per-app modes: case-insensitive substring match on the frontmost app name, first match wins; the pill's Listening line shows the resolved mode. (A4)
- Cues: off-able; macOS plays system sounds through `afplay` on a helper thread; never on the pipeline thread. (A5)
- Live preview: setting Auto (on when a host is in use or total memory ≥ 16 GB) / On / Off; preview decodes never block a chunk decode (`try_lock` only) and are discarded; the pill shows the last ~70 characters. (A1)
- Spoken commands: only the trailing phrase of a dictation is checked; English phrases first; off-able; "cancel" drops the dictation, "delete that"/"undo" undo the last paste and drop, "new line"/"new paragraph" append a break. (A3)
- No co-author trailers. App tests run with `--test-threads=1`.

## Review Focus

1. Auto-stop must never fire on a held (push-to-talk) recording, only a latched one, and never before any speech was heard. (Task 1 `auto_stop_ignores_held_and_silent_from_start`)
2. A dictation that is only a command ("cancel") must not paste anything and must not store a row. (Task 5 `cancel_alone_pastes_and_stores_nothing`)
3. The preview decode must not steal the engine from the chunk worker: when the engine is busy, the preview skips the tick. (Task 4 `preview_skips_when_engine_busy`)
4. A per-app rule for "Code" must not change the mode of a dictation already in flight when the user switches apps mid-sentence. (Task 2 `mode_resolved_at_start_only`)
5. Cues must not play when the setting is off and must not delay the paste. (Task 3 `cues_are_off_able`)

---

### Task 1: Auto-stop on silence (toggle mode)

**Files:** Create `crates/privatevox-core/src/silence.rs`; Modify `settings.rs` (`auto_stop_silence_secs: Option<u32>` default `Some(2)`, validate 1..=30), pipeline `tick`/`score_and_cut`, `PillState::Listening { stopping: bool }`, pill.html.

**Interfaces:**
- `pub struct TrailingSilence { heard_speech: bool, silent_frames: usize }` with `pub fn new()`, `pub fn push(&mut self, probs: &[f32], threshold: f32)` (speech when p ≥ threshold; resets the counter on speech, sets `heard_speech`), `pub fn silent_for(&self, frame_ms: f32) -> f32` (seconds), `pub fn due(&self, limit_secs: f32, frame_ms: f32) -> bool` (heard speech and silent ≥ limit).
- Pipeline: `Recording.silence: TrailingSilence`; in `score_and_cut` after scoring push the new probs; when the machine stage is `Recording { locked: true }` and `settings.auto_stop_silence_secs` is Some and `due` → `Stop` via `machine.force_stop()` + `self.stop()`; `stopping = silent_for >= limit - 1.0` for the pill.
- Tests: `due_only_after_speech_then_silence`, `speech_resets_the_counter`; pipeline `auto_stop_ignores_held_and_silent_from_start` (held: Key Down Ptt in PTT mode with a fixture of speech+3 s silence → no auto stop; toggle: tap then the same fixture → Stop arrives by itself, row stored) and `auto_stop_never_when_setting_is_none`.

### Task 2: Per-app default modes

**Files:** Create `crates/privatevox-core/src/appmodes.rs`; Modify `settings.rs` (`app_modes: Vec<AppMode { app: String, mode: Mode }>`), pipeline `start`, panel Settings "Apps" table.

**Interfaces:**
- `pub fn resolve(rules: &[AppMode], app_name: Option<&str>, default: Mode) -> Mode` (first rule whose `app` is a case-insensitive substring of the app name).
- Pipeline: `start` resolves the mode once into `self.dictation_mode` (new field) used by Listening, polish and the row; `paste_and_store` uses it instead of `settings.mode`. Tests: core `resolve_matches_substring_case_insensitively`, `no_rule_means_default`; pipeline `mode_resolved_at_start_only` (frontmost "Mail" with a Mail→Email rule → row mode email even though the harness frontmost changes to "Safari" before Stop) and `per_app_rule_shows_in_listening`.
- Panel: Settings → "Apps" card: rows of app name + mode select, add/remove, Save (whole-settings save like the Dictation card).

### Task 3: Sound and visual cues

**Files:** Create `src-tauri/src/cues.rs`; Modify `settings.rs` (`sound_cues: bool` default true), pipeline (`Deps.cue: Arc<dyn Fn(Cue) + Send + Sync>`), lib.rs, pill.css (flash class on state change), panel toggle.

**Interfaces:**
- `pub enum Cue { Start, Stop, NothingHeard, Error }`; `pub fn play(cue: Cue)` (macOS: `afplay /System/Library/Sounds/{Tink,Pop,Basso,Funk}.aiff` on a detached thread; other OSes: no-op).
- Pipeline calls `cue(Start)` on capture start, `cue(Stop)` on stop, `cue(NothingHeard)` on that error; gated by the setting inside the closure in lib.rs so the pipeline stays testable. Tests: pipeline `cues_fire_on_start_stop_and_nothing_heard` (counter closure) and `cues_are_off_able` (the lib.rs gate is pure: `cues::enabled(&settings, cue) -> bool`).

### Task 4: Live preview while speaking

**Files:** Modify `settings.rs` (`live_preview: Preview { Auto, On, Off }` default Auto), pipeline (`PillState::Listening { partial: Option<String> }`, preview decode in `tick`), pill.html (second line), core `memory::total_memory()` helper.

**Interfaces:**
- `pub fn preview_enabled(setting: Preview, host_in_use: bool, total_bytes: u64) -> bool` (Auto → host in use or ≥ 16 GB).
- Pipeline: every 30 ticks (3 s) while recording and enabled: take the last 6 s of the buffer, `engine.try_lock()`; if free and loaded → `transcribe` with `language` fixed and `initial_prompt` from the dictionary; `partial = Some(last 70 chars)`; if locked or not loaded → skip. Chunk results (already decoded text of finished chunks) also feed the partial: `partial = finished chunks joined + " " + window text`, trimmed to 70 chars from the end.
- Tests: core `preview_enabled_rules`; pipeline `preview_shows_partial_text` (setting On, fake engine says "partial words" → a Listening state with `partial == Some("partial words")` within 4 s using a long fixture) and `preview_skips_when_engine_busy` (fake engine with a long `load_delay` holding the lock → no partial, no extra decode calls).

### Task 5: Spoken end-of-dictation commands

**Files:** Create `crates/privatevox-core/src/commands.rs`; Modify `settings.rs` (`spoken_commands: bool` default true), pipeline normalisation step, panel toggle.

**Interfaces:**
- `pub enum SpokenCommand { NewLine, NewParagraph, DeleteThat, Undo, Cancel }`; `pub fn strip_trailing(text: &str) -> (String, Option<SpokenCommand>)` matching the last one to two words case-insensitively with optional trailing punctuation: "new line", "newline", "new paragraph", "delete that", "scratch that", "undo", "undo that", "cancel", "never mind". A command phrase that is the whole text yields an empty string.
- Pipeline (`paste_and_store` entry): if enabled, strip; `Cancel` → emit `Pasted { preview: "Cancelled" }`-style notice and return without pasting or storing; `DeleteThat`/`Undo` → run the existing undo path, then return (the spoken text is dropped); `NewLine`/`NewParagraph` → append `"\n"` / `"\n\n"` to the text and continue.
- Tests: core `strips_each_phrase`, `plain_text_is_untouched`, `phrase_alone_is_empty`; pipeline `cancel_alone_pastes_and_stores_nothing`, `new_line_appends_a_break`, `delete_that_undoes_the_last_paste`.

### Task 6: Deferred-minors cleanup

Fix, each with a test where one is reasonable: host path sends the dictionary prompt (`DictationRequest.initial_prompt`); mDNS `enable_addr_auto` so a Wi-Fi change is advertised; delete waits for / cancels a pending clip write (store `audio_path` only after the file exists and delete removes both); chunked path records `language_source`; secure-input poller only on macOS; `load_wav_16k` doc restored; `openDoc` no double-escape; `loadHistory` fetches models once per load not per keystroke; Re-run can use the host when in use (`use_remote = remote.in_use()`); old `Advertiser` dropped before a port change; `remote_models` not refetched every 3 s (cache 30 s); Rerun picker hidden on hosts without models (done) — and the phase 3 leftovers: notice during a queued decode, re-polish feedback with polish off (done), tray/panel mode sync (done).
Commit: `fix: deferred minors from the phase 3, 4a and 4b reviews`.

### Task 7: MLX measurement (spike, documented only)

- Create a venv under the scratch directory, `pip install mlx-whisper`, convert/download `mlx-community/whisper-large-v3-turbo`, time the five spike clips (`docs/superpowers/spikes/`), compare with the whisper.cpp numbers from 2026-10-28 in a new `docs/superpowers/spikes/2026-10-29-mlx-spike.md`. Decision rule from the proposal: adopt only at ≥ 30 % faster on turbo or large-v3. No app code.

### Task 8: Live check, docs, 0.5.0-alpha.1

- Live: toggle mode with auto-stop (say a sentence, wait, the pill stops itself); a Mail→Email rule with Mail in front; cues audible; preview text appears during a long sentence; "new line" at the end; "cancel" alone.
- Docs: README sections, CHANGELOG, roadmap phase 5 rows, ledger. Version 0.5.0, Mac dmg, tag `v0.5.0-alpha.1`, release with notes; CI builds the rest.
