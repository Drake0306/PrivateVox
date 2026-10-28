# Changelog

All notable changes to PrivateVox. Versions follow the plans in `docs/superpowers/plans/`.

## Unreleased

### Polish (phase 3)
- Modes: Clean (default), Email, Bullets, Code, Ask through an LLM; Raw and Translate to English without one. Mode picker in the panel header and the menu bar; prompts editable with reset.
- LLM endpoints: machines you add by `ip:port` first, then Ollama, LM Studio or llama.cpp found on this machine. Health checks every 10 s with backoff, automatic failover and fail-back, "PC unreachable, polished locally" in the pill, raw text pasted and the row marked *unpolished* when nothing answers.
- Warm-up on key press, `keep_alive` on Ollama, unload on the same idle timer as Whisper. The first dictation after a cold runtime may wait up to 25 s for the model to load instead of pasting raw text.
- Re-polish any history row with another mode; Raw toggle shows Whisper's text.
- Polish LLM selector in the memory strip; a local model's resident size counts against the budget.
- Filler words removed before saving and before the LLM (language-gated), with a toggle.
- History database migrates to schema v2 (`polish_status`).
- Turning polish off, or switching the endpoint or model, unloads the local models PrivateVox kept alive, so memory goes back to the system right away.
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
