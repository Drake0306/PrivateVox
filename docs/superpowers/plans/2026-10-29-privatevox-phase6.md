# PrivateVox Phase 6 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Every user-facing item the design doc promises that is not built yet and can be built and verified on the Mac, plus the two things the owner hit on 2026-10-29: the live preview is off by default on this Mac, and the new controls are buried at the bottom of Settings.

**Architecture:** Pure logic stays in `privatevox-core` (hotkey binding, per-app paste rule, model offering, crash report text, warm-mic timing, setup steps); the app crate wires it into the pipeline, tray and commands; the panel gets a reordered Settings tab, history filters, a mic meter and a stepped Setup tab. Every task ends with a unit test and, where the feature is visible, a screenshot check through `scripts/ui-shot.sh` (System Events click + `screencapture`).

**Tech Stack:** Rust 2021, Tauri 2 (`tauri-plugin-autostart` 2.x, `keyring` 3.x), cpal, vanilla JS; `osascript` + `screencapture` for the visual checks.

**Spec:** `docs/superpowers/specs/2026-10-28-whisper-harness-design.md` §4.1, §4.2, §4.4, §4.5, §4.6, §4.7, §4.8, §8.5, §10, §13 item 8, §18.3.

## Global Constraints

- Defaults from the spec: clipboard-only hotkey `Ctrl+Shift+Alt+Space` (§4.2); idle unload default 10 min (§4.5); Q8_0 offered when total memory ≥ 12 GB (§4.6); crash report file `crash-<ts>.txt` (§10).
- Live preview default becomes **On** (owner's call 2026-10-29); Auto keeps its rule for people who pick it.
- No co-author trailers. App tests run with `--test-threads=1`. Panel JS passes `node scripts/check-panel-js.mjs ui/panel.html ui/pill.html`.
- Debug binaries lose macOS Accessibility on every rebuild; visual checks that need a paste use the bundled `.app` from `cargo tauri build --debug` or accept "copied only".

## Review Focus

1. A clipboard-only dictation must never paste and must leave the undo target untouched. (Task 2 `copy_only_dictation_is_copied_not_pasted`)
2. "Clear all" must delete kept clips with the rows and never touch a host's history. (Task 4 `clear_all_removes_rows_and_clips`)
3. A per-app paste rule is resolved from the app in front at paste time, not at start. (Task 6 `paste_method_follows_the_app_in_front_at_paste_time`)
4. The mic test must refuse to start while a dictation is recording and must stop itself after 10 s. (Task 5 `mic_test_refuses_while_recording_and_stops_itself`)
5. A Q8_0 model is never recommended or auto-selected on a machine under 12 GB, but a file the user downloaded elsewhere still loads. (Task 7 `q8_is_hidden_below_12gb_unless_present`)

---

### Task 1: Preview on by default, Settings tab reordered, screenshot helper

**Files:** Modify `crates/privatevox-core/src/settings.rs` (`live_preview` default `Preview::On`), `crates/privatevox-core/src/preview.rs` tests, `ui/panel.html` (Settings tab order), `ui/privatevox.css`; Create `scripts/ui-shot.sh`.

**Interfaces:**
- Produces: `scripts/ui-shot.sh <tab> <out.png>` — brings the panel to the front, clicks the named tab (History/Models/Polish/Network/Settings/Setup), optional `--page-down N`, captures and crops the panel to `out.png`. Used by every later task's visual step.
- Settings tab order becomes: **Dictation behaviour** card first (live preview, auto-stop, spoken commands, sound cues, keep audio), then **Keys and activation**, then **Modes per app**, then **Paste**, then **Languages and dictionary** with the language list collapsed behind a "Choose languages (N selected)" button.

- [ ] Test `settings::tests::live_preview_defaults_on` asserts `Settings::default().live_preview == Preview::On` and `preview_enabled(Preview::On, false, 8 GB)`.
- [ ] Run `cargo test -p privatevox-core --lib live_preview_defaults_on` → FAIL (default is Auto).
- [ ] Change the default; run → PASS. Run the whole core suite.
- [ ] Reorder the Settings tab in `ui/panel.html`; language list in a `<details>` whose summary shows the count. `node scripts/check-panel-js.mjs ui/panel.html ui/pill.html` → ok.
- [ ] Write `scripts/ui-shot.sh`; run `scripts/ui-shot.sh Settings /tmp/settings.png` with the app running and read the image: the Dictation behaviour card is the first thing under the tabs.
- [ ] Commit `feat: live preview on by default; Settings tab leads with dictation behaviour; ui-shot helper`.

### Task 2: Clipboard-only dictation hotkey (§4.2)

**Files:** Modify `crates/privatevox-core/src/hotkeys.rs` (`Binding::CopyOnly`, `Stage::Recording { copy_only: bool, .. }`), `settings.rs` (`copy_only: HotkeyBinding`, default combo `Ctrl+Shift+Alt+Space`), `src-tauri/src/hotkeys.rs` (bind it), `src-tauri/src/pipeline.rs` (`Coordinator.copy_only`, set from `Effect::Start { copy_only }`, `paste_and_store` calls `paste.copy_only(&text)` and stores status `copied_only` with preview `Copied: …` when set), `ui/panel.html` (Settings → "Copy-only key" with Record key).

**Interfaces:**
- `Binding::CopyOnly` behaves as Toggle (tap start, tap stop) in `Machine::on`; `Effect::Start { copy_only: bool }` replaces the bare `Effect::Start`.
- `Cmd::Key(Input::Down(Binding::CopyOnly))` from the global-shortcut path only (combos, never a bare modifier).

- [ ] Test `hotkeys::tests::copy_only_binding_toggles_and_marks_the_recording` (core): Down(CopyOnly) → `Effect::Start { copy_only: true }`, stage `Recording { copy_only: true, .. }`, second Down → `Effect::Stop`.
- [ ] Test `pipeline::tests::copy_only_dictation_is_copied_not_pasted` (app): send `Cmd::Key(Input::Down(Binding::CopyOnly))` twice with `engine_saying("secret")`; `h.pasted` empty, `FakePaste.copied` == ["secret"], row status `copied_only`, and a following `Cmd::Undo` yields "Nothing to undo" (undo target untouched).
- [ ] Run both → FAIL (variant missing).
- [ ] Implement; run both → PASS; `cargo test -p privatevox-core --lib hotkeys` and the app suite green.
- [ ] Panel: "Copy-only key" field + Record key in the Keys card; lint ok; `scripts/ui-shot.sh Settings` shows it.
- [ ] Commit `feat: clipboard-only dictation hotkey (Ctrl+Shift+Alt+Space)`.

### Task 3: Launch at login, pill hide/show, tray model picker and Resume (§4.1, §4.5, §4.7)

**Files:** Modify `src-tauri/Cargo.toml` (`tauri-plugin-autostart = "2"`), `src-tauri/src/lib.rs` (plugin init; `apply_pill_visibility(app, show)`), `src-tauri/src/tray.rs` (CheckMenuItem `login` "Launch at login", CheckMenuItem `pill` "Show pill", Submenu "Model" from `model_entries`, MenuItem `resume` "Load model now"), `src-tauri/src/commands.rs` (`set_launch_at_login`, `set_show_pill`), `settings.rs` (`launch_at_login: bool` default false, `show_pill: bool` default true), `ui/panel.html` (two checkboxes in the Keys and activation card).

**Interfaces:**
- `tray::model_entries(models: &[ModelRow], current: &str) -> Vec<TrayModelEntry { id: String, label: String, checked: bool, enabled: bool }>` — pure; `enabled` = downloaded.
- `Cmd::Resume` → the pipeline preloads the model (same path as the first dictation's preload) without recording.

- [ ] Test `tray::tests::model_entries_check_current_and_disable_missing` (pure).
- [ ] Test `pipeline::tests::resume_loads_the_model_without_recording` (`Cmd::Resume` → `load_calls == 1`, no row, pill ends Idle).
- [ ] Test `settings::tests::login_and_pill_defaults`.
- [ ] Run → FAIL; implement; → PASS; suites green.
- [ ] Visual: tray menu listed with `osascript` shows "Launch at login", "Show pill", "Model ▸", "Load model now"; unticking "Show pill" hides the pill (screenshot), ticking shows it.
- [ ] Commit `feat: launch at login, pill show/hide, tray model picker and Load model now`.

### Task 4: History filters and Clear all (§4.4)

**Files:** Modify `crates/privatevox-core/src/store.rs` (`list_filtered(app: Option<&str>, mode: Option<&str>, limit, offset)`, `distinct_apps()`, `clear_all() -> Vec<Option<String>>` returning the audio paths it removed rows for), `src-tauri/src/commands.rs` (`list_history` gains optional `app`, `mode`; `history_apps`; `clear_history` deletes the clips returned), `ui/panel.html` (footer: app select, mode select, "Clear all…" with inline Confirm/Cancel).

- [ ] Test `store::tests::list_filtered_by_app_and_mode` and `clear_all_removes_rows_and_clips` (core: insert 3 rows with 2 clips in a tempdir, `clear_all` → count 0, both files gone; a remote history source is untouched because the command only touches the local store).
- [ ] Run → FAIL; implement; → PASS.
- [ ] Panel footer + lint; `scripts/ui-shot.sh History` shows the two selects and the button; clicking Clear all shows Confirm.
- [ ] Commit `feat: history filters by app and mode; clear all with confirm`.

### Task 5: Mic level test (§4.5)

**Files:** Modify `src-tauri/src/pipeline.rs` (`Cmd::MicTest(bool)`, `Deps.on_mic_level: Arc<dyn Fn(Option<f32>) + Send + Sync>` — `Some(db)` every 100 ms, `None` when the test ends), `src-tauri/src/lib.rs` (emits `mic-level`), `src-tauri/src/commands.rs` (`mic_test(on: bool)`), `ui/panel.html` (Test button + meter beside Microphone).

- [ ] Test `pipeline::tests::mic_test_refuses_while_recording_and_stops_itself`: with a recording in progress `Cmd::MicTest(true)` emits `None` immediately and does not touch the capture; when idle it emits ≥ 5 `Some(_)` levels in 700 ms and a `None` after `MicTest(false)`; with no stop it ends by itself after 10 s (use a 1 s limit through `Coordinator.mic_test_limit` set by the test).
- [ ] Run → FAIL; implement; → PASS.
- [ ] Panel: meter animates for a clap near the mic (screenshot during the test).
- [ ] Commit `feat: microphone level test in Settings`.

### Task 6: Paste method per app (§4.3, §4.5)

**Files:** Create `crates/privatevox-core/src/pastemethod.rs` (`pub struct AppPaste { app: String, method: PasteMethod }`, `pub fn resolve(rules: &[AppPaste], app: Option<&str>, default: PasteMethod) -> PasteMethod`, case-insensitive substring, first match wins); Modify `settings.rs` (`app_paste_methods: Vec<AppPaste>`), `src-tauri/src/pipeline.rs` (`PasteSink::paste(&mut self, text: &str, method: PasteMethod)`; resolved from `(self.deps.frontmost)()` at paste time), `src-tauri/src/adapters.rs`, `ui/panel.html` (table under Paste method, same widget as Modes per app).

- [ ] Test `pastemethod::tests::resolve_matches_substring_first_wins` (core) and `pipeline::tests::paste_method_follows_the_app_in_front_at_paste_time` (frontmost "Mail" at start, "Terminal" at paste with a Terminal → Type rule; `FakePaste.methods.last() == Type`).
- [ ] Run → FAIL; implement; → PASS.
- [ ] Panel table + lint + screenshot.
- [ ] Commit `feat: paste method per app`.

### Task 7: Q8_0 model variants (§4.6)

**Files:** Modify `crates/privatevox-core/src/models.rs` (`ModelInfo.offered_from_bytes: u64`; new entry `large-v3-turbo-q8` → `ggml-large-v3-turbo-q8_0.bin`, 874_188_075 bytes, quant `Q8_0`, offered from 12 GB; `pub fn offered(info: &ModelInfo, total_bytes: u64, present: bool) -> bool`), `memory.rs` (`estimate_footprint` for the new id), `src-tauri/src/commands.rs` (`list_models` filters with `offered`), `ui/panel.html` (Models tab shows the quant chip).

- [ ] Test `models::tests::q8_is_hidden_below_12gb_unless_present`: 8 GB → not offered; 16 GB → offered; 8 GB with the file present → offered; `large-v3-turbo` offered at 8 GB.
- [ ] Test `memory::tests::q8_footprint_is_between_q5_and_f16`.
- [ ] Run → FAIL; implement; → PASS.
- [ ] Commit `feat: Q8_0 turbo offered on machines with 12 GB or more`.

### Task 8: Crash report and "Show report" (§10)

**Files:** Create `src-tauri/src/crash.rs` (`pub fn report_text(version: &str, os: &str, model: &str, last_state: Option<&str>, panic: &str) -> String`, `pub fn write(dir: &Path, text: &str) -> PathBuf` naming `crash-<unix ts>.txt`, `pub fn latest(dir: &Path) -> Option<PathBuf>`); Modify `src-tauri/src/main.rs` or `lib.rs` (`std::panic::set_hook` calling `crash::write` with the log dir, then the default hook), `commands.rs` (`permissions` reply gains `crash_report: Option<String>`; `open_path(path)`), `ui/panel.html` (Setup: "The last session ended unexpectedly" row gets "Show report").

- [ ] Test `crash::tests::write_and_latest_pick_the_newest` and `report_text_has_version_os_model_state`.
- [ ] Run → FAIL; implement; → PASS.
- [ ] Commit `feat: crash report file on panic, shown from Setup`.

### Task 9: First-run walkthrough (§4.8)

**Files:** Modify `src-tauri/src/commands.rs` (`setup_status` → `SetupStatus { steps: Vec<SetupStep { id, label, state: "done"|"todo"|"optional", detail }> }` built by `setup::steps(perms: &Permissions, settings: &Settings, llm: &LlmStatusLite) -> Vec<SetupStep>` in Create `src-tauri/src/setup.rs`), `ui/panel.html` (Setup tab renders numbered steps: model, microphone (with the Task 5 test), accessibility, input monitoring, hotkey (shows the current key, "Change" switches to Settings), polish LLM (detected endpoints, "Add LAN endpoint" switches to Polish), "Say something" (Record/Stop tools) — each with its state).

- [ ] Test `setup::tests::steps_mark_done_and_todo` (all granted + model present + LLM reachable → all done; nothing granted → model and permissions todo, LLM optional).
- [ ] Run → FAIL; implement; → PASS; lint; `scripts/ui-shot.sh Setup` shows the numbered list.
- [ ] Commit `feat: first-run walkthrough in Setup`.

### Task 10: Keep the microphone warm (§18.3)

**Files:** Create `crates/privatevox-core/src/warmmic.rs` (`pub struct WarmMic { close_at: Option<Instant> }`, `pub fn on_stop(&mut self, now: Instant, lazy: Duration)`, `pub fn on_start(&mut self) -> Reuse|Open`, `pub fn due(&self, now) -> bool`); Modify `src-tauri/src/adapters.rs` (`MicCapture::stop` keeps the stream and pauses delivery; a timer thread closes it when `due`; `start` reuses an open stream), `settings.rs` (`mic_lazy_close_secs` default 20, validated 0..=300, 0 = close at once), `ui/panel.html` (field in Keys and activation).

- [ ] Test `warmmic::tests::stream_is_reused_within_the_window_and_closed_after` (core).
- [ ] Run → FAIL; implement; → PASS. Live: two dictations 5 s apart log `mic: reused warm stream` for the second.
- [ ] Commit `feat: microphone stays open for a few seconds after a dictation`.

### Task 11: Keychain tokens (§8.5)

**Files:** Create `src-tauri/src/secrets.rs` (`pub trait SecretStore { fn get(&self, key: &str) -> Option<String>; fn set(&self, key: &str, value: &str) -> Result<()>; fn delete(&self, key: &str) -> Result<()> }`, `KeyringStore` on `keyring` 3.x service `com.drake0306.privatevox`, `MemoryStore` for tests; `pub fn stash(settings: &mut Settings, store: &dyn SecretStore)` moves `host.token`, `remote.token`, every `llm_endpoints[i].token` into the store under `host`, `remote`, `endpoint:<id>` and leaves `Some("@keychain")` in the struct; `pub fn unstash(settings: &mut Settings, store: &dyn SecretStore)` fills them back); Modify `commands.rs` (`apply_settings` stashes before `save`, `get_settings`/startup unstash).

- [ ] Test `secrets::tests::tokens_leave_settings_json_and_come_back` with `MemoryStore`.
- [ ] Run → FAIL; implement; → PASS. Note in README: an unsigned debug build is asked by macOS for keychain access after every rebuild; the bundled app is asked once.
- [ ] Commit `feat: host, remote and endpoint tokens live in the OS keychain`.

### Task 12: macOS in the release CI (§13 item 8)

**Files:** Modify `.github/workflows/release.yml` (matrix entry `macOS (Apple Silicon)`, `os: macos-14`, `target: aarch64-apple-darwin`; the Linux/Windows-only steps keep their `if:`).

- [ ] Edit; `gh workflow view` parses; verified on the 0.6.0 tag in Task 13 (the dmg is attached by CI; the local build remains the checksum source until then).
- [ ] Commit `ci: build the macOS dmg on a macos-14 runner`.

### Task 13: Live check with screenshots, docs, roadmap status table, 0.6.0-alpha.1

- Live (screenshots in `docs/superpowers/plans/2026-10-29-privatevox-phase6-ledger.md`): Settings order; copy-only dictation; tray items; pill hide/show; history filters + clear all; mic meter; per-app paste; Q8 hidden on this 8 GB Mac; crash report (`PRIVATEVOX_DEBUG_SCRIPT=panic`); Setup steps; warm mic log line.
- Docs: README (features, developer note on Accessibility and debug builds), CHANGELOG, release notes `docs/releases/v0.6.0-alpha.1.md`.
- Roadmap `docs/privatevox-roadmap.html`: a **Feature status** section listing every design-doc item with one of: built + verified live, built + unit-tested only, pending (needs the PC), pending (needs an account), not planned; the Windows elevated-target detection (§12) is listed under "needs the PC".
- Version 0.6.0; tag `v0.6.0-alpha.1`; release with notes; CI attaches all installers including the dmg.
