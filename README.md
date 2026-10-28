# PrivateVox — PrivateVox

Hold a key, talk, let go: the words land in whatever app is in front. Whisper runs on your own GPU, nothing leaves the machine, and every dictation is kept in a local history you can search, copy and delete.

**Status:** early. macOS (Apple Silicon) is the first target; Windows (AMD/Vulkan) follows. See `docs/superpowers/specs/2026-10-28-whisper-harness-design.md` for the design and `docs/superpowers/plans/` for what is built when.

## What works today (macOS)

- Floating pill at the bottom of the screen above the Dock (drag it anywhere; it remembers), showing idle / listening / transcribing / pasted / error, visible on every space, never steals focus.
- Push-to-talk on **Right Option**, toggle on **Ctrl+Shift+Space**, hold-or-toggle, and **Ctrl+Shift+Z** to undo the last paste; all rebindable from the Settings tab with a key recorder, applied without restart.
- Three Whisper tiers switchable from the panel: large-v3-turbo (fast, default), large-v3 (accuracy), distil-large-v3 (English only). Download, delete and benchmark each from the Models tab.
- A memory strip showing total memory, what other apps hold, what the selected model will take (measured after its first load), Recommended and Best-that-fits.
- Whisper on Metal in a child worker process (crash and hang recovery, no stranded GPU memory on force quit); the model starts loading the moment you press the key.
- Silence trimming, 300 ms minimum clip, 5 minute maximum with a countdown in the last 30 s; long dictations are cut at pauses and decoded while you are still speaking.
- Reduced audio context for short clips (about 1 s per dictation on an M1).
- Paste into the frontmost app via the clipboard with the previous clipboard restored.
- SQLite history with search, inline edit, pin, copy, paste again, delete, and export to Markdown or JSON in ~/Downloads.
- Idle unload of the model after any number of minutes you choose; menu bar item to pause and unload now.
- First-run download of the model with resume and progress; Setup tab for the three macOS permissions.

## Download

Alpha builds for macOS (Apple Silicon), Windows and Linux are on the [Releases page](https://github.com/Drake0306/PrivateVox/releases). Read the release notes there for the one-time macOS permission steps; builds are unsigned for now.

## Build and run

Requirements: Rust 1.90+, Xcode Command Line Tools, `cmake`, and the Tauri CLI (`cargo install tauri-cli --version "^2" --locked`).

```bash
git clone https://github.com/Drake0306/PrivateVox.git
cd PrivateVox
cargo tauri dev            # from the repo root, or `cd src-tauri && cargo tauri dev`
```

The first build compiles whisper.cpp and the Tauri stack (several minutes). The first launch of a freshly built binary also compiles Metal shaders once (about 10 s before the pill goes idle).

### Permissions (one time)

macOS asks for three things. PrivateVox's **Setup** tab (click the pill, then Setup) has buttons that open each pane.

| Permission | Why | Where |
|---|---|---|
| Microphone | to record | prompt on first dictation |
| Accessibility | to send ⌘V into the front app | System Settings → Privacy & Security → Accessibility |
| Input Monitoring | to see the Right Option key | System Settings → Privacy & Security → Input Monitoring |

After granting Accessibility or Input Monitoring, quit PrivateVox from the menu bar and start it again. See `docs/dev/permissions.md` to reset grants while developing.

### Tests

```bash
cargo test -p privatevox-core                          # settings, audio, VAD, hotkey machine, store, lifecycle, download
cargo test -p privatevox-engine -- --test-threads=1    # protocol, policy, worker supervisor (counts worker processes)
cargo test -p privatevox                               # pipeline coordinator, paste, exit path
cargo test -p privatevox-core -- --ignored             # microphone capture (asks for the mic)
PRIVATEVOX_TEST_MODEL=~/path/to/ggml-large-v3-turbo-q5_0.bin cargo test -p privatevox-engine --test worker_integration -- --ignored
```

## Where things live

`~/Library/Application Support/com.drake0306.privatevox/`: `settings.json`, `privatevox.sqlite` (history), `models/`, `privatevox.lock`, `clipboard.bak`. Logs: `~/Library/Logs/com.drake0306.privatevox/privatevox.log`.

## Layout

- `crates/privatevox-core` — pure logic, no Tauri: settings, audio conversion, VAD, activation state machine, text clean-up, SQLite store, lock file and idle timer, downloads.
- `crates/privatevox-engine` — the `Engine` trait, the whisper-rs backend, the pipe protocol, the worker loop and the `WorkerEngine` supervisor.
- `src-tauri` — the app: pill and panel windows, tray, hotkeys, paste, pipeline coordinator, lifecycle.
- `ui/` — plain HTML/CSS/JS, no bundler.
- `spike/` — the throwaway engine benchmark; results in `docs/superpowers/spikes/`.

## Credits

Several techniques are adopted from [Handy](https://github.com/cjpais/Handy) (MIT): the worker-process engine, VAD timing, chunking at pauses, the activation state machine and the receipt-sequenced paste design. Whisper by OpenAI, whisper.cpp by Georgi Gerganov and contributors, Silero VAD by the Silero team.

## Licence

MIT. See `LICENSE`.
