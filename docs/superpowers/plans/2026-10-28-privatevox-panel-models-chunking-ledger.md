# SDD ledger — plan: docs/superpowers/plans/2026-10-28-privatevox-panel-models-chunking.md

Ruling: executing on main, no worktree — same reasons as before; cost if wrong: broken main between tasks.
Pre-flight: T1 LoadInfo consumed by T4 (mem_bytes); T2 MemoryReport consumed by T6 command; T3 catalog consumed by T4/T6; T7 Chunker consumed by pipeline; T9 undo decide consumed by pipeline; T10 export consumed by commands. Names consistent.
Task 1: real worker reports 703 MB resident for large-v3-turbo q5_0 (spike estimated 550 MB from process RSS of the bench binary); the measured number is what the strip will show.
Task 1: complete (commits 891054e..7b47c3b, tests: bash -c 'CARGO_TARGET_DIR=target/lib cargo test -p privatevox-engine -- --test-threads=1 2>&1 | grep "test result" | grep -c "ok\."' → 5)
Task 2: Ruling: sysinfo pinned to 0.37 (0.39.6 needs rustc 1.95, installed 1.93.1) - cost if wrong: none, same API.
Task 2: complete (commits 7b47c3b..d32de62, tests: bash -c 'CARGO_TARGET_DIR=target/lib cargo test -p privatevox-core memory 2>&1 | grep "test result" | head -1' → test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 52 filtered out; finished in 0.00s)
Task 3: complete (commits d32de62..e2b412d, tests: bash -c 'CARGO_TARGET_DIR=target/lib cargo test -p privatevox-core models store 2>&1 | grep "test result" | head -1; CARGO_TARGET_DIR=target/lib cargo test -p privatevox-core store 2>&1 | grep "test result" | head -1' → test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 55 filtered out; finished in 0.02s)
Task 4: manual switch check deferred to Task 6 (no panel UI for it yet).
Task 4: complete (commits e2b412d..68f96aa, tests: bash -c 'cargo test -p privatevox switch 2>&1 | grep "test result" | head -1' → test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 17 filtered out; finished in 0.65s)
Task 5: complete (commits 68f96aa..2490106, tests: bash -c 'cargo test -p privatevox benchmark 2>&1 | grep "test result" | head -1' → test result: ok. 2 passed; 0 failed; 1 ignored; 0 measured; 20 filtered out; finished in 0.49s)
Task 6: manual check of the Models tab is for the owner (no click automation); commands and report logic are unit-tested.
Task 6: complete (commits 2490106..5220ac4, tests: bash -c 'cargo test -p privatevox memory_report 2>&1 | grep "test result" | head -1' → test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 23 filtered out; finished in 0.02s)
Task 7: Ruling: a live cut is final only once the whole 40 s window plus 5 s is scored (not 5 s past the cut), so live chunks equal the batch plan exactly; first chunk decodes 45 s into a long dictation - cost if wrong: later first-chunk start than Handy may achieve.
Task 7: Ruling: one ordered decode worker thread per recording (chunks must be numbered in execution order); the whole clip is rescored every 500 ms since whisper-rs 0.16 has no incremental VAD.
Task 7: complete (commits 5220ac4..73d753e, tests: bash -c 'cargo test -p privatevox long_clip cap_countdown 2>&1 | grep "test result" | head -1; CARGO_TARGET_DIR=target/lib cargo test -p privatevox-core chunking 2>&1 | grep "test result" | head -1' → test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 64 filtered out; finished in 0.01s)
Task 8: manual picker check for the owner; validation and rebinding path unit-tested; undo binding registered ahead of Task 9.
Task 8: complete (commits 73d753e..be2ef4b, tests: bash -c 'cargo test -p privatevox hotkey_tests 2>&1 | grep "test result" | head -1' → test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 27 filtered out; finished in 0.00s)
Task 9: complete (commits be2ef4b..c5c9d27, tests: bash -c 'cargo test -p privatevox undo 2>&1 | grep "test result" | head -1; CARGO_TARGET_DIR=target/lib cargo test -p privatevox-core undo 2>&1 | grep "test result" | head -1' → test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 70 filtered out; finished in 0.00s)
Task 10: complete (commits c5c9d27..9bffb93, tests: bash -c 'cargo test -p privatevox paste_text 2>&1 | grep "test result" | head -1; CARGO_TARGET_DIR=target/lib cargo test -p privatevox-core export 2>&1 | grep "test result" | head -1' → test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 75 filtered out; finished in 0.01s)
Task 11: complete (commits 9bffb93..0ec71b5, tests: bash -c 'cargo build -p privatevox 2>&1 | tail -1' →     Finished `dev` profile [unoptimized + debuginfo] target(s) in 8.92s)
Final review: fresh reviewer (fable) on 891054e..0ec71b5 - "With fixes": 0 Critical, 7 Important, 8 Minor. All Important fixed in ONE pass (commit above); Minors deferred.
Final: fixed stale modifier mapping after rebind - modifier_mapping_is_empty_when_hold_key_is_a_combo RED->GREEN
Final: fixed whole-clip rescoring every tick (305 ms at 2 min, 758 ms at 5 min) - score_window_is_bounded_and_overlapping RED->GREEN; chunks trimmed from existing probabilities
Final: fixed pending switch lost on cancel - switch_requested_then_cancelled_is_applied RED->GREEN
Final: fixed chunk worker outliving pause/unload and reloading the model - pause_during_long_recording_stops_chunk_worker RED->GREEN; worker touches the idle timer per chunk
Final: fixed background downloads driving the pill - no pill emits when activate == false (no unit test: Tauri AppHandle)
Final: fixed undo keycode on QWERTZ - Key::Unicode('z') (no unit test: macOS keyboard API)
Final: fixed benchmark switching the active model permanently - benchmark_restores_previous_model RED->GREEN
Final: suites after the pass: core 77/77, engine 13/13, app 32/32 (+4 ignored).
Final: minor (deferred): validate_combo lowercases multi-word keys ("ArrowUp" -> "Arrowup"); keep original casing
Final: minor (deferred): set_settings saves before registering; a taken combo leaves no hotkeys; register first, reject duplicate combos
Final: minor (deferred): panel Save sends a stale model_id snapshot; set_settings should keep the server-side model_id
Final: minor (deferred): undo()/PasteText bump generation mid-recording, pausing the tick chain for 2 s
Final: minor (deferred): deferred switch overwrites the Pasted preview immediately; tail_ms == total_ms in chunked timing; duration_ms is the whole clip
Final: minor (deferred): delete_model ignores an in-progress download
Final: minor (deferred): key recorder has no cancel; Models 5 s timer polls while the panel is hidden
Final: minor (deferred): switch/benchmark check exists() while list_models checks exact size
Final: Ruling: live cut placement may differ from batch when a pause >= 10 s is in progress at scoring time - stands (harmless); cost: a cut a few seconds off
Final: Ruling: first chunk certain only at 45 s - stands (documented trade); cost: later first-chunk start
Final: Ruling: a 41 s clip splits at 20.5 s - stands (rule followed literally); cost: one extra decode on clips just over 40 s
Final: Ruling: pause scoring favours early long pauses - stands (tuning); cost: slightly uneven chunks
Final: Ruling: resident_bytes via ps rss delta may undercount Metal buffers - stands until measured on hardware; cost: strip under-reports
Final: Ruling: UTC export timestamps, open -R, no row cap - stand; cost: none for personal use
Final: Ruling: ModelReady for the old model after a first-run switch - stands (first run only); cost: one wrong load
