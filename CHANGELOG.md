# Changelog

All notable changes to PrivateVox. Versions follow the plans in `docs/superpowers/plans/`.

## 0.6.0-alpha.1 — 2026-10-31

Fifth alpha: the rest of what the design doc promised, plus the first real Windows pass. Tasks 1–3 were built and checked on the Mac (29 Oct), tasks 4–12 on a Windows PC with an AMD GPU (31 Oct), where all three test suites run green.

### Phase 6
- Live preview is on by default (an existing settings file keeps its value); the Settings tab leads with Dictation behaviour and the language list is collapsed.
- Clipboard-only dictation key (default Ctrl+Shift+Alt+Space): tap to start, tap to stop, copied not pasted.
- Launch at login, Show the pill, a Model submenu and "Load model now" in the menu bar.
- History filters by app and mode (composing with the search box) and "Clear all…" with an inline confirm that also removes kept clips; never touches a host's history.
- Microphone level test beside the microphone picker and in Setup: a meter for up to ten seconds, refused while recording.
- Paste method per app (Type the text for terminals and remote desktops), resolved from the app in front at paste time. The Paste method setting itself now applies to the next paste instead of needing a restart.
- Q8_0 turbo model (874 MB) offered on machines with 12 GB or more, or wherever the file already exists; never auto-recommended.
- Crash report file (`crash-<timestamp>.txt` next to the log) on any panic, with "Show report" in Setup.
- First-run walkthrough: Setup is a numbered list with a state per step; Windows skips the macOS-only grants and opens its microphone settings page.
- The microphone stays open for 20 s after a dictation (0–300 s, 0 = close at once), so the next one starts without reopening the device.
- Host, remote and LLM endpoint tokens live in the OS keychain (macOS Keychain, Windows Credential Manager); `settings.json` only holds `@keychain`.
- The release workflow builds the macOS dmg on a macos-14 runner.

### Windows
- The engine worker exits when the app dies on Windows too (it waits on the parent's process handle); before, a force-quit left it and its GPU memory alive.
- The benchmark clip is synthesised with Windows Speech, so Benchmark works on Windows.
- `scripts/win-cargo.ps1` reproduces the CI build environment on a PC; README has the prerequisites.
- Fixes found on the way: the model file name was assumed to end in `-q5_0.bin` (wrong for distil and the Q8 turbo); the supervisor tests and two timing-sensitive tests assumed Unix process tools or instant connection refusal.

## 0.5.0-alpha.1 — 2026-10-29

Fourth alpha: the things that make dictating feel immediate. macOS on Apple Silicon tested by hand; Windows and Linux built by CI.

### Phase 5
- Live preview while speaking: partial text on a second line of the pill (Auto / On / Off).
- Auto-stop on silence in toggle mode (default 2 s, Never possible), with "stopping…" in the pill.
- Spoken end-of-dictation commands: new line, new paragraph, delete that / undo, cancel / never mind. The destructive three need a sentence break before them or stand alone; the text before a cancel is kept in History. Off-able.
- Auto-stop ignores clicks and coughs shorter than 128 ms; the preview skips windows with no speech in them instead of inventing sentences.
- Modes per app: a table of app name → mode, applied when the dictation starts.
- Sound cues (macOS system sounds) and a pill flash on start, stop and "nothing heard". Off-able.
- Cleanup of the small items the last three reviews left: the host now receives the dictionary prompt, mDNS follows network changes, a kept clip is never orphaned by a delete, long dictations record how their language was chosen, re-run uses the host when in use, the secure-input poller runs on macOS only, and the panel fetches less.
- The pill window is taller to make room for the preview line.

## 0.4.0-alpha.1 — 2026-10-28

Third alpha: host mode (use the PC's GPU from the Mac), the personal dictionary, spoken languages, kept audio, and the macOS paste and secure-input work. macOS on Apple Silicon tested by hand; Windows and Linux built by CI.

### Extras (phase 4b)
- Personal dictionary: words go to Whisper as its prompt; a fuzzy pass fixes near-misses (strictness slider).
- Spoken languages: one is fixed; several are detected per clip and held to the list, with a re-run when detection misses; the row records the language and how it was chosen.
- Keep audio clips (off by default) with Play and Re-run with another model from history.
- Receipt-sequenced clipboard restore on macOS: the old clipboard returns after the app has read the paste, never on a timer.
- Secure-input warning in the pill when a password field would swallow the hold key.
- Long-form document view with paragraphs, Copy all and Markdown export.
- mDNS: hosts advertise `_privatevox._tcp`; "Find hosts on this network" lists them with an Add button.
- Fixes: host status flashes also when a request (not the health loop) found it down; host sessions keyed by client; host history search pages correctly; mode changes sync between tray and panel; the polish selector says "On · none reachable" instead of "Off".

### Host mode and the remote interface (phase 4a)
- Any PrivateVox can act as a host (Settings → Network): it serves transcription, polish, its history and its model list on port 8765, LAN only, optional token.
- "Use a host on this network" sends each dictation chunk and the polish to the host; the local Whisper is not loaded while the host is in use; automatic fallback to local when the host is unreachable and back again.
- The memory strip shows the host's adapter and figures while the host is in use; the Whisper picker switches the host's model.
- Two histories: the host keeps rows marked "host" with the client's name, the client keeps its copy marked "via PC"; a toggle in the History header shows the host's list.
- History rows have an `engine_kind` column (local, remote:<host>, host).

## 0.3.0-alpha.1 — 2026-10-28

Second alpha: phase 3, LLM polish and LAN failover. macOS on Apple Silicon tested by hand; Windows and Linux built by CI.

### Polish (phase 3)
- Modes: Clean (default), Email, Bullets, Code, Ask through an LLM; Raw and Translate to English without one. Mode picker in the panel header and the menu bar; prompts editable with reset.
- LLM endpoints: machines you add by `ip:port` first, then Ollama, LM Studio or llama.cpp found on this machine. Health checks every 10 s with backoff, automatic failover and fail-back, "PC unreachable, polished locally" in the pill, raw text pasted and the row marked *unpolished* when nothing answers.
- Warm-up on key press, `keep_alive` on Ollama, unload on the same idle timer as Whisper. The first dictation after a cold runtime may wait up to 25 s for the model to load instead of pasting raw text.
- Re-polish any history row with another mode; Raw toggle shows Whisper's text.
- Polish LLM selector in the memory strip; a local model's resident size counts against the budget.
- Filler words removed before saving and before the LLM (language-gated), with a toggle.
- History database migrates to schema v2 (`polish_status`).
- Turning polish off, or switching the endpoint or model, unloads the local models PrivateVox kept alive, so memory goes back to the system right away.
- Switching polish on, or choosing another endpoint or model, loads that model right away so the strip reflects it; nothing is loaded at launch until you dictate or change a setting.
- "Paste again" hides the panel first so the text lands in the app you came from; a re-polish warms the model first and says so when polish is off.
- The pill's level bars and spinner only show in the states that use them (the hidden attribute lost to a CSS rule).
- The memory strip on macOS now shows Activity Monitor's "Memory Used" (app memory + wired + compressed) instead of a figure that counted file cache, refreshes every 3 s, and no longer counts a resident local LLM twice.

## 0.2.0-alpha.1 — 2026-10-28

First public alpha. macOS on Apple Silicon is the tested platform; Windows and Linux builds are produced by CI and have not been run by a person yet.

### Dictation
- Hold Right Option (or tap Ctrl+Shift+Space) to record; release to transcribe and paste into the app in front.
- Whisper large-v3-turbo runs on the GPU in a separate worker process, so a crash or hang in the engine never takes the app down, and a force-quit never leaves memory behind.
- The model starts loading the moment you press the key; the pill says so while you keep talking.
- Silence is trimmed, clips under 300 ms are ignored, and long dictations are cut at pauses and decoded while you are still speaking. A countdown shows in the last 30 s before the 5-minute cap.
- The previous clipboard is restored after the paste. Ctrl+Shift+Z undoes the last paste if the same app is still in front.

### Models and memory
- Three models from the panel: large-v3-turbo (fast, default), large-v3 (accuracy), distil-large-v3 (English only). Download, delete, benchmark and switch without restarting.
- A memory strip shows total memory, what other apps hold, what the selected model takes (measured after its first load), and which model is recommended and which is the most accurate that fits.
- The model unloads after an idle time you choose, or at once from the menu bar.

### History
- Every dictation is saved locally (SQLite). Search, edit in place, pin, copy, paste again, show the raw Whisper text, delete, export to Markdown or JSON.

### Settings
- Activation mode (hold, toggle, hold-or-toggle), hotkeys with a key recorder, microphone, paste method, idle unload, post-release buffer, maximum clip length. Changes apply without restart.

### Known limitations
- No LLM clean-up yet (fillers, punctuation, email/bullet/code modes): that is the next phase. Transcripts are Whisper's output plus stutter collapse.
- Builds are not code-signed. macOS shows "unidentified developer" on first open; Windows shows a SmartScreen warning.
- Windows and Linux: built by CI, untested by a person. On Linux the hold key needs X11; Wayland is not supported in this build.
- Only the first spoken language in settings is used. No personal dictionary yet.
