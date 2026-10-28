# Changelog

All notable changes to PrivateVox. Versions follow the plans in `docs/superpowers/plans/`.

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
