# PrivateVox Phase 4b: Extras (Mac scope) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The remaining v1 features that run on the Mac: personal dictionary, spoken languages, kept audio with replay and re-run, receipt-sequenced clipboard restore, secure-input warning, long-form document view, mDNS discovery for hosts, and the cheapest deferred review minors.

**Architecture:** Pure logic in core (`dictionary` with the fuzzy pass, `language` decisions, `audio::write_wav`, `mdns` naming); platform code in the app crate (`paste.rs` gains a lazy pasteboard promise through objc2, `secure_input.rs` polls Carbon, `discovery.rs` wraps `mdns-sd`); the pipeline threads the prompt, language decision and audio save through existing seams; the panel gets a Dictionary card, a languages multi-select, Play and Re-run on rows, a document view, and a "Found on the network" list.

**Tech Stack:** Rust 2021; `strsim` 0.11 (Levenshtein), `mdns-sd` 0.13, `hound` (existing) for WAV, `objc2-app-kit` with the `NSPasteboard` feature, Carbon `IsSecureEventInputEnabled` via `extern "C"`.

**Spec:** `docs/superpowers/specs/2026-10-28-whisper-harness-design.md` §4.3 step 4, §4.5, §6 (`audio_path`), §8.3, §13 v1.x long-form, §18.7, §18.8, §18.11, §18.12.

## Global Constraints

- Dictionary words go to Whisper as the initial prompt; the fuzzy pass (Levenshtein, threshold "strictness" default 0.18, case pattern preserved, punctuation kept) never double-applies. (§18.8)
- One spoken language = fixed decode, no detection; several = detect, restrict to the list, the row records the language and how it was chosen; detection never blocks a transcription. (§18.7)
- Keep audio is off by default; clips are 16 kHz mono WAV under the data dir; `audio_path` on the row; deleting a row deletes its clip. (§4.5, §6)
- Receipt-sequenced restore is the default paste path on macOS: restore only after a real read that happened after the chord, after a 100 ms quiet period, guarded by the change count; bounded by a 3 s timeout whose failure mode is "transcript stays on the clipboard". The disk backup stays. (§18.11)
- Secure input: poll; a one-line pill warning naming the state; the default Right-Option hold is unaffected. (§18.12)
- mDNS service `_privatevox._tcp.local` with TXT `port`, `name`, `version`; discovered hosts are shown with an Add button and never used until added. (§8.3, §8.6)
- No co-author trailers. App tests run with `--test-threads=1`.

## Review Focus

1. A dictionary word that is a prefix of a common word ("Roy" vs "royal"): the fuzzy pass must not rewrite "royal". (Task 1 `fuzzy_pass_leaves_longer_words_alone`)
2. A multi-language user whose clip is in a language not on their list: the decode must still produce text (model's choice), and the row must say so. (Task 2 `detected_language_off_the_list_falls_back_to_the_first`)
3. The target app never reads the pasteboard (paste blocked by a modal): the previous clipboard must come back within the timeout and the transcript must not be lost. (Task 4 `timeout_restores_when_nothing_read`)
4. Keep audio on with a 5-minute dictation: the WAV write must not block the paste. (Task 3: write after the row insert, on the pipeline thread, measured under 50 ms for 5 min of audio — `wav_write_is_fast`)
5. Two hosts advertising the same name on the LAN: the list must show both with their addresses, not merge them. (Task 7 `found_hosts_are_keyed_by_address`)

---

### Task 1: Personal dictionary: prompt and fuzzy safety net

**Files:** Create `crates/privatevox-core/src/dictionary.rs`; Modify `settings.rs` (`dictionary: Vec<String>`, `dictionary_strictness: f32` default 0.18), `lib.rs`, `Cargo.toml` (`strsim = "0.11"`); pipeline: `initial_prompt` from the dictionary, fuzzy pass after normalisation.

**Interfaces:**
- `pub fn initial_prompt(words: &[String]) -> Option<String>`: `None` when empty; else the words joined with ", " (Whisper takes a plain prompt; a glossary sentence biases spelling), capped at 200 tokens ≈ 150 words.
- `pub fn correct(text: &str, words: &[String], strictness: f32) -> String`: for each token (alphanumeric core, punctuation kept), compare case-insensitively against each dictionary word; replace when `levenshtein / max_len <= strictness` and the lengths differ by at most 2 and the token is at least 4 chars; preserve the token's case pattern (all caps → caps, capitalised → capitalised, else the dictionary spelling); also join two adjacent tokens whose concatenation matches a dictionary word ("Charge B" → "ChargeBee"). Exact matches and tokens of a different length class are untouched.
- Tests: `prompt_is_none_when_empty`, `prompt_joins_words`, `fuzzy_fixes_near_misses` ("I used Charge B and Kuber netes" → "I used ChargeBee and Kubernetes"), `fuzzy_pass_leaves_longer_words_alone` ("the royal road" with ["Roy"] unchanged), `case_pattern_preserved` ("CHARGEBE" → "CHARGEBEE"), `punctuation_kept` ("chargebe," → "ChargeBee,"), `exact_words_untouched`, `settings_defaults`.
- Pipeline: `DecodeOptions.initial_prompt = initial_prompt(&settings.dictionary)`; after `normalize_with` run `correct(&text, &settings.dictionary, settings.dictionary_strictness)`; harness test `dictionary_corrects_transcript` (fake engine says "charge b works", dictionary ["ChargeBee"] → pasted "ChargeBee works"; the fake records `opts.initial_prompt == Some("ChargeBee")`).
- Commit: `feat: personal dictionary as Whisper prompt with a fuzzy safety net`.

### Task 2: Spoken languages

**Files:** Create `crates/privatevox-core/src/language.rs`; Modify `settings.rs` (`spoken_languages` already exists; validate ISO codes against a list of Whisper's 99), pipeline, store (`language_source` in `timing_json` — no schema change: store `"language_source": "fixed|detected|fallback"` in timing).

**Interfaces:**
- `pub fn decode_language(spoken: &[String]) -> Option<String>`: `Some(first)` when exactly one; `None` (auto) when several.
- `pub enum LanguageSource { Fixed, Detected, Fallback }`; `pub fn settle(spoken: &[String], detected: Option<&str>) -> (Option<String>, LanguageSource)`: one language → (first, Fixed); several and detected in the list → (detected, Detected); several and detected off the list or missing → (first, Fallback).
- `pub const WHISPER_LANGUAGES: &[(&str, &str)]` (code, English name) for the picker; `pub fn is_known(code) -> bool`.
- Pipeline: decode with `decode_language`; when several languages are set and the transcript's detected language is off the list, re-run that clip with the first language fixed (short-clip path only; chunk workers decode with the first language fixed when several are set, ruling: detection on every chunk is too slow). Row `language` = settled language; timing gets `language_source`.
- Tests: `one_language_is_fixed`, `several_languages_auto_detect`, `detected_language_off_the_list_falls_back_to_the_first`, `validate_rejects_unknown_language_code`; harness `multi_language_rerun_when_detected_is_off_the_list` (fake engine reports "fr" for the first call; spoken ["en","de"] → second decode call with language "en"; timing.language_source == "fallback").
- Panel: Settings → Dictation card: a language multi-select (checkbox list of the 99 with the current ones first), Dictionary textarea (one word per line) with strictness slider.
- Commit: `feat: spoken languages with detection restricted to the list; language picker and dictionary in Settings`.

### Task 3: Keep audio clips, replay, re-run with another model

**Files:** core `audio.rs` (`write_wav_16k(path, pcm)`), `store.rs` (`audio_path` read/write: `set_audio_path`, `Dictation.audio_path`, delete removes the file via the caller), settings `keep_audio: bool`; pipeline saves after insert; commands `read_audio(id) -> base64 wav`, `rerun_history(id, model_id)` → `Cmd::Rerun { id, model }`; panel Play button (`<audio>` from a data URL) and "Re-run with" picker.

**Interfaces:**
- `pub fn write_wav_16k(path: &Path, pcm: &[f32]) -> anyhow::Result<()>` (16-bit PCM mono 16 kHz via hound).
- `Store::set_audio_path(id, path: Option<&str>)`, `Dictation.audio_path: Option<String>`, `COLUMNS` + `map_row` extended.
- `Cmd::Rerun { id, model }`: load the clip from `audio_path`, switch model if needed (deferred switch rules apply), decode, `update_polish(id, mode, None, None, "none")` and `update_raw(id, text)` (new store method), then re-polish with the row's mode when it uses the LLM; `history-changed`; on a missing clip → `Notice("No audio kept for this row")`.
- Tests: core `wav_round_trips`, `wav_write_is_fast` (5 min of silence under 50 ms); store `audio_path_round_trip`; pipeline `keep_audio_saves_a_clip_next_to_the_row` and `rerun_updates_raw_text_from_the_clip` (fake engine says "second pass"); `delete_history_removes_the_clip`.
- Commit: `feat: keep audio clips with replay and re-run from history`.

### Task 4: Receipt-sequenced clipboard restore (macOS)

**Files:** `src-tauri/src/paste.rs` (new `macos_receipt` module), `src-tauri/Cargo.toml` (objc2-app-kit feature `NSPasteboard`).

**Interfaces:**
- `pub struct PromiseOwner` (objc2 `define_class!` NSObject subclass implementing `pasteboard:provideDataForType:`, which writes the transcript as `NSPasteboardTypeString` and records `read_at = Instant::now()` into a shared `Arc<Mutex<Receipt>>`).
- `pub fn paste_with_receipt(text, previous: Option<String>, sender, quiet: Duration /* 100 ms */, timeout: Duration /* 3 s */) -> Result<PasteOutcome>`: declare the promise (`declareTypes_owner`), remember `change_count`, send the chord, then wait: a read after the chord + quiet period → restore `previous` if `changeCount` is unchanged since our declaration; timeout with no read → restore only if `changeCount` unchanged (the chord was likely swallowed; transcript left otherwise); a changed `changeCount` (user copied meanwhile) → never restore.
- Pure decision helper for tests (no AppKit): `pub fn restore_decision(read_after_chord: Option<Instant>, now: Instant, quiet: Duration, deadline: Instant, change_count_now: i64, change_count_ours: i64) -> Decision { Wait, Restore, Leave }` with tests `restores_after_read_plus_quiet`, `waits_while_read_is_fresh`, `timeout_restores_when_nothing_read`, `never_restores_after_a_user_copy`.
- `Paster::paste` uses the receipt path on macOS when `method == Clipboard`; the 150 ms fixed delay stays as the non-macOS path. Manual check: paste into TextEdit, then Cmd+V again shows the old clipboard.
- Commit: `feat(mac): receipt-sequenced clipboard restore`.

### Task 5: Secure-input warning (macOS)

**Files:** Create `src-tauri/src/secure_input.rs`; pipeline `PillState::Idle` gains `warning: Option<String>`; pill shows it; tray tooltip too.

**Interfaces:**
- `pub fn secure_input_on() -> bool` (`IsSecureEventInputEnabled` from Carbon; `false` off macOS).
- `pub fn spawn_watch(tx: Sender<Cmd>)`: every 2 s; on change send `Cmd::SecureInput(bool)`; the coordinator re-emits Idle with `warning = Some("Secure input is on (a password field?). The hold key may not fire; use Ctrl+Shift+Space")` when true.
- Tests: `idle_warning_follows_secure_input_cmd` (harness: `Cmd::SecureInput(true)` → Idle with warning; `false` → none).
- Commit: `feat(mac): secure-input warning in the pill`.

### Task 6: Long-form document view

**Files:** `ui/panel.html`, `ui/privatevox.css`; command `export_row(id, format)`.

**Behaviour:** rows over 600 characters or 60 s show "Open as document"; a full-width view replaces the rows list with the text (paragraphs split at sentence boundaries every ~80 words), word count, Copy all, Export (Markdown file to Downloads via `export_row`), Back. Test: core `paragraphs_split_every_80_words` for the pure splitter in `export.rs`.
- Commit: `feat: long-form document view`.

### Task 7: mDNS discovery for hosts

**Files:** Create `src-tauri/src/discovery.rs`; `Cargo.toml` (`mdns-sd = "0.13"`); host advertises while running; client browses on demand; command `found_hosts() -> Vec<FoundHost { name, address, port, version }>`; panel "Found on the network" list with Add.

**Interfaces:**
- `pub fn advertise(name, port, version) -> Advertiser` (registers `_privatevox._tcp.local.` with TXT `name`, `port`, `version`; unregisters on Drop).
- `pub fn browse(for_: Duration) -> Vec<FoundHost>` (collect resolved services for 2 s, keyed by `address:port`, self excluded by matching `lan_addresses()`).
- Pure helper `pub fn merge(found: Vec<FoundHost>) -> Vec<FoundHost>` dedup by address:port, keep the latest name — test `found_hosts_are_keyed_by_address`.
- Live: with host mode on, `found_hosts()` from the same Mac lists it (self is excluded in the UI but the test allows it with a flag).
- Commit: `feat(net): mDNS advertising and discovery for hosts`.

### Task 8: Deferred minors with user effect

- `remote-changed` emitted when a failed request flips the host unhealthy (remote.rs: compare against the value before the probe *and* after; emit on any transition since the last loop pass).
- Host sessions keyed by `(client, session)`.
- `/v1/history` search honours `offset` and `total` counts the matches.
- Tray mode check marks and the panel header picker both refresh on `mode-changed` (emit from `set_mode_inner`).
- Strip selector: when polish is on but nothing is reachable, show "On · none reachable" instead of "Off"; unhealthy endpoints selectable (they become preferred and take over when they come up).
- Tests for the first three (unit level); UI by reading.
- Commit: `fix: deferred review minors (remote-changed on failure, session keys, history search paging, mode sync, strip selector)`.

### Task 9: Live check, docs, 0.4.0-alpha.1

- Live: dictionary word corrected in a real dictation; two spoken languages set and an English clip decoded; keep audio on → WAV next to the row, Play works; receipt restore: dictate into TextEdit and confirm the old clipboard returns; secure input: open Terminal with Secure Keyboard Entry on → pill warning; mDNS: host on → found list shows this Mac.
- Docs: README sections (dictionary, languages, keep audio, discovery), CHANGELOG 0.4.0-alpha.1, roadmap 4b rows (Mac items done; Windows pass and notarisation remain), ledger with measured numbers.
- Release: version 0.4.0, Mac dmg built here, tag `v0.4.0-alpha.1`, GitHub pre-release with notes and the dmg; CI builds Windows and Linux.
