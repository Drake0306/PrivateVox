# resume.conversation.md — PrivateVox working context (written 2026-10-29, late evening IST)

Read this first when resuming on another machine (the Windows PC) or in a fresh session. It is the full working
context of the Mac sessions of 2026-10-28 and 2026-10-29: what exists, how it is built and verified, what was
decided, what is half-done, and exactly where to pick up. The repo, the ledgers and the plans are the source of
truth; this file is the map.

## 1. Identity and rules

- Product **PrivateVox** (was "LVW" / "Whisper desktop dictation harness"), repo `Drake0306/PrivateVox`, local dir
  on the Mac `/Users/roy/Developer/Github/localVoiceWisper`, branch `main`, MIT.
- Owner: Roy (they/them). Wants minimal check-ins, autonomous continuation ("go ahead and build, I trust you"),
  but **asks before anything disruptive**: on 2026-10-29 Roy stopped a `git stash` and an app restart mid-command.
  Do not use `git stash`; do not kill/restart the running app while Roy is at the machine without saying so first.
- Commits: no Co-Authored-By / "Generated with Claude" trailers ("never add yourself to cc"). Chain commands with
  `&&`, never `;`, so a failed step cannot be committed.
- Git identity is repo-local: Drake0306 / abhinavroy.hello@gmail.com, `core.sshCommand` uses `~/.ssh/id_github`.
  Never touch the global git config, `~/.ssh/config`, or the "modus" work keys (`id_rsa`, `id_ed25519`). `gh` is
  authenticated as Drake0306 on the Mac.
- Don't auto-open artifacts; give Roy the link. Roadmap artifact: https://claude.ai/artifact/T4fLAdWirGB6JWBLNSDTsz
  (file `docs/privatevox-roadmap.html`, republish by publishing that file path).
- Claude memory dir (Mac only): `/Users/roy/.claude/projects/-Users-roy-Developer-Github-localVoiceWisper/memory/`.

## 2. Machines

- **Mac**: Apple M1, 8 GB, macOS 27.0.1, no `timeout` command, disk tight (delete `target/debug`, `target/release`
  or `cargo clean -p privatevox -p privatevox-core -p privatevox-engine` when space runs out; the debug tree grows past
  15 GB). Ollama 0.40 with `qwen2.5:3b`. Screenshots work from the shell (`screencapture`), System Events UI
  scripting works for iTerm2 (Accessibility granted), used by `scripts/ui-shot.sh` and `scripts/tray-menu.sh`.
- **Windows PC**: AMD RX 9070 (any Vulkan-capable AMD/NVIDIA/Intel GPU is the target; nothing is tied to this card),
  32 GB RAM, Windows 11. Set up on 2026-10-31: Visual Studio 2022 Build Tools, CMake, Ninja, Vulkan SDK 1.4.363, LLVM
  (libclang), Tauri CLI 2.12.4, Ollama (AMD AI bundle). Build with `pwsh scripts/win-cargo.ps1 <cargo args>` (imports
  vcvars64, Ninja, `CARGO_TARGET_DIR=D:\t`, `VULKAN_SDK`, `LIBCLANG_PATH`; also repoints a user-level `CMAKE` variable
  that pointed at the bin directory). The workspace builds and all suites pass there. Still by hand: install
  `PrivateVox_0.6.0_x64-setup.exe` (or `cargo tauri dev`), check the Right Alt hold key and paste, run host mode
  PC→Mac for real, time Vulkan (`cargo test -p privatevox-engine --release --lib spike_timing -- --ignored --nocapture`
  with `PRIVATEVOX_TEST_MODEL=<ggml file>` and `PRIVATEVOX_SPIKE_CLIPS=<dir with clip_30/clip_10/clip_short.wav>`; the
  Windows Speech clip generator in `benchmark.rs` makes 16 kHz mono WAVs). Known Windows behaviour: a refused loopback
  connection takes ~2 s, so a dead host reads as "did not answer in time".
- Important for debug builds on macOS: every `cargo build` changes the binary identity, so macOS **forgets the
  Accessibility grant** → pastes fall back to "copied only". Run the bundled app (`cargo tauri build --debug`, then the
  `.app`) or re-grant after each build. Roy's screen on 2026-10-29 showed "Accessibility: not granted" for the debug binary.

## 3. Releases (all with dmg, exe, msi, AppImage, deb, rpm)

- v0.2.0-alpha.1, v0.3.0-alpha.1, v0.4.0-alpha.1, **v0.5.0-alpha.1** (2026-10-29,
  https://github.com/Drake0306/PrivateVox/releases/tag/v0.5.0-alpha.1). Notes in `docs/releases/`, changelog in
  `CHANGELOG.md`. macOS dmg built locally with `cargo tauri build` (ad-hoc signed) and uploaded with
  `gh release create <tag> --prerelease --title ... --notes-file ... <dmg>`; `.github/workflows/release.yml` builds
  Windows and Linux on the tag and attaches them (tauri-action). macOS is **not** in the CI matrix yet (phase 6 Task 12).
- Version is in `Cargo.toml` (workspace) and `src-tauri/tauri.conf.json`; bumping updates `Cargo.lock` (commit it).

## 4. Architecture (what exists)

Workspace: `crates/privatevox-core` (pure logic: settings, store, hotkeys machine, chunking, VAD policy, polish,
net protocol, dictionary, language, silence, appmodes, preview, commands, audio), `crates/privatevox-engine`
(whisper-rs backend, supervisor with a child worker process, remote engine, routing), `src-tauri` (crate
`privatevox`: pipeline/coordinator, adapters, hotkeys, paste, tray, host, remote, discovery, cues, secure input,
commands), `ui/` (vanilla JS: `panel.html`, `pill.html`, `privatevox.css/js`). Tests: core ~212, engine 14 (+7
ignored/real-model), app ~125 (`cargo test -p privatevox --lib -- --test-threads=1`). Design doc:
`docs/superpowers/specs/2026-10-28-whisper-harness-design.md` (§8.6 host mode; §18 Handy techniques).

Built and reviewed, phase by phase (plans + ledgers in `docs/superpowers/plans/`):
1. Core dictation (hotkeys, capture, VAD, Whisper, paste, store, pill, tray, lifecycle, idle unload).
2. Panel, model manager (download/switch/delete/benchmark), GPU memory strip, chunked decode while speaking, undo,
   history edit/pin/paste-again/export.
3. LLM polish: modes Raw/Clean/Email/Bullets/Code/Translate/Ask, Ollama native + OpenAI endpoints, LAN failover with
   health/backoff, warm-up on key down, re-polish, filler removal, memory-strip LLM segment.
4a. Host mode (§8.6): any PrivateVox serves `/v1/status|dictations|polish|history|models|models/select` on 8765;
   the client routes chunks + polish to the host with sticky fallback; mDNS `_privatevox._tcp.local.`.
4b. Dictionary (fuzzy, Soundex-gated), spoken languages, kept audio + re-run, receipt-sequenced paste (AppKit
   pasteboard promise), secure-input warning, long-form document view, panel tabs History/Models/Polish/Network/
   Settings/Setup.
5. Auto-stop on silence in toggle mode (blip-tolerant `TrailingSilence`, `MIN_SPEECH_FRAMES` 4), per-app modes,
   sound/pill cues, live preview (helper thread, local engine only, VAD-gated, `Cmd::Preview`), spoken end-of-dictation
   commands (destructive ones need a sentence break; text before a cancel kept as a "cancelled" row), deferred-minors
   cleanup, `scripts/check-panel-js.mjs` lint in CI. MLX spike measured and rejected
   (`docs/superpowers/spikes/2026-10-29-mlx-spike.md`). Fresh review findings fixed; ledger
   `docs/superpowers/plans/2026-10-29-privatevox-phase5-ledger.md` lists the deferred minors.

Debug hook: `PRIVATEVOX_DEBUG_SCRIPT="tap:14000"` (also `start:<ms>`, `stop:<ms>`, `cancel`, `polish_on`, `polish_off`)
on `./target/debug/privatevox`. Logs: `~/Library/Logs/com.drake0306.privatevox/privatevox.log` (UTC timestamps).
Settings: `~/Library/Application Support/com.drake0306.privatevox/settings.json`; history SQLite beside it.

## 5. Phase 6 (in progress) — "build everything the design doc promises that is not built"

Why: on 2026-10-29 Roy reported "half of the features are not working", e.g. no live transcription and no switch for
it. Diagnosis from screenshots of Roy's running debug build: (a) live preview was **Auto**, which is off on an 8 GB
Mac without a host, so Roy saw nothing; (b) the new controls sat at the bottom of a long Settings tab under ~100
language checkboxes; (c) Accessibility was not granted to the debug binary, so pastes were "copied only". The audit of
design-doc items not built is in the plan.

Plan: `docs/superpowers/plans/2026-10-29-privatevox-phase6.md` (13 tasks). Executing inline with the superpowers
`executing-plans` skill; ledger `.superpowers/sdd/2026-10-29-privatevox-phase6/progress.md` (git-ignored, Mac only;
recreate it from this file if resuming elsewhere: first line `# SDD ledger — plan: docs/superpowers/plans/2026-10-29-privatevox-phase6.md`).
Scripts: `~/.claude/plugins/cache/claude-plugins-official/superpowers/7.0.0/skills/executing-plans/scripts/{task-start,task-done}`
and `.../subagent-driven-development/scripts/{sdd-workspace,review-package}`.

Status:
- Task 1 **done** (commit `23cd92e`): live preview default On (`Settings::default().live_preview == Preview::On`;
  existing settings.json files keep their value — Roy's still says `auto`, switch it in Settings → Dictation behaviour
  or edit the file), Settings tab reordered (Dictation behaviour card first, then "Keys, activation and paste", Modes
  per app, Languages and dictionary with the language list collapsed), `scripts/ui-shot.sh` screenshot helper.
- Task 2 **done** (commit `5b09b5b`): clipboard-only dictation hotkey `Binding::CopyOnly`, default
  `Ctrl+Shift+Alt+Space`, tap to start/tap to stop, result copied not pasted, row status `copied_only`, undo target
  untouched; Settings field "Copy-only key".
- Task 3 **code complete, verification half done**: `launch_at_login` (tauri-plugin-autostart 2.7, LaunchAgent) and
  `show_pill` settings; tray gained "Load model now" (`Cmd::Resume` → preload), a "Model" submenu (ticked active,
  greyed "(not downloaded)"), "Show pill" and "Launch at login" check items; `tray::refresh(app)` re-ticks after a
  switch/download/save; commands `set_launch_at_login`, `set_show_pill`, `apply_pill_visibility`; panel checkboxes in
  the keys card. Tests green (`model_entries_check_current_and_disable_missing`, `resume_loads_the_model_without_recording`,
  `login_and_pill_defaults`), full app suite 125/125, app starts (the first build aborted because the tray read app
  state before `manage()`; fixed by passing the initial rows/flags into `tray::build`). **Not yet verified by eye**:
  the tray menu would not open under System Events clicks after the rebuild (`scripts/tray-menu.sh list` → "menu did
  not open"); a real click at the icon also showed no menu in a screenshot. Unknown whether that is the AX scripting
  (it opened earlier in the session on the old menu) or the new menu. Next step: Roy clicks the menu bar icon by hand
  and says whether the new items appear; or verify through the Settings checkboxes (same inner functions), then
  commit `feat: launch at login, pill show/hide, tray model picker and Load model now`, `task-done 3 5b09b5b`,
  `task-start 4`.
- Tasks 4–12 **done on the Windows PC on 2026-10-31** (branch `claude/new-session-114a90`, one commit per task, all
  three suites green on Windows: core 219, engine 14 + 7 supervisor, app 137). Task 13: docs, changelog, release notes,
  roadmap Feature status table and the 0.6.0 version bump are done; the live check with screenshots and the
  `v0.6.0-alpha.1` tag are **left to Roy** (tagging publishes a release). Full per-task notes, Windows build setup and
  the Windows fixes (worker parent watch, benchmark clip via Windows Speech, `scripts/win-cargo.ps1`, model file name
  fix) are in `docs/superpowers/plans/2026-10-29-privatevox-phase6-ledger.md`, which is now the tracked progress file.
- Not buildable without the PC (list in the roadmap under "needs the PC"): Windows admin-elevated target detection
  (§12), the Windows pass, RX 9070 timing, real PC→Mac host run. Needs an account: notarisation/signing (C2).
- Not picked from the phase 5 proposal: B2 LLM translate, C3 host extras, C4 Wayland.

## 6. Helpers added for verification (macOS)

- `scripts/ui-shot.sh <Tab> <out.png> [--page-down N] [--click X,Y]`: shows the panel via the single-instance hook
  (launching the binary again), clicks the tab by coordinates relative to the panel window, captures and crops. Page
  Down only scrolls after a click on the empty tab-bar area; it sometimes only scrolls one screen.
- `scripts/tray-menu.sh list | "<item>" | <Submenu> <item>`: clicks the menu bar menu through System Events, retries.
  Was flaky/failing at the end of the session (see Task 3).
- `scripts/check-panel-js.mjs ui/panel.html ui/pill.html`: syntax + const-reassignment lint (in release CI).
- Both windows are titled "PrivateVox"; the panel is the one wider than 600 pt.

## 7. Rulings worth remembering (full lists in the ledgers)

- Executed on `main` without worktrees (standing consent).
- Chunked dictations keep the first spoken language; preview decodes a 6 s window every 3 s only when the local engine
  is free, never through a host; destructive spoken commands need a sentence break; blips under 128 ms are silence.
- Recordings started from the tray never auto-stop (not latched by the key machine); per-app rules are substring,
  first match wins; spoken commands are English-only; afplay assumed present.
- Phase 6: copy-only is read from `Stage::Recording { binding: CopyOnly }` rather than a new Effect field.

## 8. How to resume (updated 2026-10-31)

1. `git pull`; read this file, then `docs/superpowers/plans/2026-10-29-privatevox-phase6-ledger.md` (the tracked
   progress table); run the three suites to confirm green (`pwsh scripts/win-cargo.ps1 test ...` on the PC).
2. Roy: merge `claude/new-session-114a90` into `main`, tag `v0.6.0-alpha.1`, watch the three CI builds (the dmg is new
   there). Then the PC checklist in §2 by hand, and on the Mac a single click on the menu bar icon to see Task 3's items.
3. Next engineering work, in order of value: the fresh whole-branch review of phase 6 (Agent, code-reviewer prompt,
   one fix pass); the Windows elevated-target detection (§12); B2 translate via the LLM; C3 host extras after the first
   real PC→Mac run; C4 Wayland.
4. Rules unchanged: no co-author trailers, `&&` not `;`, no `git stash`, ask before restarting a running app.
