# PrivateVox

[![Release builds](https://github.com/Drake0306/PrivateVox/actions/workflows/release.yml/badge.svg)](https://github.com/Drake0306/PrivateVox/actions/workflows/release.yml)
[![Latest release](https://img.shields.io/github/v/release/Drake0306/PrivateVox?include_prereleases&label=release)](https://github.com/Drake0306/PrivateVox/releases)
[![Licence: MIT](https://img.shields.io/badge/licence-MIT-0f766e.svg)](LICENSE)
![Platforms](https://img.shields.io/badge/macOS%20%7C%20Windows%20%7C%20Linux-local%20only-5b6b69)

Hold a key, talk, let go: the words land in whatever app is in front. Whisper runs on your own GPU, nothing leaves the machine, and every dictation is kept in a local history you can search, copy and delete.

## Quick start

1. Download the installer for your machine from the [Releases page](https://github.com/Drake0306/PrivateVox/releases): `.dmg` for a Mac with Apple Silicon, `.exe` or `.msi` for Windows, AppImage, `.deb` or `.rpm` for Linux. Builds are unsigned alphas: on macOS right-click → Open the first time; on Windows choose "More info → Run anyway" in SmartScreen.
2. Start it. A small pill appears at the bottom of the screen and the Whisper model (574 MB) downloads once. On macOS, the Setup tab lists the two permissions to grant (Accessibility to paste, Input Monitoring for the hold key); Windows needs none.
3. Put the cursor in any app, hold **Right ⌥ Option** (Mac) or **Right Alt** (Windows), speak, let go. The text is pasted where the cursor was. Click the pill for history and settings.

Everything stays on your machine. The optional clean-up step uses a small language model you run yourself (Ollama, LM Studio or llama.cpp, on this machine or another one on your network); there is no account and no cloud.

**Status:** alpha. macOS (Apple Silicon, Metal) is the platform it was built and used on. Windows builds with whisper.cpp's Vulkan backend, which runs on any recent AMD, NVIDIA or Intel GPU; the full test suites pass on a Windows PC with an AMD card, and the hold key and paste there are next to be checked by hand. See `docs/superpowers/specs/2026-10-28-whisper-harness-design.md` for the design, `docs/superpowers/plans/` for what is built when, and `docs/privatevox-roadmap.html` for the feature status table.

## What it does

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

## Polish: cleaning up the text with an LLM

Whisper's transcript goes through a small language model before it is pasted, unless the mode is Raw or Translate. Modes: **Clean** (default), **Email**, **Bullets**, **Code**, **Ask** (answers the question instead of transcribing it), plus **Raw** and **Translate to English** which skip the LLM. Pick the mode in the panel header, the menu bar, or re-polish any history row with another mode. Prompts are editable under Settings → Polish, with a reset.

Where the LLM runs, in priority order:

1. **Machines you added** (Settings → Polish → Add endpoint, as `ip:port`). Typically the PC with the big GPU, running LM Studio with "Serve on local network" on, or Ollama with `OLLAMA_HOST=0.0.0.0`.
2. **This machine**, found automatically: Ollama on port 11434, LM Studio on 1234, llama.cpp server on 8080.

Every endpoint is health-checked every 10 seconds. A dictation uses the first reachable one; if it fails mid-request the next one is tried and the pill says "PC unreachable, polished locally". If nothing answers within the timeout (8 s by default) the raw transcript is pasted and the history row is marked *unpolished*. When the preferred machine comes back it is used again without any action.

On an 8 GB Mac, `ollama pull qwen2.5:3b` is the recommended local model (about 2 GB resident, roughly one second per dictation once warm). The model is warmed when you press the key and unloaded on the same idle timer as Whisper. Filler words ("uh", "hmm", and "um" when the language is English) are removed before the LLM and in Raw mode too; switch that off under Settings → Polish.

## While you speak

- **Live preview** (Settings → Dictation): the words appear on a second line of the pill as Whisper decodes, so you can see it heard you and stop early. Auto means on when a host does the work or the machine has 16 GB or more; set it On or Off yourself.
- **Auto-stop** in toggle mode: tap, talk, pause, and after two seconds of silence (your choice, or never) the dictation ends itself. The pill says "stopping…" in the last second.
- **Spoken commands** at the very end of a dictation: "new line", "new paragraph", "delete that" or "undo" (undoes the last paste), "cancel" or "never mind" (drops the dictation). Only the trailing phrase is checked, and the three that drop text are taken only on their own or after a sentence break ("Send the report. Cancel."), so "tell him I will cancel" is just text. Whatever came before a cancel stays in History as a cancelled row. Off-able.
- **Modes per app** (Settings → Modes per app): "Mail" → Email, "Code" → Code, from a small table; the pill shows the mode it picked.
- **Sound cues** on start, stop and "nothing heard", plus a short flash of the pill. Off-able.

## Dictionary, languages, kept audio

- **Personal dictionary** (Settings → Dictation): names and jargon, one per line. They go to Whisper as its prompt, and a fuzzy pass fixes near-misses afterwards ("charge bee" → "ChargeBee"). The slider sets how close a word has to be; 0 switches the pass off.
- **Spoken languages**: one language means Whisper decodes with it fixed and never guesses. Several means Whisper detects per clip and is held to your list; a clip detected outside the list is decoded again with your first language. The history row records which language and how it was chosen.
- **Keep audio clips** (off by default): the trimmed audio of each dictation is kept next to its row. Rows then show Play and Re-run, which decodes the clip again with any downloaded model, useful after switching to a bigger one.
- Long dictations get an **Open as document** button: the text in paragraphs, with Copy all and a Markdown export.
- On macOS the old clipboard comes back only after the target app has actually read the paste (no fixed delay), and the pill warns when a password field has secure input on, since the hold key may not fire then.

## Settings, setup and safety nets

- **Copy-only key** (default Ctrl+Shift+Alt+Space): tap, talk, tap; the text is copied, never pasted, and the undo target is left alone.
- **History filters**: narrow the list by app and by mode together with the search box; **Clear all…** with an inline confirm deletes every local row and the kept clips (a host's history is never touched).
- **Microphone test**: a Test button and a level meter beside the microphone picker (and in the Setup walkthrough), ending by itself after ten seconds and refused while a dictation records.
- **Paste method per app**: terminals and remote desktops that ignore the paste keystroke get the text typed, from a small table like Modes per app; the rule is read from the app in front when the text lands. Changing the Paste method setting applies to the next paste, no restart.
- **GPU or CPU**: a switch in the memory strip. GPU keeps Whisper in the card's memory (VRAM on a PC, unified memory on a Mac); CPU moves it into system RAM and leaves the GPU alone, unloading it from the GPU at once. The strip names the adapter or the processor and shows the memory in use, so a PC reads "AMD Radeon RX 9070 · 16 GB VRAM" in GPU mode and the processor with 32 GB RAM in CPU mode.
- **Models**: the Q8_0 turbo (`large-v3-turbo-q8`, 874 MB, closer to the f16 original) is offered on machines with 12 GB or more; the menu bar has a Model submenu and **Load model now**.
- **Launch at login** and **Show the pill** switches (hotkeys work with the pill hidden).
- **Keep the microphone open** for a few seconds after a dictation (default 20 s, 0 to close at once): the next one starts without reopening the device.
- **First-run walkthrough**: the Setup tab is a numbered list (model, microphone, the macOS grants where they exist, dictation key, polish LLM, say something) with a state for each.
- **Crash report**: a panic writes `crash-<timestamp>.txt` next to the log with the version, OS, model and last pill state; Setup shows **Show report**.
- **Tokens in the OS keychain**: host, remote and LLM endpoint tokens live in the macOS Keychain or the Windows Credential Manager; `settings.json` only holds `@keychain`.
- **Windows, administrator windows**: Windows drops keystrokes sent to an app that runs as administrator. The pill warns while such a window is in front, and a dictation into it ends as "Copied (paste blocked)" with the text on the clipboard.

## Use your PC as the host

Any PrivateVox can do the work for another one on the same network. On the machine with the big GPU, open Settings → Network and switch on **Act as host for other machines** (port 8765, optional token); the card shows the address to type elsewhere. On the Mac, switch on **Use a host on this network**, enter that `ip:port`, press Test, then Apply, or press **Find hosts on this network** and Add the one it lists. From then on, while the host answers, what you dictate on the Mac is transcribed and polished on the PC; the memory strip shows the PC's figures; nothing is loaded on the Mac. When the PC is not reachable the Mac does the work itself and the pill says so, and it goes back to the PC as soon as it answers again.

Both machines keep a history: the PC stores what it processed (marked "host", with the client's name), the Mac stores its copy (marked "via PC"). The toggle in the History header shows the PC's list from the Mac.

## Download

Alpha builds for macOS (Apple Silicon), Windows and Linux are on the [Releases page](https://github.com/Drake0306/PrivateVox/releases); 0.6.0-alpha.1 is the current one. Read the release notes there for the one-time macOS permission steps; builds are unsigned for now.

## Build and run

Requirements: Rust 1.90+, Xcode Command Line Tools, `cmake`, and the Tauri CLI (`cargo install tauri-cli --version "^2" --locked`).

```bash
git clone https://github.com/Drake0306/PrivateVox.git
cd PrivateVox
cargo tauri dev            # from the repo root, or `cd src-tauri && cargo tauri dev`
```

The first build compiles whisper.cpp and the Tauri stack (several minutes). The first launch of a freshly built binary also compiles Metal shaders once (about 10 s before the pill goes idle).

### Windows

Needs Visual Studio 2022 Build Tools with the C++ workload, CMake, Ninja, the Vulkan SDK (`winget install KhronosGroup.VulkanSDK`, for the shader compiler; open a new terminal afterwards so `VULKAN_SDK` is set), LLVM (`winget install LLVM.LLVM`, bindgen needs `libclang.dll`), PowerShell 7 and the Tauri CLI. Then a plain `cargo tauri dev` from the repo root works: on Windows the Tauri CLI runs cargo through `scripts/win-cargo.cmd` (set as `build.runner` in `src-tauri/tauri.windows.conf.json`), a shim for `scripts/win-cargo.ps1`, which imports the MSVC developer environment, uses Ninja, builds into a short target directory (`D:\t` when a D: drive exists, else `%LOCALAPPDATA%\pvx-target`), and puts itself, cargo and the app into one Windows job that it closes when the shim is killed, so that stopping the dev session (Ctrl+C, or a rebuild on a file change) also stops the app. If a build ever says `failed to remove privatevox.exe: Access is denied`, an instance is still running: quit it from the tray. Without that, whisper.cpp's nested shader project fails: MSVC's build tracker cannot write under the long default `target\…` path and `cl.exe` is not on a plain terminal's PATH. `.cargo/config.toml` also points the build at `cmake` on PATH, because the CMake installer's "add to environment" option sets a `CMAKE` variable to a *folder*, which fails with "Access is denied".

For cargo commands other than `tauri dev` / `tauri build`, call the wrapper yourself:

```powershell
pwsh scripts/win-cargo.ps1 test -p privatevox --lib -- --test-threads=1
pwsh scripts/win-cargo.ps1 tauri build
```

Whisper runs through Vulkan, so any recent AMD, NVIDIA or Intel GPU with a current driver works; nothing is tied to one card.

### Permissions (one time)

macOS asks for three things. PrivateVox's **Setup** tab (click the pill, then Setup) has buttons that open each pane.

| Permission | Why | Where |
|---|---|---|
| Microphone | to record | prompt on first dictation |
| Accessibility | to send ⌘V into the front app | System Settings → Privacy & Security → Accessibility |
| Input Monitoring | to see the Right Option key | System Settings → Privacy & Security → Input Monitoring |

After granting Accessibility or Input Monitoring, quit PrivateVox from the menu bar and start it again. See `docs/dev/permissions.md` to reset grants while developing.

Host, remote and LLM endpoint tokens are kept in the macOS Keychain (Windows: Credential Manager); `settings.json` only holds `@keychain` in their place. A debug binary is not signed, so macOS asks for keychain access again after every rebuild; the bundled app (`cargo tauri build --debug`) is asked once. Grants and Keychain entries both key on the binary's identity, which is why developing from the bundled app is less noisy.

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

## Feedback and contributing

- Bugs and ideas: [GitHub issues](https://github.com/Drake0306/PrivateVox/issues). Say which OS and GPU, what you expected, and attach the log (`~/Library/Logs/com.drake0306.privatevox/privatevox.log` on macOS, `%LOCALAPPDATA%\com.drake0306.privatevox\logs\privatevox.log` on Windows). A crash leaves a `crash-<timestamp>.txt` next to the log; the Setup tab's "Show report" opens it.
- Pull requests are welcome. Keep the three test suites green (`cargo test` per crate as above, the app crate with `--test-threads=1`), run `node scripts/check-panel-js.mjs ui/panel.html ui/pill.html` for the panel, and add a test with each change; the design document in `docs/superpowers/specs/` says why things are the way they are.
- Where the project is and what is left: `docs/privatevox-roadmap.html` (open it in a browser).

## Credits

Several techniques are adopted from [Handy](https://github.com/cjpais/Handy) (MIT): the worker-process engine, VAD timing, chunking at pauses, the activation state machine and the receipt-sequenced paste design. Whisper by OpenAI, whisper.cpp by Georgi Gerganov and contributors, Silero VAD by the Silero team.

## Licence

MIT. See `LICENSE`.
