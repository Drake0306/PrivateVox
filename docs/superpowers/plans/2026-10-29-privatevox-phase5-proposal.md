# PrivateVox Phase 5: candidates (proposal, 2026-10-29)

Everything in the original design through phase 4b is built on the Mac. This is the list of what could come next, for Roy to pick from. Nothing here is started. Each item says what the user gets, what it costs to build, whether it needs the PC, and a recommendation. Effort is in sessions of the kind we have been doing (plan, tests, review, live check).

## A. Recommended for phase 5 (all Mac)

### A1. Live preview while speaking **(from the design's v2 list)**
What you get: while you hold the key, the words appear in the pill (or a floating caption under it) as Whisper decodes each chunk, so you see it is hearing you right and can stop early. Off by default on 8 GB machines (it keeps the model busy), on by default when a host does the work.
How: the chunk worker already produces per-chunk text; emit it as `PillState::Listening { partial }` and render a two-line pill. For sub-chunk latency, decode a sliding 5 s window every 2 s on top of the chunker (extra GPU work; that is why it is off on small machines).
Effort: 1 session. Risk: low; it reuses the chunk pipeline.

### A2. Auto-stop on silence in toggle mode
What you get: tap to start, talk, stop talking, and after a pause you choose (default 2 s, Never possible) the dictation ends itself; no second tap. The pill shows the countdown in its last second.
How: the VAD already scores every frame; a trailing-silence counter in `tick` sends Stop. Settings: "Stop after N s of silence (toggle mode)".
Effort: half a session. Risk: low.

### A3. Spoken commands at the end of a dictation
What you get: say "new line", "new paragraph", "delete that", "undo", "cancel" and the app does it instead of typing the words. Only the last phrase of a dictation is checked, so normal text is never eaten.
How: a small pure matcher over the final tokens, per language (English first), with a settings toggle and an editable list.
Effort: 1 session. Risk: medium (false positives); mitigated by end-of-dictation only.

### A4. Per-app default modes
What you get: Mail opens in Email mode, VS Code and Terminal in Code mode, Slack in Clean, without touching the picker. A table in Settings: app name → mode; the pill shows the mode it chose.
How: `frontmost_app_name` already exists; a map in settings; the pipeline resolves the mode at Start.
Effort: half a session. Risk: low.

### A5. Sound and visual cues
What you get: a soft click when recording starts and stops, a different one on "Nothing heard", and a brief flash of the pill; all off-able. Useful when the pill is hidden or on another screen.
How: two short bundled sounds played through `afplay`/`rodio`; the pill already changes class per state.
Effort: half a session. Risk: none.

### A6. Deferred-minors cleanup
What you get: the twenty small items the three reviews left (the host dropping the dictionary prompt, mDNS re-advertising after a Wi-Fi change, a delete racing a clip write, host search paging and friends). Nothing visible on a good day; fewer surprises on a bad one.
Effort: 1 session. Risk: none.

## B. Worth measuring before deciding (Mac)

### B1. MLX engine on macOS **(from the design's v2 list)**
What you might get: faster large-v3 on Apple Silicon. MLX's Whisper runs well on M-series; whisper.cpp with Metal is already about one second per short clip on the M1 with turbo, so the gain is uncertain for turbo and likelier for large-v3.
How: a spike first (one afternoon): run mlx-whisper on the same five clips and compare with the spike numbers in `docs/superpowers/spikes/`. Only if it wins by 30 % or more does it become an `Engine` implementation behind the trait, selected per model in the picker. It would be a Python-free port (mlx-rs) or a sidecar; the sidecar is the cheap path.
Effort: spike half a day; engine 2 sessions if adopted. Risk: medium (new runtime, model conversions).

### B2. LLM translate mode
What you get: dictate in English, paste in German (or any language the LLM knows), chosen per mode. Today Translate only goes to English through Whisper.
How: one more prompt with a target-language setting; reuses the polish chain.
Effort: half a session. Risk: low. Decide if you actually need it.

## C. Later or with the PC

### C1. Windows pass and RX 9070 timing (needs you at the PC; already planned)
### C2. Auto-update and a signed, notarised Mac build
What you get: "A new version is available" in the panel; no more right-click-to-open. Needs an Apple Developer account (about 99 USD a year) and a Windows code-signing certificate for SmartScreen quiet; the updater itself is Tauri's built-in.
Effort: 1 session once the certificates exist. Risk: low, but it is money and paperwork first.
### C3. Host mode extras
Shared dictionary between host and client, the client's model picker driving the host's download, a client queue indicator on the host, settings pushed to the PC. Best decided after the first real PC-to-Mac run.
### C4. Wayland hold key on Linux, Linux tray polish
Only if someone on Linux asks.

## D. Not recommended now
- Snippets / text expansion: other tools do it better; out of scope for a dictation tool.
- Stats page (words per day, time saved): fun, low value; the history already has counts.
- Streaming the LLM's answer into the pill: the polish takes one second when warm; nothing to stream.
- Cloud anything: against the premise.

## Suggested phase 5 (if Roy agrees)
A1 live preview, A2 auto-stop, A4 per-app modes, A5 cues, A3 spoken commands, A6 cleanup, in that order, plus the B1 spike. About five sessions on the Mac. The PC day (C1) can happen any time in between.
