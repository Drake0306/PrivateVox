# Phase 6 ledger — progress tracker

Plan: `docs/superpowers/plans/2026-10-29-privatevox-phase6.md` (13 tasks). This file is the one to open to see where phase 6 stands; it is updated after every task. The wider map is `resume.conversation.md`.

Machines: Mac M1 8 GB (tasks 1–3, 29 Oct) and the Windows PC with an AMD GPU and 32 GB (from 31 Oct). The Windows build uses whisper.cpp's Vulkan backend, which runs on any recent AMD, NVIDIA or Intel GPU; nothing is tied to one card.

## Status

| # | Task | State | Commit | Verified |
|---|------|-------|--------|----------|
| 1 | Live preview on by default, Settings reordered, `ui-shot.sh` | done | `23cd92e` | screenshot on the Mac |
| 2 | Clipboard-only dictation hotkey `Ctrl+Shift+Alt+Space` | done | `5b09b5b` | unit tests; live on the Mac |
| 3 | Launch at login, pill show/hide, tray model picker, Load model now | done, tray check pending | `d160207` | unit tests green; the tray menu was not seen open by eye yet (Mac scripting failed). Check by hand on either machine. |
| 4 | History filters by app and mode, Clear all with confirm | done (31 Oct, PC) | `95e79b5` | unit tests (core + app); panel not yet seen running on the PC |
| — | Windows pass, part 1: worker parent watch, benchmark clip, build wrapper, Unix-only tests | done (31 Oct, PC) | `e7059a2` | all three suites green on Windows |
| 5 | Microphone level test | done (31 Oct, PC) | `d64f713` | unit test; not yet watched with a real microphone |
| 6 | Paste method per app | done (31 Oct, PC) | `249942a` | unit tests (core + app) |
| 7 | Q8_0 turbo offered from 12 GB | done (31 Oct, PC) | `43b17ef` | unit tests; the PC (32 GB) will show it, the 8 GB Mac will not |
| 8 | Crash report file, Show report in Setup | done (31 Oct, PC) | `ce1f6b3` | unit tests; live check with `PRIVATEVOX_DEBUG_SCRIPT=panic` pending |
| 9 | First-run walkthrough in Setup | done (31 Oct, PC) | `aaa0e82` | unit tests; Setup tab not yet seen running |
| 10 | Keep the microphone warm | done (31 Oct, PC) | `e3ed33a` | core unit test; the `mic: reused warm stream` log line not yet seen live |
| 11 | Tokens in the OS keychain | done (31 Oct, PC) | see git log | unit tests with the memory store; real Credential Manager / Keychain round trip not yet watched |
| 12 | macOS in the release CI | not started | | |
| 13 | Live check, docs, roadmap status table, 0.6.0-alpha.1 | not started | | |

## Task notes

### Task 4 — History filters and Clear all (31 Oct, Windows PC)

- Core: `Store::list_filtered(&HistoryFilter { app, mode, query }, limit, offset)` composes the three filters with the search box (the plan listed app and mode only; the query joined so a search can be narrowed by app). `Store::distinct_apps()` lists app names most dictated first. `Store::clear_all()` soft-deletes every visible row in one transaction, clearing `audio_path` first, and returns the clip paths for the caller to delete.
- App: `list_history` gained optional `app`, `mode`, `query`; new commands `history_apps` and `clear_history` (returns the number of clip files removed). `clear_rows_and_clips` tolerates clips that already vanished. The host's history is untouched: the command only knows the local store and the panel hides the bar while the host's list is shown.
- Panel: a bar above the rows with "Show: All apps / All modes", a "N of M" count when narrowed, and "Clear all…" that turns into an inline "Delete all / Cancel" confirm. Pinned rows are cleared too.
- Tests: `store::tests::list_filtered_by_app_and_mode`, `store::tests::clear_all_removes_rows_and_returns_clips`, `commands::clip_tests::clear_all_removes_rows_and_clips`.

### Task 5 — Microphone level test (31 Oct, Windows PC)

- Pipeline: `Cmd::MicTest(bool)` and `Cmd::MicTestTick(gen)`; `Deps.on_mic_level(Option<f32>)` gets a dBFS level every 100 ms and one `None` when the test ends. Refused with an immediate `None` while a dictation is recording; a dictation that starts mid-test ends the test first; the test ends itself after `MIC_TEST_LIMIT` (10 s; the harness sets 1 s through `Coordinator.mic_test_limit`). A capture error mid-test posts a pill notice and ends the test.
- App: `mic_test(on)` command; `mic-level` event (number or `null`).
- Panel: Test/Stop button and a meter beside the Microphone picker; the bar maps −60…0 dBFS to 0…100 % and turns red above −6 dBFS.
- Test: `pipeline::tests::mic_test_refuses_while_recording_and_stops_itself` (refusal, ≥5 levels in 700 ms, end marker on stop, nothing after stop, self-stop at the limit, no history row). Not yet watched live with a real microphone.

### Task 6 — Paste method per app (31 Oct, Windows PC)

- Core: `pastemethod::AppPaste { app, method }` and `resolve(rules, app, default)` (case-insensitive substring, first match wins, blank patterns never match); `Settings.app_paste_methods` (old settings files load with an empty table).
- App: `PasteSink::paste(text, method)`; the coordinator resolves the method from the app in front **at paste time** (Review Focus 3), for dictations and for "Paste again". Side effect worth knowing: the Paste method setting itself is now read per paste too, so changing it in Settings applies to the next paste. Before, `Paster` was built once at startup with the setting, so a change needed a restart despite the "applies without restart" note in the panel.
- Panel: "Paste method per app" card under Modes per app, same row widget (Add rule, ↑, Remove, Save rules).
- Tests: `pastemethod::tests::resolve_matches_substring_first_wins`, `settings_default_has_no_rules_and_old_files_load`, `pipeline::tests::paste_method_follows_the_app_in_front_at_paste_time` (Mail at start, Terminal at paste → Type; setting flipped to Type → next paste types).

### Task 7 — Q8_0 turbo (31 Oct, Windows PC)

- Core: `download::TURBO_Q8_0` (`ggml-large-v3-turbo-q8_0.bin`, 874,188,075 bytes, whisper.cpp HF repo); catalog entry `large-v3-turbo-q8` with `offered_from_bytes` = 12 GB; `models::offered(info, total, present)` and `offered_catalog(dir, total)`; `estimate_footprint` 900 MB. Q8 is **not** in `memory::TIERS`, so it is never Recommended or Best-that-fits: an explicit pick (Review Focus 5).
- App: `list_models` and the tray's Model submenu list `offered_catalog`, so the entry appears only on machines with ≥ 12 GB or where the file already exists (a file downloaded elsewhere still loads and can be deleted). `ModelRow.estimated_bytes` added so the panel has a size for models outside the tier table.
- Panel: "Q8 · higher precision" badge on the card; short name `turbo q8` in rows.
- Tests: `models::tests::q8_is_hidden_below_12gb_unless_present`, `catalog_has_four_entries_with_spec_sizes`, `memory::tests::q8_footprint_is_between_q5_and_f16`. The host's `/v1/models` still lists the full catalog (a client switching the host's model sees everything the host could download); left as is.

### Task 8 — Crash report (31 Oct, Windows PC)

- `src-tauri/src/crash.rs`: `report_text` (version, OS, model, last pill state, panic message), `write` (`crash-<unix ts>.txt`, a suffix when two land in the same second), `latest`, `install_hook` (panic hook that writes the report, then the default hook). Installed in setup with the log directory (`app_log_dir`, now carried in `AppPaths.log_dir`), reading the model id and the last pill state at panic time.
- `permissions` reply gains `crash_report` (path of the newest report); new command `open_crash_report` opens it with the OS opener (`cmd /C start`, `open`, `xdg-open`). Deviation from the plan: no general `open_path(path)` command; the app only ever opens its own newest report, so nothing in the webview can ask the OS to open an arbitrary file.
- Panel Setup: "Show report" on the stale-lock line when a report exists; a quieter line with the same button when a report exists without a stale lock. Debug script action `panic` for the live check (`PRIVATEVOX_DEBUG_SCRIPT=panic`: the helper thread panics, the app stays up, a report appears).
- Found and fixed on the way: `AppPaths::model_file` built the file name as `ggml-<id>-q5_0.bin`, wrong for distil (`ggml-distil-large-v3.bin`) and for the new Q8 entry; it now asks the catalog. Before, a restart with distil selected started without a model path.
- Tests: `crash::tests::report_text_has_version_os_model_state`, `write_and_latest_pick_the_newest`, `os_label_names_the_platform`.

### Task 9 — First-run walkthrough (31 Oct, Windows PC)

- `src-tauri/src/setup.rs`: `steps(&SetupInputs)` → numbered `SetupStep { id, label, state: done|todo|optional, detail, action }`: model (Download), microphone (Test, the Task 5 meter; done once a dictation exists), Accessibility and Input Monitoring (macOS only, Open setting), dictation key (shows `hotkey_hint`, Change → Settings), polish LLM (done / todo when on but nothing answers / optional when off), say something (Record / Stop; done once History has a row).
- Command `setup_status` gathers permissions, settings, the polisher's selected endpoint, the history count and the OS. `open_privacy` on Windows opens `ms-settings:privacy-microphone` for the microphone and refuses the two macOS-only panes; Linux has no pane.
- Panel: the Setup card is a rendered list with a summary line ("N steps left before the first dictation"); the macOS "quit and start again" note shows only while a macOS grant is missing. The walkthrough refreshes on `history-changed` while visible. Tools card unchanged.
- Tests: `setup::tests::steps_mark_done_and_todo`, `windows_and_linux_skip_the_macos_permissions`, `microphone_step_names_the_chosen_device`.

### Task 10 — Keep the microphone warm (31 Oct, Windows PC)

- Core: `warmmic::WarmMic` with `on_start() -> Reuse|Open`, `on_stop(now, lazy) -> Keep|Close`, `due(now)`, `mark_closed()`. `Settings.mic_lazy_close_secs` existed unused since phase 1 (default 60); the default is now 20 as the plan says and it is validated 0..=300 (0 = close at once, the old behaviour).
- App: `MicCapture` keeps the cpal stream open after a stop with delivery paused (the callbacks forward into a swappable sink), reuses it on the next start (`mic: reused warm stream` in the log), and a timer thread closes it when the window passes (`mic: closed (20 s idle)`). A newer stop re-arms the window (generation counter); a changed input device forces a fresh open; the stream, the device and the timing state sit behind one lock so the timer and a start cannot race. The mic test (Task 5) goes through the same adapter and benefits too.
- Panel: "Keep the microphone open after a dictation (s, 0–300)" in Keys and activation.
- Tests: `warmmic::tests::stream_is_reused_within_the_window_and_closed_after`, `zero_window_closes_at_once`, `settings::tests::validate_bounds_mic_lazy_close`. Known trade-off (as in Handy): the OS microphone indicator stays on for the window after a dictation.

### Task 11 — Tokens in the OS keychain (31 Oct, Windows PC)

- `src-tauri/src/secrets.rs`: `SecretStore` trait with `KeyringStore` (keyring 3: macOS Keychain, Windows Credential Manager; service `com.drake0306.privatevox`, keys `host`, `remote`, `endpoint:<id>`), `MemoryStore` (tests) and `NoStore` (Linux, until the secret service is wired; tokens stay in the file there). `stash` moves the tokens out and leaves `@keychain`; a store that refuses leaves the token in the file with a warning rather than losing it; a cleared token deletes its entry; `unstash` fills them back, a placeholder without an entry becomes "no token" with a warning.
- Every settings write in the app goes through `secrets::save` (apply_settings, the post-switch save, the pill-position save) and the startup load through `secrets::load`, so in memory the settings are always whole and only the file is redacted. `get_settings` to the panel is unchanged (the panel still shows the real token in its field).
- README: the macOS note about unsigned debug builds and keychain prompts.
- Tests: `secrets::tests::tokens_leave_settings_json_and_come_back`, `a_store_that_refuses_keeps_the_token_in_the_file`, `stash_is_idempotent_on_already_stashed_settings`. Known gap: an endpoint removed from the list leaves its entry in the keychain (harmless; cleaned up when the same id is re-added).

## Windows PC notes (31 Oct)

- Toolchain found: Rust 1.95, Visual Studio 2022 Build Tools, CMake 3.31, Ninja, Node 22, Ollama (AMD AI bundle). Installed this session: Vulkan SDK 1.4.363 (`winget install KhronosGroup.VulkanSDK`), LLVM (`winget install LLVM.LLVM`, bindgen needs `libclang.dll`), Tauri CLI 2.12.4. Target dir `D:\t` (short path, 140 GB free).
- `scripts/win-cargo.ps1 <cargo args>` wraps cargo in the same environment CI uses: vcvars64 imported, `CMAKE_GENERATOR=Ninja`, `CARGO_TARGET_DIR=D:\t`, `VULKAN_SDK`, `LIBCLANG_PATH`. Gotcha found on this PC: a user-level `CMAKE` variable pointing at CMake's *bin directory* makes the cmake crate fail with "Access is denied"; the wrapper repoints it at `cmake.exe`.
- First Windows compile of the workspace with the Vulkan backend: core builds and its suite is 211 green plus one test that assumed port 1 refuses connections. On Windows the firewall drops that port silently, so the client reported Timeout instead of Unreachable; the test now binds and releases a port and connects to that (refused at once on every OS). No product change: both errors mean "host down" to the failover.
- Windows gaps found by the first full test run and fixed the same day:
  - **Worker outlived a dead app.** The worker's parent watch used `getppid()`, Unix only, so on Windows a force-quit or crash of the app left the worker (and its GPU memory) alive until the next stdin read. The supervisor now passes its pid in `PRIVATEVOX_PARENT_PID`; on Windows the worker opens that process and waits on the handle, exiting the moment it signals. `tests/supervisor.rs` runs on both OSes now (`tasklist`/`taskkill` instead of `pgrep`/`kill`; a PowerShell throwaway parent instead of bash) and the "parent dies mid-decode" case passes on the PC.
  - **Benchmark had no clip on Windows.** `ensure_benchmark_clip` was macOS-only (`say` + `afconvert`); it now uses Windows Speech (SAPI through Windows PowerShell) writing 16 kHz mono 16-bit WAV directly, no console window. Linux still has no generator.
  - One pipeline test gave the host fallback 800 ms; on Windows the dead host takes the full 1 s connect timeout, so the test polls for the paste for up to 4 s instead.
- Suites on the PC after the fixes: core 213 (2 ignored), engine 14 + 7 supervisor, app 126 (5 ignored), all green.
- The §2 PC checklist from `resume.conversation.md` (install the 0.5.0 build, hold key and paste, host mode PC→Mac, Vulkan timing) still needs a person at the machine.
