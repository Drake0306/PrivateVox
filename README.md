# PrivateVox

[![Release builds](https://github.com/Drake0306/PrivateVox/actions/workflows/release.yml/badge.svg)](https://github.com/Drake0306/PrivateVox/actions/workflows/release.yml)
[![Latest release](https://img.shields.io/github/v/release/Drake0306/PrivateVox?include_prereleases&label=release)](https://github.com/Drake0306/PrivateVox/releases)
[![Licence: MIT](https://img.shields.io/badge/licence-MIT-0f766e.svg)](LICENSE)
![Platforms](https://img.shields.io/badge/macOS%20%7C%20Windows%20%7C%20Linux-runs%20on%20your%20own%20machine-5b6b69)

**Talk instead of typing, in any app, without sending your voice anywhere.**

Hold a key, say what you want to write, let go. A second or two later the words appear where your cursor is, in your email, your chat, your editor, your browser. Everything happens on your own computer: the speech recognition runs on your graphics card, nothing is uploaded, and there is no account to create.

## How it works, in plain words

- You press and hold one key (Right Option on a Mac, Right Alt on Windows) and speak.
- When you let go, PrivateVox turns the speech into text using Whisper, the open speech model from OpenAI, running on your machine.
- The text is pasted into whatever app is in front, as if you had typed it.
- If you like, a small local AI tidies the text first: punctuation, capital letters, "um"s removed, or reshaped into an email or a bullet list. That step is optional and also runs on your machine.
- Every dictation is kept in a private history on your computer, so you can find it again, copy it, fix it, or delete it.

A small pill at the bottom of the screen shows what is happening: listening, transcribing, pasted. Click it to open the history and settings.

## Get started in three steps

1. **Download** the installer for your computer from the [Releases page](https://github.com/Drake0306/PrivateVox/releases). Mac: the `.dmg`. Windows: the `.exe` (or `.msi`). Linux: the AppImage, `.deb` or `.rpm`.
   These are early builds and are not yet signed, so the system will warn you the first time. On a Mac, right-click the app and choose Open. On Windows, click "More info", then "Run anyway".
2. **Start it.** The pill appears at the bottom of the screen and the speech model (about 570 MB) downloads once. On a Mac, open the Setup tab (click the pill) and grant the two permissions it asks for, then restart the app once. Windows needs no permissions.
3. **Dictate.** Put the cursor where you want text, hold **Right Option** (Mac) or **Right Alt** (Windows), speak, let go.

That is all. The rest of this page is for when you want more.

## What you can do with it

**Dictating**
- Hold to talk, or tap to start and tap again to stop; a tapped dictation also stops by itself after two seconds of silence.
- See the words appear on the pill while you speak, so you know it heard you.
- Say "new line", "new paragraph", "delete that" or "cancel" at the end of a sentence and it does what you said.
- A second key copies the text to the clipboard instead of pasting it, for when you want to paste it yourself.
- Undo the last paste with one key.

**Making the text read well (optional)**
- A small AI model on your machine cleans the text before it is pasted: punctuation, capitals, filler words removed.
- Modes: Clean, Email, Bullets, Code, Ask (answers your question instead of writing it down), Raw (exactly what you said), Translate to English.
- Per-app defaults: Mail always in Email mode, your code editor always in Code mode.
- Nothing is lost if the AI is slow or off: the plain transcript is pasted instead, and you can re-run the clean-up later from the history.

**Your words, your languages**
- A personal dictionary for names and jargon that generic speech recognition gets wrong.
- One language fixed, or several, with the language detected per dictation and recorded next to it.

**History**
- Search, edit, pin, copy, paste again, delete, export to Markdown or JSON.
- Filter by app or by mode; Clear all with a confirmation.
- Keep the audio of each dictation if you want, and run it again later with a better model.

**Choosing where the work runs**
- Three speech models: fast (default), most accurate, and an English-only one; a fourth, higher-precision fast model on machines with plenty of memory. Switch any time.
- A memory bar shows what each model needs, what your machine has, and what fits.
- GPU or CPU: keep the model on the graphics card, or move it to the main memory and leave the graphics card alone, with one switch.
- Use a stronger computer on your home network to do the heavy lifting while you dictate on a laptop.

**Quiet safety nets**
- The pill tells you when a paste cannot land: a password field on a Mac, or an app running as administrator on Windows. The text is still on your clipboard.
- A microphone test with a level meter, a numbered first-run checklist, a crash report you can open from Setup.
- Passwords and tokens for network features are kept in the system keychain, not in a text file.

## Privacy

Your voice never leaves your computer. The speech model and the optional clean-up model both run locally, or on another computer of yours on the same network if you set that up. There is no account, no telemetry and no cloud service behind this app. The history lives in a file on your disk; delete rows or clear it all whenever you like.

## What you need

PrivateVox does real work on your machine, so the hardware matters more than for most apps. These are the comfortable minimums.

**Mac**
- A Mac with Apple Silicon (M1 or newer) running macOS 12 or later. Intel Macs are not supported.
- 8 GB of memory works with the fast model. 16 GB is better, especially if you also run the clean-up AI.
- About 1 GB of free disk space for the speech model, more if you keep audio clips.

**Windows**
- Windows 10 or 11, 64-bit.
- A graphics card from the last few years with up-to-date drivers. AMD, NVIDIA and Intel all work; PrivateVox uses the Vulkan standard, so it is not tied to one brand. 4 GB of graphics memory or more is comfortable for the fast model. No suitable card? Switch to CPU mode and it runs in main memory instead, more slowly.
- 8 GB of main memory, 16 GB if you run the clean-up AI on the same machine.
- About 1 GB of free disk space.

**Linux**
- A desktop on X11 with Vulkan drivers installed. Builds exist but no one has run them by hand yet; expect rough edges, and tell us what you find.

**Any computer**
- A microphone. The built-in one is fine; a headset is better in a noisy room.
- For the optional clean-up: [Ollama](https://ollama.com) installed, with one small model pulled (see below).

### For the smoothest experience

- Keep your graphics drivers current. Most "it runs on the CPU and is slow" reports on Windows come down to old drivers.
- Give it memory. Close memory-hungry apps if the memory bar in the panel turns red; or pick a smaller model there.
- Grant the Mac permissions once and restart the app. Without Accessibility, the text is copied to the clipboard but cannot be pasted for you.
- Leave the app running in the menu bar or tray. The first dictation after a long pause takes a little longer while the model loads; turn on "Launch at login" in Settings and forget about it.
- Use a key combination instead of the lone Right Alt if your keyboard layout uses Right Alt to type accented letters (Settings → Keys).

## Running it

### Mac

1. Open the `.dmg`, drag PrivateVox into Applications, right-click it and choose Open the first time.
2. Click the pill, open **Setup**, and use the buttons to grant:

| Permission | Why it is needed |
|---|---|
| Microphone | to hear you |
| Accessibility | to paste the text into the app in front |
| Input Monitoring | to notice the Right Option key while another app is in front |

3. Quit PrivateVox from the menu bar and start it again. The Setup tab shows green when everything is in place.

### Windows

1. Run the `.exe` installer. If SmartScreen appears, choose "More info", then "Run anyway".
2. Start PrivateVox. It needs no special permissions. The first start downloads the speech model.
3. Hold Right Alt and talk. If an app runs as administrator (some system tools), Windows does not let other programs type into it; the pill will say so, and the text waits on your clipboard.

### Linux

Run the AppImage (make it executable first) or install the `.deb` / `.rpm`. The hold key needs X11; on Wayland set a key combination in Settings instead.

### The optional clean-up AI

1. Install [Ollama](https://ollama.com).
2. In a terminal, run `ollama pull qwen2.5:3b`. It is about 2 GB and good enough for punctuation and tidying.
3. Start PrivateVox. It finds Ollama by itself; the "Polish LLM" box in the panel shows it. Pick a mode in the panel header.

Have a bigger computer on your network? Run Ollama or LM Studio there, allow network access in its settings, and add its address under Settings → Polish. PrivateVox uses it when it answers and falls back to local when it does not.

### Using a stronger computer for everything

On the strong machine, turn on **Act as host for other machines** under Settings → Network. On the laptop, turn on **Use a host on this network**, enter the address the host shows (or press Find hosts), Test, Apply. From then on the laptop records and the host does the recognition and the clean-up. When the host is away, the laptop quietly does the work itself.

### If something looks wrong

- **"Copied, paste blocked"** on a Mac: Accessibility is not granted yet, or the app was rebuilt since you granted it. Setup → Accessibility.
- **Nothing heard**: Settings → Microphone → Test and watch the meter while you speak; pick another input if it stays flat.
- **The model does not fit**: the memory bar turns red. Pick a smaller model, close other apps, or switch to CPU mode on a PC.
- **It crashed**: the Setup tab offers "Show report". Attach that file to an issue.
- **Where files are**: on a Mac, `~/Library/Application Support/com.drake0306.privatevox/` (settings, history, models) and `~/Library/Logs/com.drake0306.privatevox/privatevox.log`; on Windows, `%APPDATA%\com.drake0306.privatevox\` and `%LOCALAPPDATA%\com.drake0306.privatevox\logs\privatevox.log`.

## Where the project stands

This is an alpha. macOS on Apple Silicon is where it has been used every day; Windows works and all its tests pass, with the first real use by a person on 31 October 2026; Linux is built but untested. What is built, what is left and what is not planned: open `docs/privatevox-roadmap.html` in a browser.

## For developers

### Building

You need Rust 1.90 or newer, CMake, and the Tauri CLI (`cargo install tauri-cli --version "^2" --locked`).

- **Mac**: Xcode Command Line Tools. Then:

  ```bash
  git clone https://github.com/Drake0306/PrivateVox.git
  cd PrivateVox
  cargo tauri dev
  ```

  The first build compiles whisper.cpp and Tauri (several minutes). A freshly built binary compiles Metal shaders once at first launch (about ten seconds before the pill goes idle). Debug builds lose the macOS Accessibility grant on every rebuild; develop from the bundled app (`cargo tauri build --debug`) when you need pasting to work.

- **Windows**: Visual Studio 2022 Build Tools with the C++ workload, CMake, Ninja, the Vulkan SDK (`winget install KhronosGroup.VulkanSDK`; open a new terminal afterwards), LLVM (`winget install LLVM.LLVM`, for the C bindings generator) and PowerShell 7. Then the same `cargo tauri dev` from the repo root works. Under the hood the Tauri CLI runs cargo through `scripts/win-cargo.cmd`, which sets up the MSVC environment, uses Ninja and builds into a short directory (`D:\t` if you have a D: drive, otherwise under `%LOCALAPPDATA%`), because whisper.cpp's shader step breaks on Windows' path length limit. For other cargo commands use the wrapper directly:

  ```powershell
  pwsh scripts/win-cargo.ps1 test -p privatevox --lib -- --test-threads=1
  pwsh scripts/win-cargo.ps1 tauri build
  ```

  If a build says `failed to remove privatevox.exe: Access is denied`, a PrivateVox is still running; quit it from the tray (the wrapper also clears leftovers from earlier dev sessions by itself).

- **Linux**: the packages the release workflow installs are the list to copy: WebKitGTK 4.1, libayatana-appindicator, librsvg, ALSA, libxdo, libxi, libxtst, libx11, libxkbcommon, Vulkan headers, cmake and pkg-config, plus the Vulkan SDK for the shader compiler.

### Tests

```bash
cargo test -p privatevox-core                          # settings, audio, VAD, hotkeys, history store, downloads
cargo test -p privatevox-engine -- --test-threads=1    # protocol, worker supervisor (counts worker processes)
cargo test -p privatevox --lib -- --test-threads=1     # the app: pipeline, paste, setup, memory, secrets
node scripts/check-panel-js.mjs ui/panel.html ui/pill.html   # panel JavaScript lint
```

Two more need hardware or files: `cargo test -p privatevox-core -- --ignored` uses the microphone, and the worker integration test needs `PRIVATEVOX_TEST_MODEL` pointing at a model file.

### Layout

- `crates/privatevox-core`: pure logic, no UI. Settings, audio conversion, voice activity detection, the hotkey state machine, text clean-up, the history database, memory figures, downloads.
- `crates/privatevox-engine`: the speech engine. The whisper.cpp backend, the worker process that runs it, and the supervisor that restarts it if it crashes or hangs.
- `src-tauri`: the app. Pill and panel windows, tray, hotkeys, paste, the pipeline that ties it together, host mode, keychain, crash reports.
- `ui/`: the pill and the panel, plain HTML, CSS and JavaScript.
- `docs/`: the design document, the plan and ledger for each phase, release notes and the roadmap page.

### Contributing

Issues and pull requests are welcome at [github.com/Drake0306/PrivateVox](https://github.com/Drake0306/PrivateVox). For a bug, say which OS and graphics card you have, what you expected, and attach the log file named above. For a change, keep the three test suites and the panel lint green and add a test with it; the design document in `docs/superpowers/specs/` explains why things are the way they are.

## Credits

Several techniques are adopted from [Handy](https://github.com/cjpais/Handy) (MIT): the worker-process engine, the voice-activity timing, chunking at pauses, the activation state machine and the paste design. Whisper is by OpenAI, whisper.cpp by Georgi Gerganov and contributors, Silero VAD by the Silero team.

## Licence

MIT. See `LICENSE`.
