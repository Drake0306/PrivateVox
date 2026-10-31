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
| 7 | Q8_0 turbo offered from 12 GB | done (31 Oct, PC) | see git log | unit tests; the PC (32 GB) will show it, the 8 GB Mac will not |
| 8 | Crash report file, Show report in Setup | not started | | |
| 9 | First-run walkthrough in Setup | not started | | |
| 10 | Keep the microphone warm | not started | | |
| 11 | Tokens in the OS keychain | not started | | |
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
