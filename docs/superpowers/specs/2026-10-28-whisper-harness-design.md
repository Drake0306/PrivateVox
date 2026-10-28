# Whisper Harness, design

Date: 2026-10-28
Status: draft for approval
Companion page (mockups, VRAM gauge, idea bank): `docs/whisper-harness-brainstorm.html`

Tags used below: **[user]** decided by the product owner during brainstorming, **[call]** a default chosen by the author that can be overridden, **[builder]** deliberately left to whoever implements it.

---

## 1. Intent

A local, always-running dictation tool. Hold a key, speak, release, and clean text appears at the cursor in whatever app is in front. Whisper runs on the user's own GPU and stays resident so the first word is never delayed by a model load. A small LLM pass turns raw speech into tidy text. Everything dictated is kept in a local history that can be searched, edited, copied, re-pasted and deleted.

Why it exists: existing tools are either cloud-bound (privacy, latency, subscription) or NVIDIA-only. The owner's machines are an AMD GPU Windows PC and an Apple Silicon Mac. One codebase must serve both, with the heavy lifting optionally shared over the home LAN.

## 2. Goals, non-goals, anti-goals

**Goals**
- Sub-3-second round trip from key release to pasted text for a 10-second utterance, on both target machines, with LLM polish on. Sub-1-second with polish off.
- Works in any foreground app on Windows 11 and macOS.
- Fully offline capable. Nothing leaves the machine unless the user points the app at another machine on their LAN.
- Three Whisper checkpoints installed and switchable at runtime: large-v3-turbo (default), large-v3 ("accuracy"), distil-large-v3 (English only). **[user]**
- Live view of GPU memory: total, in use by others, cost of the selected model and LLM, recommended and best-fitting model, projected usage on selection. **[user]**
- History panel with view, search, edit, copy, re-paste, delete, export. Model switcher and GPU strip live in the same panel. **[user]**
- Clean release of GPU and RAM on every kind of stop: quit, OS shutdown, force close, crash. **[user]**
- Idle unload after a user-configurable number of minutes. **[user]**

**Non-goals (v1)**
- Meeting recording, system-audio capture, speaker labels.
- Internet cloud LLMs. The connector could support them later since it uses the OpenAI-compatible chat shape, but v1 ships with no cloud provider configured and no API-key UI. **[call]** (The owner's "cloud" means another machine on the same network, see §8.)
- Mobile, Linux desktop. Linux is likely to work through Tauri and Vulkan but is not tested or packaged.
- A general note-taking app. History is a scratch log.

**Anti-goals (would make it a failure even if it works)**
- Any hotkey press that waits for a model to load when the model should already be warm.
- A force close that leaves VRAM held by an orphaned process.
- Text pasted into the wrong window with no way to take it back.
- Clipboard left holding the transcript after a paste.

## 3. Target hardware and constraints

| Machine | GPU / memory | Whisper backend | Local LLM runtime |
|---|---|---|---|
| Windows 11 PC **[user]** | AMD Radeon RX 9070, 16 GB VRAM (RDNA 4) | whisper.cpp with Vulkan. ROCm/HIP is a build-time option to benchmark once Vulkan works; recent ROCm releases list RDNA 4, but Vulkan is the path that is known to work on every driver. **[call]** | LM Studio or llama.cpp server (Vulkan or ROCm), or Ollama where its ROCm build supports the card. Any of them, via the OpenAI-compatible API. |
| Mac **[user]** | Apple M1 family, unified memory (exact model and memory unknown; assume 8 or 16 GB) | whisper.cpp with Metal, plus the optional Core ML encoder. mlx-whisper as a later alternative engine. | Ollama or LM Studio locally, or the Windows PC over LAN. |

Memory budget (measured once per model on first load; these are planning estimates):

| Resident | Estimate |
|---|---|
| large-v3-turbo, Q5_0, with working buffers | ~1.3 GB |
| large-v3, Q5_0, with working buffers | ~2.4 GB |
| distil-large-v3, Q5_0, with working buffers | ~1.2 GB |
| Polish LLM 3B class, Q4_K_M, 4k context | ~2.3 GB |
| Polish LLM 7–8B class, Q4_K_M, 4k context | ~5.0 GB |

On the RX 9070 the default stack is turbo plus an 8B polish model (~6.3 GB, under 40%). On an 8 GB M1 it is turbo plus a 3B model, or turbo locally with polish sent to the PC over LAN. The app only ever has one Whisper model resident; switching unloads the old one first. **[call]**

## 4. User-facing behaviour

### 4.1 The pill
- Frameless, transparent, always-on-top window, draggable anywhere, position remembered per display. Shown on all desktops/spaces.
- States: Idle (shows current hotkey), Listening (level meter + timer + current mode), Transcribing, Polishing, Pasted (first words of the result for ~2 s, then back to Idle), Loading model, Error (mic blocked, engine unreachable, paste failed).
- Click or right-click opens the panel (§4.4) anchored below the pill. The pill can be hidden from the tray; hotkeys keep working.

### 4.2 Hotkeys **[user]** with **[call]** defaults
- Push-to-talk: hold to record, release to transcribe. Default Right Option (macOS) / Right Alt (Windows).
- Toggle: tap to start, tap to stop. Default Ctrl+Shift+Space on both.
- Clipboard-only dictation: same as push-to-talk but no paste. Default Ctrl+Shift+Alt+Space / Ctrl+Shift+Option+Space.
- Undo last paste: Default Ctrl+Shift+Z while the pill is in Pasted state, or from the panel. Implementation: select the pasted span length backwards is not reliable across apps, so undo sends the platform undo keystroke once to the same frontmost app if it is still frontmost, and otherwise shows a "copy again" prompt. **[call]**
- All rebindable in Settings. Single-modifier holds use a low-level key hook (rdev); combinations use the Tauri global-shortcut plugin. Both paths produce the same `HotkeyEvent::{Down, Up, Tap}`.

### 4.3 Capture, transcribe, polish, paste
1. On hotkey down: start the audio stream (cpal, selected input device, 16 kHz mono f32), show Listening. If a model is not resident (idle-unloaded), start loading in parallel and show Loading; recording continues regardless.
2. On hotkey up or toggle stop: stop the stream. Clips under 300 ms are discarded with no action. **[call]**
   Maximum clip length in v1 is 5 minutes for both gestures. At 4:30 the pill shows a countdown; at 5:00 recording auto-stops and the clip is transcribed as normal. Long-form (v1.x) lifts this cap with chunking. The capture ring buffer is sized for 5 minutes at 16 kHz f32 (~19 MB). **[call]**
3. VAD (Silero via whisper.cpp's VAD support, or an equivalent) trims leading/trailing silence. Clips that are all silence are discarded with a short "Nothing heard" in the pill. **[call]**
4. Whisper transcribes with: selected model, language (auto or pinned), initial prompt built from the personal dictionary, translate flag if the Translate mode is active.
5. If the active mode has an LLM step and an LLM endpoint is reachable, send `{mode prompt, raw transcript}` and wait with a timeout (default 8 s). On timeout or error, fall back to the raw transcript and mark the history row "unpolished". **[call]**
   The warm-model rule applies to the LLM too. **[call]** Local runtimes unload on their own idle timers (Ollama after 5 min, LM Studio by TTL), so the app: (a) on hotkey down fires a tiny warm-up request to the active LLM endpoint in parallel with recording, so a cold model loads while the user is still speaking; (b) sends `keep_alive: -1` to Ollama and relies on the user setting LM Studio's TTL to "never" (the first-run flow explains this); (c) sends an explicit unload (`keep_alive: 0` on Ollama) from the same idle-unload path that frees Whisper, so the configurable idle timer governs both. The GPU strip counts the LLM as resident only when the endpoint is on this machine and `/v1/models` reports a loaded model.
6. Paste: save current clipboard to disk-backed store, write the text, send Cmd+V / Ctrl+V to the frontmost app, wait ~150 ms, restore the previous clipboard. Fallback "type it" mode (keystroke injection) per app, selectable in Settings.
7. Save the dictation row (§6). Show Pasted.

### 4.4 The panel
Single window with these regions, top to bottom:
- Header: title, search field, Whisper model dropdown (three entries; selecting loads that model and persists the choice), stats (count, words).
- GPU memory strip: detected adapter name and total memory; stacked bar (other apps, Whisper model, polish LLM, free); legend with GB values; three tiles: Selected (projected total, percent, verdict), Recommended (fastest that fits under 80% of total with the current LLM), Best accuracy that fits. Updates on model, LLM endpoint, or every 5 s while the panel is open. **[user]**
- Rows: time, app, mode, model, duration; polished text; actions Copy, Edit (inline, Enter saves, Esc cancels), Raw (toggle Whisper's original), Re-polish (choose mode), Paste again, Delete. Multi-line text preserved.
- Footer: filter by app/mode, export (Markdown, JSON), clear all with an in-page confirm.

### 4.5 Settings
- Hotkeys; mic device and level test; language (auto or pinned); modes and their prompts (editable); personal dictionary (list of words/phrases); keep audio clips on/off; idle unload minutes (1–240 or Never, default 10) **[user]**; launch at login; show pill; paste method per app; LLM endpoints (§8); engines (local, LAN); model manager (§4.6).
- Changing idle-unload takes effect immediately, no restart. The tray menu shows "Unloads in N min" while a model is resident and idle.

### 4.6 Model manager
- Lists the three Whisper checkpoints with download state, size, quantization, measured load time and speed (seconds of audio per second) from the last benchmark.
- Downloads from Hugging Face (ggml files from the whisper.cpp model repo) with progress and resume; verifies size/hash.
- Picks the quantization for the detected memory: Q5_0 by default; Q8_0 offered when total VRAM ≥ 12 GB. **[call]**
- "Benchmark" runs a fixed 30-second bundled sample through the model and records the numbers.

### 4.7 Tray / menu bar
Pause and unload, Resume, mode picker, model picker, open panel, open settings, launch at login toggle, Quit. Icon reflects state (idle, listening, unloaded, error).

### 4.8 First run
Permissions walkthrough (mic, Accessibility and Input Monitoring on macOS), mic test, download turbo, optional download of the other two, choose hotkey, detect LLM endpoints (§8.3), done.

## 5. Architecture

**Shell:** Tauri 2. UI in plain HTML/CSS/JS (no framework required; builder may add one). **[call]** Rust core as a set of modules behind Tauri commands and events:

| Module | Responsibility | Crates (suggested) |
|---|---|---|
| `audio` | device enumeration, capture stream, ring buffer, level meter | cpal |
| `vad` | silence trimming, "nothing heard" detection | whisper.cpp VAD or silero via onnxruntime |
| `engine` | `trait Engine { load(model), unload(), transcribe(audio, opts) -> Transcript, mem_usage() }`; implementations `LocalWhisperCpp` (whisper-rs, features metal/vulkan) and `RemoteHttp` (§8.4) | whisper-rs, reqwest |
| `polish` | mode prompts, OpenAI-compatible chat client, endpoint registry and health, fallback chain | reqwest |
| `hotkeys` | low-level hook and global-shortcut plugin unified into one event stream | rdev, tauri-plugin-global-shortcut |
| `paste` | clipboard save/restore, keystroke injection, per-app method, frontmost app name | arboard or tauri-plugin-clipboard-manager, enigo, platform APIs for frontmost app |
| `store` | SQLite (WAL), migrations, settings, history CRUD, export | tauri-plugin-sql or rusqlite |
| `gpu` | adapter name, total and used memory (DXGI on Windows, Metal/IOKit on macOS), measured model footprints | windows-rs, metal/objc2 |
| `lifecycle` | lock file, signal/close handlers, single unload path, idle timer, watchdog, crash report | ctrlc / platform handlers |
| `discovery` | mDNS browse/advertise for LAN endpoints | mdns-sd |

One `Engine` owner: a single `EngineManager` task holds the resident engine and is the only place that loads, swaps or frees it. Exit, idle unload, Pause, model switch and watchdog recovery all call `EngineManager::unload()`.

Whisper runs in-process. No detached helper. If a sidecar is ever added (e.g. mlx-whisper), it is spawned as a child with parent-death signalling (`PR_SET_PDEATHSIG` equivalent on macOS is a kqueue watch; on Windows a job object with kill-on-close).

## 6. Data model

SQLite, WAL mode, one transaction per dictation.

```
dictations(id, created_at, app_name, mode, model, engine_kind, language,
           duration_ms, raw_text, polished_text, polished_by (endpoint id or null),
           status ('ok'|'unpolished'|'failed'), audio_path (null unless keep-audio),
           pinned bool, deleted_at null)
settings(key, value_json)
models(id, name, file, quant, size_bytes, downloaded bool, load_ms, speed_x, mem_bytes)
endpoints(id, kind ('llm'|'whisper'), label, base_url, token (nullable), source ('manual'|'mdns'), last_ok_at, priority)
dictionary(id, phrase)
```

Export writes Markdown (one dictation per heading with metadata line) or JSON (rows as stored, minus audio paths unless requested).

## 7. LLM polish

Modes shipped: Raw (no LLM), Clean (default), Email, Bullets, Code, Translate (Whisper-native, no LLM), Ask (answer instead of transcript). Each LLM mode is a system prompt plus rules: keep the user's words, never add content, output only the text to paste, preserve spoken language unless the mode says otherwise. Prompts are editable in Settings; a "reset to default" exists.

Request shape: OpenAI-compatible `POST {base_url}/v1/chat/completions`, non-streaming, `temperature 0.2`, `max_tokens` sized to 2× the input tokens plus 64. Model name is per endpoint (whatever the server lists at `/v1/models`; the app shows that list and lets the user pick). **[call]**

Recommended polish models: Qwen2.5-7B-Instruct Q4_K_M on the RX 9070; Qwen2.5-3B-Instruct or Llama-3.2-3B-Instruct Q4_K_M on an 8 GB Mac. The app does not download LLMs itself in v1; it points at a runtime the user already runs. **[call]**

## 8. Local first, LAN second **[user]**

The owner's wording: local first; "cloud" means a machine on the same Wi-Fi/LAN reached by IP and port, and it should "work seamlessly".

### 8.1 Endpoint kinds
- **LLM endpoints**: any OpenAI-compatible server. Local candidates are probed on known ports: LM Studio `http://127.0.0.1:1234`, Ollama `http://127.0.0.1:11434`, llama.cpp server `http://127.0.0.1:8080`. LAN candidates come from mDNS or manual entry (`host:port`).
- **Whisper endpoints**: a remote Whisper Harness in "share engine" mode (§8.4), or a whisper.cpp server, both speaking `POST /v1/audio/transcriptions` (multipart audio, returns JSON text). Allows the Mac to use the PC's GPU.

### 8.2 Priority and fallback
Endpoints have a user-ordered priority list per kind. Default order after first run: LAN endpoints the user explicitly added or confirmed, then local, then none. The reason LAN goes first when configured: the user adds a LAN box precisely because it is the stronger GPU. **[call]** Runtime selection on every dictation:
1. Take the highest-priority endpoint whose last health check (≤ 10 s old) succeeded.
2. On request failure or timeout, mark it unhealthy, try the next, and surface a one-line notice in the pill ("PC unreachable, polished locally").
3. If no LLM endpoint is healthy, paste the raw transcript and mark the row "unpolished".
4. (v1.x, once remote Whisper exists) If no Whisper endpoint is healthy and the local engine is disabled, show Error and keep the audio in memory for one retry.

Scope note: in v1 the only Whisper engine is the local one, and the endpoint registry holds LLM endpoints only. The `RemoteHttp` engine, `endpoints.kind='whisper'`, the "local engine disabled" setting and the remote-Whisper handling in the GPU strip all arrive with share mode in v1.x. The `Engine` trait and the `endpoints` table are shaped for it from day one. **[call]**

Health checks: `GET /v1/models` with a 1 s timeout, every 10 s while the app is awake, immediately on network change. Unhealthy endpoints are retried with backoff up to 60 s.

### 8.3 Discovery
- Advertise: a harness in share mode registers `_whisperharness._tcp.local` with TXT records `role=whisper|llm|both`, `port`, `name`, `version`.
- Browse: all harnesses browse the same service type and show found peers in Settings with an "Add" button, prefilled. Discovered peers are not used until the user adds them. **[call]** Manual entry (`192.168.1.20:1234`) always works for runtimes that do not advertise (LM Studio, Ollama).
- "Seamless" in practice: once added, a LAN endpoint is used automatically when reachable and dropped automatically when not. Switching Wi-Fi networks triggers an immediate recheck. No user action day to day.

### 8.4 Share engine mode (remote engine pick)
A harness can expose its own Whisper engine on the LAN: Settings toggle "Share Whisper on this network", port default 8765, optional shared token. It serves `POST /v1/audio/transcriptions` and `GET /v1/models`, bound to LAN interfaces only, and advertises over mDNS. The LLM is not proxied; the peer talks to LM Studio/Ollama on the PC directly (user enables "serve on local network" in that runtime). **[call]** Scheduled after v1 (§13).

### 8.5 Security posture
LAN only, no internet listener, no authentication required by default on a home LAN, optional bearer token for both the shared Whisper endpoint and any LLM endpoint that needs one. Tokens stored in the OS keychain/credential manager. Audio sent to a LAN Whisper endpoint is not stored by the peer unless its own keep-audio is on.

## 9. GPU memory strip logic

- Total and used memory read from the platform (DXGI adapter memory info on Windows; on macOS unified memory, report system memory total and current used, labelled "shared").
- "Other apps" = used memory minus what this app has resident.
- Model costs = measured footprint from the last load of that model on this machine; planning estimates from §3 until measured.
- Selected = other + selected Whisper + LLM (0 if the LLM endpoint is on another machine or Off).
- Recommended = turbo if `other + turbo + llm ≤ 0.8 × total`, else distil if it fits, else "lower the LLM first".
- Best = v3 if it fits under 0.8 × total, else turbo, else distil, else none.
- Verdict thresholds: ≤ 80% good, 80–90% tight, > 90% will not fit comfortably (bar outlined red).
- Refresh every 5 s while visible and on every model/endpoint change.

## 10. Lifecycle and exit paths **[user]**

| Stop kind | Handling |
|---|---|
| Normal quit (tray Quit, Cmd+Q, window close) | cancel in-flight dictation, flush store, `EngineManager::unload()`, stop audio, release hotkey hooks, stop share server, remove lock file, exit. Target < 1 s. |
| OS-initiated (SIGTERM/SIGINT; Windows `WM_CLOSE`, `WM_QUERYENDSESSION`, console ctrl; logout/shutdown) | same path from the handler, 2 s deadline, then `process::exit`. |
| Force close (kill -9, End task, Force Quit) | no code runs; OS reclaims RAM, driver reclaims VRAM on handle close. In-process engine guarantees no orphan. |
| Crash (panic, driver fault, OOM) | panic hook writes a report (`crash-<ts>.txt`: version, OS, GPU, model, last state) and exits. Stale lock file on next launch triggers a one-line notice with "show report". No telemetry. |

Supporting rules: single engine owner (§5); SQLite WAL; lock file as heartbeat and single-instance guard; clipboard backup persisted to disk before paste and restored on next launch if a restore was pending; a watchdog on every transcribe call with a deadline of `15 s + 3 × clip duration` (so a 5-minute clip gets 15 min 15 s, a 10-second clip 45 s) that first requests cooperative cancellation through whisper.cpp's abort callback, waits 5 s, and only then drops and reloads the engine and tells the user **[call]**; idle timer (§4.5) and Pause use the same unload path.

## 11. Performance targets (to be replaced with measurements from the engine spike)

These are planning targets, not acceptance criteria. **[call]** The engine spike records actual numbers on both machines; if a target is missed the plan gains a contingency step (ROCm build, Core ML encoder made mandatory on Mac, Vulkan pipeline-cache warm-up at startup, Q8 vs Q5) rather than blocking release. The §2 goals hold the same status: they are what the design aims at, and the spike tells us how close we get.

| Step | Target |
|---|---|
| Hotkey down → Listening shown | < 50 ms |
| Model load, turbo Q5_0 | < 1.5 s Mac M1 Metal, < 1.5 s RX 9070 Vulkan |
| Transcribe 10 s clip, turbo | < 1 s on both |
| Transcribe 10 s clip, large-v3 | < 3 s on both |
| LLM polish, 100 words, 7–8B on RX 9070 | < 2 s |
| LLM polish, 100 words, 3B on M1 | < 2 s |
| Paste + clipboard restore | < 200 ms |
| Idle RAM of the shell with model unloaded | < 80 MB |

## 12. Platform specifics

**Windows 11, RX 9070:** build whisper.cpp with `GGML_VULKAN`. Provide an experimental ROCm/HIP build flag and a benchmark comparison in the engine spike; ship whichever is faster and stable, Vulkan if in doubt. Frontmost app via `GetForegroundWindow` + process name. Paste via `SendInput`. Admin-elevated targets cannot receive injected input; detect and tell the user. Hotkey hook needs no special permission.

**macOS, Apple Silicon:** build with Metal; optional Core ML encoder model downloaded alongside (first use compiles for the Neural Engine, can take a minute; show progress). Permissions: Microphone, Accessibility (paste and undo), Input Monitoring (single-modifier hotkey). Frontmost app via `NSWorkspace`. Notarised, hardened runtime, entitlements for mic.

Both: respect reduced motion, dark/light theme following the OS, high-DPI pill rendering.

## 13. Phasing

**v1 (first usable, then polish to shippable)**
1. Engine spike (throwaway): whisper.cpp + the three models on both machines; load time, speed, VRAM footprint, Vulkan vs ROCm on the RX 9070.
2. Tauri shell: pill, tray, settings window, SQLite, lifecycle (lock file, handlers, single unload path, idle timer).
3. Hotkey → capture → VAD → Whisper → paste, Raw and Clean modes, LLM endpoint auto-detect on localhost, LAN endpoint manual entry with fallback chain.
4. Panel: history CRUD, search, re-paste, export; model switcher; GPU memory strip.
5. Model manager and first-run flow.
6. Modes (Email, Bullets, Code, Translate, Ask), personal dictionary, undo last paste, silence trimming tuning, language pinning, keep audio clips.
7. Packaging for personal use: ad-hoc-signed .app on the Mac. Windows build, RX 9070 spike and Windows installer happen in a later pass when the owner is at the PC (§15).
8. Open-source release pass: licence, README, CI on macOS and Windows runners, notarised .dmg, Windows installer (MSI or NSIS via Tauri) bundling the Vulkan build, own name and icon.

**v1.x**
- Share engine mode and mDNS discovery (remote engine).
- Long-form notes: toggle mode without the 30 s expectation; chunked transcription; result opens in the panel as a document to copy or export.

**v2**
- Live preview while speaking (streaming partials, toggle, off by default on ≤ 8 GB).
- MLX engine on macOS behind the `Engine` trait.

**Parked (not picked):** per-app automatic modes, spoken commands, snippets, stats.

## 14. Decisions log

**[user]** Mac first, Windows when the owner is at the PC; personal use first, open source after; Three switchable Whisper models; switcher and GPU strip inside the history panel; configurable idle unload; full exit/crash cleanup; local-first LLM with LAN machines as the second tier reached by IP and port, seamless failover; Windows PC is an RX 9070 16 GB; Mac is Apple Silicon M1 family; the 12 idea-bank picks listed in §13.

**[call]** 5-minute clip cap in v1 with auto-stop; watchdog deadline scaled to clip length with cooperative cancel first; LLM kept warm alongside Whisper and unloaded by the same idle timer; remote Whisper client deferred to v1.x; §11 numbers are targets, not acceptance criteria; Tauri 2 shell and Rust core; whisper.cpp via whisper-rs, Vulkan on Windows and Metal on macOS; large-v3-turbo as the first-launch default; Q5_0 quantization by default; one resident model at a time; default hotkeys; 300 ms minimum clip; 8 s LLM timeout then raw fallback; LAN endpoints ordered before local once added; discovered peers need an explicit Add; share mode after v1; no internet cloud provider in v1; recommended/best thresholds at 80%/90%; SQLite schema in §6.

**[builder]** UI framework (or none); exact crate choices; VAD implementation; how model footprints are measured; mDNS library; window positioning details across multi-monitor setups; installer technology; crash report format; test strategy for paste injection across apps.

## 15. Development order, audience and licensing **[user]**

- **Mac first.** All v1 work is built and tested on the Mac. Windows work (Vulkan build, RX 9070 benchmarks, Windows installer, Windows paste/hotkey specifics) waits until the owner is at the PC and runs the build and spike scripts there; the Rust code is written cross-platform from the start so that step is a build-and-fix pass, not a port.
- **Personal first, open source next.** v1 is for the owner's own machines. Packaging in v1 is an ad-hoc-signed .app for the Mac. Notarisation, a Windows installer, and the first-run flow aimed at strangers come with the open-source release.
- **Open-source readiness** is a work item in its own right: pick a licence (MIT proposed **[call]**), README with build steps for both platforms, a CONTRIBUTING note, CI that builds on macOS and Windows runners, and a distinct name and icon (see §16 on why).

## 16. Prior art, and build versus fork

Checked on 2026-10-28. The closest existing projects:

| Project | Stack | Licence | State | Overlap with this design | Missing versus this design |
|---|---|---|---|---|---|
| **Handy** (cjpais/Handy) | Tauri 2, Rust, React UI, whisper GGML via ggml, Silero VAD, Vulkan on Windows, Parakeet models | MIT (name, logo and brand are not open; forks must rebrand) | ~35k stars, v0.9.8 released 2026-10-03, active | Hold and toggle hotkeys, paste into any app, recording overlay, history with audio playback, model manager with Hugging Face downloads, LLM post-processing over an HTTP API, GPU detection, streaming models | GPU-memory strip with recommended/best, LAN endpoint failover and discovery, share-engine mode, configurable idle unload timer, three fixed Whisper tiers (distil must be added as a custom model), documented exit/crash lifecycle |
| **Wisper** (hudsonbrendon/wisper) | Tauri 2, Rust, whisper-rs, Metal only | MIT | New, 0 stars, 159 commits | Push-to-talk and hands-free, type or paste injection, pill overlay, history with stats, model downloads | No AMD/Vulkan, no LLM cleanup of dictation, no LAN, no VRAM view |
| **Whispering** (epicenter-so/epicenter) | Tauri 2, Svelte | AGPL-3.0 | Part of the Epicenter monorepo, ~4.8k stars | Shortcuts, local whisper.cpp, chained transformations, local and cloud providers | Copyleft licence, broad scope, no GPU-memory view or LAN failover |
| **VoiceInk** (Beingpax/VoiceInk) | Swift, whisper.cpp, macOS only | GPL-3.0, paid binary | ~5k stars | Polished Mac dictation with AI enhancement | macOS only, Swift, GPL |
| **OpenWhispr** | Electron, whisper.cpp | MIT, freemium | Active | Cross-platform push-to-talk with AI cleanup | Electron footprint, commercial tier |

Conclusion: Handy already does roughly two thirds of v1 on the same stack this design chose. The things it does not do are exactly the things the owner asked for by name: the GPU-memory strip with recommendations, local-first with seamless LAN failover, the configurable idle unload, the exit/crash contract, and later the share-engine mode.

Two ways forward, for the owner to choose:

- **A. Own codebase, borrow from Handy.** Build to this design, and lift proven, MIT-licensed pieces from Handy with attribution where they save weeks: the paste reliability work, the overlay positioning code, Vulkan build configuration, Hugging Face download with mirror fallback. Keeps the architecture (single engine owner, endpoint registry, lifecycle) clean and the project ours to open source under its own name. Slower to first working build.
- **B. Fork Handy, rebrand, add the missing features.** A working dictation app on day one; the design's new parts become features on top. Costs: inherit a React/TypeScript UI and a codebase shaped by someone else's decisions, carry the burden of tracking upstream or diverging, and the open-source story becomes "a Handy fork with X" rather than a project of its own.

Author's recommendation: **A**, with Handy kept open in a second window as the reference implementation and the first place to look when a platform quirk bites. **[call]**

## 17. Open items the owner may still want to decide
- Build versus fork (§16).
- Exact Mac model and memory (changes the default polish model on the Mac and whether LAN polish should be the default there).
- Whether the Mac should default to using the PC's Whisper over LAN when both are on, or always transcribe locally and only send polish to the PC. Current design: local Whisper on each machine, polish wherever the priority list says. **[call]**
